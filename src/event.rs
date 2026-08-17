use std::path::PathBuf;

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
    pub failed: Vec<BatchFailure>,
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
    BatchVideoSucceeded {
        current: usize,
        total: usize,
        video: PathBuf,
        output: PathBuf,
    },
    BatchVideoFailed {
        current: usize,
        total: usize,
        video: PathBuf,
        error: String,
    },
    Probing,
    TracksLoaded(MediaProbe),
    ExtractingSubtitle,
    ExtractingAudio,
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
    Writing,
    Finished(PathBuf),
    BatchFinished(BatchSummary),
    Failed(String),
    Cancelled,
    ConfigReloaded(Box<Config>),
    ConfigReloadFailed(String),
}
