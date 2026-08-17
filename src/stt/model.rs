use std::path::PathBuf;

use crate::{config::LanguageCode, subtitle::SpeechSegment};

#[derive(Clone, Debug)]
pub struct SttInput {
    pub audio_path: PathBuf,
    pub language: Option<LanguageCode>,
}

#[derive(Clone, Debug)]
pub struct SttResult {
    pub language: Option<LanguageCode>,
    pub segments: Vec<SpeechSegment>,
}
