use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

use crate::{config::Config, media::MediaProbe};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchFailure {
    pub video: PathBuf,
    pub error: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BatchSummary {
    pub total: usize,
    pub succeeded: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
    pub failed: Vec<BatchFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointPhase {
    Stt,
    Translation,
}

/// Events published by background commands. The TUI consumes these events but
/// never reaches into a running pipeline to inspect mutable state.
#[derive(Clone, Debug)]
pub enum TaskEvent {
    BatchStarted {
        total: usize,
    },
    BatchVideoStarted {
        current: usize,
        total: usize,
        video: PathBuf,
    },
    /// A pipeline event associated with one video in a concurrent batch.
    BatchVideoEvent {
        current: usize,
        total: usize,
        video: PathBuf,
        event: Box<Self>,
    },
    BatchVideoSucceeded {
        current: usize,
        total: usize,
        video: PathBuf,
        output: PathBuf,
    },
    BatchVideoSkipped {
        current: usize,
        total: usize,
        video: PathBuf,
    },
    BatchVideoFailed {
        current: usize,
        total: usize,
        video: PathBuf,
        error: String,
    },
    BatchRetrySucceeded {
        failed_index: usize,
        output: PathBuf,
    },
    BatchRetrySkipped {
        failed_index: usize,
    },
    BatchRetryFailed {
        failed_index: usize,
        error: String,
    },
    BatchRetryCancelled,
    ProbeSucceeded {
        request_id: u64,
        probe: MediaProbe,
    },
    ProbeFailed {
        request_id: u64,
        error: String,
    },
    Probing,
    TracksLoaded(MediaProbe),
    ExtractingSubtitle,
    ExtractingAudio,
    CheckpointResumed {
        phase: CheckpointPhase,
        completed: usize,
        total: usize,
    },
    SttStarted {
        current: usize,
        total: usize,
    },
    SttProgress {
        current: usize,
        total: Option<usize>,
    },
    TranslationStarted {
        total: usize,
    },
    TranslationProgress {
        completed: usize,
        total: usize,
        request: usize,
    },
    OverwriteRequested {
        output: PathBuf,
        response: UnboundedSender<bool>,
    },
    /// One shared overwrite decision for all existing outputs in a batch.
    BatchOverwriteRequested {
        output: PathBuf,
        response: UnboundedSender<bool>,
    },
    Writing,
    Finished(PathBuf),
    BatchFinished(BatchSummary),
    Failed(String),
    Cancelled,
    ConfigReloaded(Box<Config>),
    ConfigReloadFailed(String),
}
