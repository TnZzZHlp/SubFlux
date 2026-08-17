use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;

use crate::{
    error::Result,
    media::{Ffmpeg, SubtitleTrack},
};

use super::{SubtitleDocument, parse, parse_file};

#[async_trait]
pub trait SubtitleSource: Send + Sync {
    async fn load(&self) -> Result<SubtitleDocument>;
}

#[derive(Clone, Debug)]
pub struct ExternalSubtitleSource {
    path: PathBuf,
}

impl ExternalSubtitleSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl SubtitleSource for ExternalSubtitleSource {
    async fn load(&self) -> Result<SubtitleDocument> {
        let content = tokio::fs::read_to_string(&self.path).await?;
        parse_file(&self.path, &content)
    }
}

#[derive(Clone, Debug)]
pub struct EmbeddedSubtitleSource {
    video: PathBuf,
    track: SubtitleTrack,
    ffmpeg: Arc<Ffmpeg>,
    cancellation: tokio_util::sync::CancellationToken,
}

impl EmbeddedSubtitleSource {
    pub fn new(
        video: impl Into<PathBuf>,
        track: SubtitleTrack,
        ffmpeg: Arc<Ffmpeg>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            video: video.into(),
            track,
            ffmpeg,
            cancellation,
        }
    }
}

#[async_trait]
impl SubtitleSource for EmbeddedSubtitleSource {
    async fn load(&self) -> Result<SubtitleDocument> {
        let extracted = self
            .ffmpeg
            .extract_subtitle(&self.video, &self.track, &self.cancellation)
            .await?;
        let mut document = parse(extracted.format, &extracted.content)?;
        document.metadata.source_path = Some(self.video.clone());
        Ok(document)
    }
}
