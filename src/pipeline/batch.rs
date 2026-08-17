use std::{future::Future, path::PathBuf, sync::Arc};

use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::{Config, LanguageCode},
    error::{AppError, Result},
    event::{BatchFailure, BatchSummary, TaskEvent},
    services::Services,
    subtitle::SubtitleOutputMode,
};

use super::{PipelineJob, SubtitleInput, run_pipeline};

/// Subtitle sources that can be applied safely to every independently probed
/// video in a batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchSubtitleInput {
    /// Each video selects its own default text track and falls back to STT.
    Auto,
    /// Each video uses speech recognition regardless of embedded tracks.
    Stt,
}

#[derive(Clone, Debug)]
pub struct BatchJob {
    pub videos: Vec<PathBuf>,
    pub subtitle_input: BatchSubtitleInput,
    pub source_language: LanguageCode,
    pub target_language: LanguageCode,
    pub output_mode: SubtitleOutputMode,
    pub config: Config,
}

impl BatchJob {
    fn pipeline_job(&self, video: PathBuf) -> PipelineJob {
        PipelineJob {
            video: Some(video),
            input: match self.subtitle_input {
                BatchSubtitleInput::Auto => SubtitleInput::Auto,
                BatchSubtitleInput::Stt => SubtitleInput::Stt,
            },
            source_language: self.source_language.clone(),
            target_language: self.target_language.clone(),
            output_mode: self.output_mode,
            config: self.config.clone(),
        }
    }
}

/// Runs videos in discovery order. A per-video failure is recorded and the
/// next video still starts; cancellation is the only condition that stops the
/// sequence early.
pub async fn run_batch(
    job: BatchJob,
    services: Arc<Services>,
    cancellation: CancellationToken,
    events: UnboundedSender<TaskEvent>,
) -> Result<BatchSummary> {
    let summary = run_batch_jobs(&job, &cancellation, &events, |pipeline_job| {
        run_pipeline(
            pipeline_job,
            Arc::clone(&services),
            cancellation.clone(),
            events.clone(),
        )
    })
    .await?;

    debug!(
        total = summary.total,
        succeeded = summary.succeeded.len(),
        failed = summary.failed.len(),
        "subtitle batch pipeline completed"
    );
    Ok(summary)
}

