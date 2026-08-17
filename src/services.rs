use std::sync::Arc;

use crate::{
    config::Config,
    error::Result,
    media::Ffmpeg,
    stt::{DynSttProvider, build_stt_provider},
    subtitle::{FileSubtitleWriter, SubtitleWriter},
    translator::{DynTranslator, build_translator},
};

/// The only provider-instantiation boundary. Pipeline code receives traits and
/// never branches on a provider name.
#[derive(Clone)]
pub struct Services {
    pub translator: DynTranslator,
    /// Optional so users translating an existing subtitle do not need to
    /// configure an STT API key. The STT path reports a precise missing-config
    /// error when it is actually selected.
    pub stt: Option<DynSttProvider>,
    pub subtitle_writer: Arc<dyn SubtitleWriter>,
    pub ffmpeg: Arc<Ffmpeg>,
}

impl Services {
    pub fn from_config(config: &Config) -> Result<Self> {
        Ok(Self {
            translator: build_translator(config)?,
            stt: config
                .stt
                .api_key
                .is_configured()
                .then(|| build_stt_provider(config))
                .transpose()?,
            subtitle_writer: Arc::new(FileSubtitleWriter),
            ffmpeg: Arc::new(Ffmpeg),
        })
    }
}
