use std::{future::Future, path::PathBuf, sync::Arc};

use tokio::{
    sync::{Semaphore, mpsc::UnboundedSender},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::{Config, LanguageCode},
    error::{AppError, Result},
    event::{BatchFailure, BatchSummary, TaskEvent},
    services::Services,
    subtitle::SubtitleOutputMode,
};

use super::{
    PipelineJob, SubtitleInput,
    job::{BatchOverwrite, run_pipeline_with_batch_overwrite},
};

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
    pub(crate) fn pipeline_job(&self, video: PathBuf) -> PipelineJob {
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

/// Runs independent videos with the configured concurrency.
///
/// A per-video failure is recorded without stopping other videos. Cancellation
/// stops new videos and cancels running pipelines through the shared token.
pub async fn run_batch(
    job: BatchJob,
    services: Arc<Services>,
    cancellation: CancellationToken,
    events: UnboundedSender<TaskEvent>,
) -> Result<BatchSummary> {
    let run_cancellation = cancellation.clone();
    let run_services = Arc::clone(&services);
    let batch_overwrite = BatchOverwrite::default();
    let summary = run_batch_jobs(
        &job,
        &cancellation,
        &events,
        move |pipeline_job, pipeline_events| {
            run_pipeline_with_batch_overwrite(
                pipeline_job,
                Arc::clone(&run_services),
                run_cancellation.clone(),
                pipeline_events,
                batch_overwrite.clone(),
            )
        },
    )
    .await?;

    debug!(
        total = summary.total,
        succeeded = summary.succeeded.len(),
        skipped = summary.skipped.len(),
        failed = summary.failed.len(),
        concurrency = job.config.batch_concurrency,
        "subtitle batch pipeline completed"
    );
    Ok(summary)
}

async fn run_batch_jobs<F, Fut>(
    job: &BatchJob,
    cancellation: &CancellationToken,
    events: &UnboundedSender<TaskEvent>,
    run_one: F,
) -> Result<BatchSummary>
where
    F: Fn(PipelineJob, UnboundedSender<TaskEvent>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<PathBuf>> + Send + 'static,
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

    let semaphore = Arc::new(Semaphore::new(job.config.batch_concurrency.min(total)));
    let run_one = Arc::new(run_one);
    let mut tasks = JoinSet::new();

    for (index, video) in job.videos.iter().cloned().enumerate() {
        let pipeline_job = job.pipeline_job(video.clone());
        let semaphore = Arc::clone(&semaphore);
        let run_one = Arc::clone(&run_one);
        let cancellation = cancellation.clone();
        let events = events.clone();
        tasks.spawn(async move {
            let permit = tokio::select! {
                permit = semaphore.acquire_owned() => permit.expect("batch semaphore cannot be closed"),
                () = cancellation.cancelled() => {
                    return (index, video, Err(AppError::Cancelled));
                }
            };
            if cancellation.is_cancelled() {
                drop(permit);
                return (index, video, Err(AppError::Cancelled));
            }

            let current = index + 1;
            send(
                &events,
                TaskEvent::BatchVideoStarted {
                    current,
                    total,
                    video: video.clone(),
                },
            );
            let (pipeline_events, mut received_events) = tokio::sync::mpsc::unbounded_channel();
            let forwarded_events = events.clone();
            let event_video = video.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(event) = received_events.recv().await {
                    send(
                        &forwarded_events,
                        TaskEvent::BatchVideoEvent {
                            current,
                            total,
                            video: event_video.clone(),
                            event: Box::new(event),
                        },
                    );
                }
            });
            let result = run_one(pipeline_job, pipeline_events).await;
            let result = match forwarder.await {
                Ok(()) => result,
                Err(error) => Err(AppError::TranslationError(format!(
                    "batch event forwarder failed: {error}"
                ))),
            };
            drop(permit);
            (index, video, result)
        });
    }

    let mut successes = Vec::new();
    let mut skipped = Vec::new();
    let mut failures = Vec::new();
    let mut cancelled = false;
    while let Some(result) = tasks.join_next().await {
        let (index, video, result) = result
            .map_err(|error| AppError::TranslationError(format!("batch task failed: {error}")))?;
        let current = index + 1;
        match result {
            Ok(output) => {
                successes.push((index, output.clone()));
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
            Err(AppError::Skipped(_)) => {
                skipped.push((index, video.clone()));
                send(
                    events,
                    TaskEvent::BatchVideoSkipped {
                        current,
                        total,
                        video,
                    },
                );
            }
            Err(AppError::Cancelled) => cancelled = true,
            Err(error) => {
                let error = error.safe_message();
                warn!(video = %video.display(), error = %error, "batch subtitle pipeline failed for a video");
                failures.push((
                    index,
                    BatchFailure {
                        video: video.clone(),
                        error: error.clone(),
                    },
                ));
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

    if cancelled || cancellation.is_cancelled() {
        return Err(AppError::Cancelled);
    }

    successes.sort_unstable_by_key(|(index, _)| *index);
    skipped.sort_unstable_by_key(|(index, _)| *index);
    failures.sort_unstable_by_key(|(index, _)| *index);
    summary.succeeded = successes.into_iter().map(|(_, output)| output).collect();
    summary.skipped = skipped.into_iter().map(|(_, video)| video).collect();
    summary.failed = failures.into_iter().map(|(_, failure)| failure).collect();
    Ok(summary)
}

fn send(sender: &UnboundedSender<TaskEvent>, event: TaskEvent) {
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

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
            |job, _pipeline_events| async move {
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
    async fn batch_records_skipped_outputs_without_failure() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let batch = BatchJob {
            videos: vec![PathBuf::from("existing.mkv"), PathBuf::from("fresh.mkv")],
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
            |job, _pipeline_events| async move {
                let video = job.video.unwrap();
                if video == *"existing.mkv" {
                    Err(AppError::Skipped(video.with_extension("zh-CN.srt")))
                } else {
                    Ok(video.with_extension("zh-CN.srt"))
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(summary.succeeded, vec![PathBuf::from("fresh.zh-CN.srt")]);
        assert_eq!(summary.skipped, vec![PathBuf::from("existing.mkv")]);
        assert_eq!(summary.failed, Vec::new());
        assert!(
            std::iter::from_fn(|| received_events.try_recv().ok()).any(|event| matches!(
                event,
                TaskEvent::BatchVideoSkipped {
                    current: 1,
                    total: 2,
                    ..
                }
            ))
        );
    }

    #[tokio::test]
    async fn batch_forwards_pipeline_events_with_their_video() {
        let config = Config::from_map(&HashMap::new()).unwrap();
        let video = PathBuf::from("episode.mkv");
        let batch = BatchJob {
            videos: vec![video.clone()],
            subtitle_input: BatchSubtitleInput::Auto,
            source_language: LanguageCode::auto(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config,
        };
        let (events, mut received_events) = unbounded_channel();

        run_batch_jobs(
            &batch,
            &CancellationToken::new(),
            &events,
            |job, pipeline_events| async move {
                pipeline_events
                    .send(TaskEvent::TranslationStarted { total: 4 })
                    .unwrap();
                pipeline_events
                    .send(TaskEvent::TranslationProgress {
                        completed: 2,
                        total: 4,
                        request: 1,
                    })
                    .unwrap();
                Ok(job.video.unwrap().with_extension("zh-CN.srt"))
            },
        )
        .await
        .unwrap();

        let events: Vec<_> = std::iter::from_fn(|| received_events.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            TaskEvent::BatchVideoEvent {
                current: 1,
                total: 1,
                video: event_video,
                event: progress,
            } if event_video == &video && matches!(
                progress.as_ref(),
                TaskEvent::TranslationProgress {
                    completed: 2,
                    total: 4,
                    request: 1,
                }
            )
        )));
    }

    #[tokio::test]
    async fn batch_honors_the_configured_video_concurrency() {
        let config = Config::from_map(&HashMap::from([(
            "SUBFLUX_BATCH_CONCURRENCY".into(),
            "2".into(),
        )]))
        .unwrap();
        let batch = BatchJob {
            videos: (0..4)
                .map(|index| PathBuf::from(format!("video-{index}.mkv")))
                .collect(),
            subtitle_input: BatchSubtitleInput::Auto,
            source_language: LanguageCode::auto(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            output_mode: SubtitleOutputMode::Translated,
            config,
        };
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let (events, _received_events) = unbounded_channel();

        let summary = run_batch_jobs(&batch, &CancellationToken::new(), &events, {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            move |job, _pipeline_events| {
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(count, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(job.video.unwrap().with_extension("zh-CN.srt"))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(summary.succeeded.len(), 4);
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn batch_translates_each_discovered_video() {
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
