use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::{
    Mutex, OnceCell, OwnedMutexGuard,
    mpsc::{UnboundedSender, unbounded_channel},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::{Config, LanguageCode},
    error::{AppError, Result},
    event::{CheckpointPhase, TaskEvent},
    media::{SubtitleTrack, TrackIndex, plan_audio_chunks, probe_duration, probe_media},
    output::build_output_path,
    services::Services,
    stt::SttInput,
    subtitle::{
        EmbeddedSubtitleSource, ExternalSubtitleSource, SubtitleDocument, SubtitleFormat,
        SubtitleOutputMode, SubtitleSource,
    },
};

use super::{
    checkpoint::{
        CheckpointIdentity, CheckpointStore, SttCheckpointSettings, TranslatorCheckpointSettings,
        fingerprint,
    },
    stt::{ChunkedSttResults, document_from_stt_result},
    subtitle::{
        TranslationContext, checkpoint_translations_valid, translate_document_with_checkpoint,
    },
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
    output_locks: OutputLocks,
}

#[derive(Clone, Default)]
struct OutputLocks {
    locks: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>,
}

impl OutputLocks {
    async fn lock(
        &self,
        output: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> Result<OwnedMutexGuard<()>> {
        let lock = {
            let mut locks = self.locks.lock().await;
            Arc::clone(
                locks
                    .entry(output.to_path_buf())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        tokio::select! {
            guard = lock.lock_owned() => Ok(guard),
            () = cancellation.cancelled() => Err(AppError::Cancelled),
        }
    }
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

    async fn lock_output(
        &self,
        output: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> Result<OwnedMutexGuard<()>> {
        self.output_locks.lock(output, cancellation).await
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
    let resolved = resolve_input(&job, &events).await?;
    check_cancelled(&cancellation)?;
    let output = build_output_path(
        job.video.as_deref(),
        job.external_subtitle_path(),
        &job.target_language,
        resolved.format(),
    )?;
    let _output_lock = match batch_overwrite {
        Some(batch_overwrite) => Some(batch_overwrite.lock_output(&output, &cancellation).await?),
        None => None,
    };
    check_cancelled(&cancellation)?;
    let overwrite = confirm_output(&output, &events, &cancellation, batch_overwrite).await?;

    let identity = checkpoint_identity(&job, &resolved)?;
    let mut checkpoint = CheckpointStore::load(&output, identity).await?;
    let mut document = load_document(
        &job,
        &resolved,
        &services,
        &cancellation,
        &events,
        Some(&mut checkpoint),
    )
    .await?;
    if job.output_mode.needs_translation()
        && !checkpoint_translations_valid(
            &document,
            &job.source_language,
            &job.target_language,
            job.config.translator.chunk_size,
            job.config.translator.context_before,
            job.config.translator.context_after,
            &checkpoint,
        )?
    {
        checkpoint.clear().await?;
        if resolved.is_stt() {
            document = load_document(
                &job,
                &resolved,
                &services,
                &cancellation,
                &events,
                Some(&mut checkpoint),
            )
            .await?;
        }
    }
    if job.output_mode.needs_translation() {
        let translator = services
            .translator
            .as_deref()
            .ok_or(AppError::MissingConfiguration("SUBFLUX_TRANSLATOR_API_KEY"))?;
        translate_document_with_checkpoint(
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
            &mut checkpoint,
        )
        .await?;
    }
    check_cancelled(&cancellation)?;
    send(&events, TaskEvent::Writing);
    let output = services
        .subtitle_writer
        .write(&document, &output, job.output_mode, overwrite)
        .await?;
    if let Err(error) = checkpoint.remove_after_success().await {
        warn!(error = %error.safe_message(), "could not remove completed checkpoint");
    }
    debug!(output = %output.display(), "subtitle output pipeline completed");
    Ok(output)
}

async fn confirm_output(
    output: &std::path::Path,
    events: &UnboundedSender<TaskEvent>,
    cancellation: &CancellationToken,
    batch_overwrite: Option<&BatchOverwrite>,
) -> Result<bool> {
    if !output.exists() {
        return Ok(false);
    }
    let approved = match batch_overwrite {
        Some(batch_overwrite) => {
            batch_overwrite
                .confirm(output, events, cancellation)
                .await?
        }
        None => confirm_overwrite(output, events, cancellation).await?,
    };
    if approved {
        Ok(true)
    } else if batch_overwrite.is_some() {
        Err(AppError::Skipped(output.to_path_buf()))
    } else {
        Err(AppError::OutputExists(output.to_path_buf()))
    }
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

#[derive(Clone, Debug)]
enum ResolvedInput {
    External {
        path: PathBuf,
        format: SubtitleFormat,
    },
    Embedded {
        track: SubtitleTrack,
        format: SubtitleFormat,
    },
    Stt,
}

impl ResolvedInput {
    const fn format(&self) -> SubtitleFormat {
        match self {
            Self::External { format, .. } | Self::Embedded { format, .. } => *format,
            Self::Stt => SubtitleFormat::Srt,
        }
    }

    const fn is_stt(&self) -> bool {
        matches!(self, Self::Stt)
    }

    fn external_path(&self) -> Option<&Path> {
        match self {
            Self::External { path, .. } => Some(path),
            Self::Embedded { .. } | Self::Stt => None,
        }
    }

    const fn track_index(&self) -> Option<u32> {
        match self {
            Self::Embedded { track, .. } => Some(track.index.0),
            Self::External { .. } | Self::Stt => None,
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::External { .. } => "external",
            Self::Embedded { .. } => "embedded",
            Self::Stt => "stt",
        }
    }
}

async fn resolve_input(
    job: &PipelineJob,
    events: &UnboundedSender<TaskEvent>,
) -> Result<ResolvedInput> {
    match &job.input {
        SubtitleInput::External(path) => {
            let format = SubtitleFormat::from_path(path).ok_or_else(|| {
                AppError::UnsupportedSubtitleFormat(
                    path.extension()
                        .and_then(|extension| extension.to_str())
                        .unwrap_or("<no extension>")
                        .to_owned(),
                )
            })?;
            Ok(ResolvedInput::External {
                path: path.clone(),
                format,
            })
        }
        SubtitleInput::Stt => {
            let _ = job.video_required()?;
            Ok(ResolvedInput::Stt)
        }
        SubtitleInput::Auto | SubtitleInput::Embedded(_) => {
            let video = job.video_required()?;
            send(events, TaskEvent::Probing);
            let probe = probe_media(video).await?;
            send(events, TaskEvent::TracksLoaded(probe.clone()));
            let track = match &job.input {
                SubtitleInput::Auto => probe.auto_track().cloned(),
                SubtitleInput::Embedded(index) => probe.track(*index).cloned(),
                SubtitleInput::External(_) | SubtitleInput::Stt => unreachable!(),
            };
            match track {
                Some(track) if track.is_text() => {
                    let format = track
                        .format()
                        .ok_or_else(|| AppError::UnsupportedSubtitleCodec(track.codec.clone()))?;
                    Ok(ResolvedInput::Embedded { track, format })
                }
                Some(track) => Err(AppError::UnsupportedSubtitleCodec(format!(
                    "{}：当前字幕轨为图像字幕，不支持直接翻译。请选择 STT 模式。",
                    track.codec
                ))),
                None if matches!(&job.input, SubtitleInput::Auto) => Ok(sidecar_subtitle(video)
                    .map_or(ResolvedInput::Stt, |(path, format)| {
                        ResolvedInput::External { path, format }
                    })),
                None => Err(AppError::ProbeFailed(
                    "selected subtitle track no longer exists in this video".into(),
                )),
            }
        }
    }
}

fn sidecar_subtitle(video: &Path) -> Option<(PathBuf, SubtitleFormat)> {
    [
        SubtitleFormat::Srt,
        SubtitleFormat::Ass,
        SubtitleFormat::Ssa,
        SubtitleFormat::Vtt,
    ]
    .into_iter()
    .find_map(|format| {
        let path = video.with_extension(format.extension());
        path.is_file().then_some((path, format))
    })
}

fn checkpoint_identity(job: &PipelineJob, resolved: &ResolvedInput) -> Result<CheckpointIdentity> {
    let mut inputs = Vec::new();
    if let Some(video) = job.video.as_deref() {
        inputs.push(fingerprint(video)?);
    }
    if let Some(external) = resolved.external_path() {
        inputs.push(fingerprint(external)?);
    }
    let requested_input = match &job.input {
        SubtitleInput::Auto => "auto",
        SubtitleInput::Embedded(_) => "embedded",
        SubtitleInput::External(_) => "external",
        SubtitleInput::Stt => "stt",
    };
    Ok(CheckpointIdentity {
        inputs,
        requested_input: requested_input.into(),
        resolved_input: resolved.kind().into(),
        track_index: resolved.track_index(),
        format: resolved.format().extension().into(),
        source_language: job.source_language.to_string(),
        target_language: job.target_language.to_string(),
        http_timeout_seconds: job.config.http_timeout.as_secs(),
        stt: SttCheckpointSettings {
            provider: job.config.stt.provider.clone(),
            base_url: job.config.stt.base_url.clone(),
            model: job.config.stt.model.clone(),
            language: job.config.stt.language.to_string(),
            chunk_seconds: job.config.stt.chunk_seconds,
            chunk_overlap_seconds: job.config.stt.chunk_overlap_seconds,
        },
        translator: TranslatorCheckpointSettings {
            provider: job.config.translator.provider.clone(),
            api_format: format!("{:?}", job.config.translator.api_format),
            base_url: job.config.translator.base_url.clone(),
            model: job.config.translator.model.clone(),
            chunk_size: job.config.translator.chunk_size,
            context_before: job.config.translator.context_before,
            context_after: job.config.translator.context_after,
            max_retries: job.config.translator.max_retries,
        },
    })
}

async fn load_document(
    job: &PipelineJob,
    resolved: &ResolvedInput,
    services: &Services,
    cancellation: &CancellationToken,
    events: &UnboundedSender<TaskEvent>,
    checkpoint: Option<&mut CheckpointStore>,
) -> Result<SubtitleDocument> {
    match resolved {
        ResolvedInput::External { path, .. } => {
            send(events, TaskEvent::ExtractingSubtitle);
            ExternalSubtitleSource::new(path).load().await
        }
        ResolvedInput::Embedded { track, .. } => {
            let video = job.video_required()?;
            send(events, TaskEvent::ExtractingSubtitle);
            EmbeddedSubtitleSource::new(
                video,
                track.clone(),
                services.ffmpeg.clone(),
                cancellation.clone(),
            )
            .load()
            .await
        }
        ResolvedInput::Stt => {
            load_stt_document(job, services, cancellation, events, checkpoint).await
        }
    }
}

async fn load_stt_document(
    job: &PipelineJob,
    services: &Services,
    cancellation: &CancellationToken,
    events: &UnboundedSender<TaskEvent>,
    mut checkpoint: Option<&mut CheckpointStore>,
) -> Result<SubtitleDocument> {
    let video = job.video_required()?;
    send(events, TaskEvent::Probing);
    // Detect stream layout before planning chunks so a file with no audio
    // track fails with a clear diagnostic instead of an ffmpeg
    // "output file does not contain any stream" error mid-extraction.
    let probe = probe_media(video).await?;
    if !probe.has_audio_stream() {
        return Err(AppError::NoAudioStream);
    }
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
    let saved_results = if let Some(store) = checkpoint.as_deref_mut() {
        if store.stt_len() > chunks.len()
            || (store.has_translations() && store.stt_len() != chunks.len())
        {
            store.clear().await?;
            Vec::new()
        } else {
            store.stt_results()?
        }
    } else {
        Vec::new()
    };
    let total = chunks.len();
    let resumed = saved_results.len();
    let mut results = ChunkedSttResults::default();
    for (index, result) in saved_results.into_iter().enumerate() {
        results.absorb(result, &chunks[index]);
    }
    if resumed > 0 {
        send(
            events,
            TaskEvent::CheckpointResumed {
                phase: CheckpointPhase::Stt,
                completed: resumed,
                total,
            },
        );
    }
    let language = if job.source_language != LanguageCode::auto() {
        Some(job.source_language.clone())
    } else if job.config.stt.language != LanguageCode::auto() {
        Some(job.config.stt.language.clone())
    } else {
        None
    };
    for (index, chunk) in chunks.iter().enumerate().skip(resumed) {
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
        let stt = services
            .stt
            .as_ref()
            .ok_or(AppError::MissingConfiguration("SUBFLUX_STT_API_KEY"))?;
        let result = stt
            .transcribe(
                SttInput {
                    audio_path: audio.path().to_path_buf(),
                    language: language.clone(),
                },
                cancellation,
            )
            .await?;
        if let Some(store) = checkpoint.as_deref_mut() {
            store.record_stt(index, &result).await?;
        }
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
    use std::{
        collections::HashMap,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

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

    struct FailsAfterOneTranslator {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Translator for FailsAfterOneTranslator {
        async fn translate(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(translated_response(request))
            } else {
                Err(AppError::TranslationError("intentional failure".into()))
            }
        }
    }

    struct RecordingTranslator {
        requests: Mutex<Vec<Vec<u64>>>,
    }

    #[async_trait]
    impl Translator for RecordingTranslator {
        async fn translate(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            self.requests
                .lock()
                .unwrap()
                .push(request.segments.iter().map(|entry| entry.id).collect());
            Ok(translated_response(request))
        }
    }

    struct FailsAfterOneStt {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl SttProvider for FailsAfterOneStt {
        async fn transcribe(
            &self,
            _input: SttInput,
            _cancellation: &CancellationToken,
        ) -> Result<SttResult> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(stt_result("first"))
            } else {
                Err(AppError::SttError("intentional failure".into()))
            }
        }
    }

    struct CountingStt {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl SttProvider for CountingStt {
        async fn transcribe(
            &self,
            _input: SttInput,
            _cancellation: &CancellationToken,
        ) -> Result<SttResult> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(stt_result(&format!("remaining {call}")))
        }
    }

    fn translated_response(request: TranslationRequest) -> TranslationResponse {
        TranslationResponse {
            entries: request
                .segments
                .into_iter()
                .map(|entry| TranslationItem {
                    id: entry.id,
                    text: format!("译文: {}", entry.text),
                })
                .collect(),
        }
    }

    fn stt_result(text: &str) -> SttResult {
        SttResult {
            language: Some(LanguageCode::parse("ja").unwrap()),
            segments: vec![SpeechSegment {
                start_ms: 100,
                end_ms: 900,
                text: text.into(),
            }],
        }
    }

    fn checkpoint_file(directory: &Path) -> PathBuf {
        let mut entries = std::fs::read_dir(directory.join(".subflux")).unwrap();
        let checkpoint = entries.next().unwrap().unwrap().path();
        assert!(entries.next().is_none());
        checkpoint
    }

    #[test]
    fn auto_discovers_a_same_stem_sidecar_subtitle() {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("movie.mp4");
        let subtitle = video.with_extension("srt");
        std::fs::write(&video, []).unwrap();
        std::fs::write(&subtitle, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();

        assert_eq!(
            sidecar_subtitle(&video),
            Some((subtitle, SubtitleFormat::Srt))
        );
    }

    #[test]
    fn auto_sidecar_is_included_in_the_checkpoint_identity() {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("movie.mp4");
        let subtitle = video.with_extension("srt");
        std::fs::write(&video, []).unwrap();
        std::fs::write(&subtitle, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
        let job = PipelineJob {
            video: Some(video),
            input: SubtitleInput::Auto,
            source_language: LanguageCode::parse("en").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config: Config::from_map(&HashMap::new()).unwrap(),
        };

        let identity = checkpoint_identity(
            &job,
            &ResolvedInput::External {
                path: subtitle.clone(),
                format: SubtitleFormat::Srt,
            },
        )
        .unwrap();

        assert_eq!(identity.inputs.len(), 2);
        assert_eq!(identity.inputs[1], fingerprint(&subtitle).unwrap());
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

        let TaskEvent::OverwriteRequested {
            output: requested,
            response,
        } = received_events.recv().await.unwrap()
        else {
            panic!("expected overwrite prompt");
        };
        assert_eq!(requested, output);
        response.send(true).unwrap();
        assert!(matches!(
            received_events.recv().await,
            Some(TaskEvent::ExtractingSubtitle)
        ));

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
    async fn batch_serializes_jobs_with_the_same_output_path() {
        let directory = tempfile::tempdir().unwrap();
        let first_source = directory.path().join("episode.en.srt");
        let second_source = directory.path().join("episode.ja.srt");
        let output = directory.path().join("episode.zh-CN.srt");
        for source in [&first_source, &second_source] {
            tokio::fs::write(source, "1\n00:00:00,000 --> 00:00:01,000\nsource\n")
                .await
                .unwrap();
        }
        let config = Config::from_map(&HashMap::new()).unwrap();
        let first_job = PipelineJob {
            video: None,
            input: SubtitleInput::External(first_source),
            source_language: LanguageCode::parse("en").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Original,
            config: config.clone(),
        };
        let second_job = PipelineJob {
            video: None,
            input: SubtitleInput::External(second_source),
            source_language: LanguageCode::parse("ja").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Original,
            config,
        };
        let services = Arc::new(Services {
            translator: None,
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let overwrite = BatchOverwrite::default();
        let cancellation = CancellationToken::new();
        let (events, mut received_events) = unbounded_channel();
        let first = {
            let services = Arc::clone(&services);
            let events = events.clone();
            let overwrite = overwrite.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                run_pipeline_with_batch_overwrite(
                    first_job,
                    services,
                    cancellation,
                    events,
                    overwrite,
                )
                .await
            })
        };
        let second = {
            let services = Arc::clone(&services);
            let events = events.clone();
            let overwrite = overwrite.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                run_pipeline_with_batch_overwrite(
                    second_job,
                    services,
                    cancellation,
                    events,
                    overwrite,
                )
                .await
            })
        };

        let response = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match received_events.recv().await {
                    Some(TaskEvent::BatchOverwriteRequested { response, .. }) => break response,
                    Some(_) => {}
                    None => panic!("expected a shared overwrite prompt"),
                }
            }
        })
        .await
        .expect("expected the duplicate output to wait for confirmation");
        response.send(false).unwrap();

        let results = [first.await.unwrap(), second.await.unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(AppError::Skipped(_))))
                .count(),
            1
        );
        assert!(output.exists());
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
    async fn rejected_existing_stt_output_skips_before_probing() {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("movie.mkv");
        let output = directory.path().join("movie.zh-CN.srt");
        tokio::fs::write(&video, []).await.unwrap();
        tokio::fs::write(&output, "completed").await.unwrap();
        let config = Config::from_map(&HashMap::new()).unwrap();
        let services = Arc::new(Services::from_config(&config, true).unwrap());
        let (events, mut received_events) = unbounded_channel();
        let task = tokio::spawn(run_pipeline(
            PipelineJob {
                video: Some(video),
                input: SubtitleInput::Stt,
                source_language: LanguageCode::auto(),
                target_language: LanguageCode::parse("zh-CN").unwrap(),
                output_mode: SubtitleOutputMode::Translated,
                config,
            },
            services,
            CancellationToken::new(),
            events,
        ));

        let TaskEvent::OverwriteRequested { response, .. } = received_events.recv().await.unwrap()
        else {
            panic!("expected overwrite prompt before STT probing");
        };
        response.send(false).unwrap();

        assert!(matches!(
            task.await.unwrap(),
            Err(AppError::OutputExists(path)) if path == output
        ));
        assert!(received_events.try_recv().is_err());
    }

    #[tokio::test]
    async fn translation_resume_skips_persisted_chunks_and_cleans_up() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("movie.ja.srt");
        tokio::fs::write(
            &source,
            concat!(
                "1\n00:00:00,000 --> 00:00:00,500\none\n\n",
                "2\n00:00:01,000 --> 00:00:01,500\ntwo\n\n",
                "3\n00:00:02,000 --> 00:00:02,500\nthree\n"
            ),
        )
        .await
        .unwrap();
        let config = Config::from_map(&HashMap::from([
            ("SUBFLUX_TRANSLATOR_CHUNK_SIZE".into(), "1".into()),
            ("SUBFLUX_TRANSLATOR_MAX_RETRIES".into(), "0".into()),
        ]))
        .unwrap();
        let job = PipelineJob {
            video: None,
            input: SubtitleInput::External(source),
            source_language: LanguageCode::parse("en").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config,
        };
        let failing = Arc::new(FailsAfterOneTranslator {
            calls: AtomicUsize::new(0),
        });
        let first_services = Arc::new(Services {
            translator: Some(failing),
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, _received_events) = unbounded_channel();
        assert!(matches!(
            run_pipeline(
                job.clone(),
                first_services,
                CancellationToken::new(),
                events,
            )
            .await,
            Err(AppError::TranslationError(_))
        ));
        let checkpoint = checkpoint_file(directory.path());
        assert!(checkpoint.exists());

        let translator = Arc::new(RecordingTranslator {
            requests: Mutex::new(Vec::new()),
        });
        let services = Arc::new(Services {
            translator: Some(Arc::clone(&translator) as Arc<dyn Translator>),
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();
        let output = run_pipeline(job, services, CancellationToken::new(), events)
            .await
            .unwrap();

        assert_eq!(
            translator.requests.lock().unwrap().as_slice(),
            &[vec![2], vec![3]]
        );
        let events: Vec<_> = std::iter::from_fn(|| received_events.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            TaskEvent::CheckpointResumed {
                phase: CheckpointPhase::Translation,
                completed: 1,
                total: 3,
            }
        )));
        assert_eq!(output, directory.path().join("movie.zh-CN.srt"));
        assert!(!checkpoint.exists());
    }

    #[tokio::test]
    async fn stt_resume_skips_persisted_audio_chunks() {
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
        assert!(generated.status.success());
        let config = Config::from_map(&HashMap::from([
            ("SUBFLUX_STT_CHUNK_SECONDS".into(), "1".into()),
            ("SUBFLUX_STT_CHUNK_OVERLAP_SECONDS".into(), "0".into()),
        ]))
        .unwrap();
        let job = PipelineJob {
            video: Some(video),
            input: SubtitleInput::Stt,
            source_language: LanguageCode::auto(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Original,
            config,
        };
        let first_services = Arc::new(Services {
            translator: None,
            stt: Some(Arc::new(FailsAfterOneStt {
                calls: AtomicUsize::new(0),
            })),
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, _received_events) = unbounded_channel();
        assert!(matches!(
            run_pipeline(
                job.clone(),
                first_services,
                CancellationToken::new(),
                events,
            )
            .await,
            Err(AppError::SttError(_))
        ));
        let checkpoint = checkpoint_file(directory.path());
        assert!(checkpoint.exists());

        let stt = Arc::new(CountingStt {
            calls: AtomicUsize::new(0),
        });
        let services = Arc::new(Services {
            translator: None,
            stt: Some(Arc::clone(&stt) as Arc<dyn SttProvider>),
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();
        run_pipeline(job, services, CancellationToken::new(), events)
            .await
            .unwrap();

        let events: Vec<_> = std::iter::from_fn(|| received_events.try_recv().ok()).collect();
        let total = events
            .iter()
            .find_map(|event| match event {
                TaskEvent::CheckpointResumed {
                    phase: CheckpointPhase::Stt,
                    completed: 1,
                    total,
                } => Some(*total),
                _ => None,
            })
            .expect("expected STT resume event");
        assert!(total >= 2);
        assert_eq!(stt.calls.load(Ordering::SeqCst), total - 1);
        assert!(!checkpoint.exists());
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
        let document = load_stt_document(&job, &services, &CancellationToken::new(), &events, None)
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

    #[tokio::test]
    async fn stt_rejects_video_without_an_audio_track() {
        if !check_tools().is_ready() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("no-audio.mkv");
        // Build a container with a video stream only: ffmpeg's `color` source
        // yields pixels but no audio, matching the Tubi stream that surfaced
        // the "output file does not contain any stream" failure.
        let generated = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:d=1",
                "-c:v",
                "mpeg4",
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
        let (events, _received_events) = unbounded_channel();
        let result =
            load_stt_document(&job, &services, &CancellationToken::new(), &events, None).await;
        assert!(
            matches!(result, Err(AppError::NoAudioStream)),
            "expected NoAudioStream, got {result:?}"
        );
    }
}
