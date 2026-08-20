use std::{path::PathBuf, sync::Arc};

use tokio::sync::{
    OnceCell,
    mpsc::{UnboundedSender, unbounded_channel},
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    config::{Config, LanguageCode},
    error::{AppError, Result},
    event::TaskEvent,
    media::{TrackIndex, plan_audio_chunks, probe_duration, probe_media},
    output::build_output_path,
    services::Services,
    stt::SttInput,
    subtitle::{
        EmbeddedSubtitleSource, ExternalSubtitleSource, SubtitleDocument, SubtitleOutputMode,
        SubtitleSource,
    },
};

use super::{
    stt::{ChunkedSttResults, document_from_stt_result},
    subtitle::{TranslationContext, translate_document},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubtitleInput {
    Auto,
    Embedded(TrackIndex),
    External(PathBuf),
    Stt,
}

#[derive(Clone, Debug)]
pub struct PipelineJob {
    /// Optional only for a standalone external subtitle source.
    pub video: Option<PathBuf>,
    pub input: SubtitleInput,
    pub source_language: LanguageCode,
    pub target_language: LanguageCode,
    pub output_mode: SubtitleOutputMode,
    pub config: Config,
}

impl PipelineJob {
    pub fn external_subtitle_path(&self) -> Option<&std::path::Path> {
        match &self.input {
            SubtitleInput::External(path) => Some(path),
            SubtitleInput::Auto | SubtitleInput::Embedded(_) | SubtitleInput::Stt => None,
        }
    }

    fn video_required(&self) -> Result<&std::path::Path> {
        self.video.as_deref().ok_or_else(|| {
            AppError::InvalidConfig("a video path is required for embedded subtitles or STT".into())
        })
    }
}

#[derive(Clone, Default)]
pub(crate) struct BatchOverwrite {
    decision: Arc<OnceCell<bool>>,
}

impl BatchOverwrite {
    async fn confirm(
        &self,
        output: &std::path::Path,
        events: &UnboundedSender<TaskEvent>,
        cancellation: &CancellationToken,
    ) -> Result<bool> {
        let decision = self
            .decision
            .get_or_try_init(|| confirm_batch_overwrite(output, events, cancellation))
            .await?;
        Ok(*decision)
    }
}

pub async fn run_pipeline(
    job: PipelineJob,
    services: Arc<Services>,
    cancellation: CancellationToken,
    events: UnboundedSender<TaskEvent>,
) -> Result<PathBuf> {
    run_pipeline_inner(job, services, cancellation, events, None).await
}

pub(crate) async fn run_pipeline_with_batch_overwrite(
    job: PipelineJob,
    services: Arc<Services>,
    cancellation: CancellationToken,
    events: UnboundedSender<TaskEvent>,
    batch_overwrite: BatchOverwrite,
) -> Result<PathBuf> {
    run_pipeline_inner(job, services, cancellation, events, Some(&batch_overwrite)).await
}

async fn run_pipeline_inner(
    job: PipelineJob,
    services: Arc<Services>,
    cancellation: CancellationToken,
    events: UnboundedSender<TaskEvent>,
    batch_overwrite: Option<&BatchOverwrite>,
) -> Result<PathBuf> {
    check_cancelled(&cancellation)?;
    let mut document = load_document(&job, &services, &cancellation, &events).await?;
    check_cancelled(&cancellation)?;
    let output = build_output_path(
        job.video.as_deref(),
        job.external_subtitle_path(),
        &job.target_language,
        document.format,
    )?;
    let overwrite = if output.exists() {
        let approved = match batch_overwrite {
            Some(batch_overwrite) => {
                batch_overwrite
                    .confirm(&output, &events, &cancellation)
                    .await?
            }
            None => confirm_overwrite(&output, &events, &cancellation).await?,
        };
        if approved {
            true
        } else {
            return Err(if batch_overwrite.is_some() {
                AppError::Skipped(output)
            } else {
                AppError::OutputExists(output)
            });
        }
    } else {
        false
    };
    if job.output_mode.needs_translation() {
        let translator = services
            .translator
            .as_deref()
            .ok_or(AppError::MissingConfiguration("SUBFLUX_TRANSLATOR_API_KEY"))?;
        translate_document(
            &mut document,
            TranslationContext {
                source_language: &job.source_language,
                target_language: &job.target_language,
                chunk_size: job.config.translator.chunk_size,
                context_before: job.config.translator.context_before,
                context_after: job.config.translator.context_after,
                max_retries: job.config.translator.max_retries,
                translator,
                cancellation: &cancellation,
                events: &events,
            },
        )
        .await?;
    }
    check_cancelled(&cancellation)?;
    send(&events, TaskEvent::Writing);
    let output = services
        .subtitle_writer
        .write(&document, &output, job.output_mode, overwrite)
        .await?;
    debug!(output = %output.display(), "subtitle output pipeline completed");
    Ok(output)
}

async fn confirm_overwrite(
    output: &std::path::Path,
    events: &UnboundedSender<TaskEvent>,
    cancellation: &CancellationToken,
) -> Result<bool> {
    let (response, mut responses) = unbounded_channel();
    if events
        .send(TaskEvent::OverwriteRequested {
            output: output.to_path_buf(),
            response,
        })
        .is_err()
    {
        return Err(AppError::OutputExists(output.to_path_buf()));
    }
    tokio::select! {
        decision = responses.recv() => decision.ok_or(AppError::Cancelled),
        () = cancellation.cancelled() => Err(AppError::Cancelled),
    }
}

async fn confirm_batch_overwrite(
    output: &std::path::Path,
    events: &UnboundedSender<TaskEvent>,
    cancellation: &CancellationToken,
) -> Result<bool> {
    check_cancelled(cancellation)?;
    let (response, mut responses) = unbounded_channel();
    if events
        .send(TaskEvent::BatchOverwriteRequested {
            output: output.to_path_buf(),
            response,
        })
        .is_err()
    {
        return Err(AppError::OutputExists(output.to_path_buf()));
    }
    tokio::select! {
        decision = responses.recv() => decision.ok_or(AppError::Cancelled),
        () = cancellation.cancelled() => Err(AppError::Cancelled),
    }
}

async fn load_document(
    job: &PipelineJob,
    services: &Services,
    cancellation: &CancellationToken,
    events: &UnboundedSender<TaskEvent>,
) -> Result<SubtitleDocument> {
    match &job.input {
        SubtitleInput::External(path) => {
            send(events, TaskEvent::ExtractingSubtitle);
            ExternalSubtitleSource::new(path).load().await
        }
        SubtitleInput::Stt => load_stt_document(job, services, cancellation, events).await,
        SubtitleInput::Auto | SubtitleInput::Embedded(_) => {
            let video = job.video_required()?;
            send(events, TaskEvent::Probing);
            let probe = probe_media(video).await?;
            send(events, TaskEvent::TracksLoaded(probe.clone()));
            let track = match job.input {
                SubtitleInput::Auto => probe.auto_track().cloned(),
                SubtitleInput::Embedded(index) => probe.track(index).cloned(),
                SubtitleInput::External(_) | SubtitleInput::Stt => unreachable!(),
            };
            match track {
                Some(track) if track.is_text() => {
                    send(events, TaskEvent::ExtractingSubtitle);
                    EmbeddedSubtitleSource::new(
                        video,
                        track,
                        services.ffmpeg.clone(),
                        cancellation.clone(),
                    )
                    .load()
                    .await
                }
                Some(track) => Err(AppError::UnsupportedSubtitleCodec(format!(
                    "{}：当前字幕轨为图像字幕，不支持直接翻译。请选择 STT 模式。",
                    track.codec
                ))),
                None if matches!(job.input, SubtitleInput::Auto) => {
                    // Auto's documented fallback: no usable text track means STT.
                    load_stt_document(job, services, cancellation, events).await
                }
                None => Err(AppError::ProbeFailed(
                    "selected subtitle track no longer exists in this video".into(),
                )),
            }
        }
    }
}

async fn load_stt_document(
    job: &PipelineJob,
    services: &Services,
    cancellation: &CancellationToken,
    events: &UnboundedSender<TaskEvent>,
) -> Result<SubtitleDocument> {
    let video = job.video_required()?;
    send(events, TaskEvent::Probing);
    let duration = probe_duration(video).await?;
    let chunks = plan_audio_chunks(
        duration,
        job.config.stt.chunk_seconds,
        job.config.stt.chunk_overlap_seconds,
    );
    if chunks.is_empty() {
        return Err(AppError::SttError(
            "media has no audio duration available for speech recognition".into(),
        ));
    }
    let language = if job.source_language != LanguageCode::auto() {
        Some(job.source_language.clone())
    } else if job.config.stt.language != LanguageCode::auto() {
        Some(job.config.stt.language.clone())
    } else {
        None
    };
    let stt = services
        .stt
        .as_ref()
        .ok_or(AppError::MissingConfiguration("SUBFLUX_STT_API_KEY"))?;
    let total = chunks.len();
    let mut results = ChunkedSttResults::default();
    for (index, chunk) in chunks.iter().enumerate() {
        check_cancelled(cancellation)?;
        send(events, TaskEvent::ExtractingAudio);
        let audio = services
            .ffmpeg
            .extract_audio_chunk(video, chunk, cancellation)
            .await?;
        check_cancelled(cancellation)?;
        send(
            events,
            TaskEvent::SttStarted {
                current: index,
                total,
            },
        );
        let result = stt
            .transcribe(
                SttInput {
                    audio_path: audio.path().to_path_buf(),
                    language: language.clone(),
                },
                cancellation,
            )
            .await?;
        results.absorb(result, chunk);
        send(
            events,
            TaskEvent::SttProgress {
                current: index + 1,
                total: Some(total),
            },
        );
    }
    document_from_stt_result(results.finish())
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}

fn send(sender: &UnboundedSender<TaskEvent>, event: TaskEvent) {
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path, sync::Arc};

    use async_trait::async_trait;
    use tokio::process::Command;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::{
        config::LanguageCode,
        media::{Ffmpeg, check_tools},
        services::Services,
        stt::{SttProvider, SttResult},
        subtitle::{FileSubtitleWriter, SpeechSegment, SubtitleOutputMode, SubtitleWriter},
        translator::{TranslationItem, TranslationRequest, TranslationResponse, Translator},
    };

    use super::*;

    struct MockTranslator;

    struct FlacCheckingStt;

    #[async_trait]
    impl SttProvider for FlacCheckingStt {
        async fn transcribe(
            &self,
            input: SttInput,
            _cancellation: &CancellationToken,
        ) -> Result<SttResult> {
            assert_eq!(
                input
                    .audio_path
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("flac")
            );
            Ok(SttResult {
                language: Some(LanguageCode::parse("ja").unwrap()),
                segments: vec![SpeechSegment {
                    start_ms: 100,
                    end_ms: 900,
                    text: "テスト".into(),
                }],
            })
        }
    }

    #[async_trait]
    impl Translator for MockTranslator {
        async fn translate(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            Ok(TranslationResponse {
                entries: request
                    .segments
                    .into_iter()
                    .map(|entry| TranslationItem {
                        id: entry.id,
                        text: format!("译文: {}", entry.text),
                    })
                    .collect(),
            })
        }
    }

    #[tokio::test]
    async fn external_ass_pipeline_writes_next_to_associated_video() {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("movie.mkv");
        let source = directory.path().join("movie.ja.ass");
        tokio::fs::write(&video, []).await.unwrap();
        tokio::fs::write(
            &source,
            concat!(
                "[Script Info]\nTitle: untouched\n\n[Events]\n",
                "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
                "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8}hello\n"
            ),
        )
        .await
        .unwrap();
        let config = Config::from_map(&HashMap::new()).unwrap();
        let services = Arc::new(Services {
            translator: Some(Arc::new(MockTranslator)),
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();
        let output = run_pipeline(
            PipelineJob {
                video: Some(video),
                input: SubtitleInput::External(source),
                source_language: LanguageCode::parse("en").unwrap(),
                target_language: LanguageCode::parse("zh-CN").unwrap(),
                output_mode: SubtitleOutputMode::Translated,
                config,
            },
            services,
            CancellationToken::new(),
            events,
        )
        .await
        .unwrap();
        assert_eq!(output, directory.path().join("movie.zh-CN.ass"));
        let written = tokio::fs::read_to_string(&output).await.unwrap();
        assert!(written.contains("Title: untouched"));
        assert!(written.contains("{\\an8}译文: hello"));
        assert!(received_events.try_recv().is_ok());
    }

    #[tokio::test]
    async fn original_output_writes_source_without_a_translator() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("movie.ja.srt");
        let source_content = "1\n00:00:01,000 --> 00:00:02,000\nhello\n";
        tokio::fs::write(&source, source_content).await.unwrap();
        let config = Config::from_map(&HashMap::new()).unwrap();
        let services = Arc::new(Services {
            translator: None,
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();
        let output = run_pipeline(
            PipelineJob {
                video: None,
                input: SubtitleInput::External(source),
                source_language: LanguageCode::parse("en").unwrap(),
                target_language: LanguageCode::parse("zh-CN").unwrap(),
                output_mode: SubtitleOutputMode::Original,
                config,
            },
            services,
            CancellationToken::new(),
            events,
        )
        .await
        .unwrap();

        assert_eq!(output, directory.path().join("movie.zh-CN.srt"));
        assert_eq!(
            tokio::fs::read_to_string(output).await.unwrap(),
            source_content
        );
        let emitted: Vec<_> = std::iter::from_fn(|| received_events.try_recv().ok()).collect();
        assert!(
            emitted
                .iter()
                .all(|event| !matches!(event, TaskEvent::TranslationStarted { .. }))
        );
    }

    #[tokio::test]
    async fn existing_output_waits_for_overwrite_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("movie.ja.srt");
        let output = directory.path().join("movie.zh-CN.srt");
        let source_content = "1\n00:00:01,000 --> 00:00:02,000\nhello\n";
        tokio::fs::write(&source, source_content).await.unwrap();
        tokio::fs::write(&output, "old output").await.unwrap();
        let config = Config::from_map(&HashMap::new()).unwrap();
        let services = Arc::new(Services {
            translator: None,
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();
        let task = tokio::spawn(run_pipeline(
            PipelineJob {
                video: None,
                input: SubtitleInput::External(source),
                source_language: LanguageCode::parse("en").unwrap(),
                target_language: LanguageCode::parse("zh-CN").unwrap(),
                output_mode: SubtitleOutputMode::Original,
                config,
            },
            services,
            CancellationToken::new(),
            events,
        ));

        assert!(matches!(
            received_events.recv().await,
            Some(TaskEvent::ExtractingSubtitle)
        ));
        let TaskEvent::OverwriteRequested {
            output: requested,
            response,
        } = received_events.recv().await.unwrap()
        else {
            panic!("expected overwrite prompt");
        };
        assert_eq!(requested, output);
        response.send(true).unwrap();

        assert_eq!(task.await.unwrap().unwrap(), output);
        assert_eq!(
            tokio::fs::read_to_string(output).await.unwrap(),
            source_content
        );
    }

    #[tokio::test]
    async fn rejected_batch_overwrite_skips_an_existing_output() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("movie.ja.srt");
        let output = directory.path().join("movie.zh-CN.srt");
        tokio::fs::write(&source, "1\n00:00:01,000 --> 00:00:02,000\nhello\n")
            .await
            .unwrap();
        tokio::fs::write(&output, "old output").await.unwrap();
        let services = Arc::new(Services {
            translator: None,
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();
        let task = tokio::spawn(run_pipeline_with_batch_overwrite(
            PipelineJob {
                video: None,
                input: SubtitleInput::External(source),
                source_language: LanguageCode::parse("en").unwrap(),
                target_language: LanguageCode::parse("zh-CN").unwrap(),
                output_mode: SubtitleOutputMode::Original,
                config: Config::from_map(&HashMap::new()).unwrap(),
            },
            services,
            CancellationToken::new(),
            events,
            BatchOverwrite::default(),
        ));

        assert!(matches!(
            received_events.recv().await,
            Some(TaskEvent::ExtractingSubtitle)
        ));
        let TaskEvent::BatchOverwriteRequested { response, .. } =
            received_events.recv().await.unwrap()
        else {
            panic!("expected batch overwrite prompt");
        };
        response.send(false).unwrap();

        assert!(matches!(
            task.await.unwrap(),
            Err(AppError::Skipped(path)) if path == output
        ));
        assert_eq!(
            tokio::fs::read_to_string(output).await.unwrap(),
            "old output"
        );
    }

    #[tokio::test]
    async fn batch_overwrite_confirmation_is_requested_once() {
        let overwrite = BatchOverwrite::default();
        let cancellation = CancellationToken::new();
        let (events, mut received_events) = unbounded_channel();
        let first = {
            let overwrite = overwrite.clone();
            let events = events.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                overwrite
                    .confirm(Path::new("first.zh-CN.srt"), &events, &cancellation)
                    .await
            })
        };
        let TaskEvent::BatchOverwriteRequested { response, .. } =
            received_events.recv().await.unwrap()
        else {
            panic!("expected one batch overwrite prompt");
        };
        let second = {
            let overwrite = overwrite.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                overwrite
                    .confirm(Path::new("second.zh-CN.srt"), &events, &cancellation)
                    .await
            })
        };

        response.send(true).unwrap();
        assert!(first.await.unwrap().unwrap());
        assert!(second.await.unwrap().unwrap());
        assert!(received_events.try_recv().is_err());
    }

    #[tokio::test]
    async fn stt_extracts_a_flac_fragment_before_transcribing() {
        if !check_tools().is_ready() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("audio.mkv");
        let generated = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:duration=2",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&video)
            .output()
            .await
            .unwrap();
        assert!(
            generated.status.success(),
            "fixture generation failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );

        let config = Config::from_map(&HashMap::new()).unwrap();
        let job = PipelineJob {
            video: Some(video),
            input: SubtitleInput::Stt,
            source_language: LanguageCode::auto(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config,
        };
        let services = Services {
            translator: Some(Arc::new(MockTranslator)),
            stt: Some(Arc::new(FlacCheckingStt)),
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        };
        let (events, mut received_events) = unbounded_channel();
        let document = load_stt_document(&job, &services, &CancellationToken::new(), &events)
            .await
            .unwrap();

        assert_eq!(document.entries.len(), 1);
        assert_eq!(document.entries[0].translatable_text, "テスト");
        assert!(matches!(received_events.try_recv(), Ok(TaskEvent::Probing)));
        assert!(matches!(
            received_events.try_recv(),
            Ok(TaskEvent::ExtractingAudio)
        ));
        assert!(matches!(
            received_events.try_recv(),
            Ok(TaskEvent::SttStarted {
                current: 0,
                total: 1
            })
        ));
        assert!(matches!(
            received_events.try_recv(),
            Ok(TaskEvent::SttProgress {
                current: 1,
                total: Some(1)
            })
        ));
    }
}
