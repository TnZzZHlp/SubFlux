use std::path::Path;

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{AppError, Result},
    event::TaskEvent,
    media::{MediaProbe, probe_media},
    output::protect::same_file,
    subtitle::catalog::{
        discover_related_subtitles, discover_subtitles, filename_language, language_matches,
    },
};

use super::PipelineJob;

pub(super) async fn existing_target(
    job: &PipelineJob,
    output: &Path,
    probe: Option<&MediaProbe>,
) -> Result<Option<String>> {
    if tokio::fs::try_exists(output).await? {
        return Ok(Some(output.display().to_string()));
    }
    if let Some(video) = job.video.as_deref() {
        let loaded;
        let probe = if let Some(probe) = probe {
            Some(probe)
        } else {
            loaded = probe_media(video).await.ok();
            loaded.as_ref()
        };
        if let Some(track) = probe.and_then(|probe| {
            probe.subtitle_tracks.iter().find(|track| {
                track
                    .language
                    .as_deref()
                    .is_some_and(|language| language_matches(language, &job.target_language))
            })
        }) {
            return Ok(Some(track.display_label()));
        }
        for file in discover_subtitles(video).await? {
            if file
                .language
                .as_deref()
                .is_some_and(|language| language_matches(language, &job.target_language))
                && !same_file(&file.path, output).await?
            {
                return Ok(Some(file.display_label()));
            }
        }
    }
    if let Some(source) = job.external_subtitle_path() {
        for file in discover_related_subtitles(source).await? {
            if file
                .language
                .as_deref()
                .is_some_and(|language| language_matches(language, &job.target_language))
                && !same_file(&file.path, output).await?
            {
                return Ok(Some(file.display_label()));
            }
        }
        if filename_language(source)
            .is_some_and(|language| language_matches(&language, &job.target_language))
            && !same_file(source, output).await?
        {
            return Ok(Some(source.display().to_string()));
        }
    }
    Ok(None)
}

pub(super) async fn confirm_translation(
    subject: &Path,
    subtitle: &str,
    batch: bool,
    events: &UnboundedSender<TaskEvent>,
    cancellation: &CancellationToken,
) -> Result<bool> {
    let (response, mut responses) = unbounded_channel();
    events
        .send(TaskEvent::TargetSubtitleRequested {
            subject: subject.to_path_buf(),
            subtitle: subtitle.into(),
            batch,
            response,
        })
        .map_err(|_| AppError::Cancelled)?;
    tokio::select! {
        response = responses.recv() => response.ok_or(AppError::Cancelled),
        () = cancellation.cancelled() => Err(AppError::Cancelled),
    }
}
