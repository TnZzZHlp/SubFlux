use std::path::PathBuf;

use crate::{config::Config, media::MediaProbe};

/// Events published by background commands. The TUI consumes these events but
/// never reaches into a running pipeline to inspect mutable state.
#[derive(Clone, Debug)]
pub enum TaskEvent {
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
    Failed(String),
    Cancelled,
    ConfigReloaded(Box<Config>),
    ConfigReloadFailed(String),
}