async fn run_batch_jobs<F, Fut>(
    job: &BatchJob,
    cancellation: &CancellationToken,
    events: &UnboundedSender<TaskEvent>,
    mut run_one: F,
) -> Result<BatchSummary>
where
    F: FnMut(PipelineJob) -> Fut,
    Fut: Future<Output = Result<PathBuf>>,
{
    if job.videos.is_empty() {
        return Err(AppError::InvalidConfig(
            "batch processing requires at least one video".into(),
        ));
    }

    let total = job.videos.len();
    let mut summary = BatchSummary {
        total,
        ..BatchSummary::default()
    };
    send(events, TaskEvent::BatchStarted { total });

    for (index, video) in job.videos.iter().cloned().enumerate() {
        check_cancelled(cancellation)?;
        let current = index + 1;
        send(
            events,
            TaskEvent::BatchVideoStarted {
                current,
                total,
                video: video.clone(),
            },
        );

        match run_one(job.pipeline_job(video.clone())).await {
            Ok(output) => {
                summary.succeeded.push(output.clone());
                send(
                    events,
                    TaskEvent::BatchVideoSucceeded {
                        current,
                        total,
                        video,
                        output,
                    },
                );
            }
            Err(AppError::Cancelled) => return Err(AppError::Cancelled),
            Err(error) => {
                let error = error.safe_message();
                warn!(video = %video.display(), error = %error, "batch subtitle pipeline failed for a video");
                summary.failed.push(BatchFailure {
                    video: video.clone(),
                    error: error.clone(),
                });
                send(
                    events,
                    TaskEvent::BatchVideoFailed {
                        current,
                        total,
                        video,
                        error,
                    },
                );
            }
        }
    }

    Ok(summary)
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
    use std::{collections::HashMap, path::Path};

    use async_trait::async_trait;
    use tokio::process::Command;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::{
        config::Config,
        media::{Ffmpeg, check_tools},
        services::Services,
        subtitle::{FileSubtitleWriter, SubtitleOutputMode, SubtitleWriter},
        translator::{TranslationItem, TranslationRequest, TranslationResponse, Translator},
    };

    use super::*;

    struct MockTranslator;

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

    #[test]
    fn each_batch_video_inherits_the_shared_options() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let source_language = LanguageCode::parse("ja").unwrap();
        let target_language = LanguageCode::parse("zh-CN").unwrap();
        let batch = BatchJob {
            videos: vec![PathBuf::from("episode.mkv")],
            subtitle_input: BatchSubtitleInput::Auto,
            source_language: source_language.clone(),
            target_language: target_language.clone(),
            output_mode: SubtitleOutputMode::Bilingual,
            config,
        };

        let job = batch.pipeline_job(PathBuf::from("episode.mkv"));

        assert!(matches!(job.input, SubtitleInput::Auto));
        assert_eq!(job.source_language, source_language);
        assert_eq!(job.target_language, target_language);
        assert_eq!(job.output_mode, SubtitleOutputMode::Bilingual);
    }

    #[tokio::test]
    async fn batch_continues_after_one_video_fails() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let batch = BatchJob {
            videos: vec![
                PathBuf::from("first.mkv"),
                PathBuf::from("broken.mkv"),
                PathBuf::from("third.mkv"),
            ],
            subtitle_input: BatchSubtitleInput::Auto,
            source_language: LanguageCode::auto(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config,
        };
        let (events, mut received_events) = unbounded_channel();

        let summary = run_batch_jobs(
            &batch,
            &CancellationToken::new(),
            &events,
            |job| async move {
                let video = job.video.expect("batch jobs always have a video");
                if video == *"broken.mkv" {
                    Err(AppError::ProbeFailed("broken fixture".into()))
                } else {
                    Ok(video.with_extension("zh-CN.srt"))
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(summary.total, 3);
        assert_eq!(
            summary.succeeded,
            vec![
                PathBuf::from("first.zh-CN.srt"),
                PathBuf::from("third.zh-CN.srt")
            ]
        );
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].video, PathBuf::from("broken.mkv"));
        assert_eq!(summary.failed[0].error, "broken fixture");

        let events: Vec<_> = std::iter::from_fn(|| received_events.try_recv().ok()).collect();
        let started: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TaskEvent::BatchVideoStarted { video, .. } => Some(video.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(started, batch.videos);
        assert!(events.iter().any(|event| matches!(
            event,
            TaskEvent::BatchVideoFailed {
                current: 2,
                total: 3,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn batch_translates_each_discovered_video_sequentially() {
        if !check_tools().is_ready() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let first = create_video_with_subtitle(directory.path(), "first", "first source").await;
        let second = create_video_with_subtitle(directory.path(), "second", "second source").await;
        let config = Config::from_map(&HashMap::new()).unwrap();
        let job = BatchJob {
            videos: vec![first.clone(), second.clone()],
            subtitle_input: BatchSubtitleInput::Auto,
            source_language: LanguageCode::parse("en").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config,
        };
        let services = Arc::new(Services {
            translator: Some(Arc::new(MockTranslator)),
            stt: None,
            subtitle_writer: Arc::new(FileSubtitleWriter) as Arc<dyn SubtitleWriter>,
            ffmpeg: Arc::new(Ffmpeg),
        });
        let (events, mut received_events) = unbounded_channel();

        let summary = run_batch(job, services, CancellationToken::new(), events)
            .await
            .unwrap();

        assert_eq!(summary.total, 2);
        assert_eq!(summary.failed, Vec::<crate::event::BatchFailure>::new());
        assert_eq!(
            summary.succeeded,
            vec![
                directory.path().join("first.zh-CN.srt"),
                directory.path().join("second.zh-CN.srt"),
            ]
        );
        assert!(
            tokio::fs::read_to_string(directory.path().join("first.zh-CN.srt"))
                .await
                .unwrap()
                .contains("译文: first source")
        );
        assert!(
            tokio::fs::read_to_string(directory.path().join("second.zh-CN.srt"))
                .await
                .unwrap()
                .contains("译文: second source")
        );

        let events: Vec<_> = std::iter::from_fn(|| received_events.try_recv().ok()).collect();
        let started: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TaskEvent::BatchVideoStarted { video, .. } => Some(video.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![first, second]);
    }

    async fn create_video_with_subtitle(directory: &Path, name: &str, text: &str) -> PathBuf {
        let source = directory.join(format!("{name}.srt"));
        let video = directory.join(format!("{name}.mkv"));
        tokio::fs::write(
            &source,
            format!("1\n00:00:00,000 --> 00:00:00,800\n{text}\n"),
        )
        .await
        .unwrap();
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
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:duration=1",
                "-i",
            ])
            .arg(&source)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-map",
                "2:0",
                "-c:v",
                "mpeg4",
                "-c:a",
                "pcm_s16le",
                "-c:s",
                "srt",
                "-shortest",
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
        video
    }
}
