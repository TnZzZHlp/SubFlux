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
    /// Omitted for original-only output, which must not require a translator
    /// API key or construct a provider.
    pub translator: Option<DynTranslator>,
    /// Optional so users translating an existing subtitle do not need to
    /// configure an STT API key. The STT path reports a precise missing-config
    /// error when it is actually selected.
    pub stt: Option<DynSttProvider>,
    pub subtitle_writer: Arc<dyn SubtitleWriter>,
    pub ffmpeg: Arc<Ffmpeg>,
}

impl Services {
    pub fn from_config(config: &Config, needs_translation: bool) -> Result<Self> {
        Ok(Self {
            translator: needs_translation
                .then(|| build_translator(config))
                .transpose()?,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::config::Config;

    use super::*;

    #[test]
    fn original_only_setup_skips_translator_configuration() {
        let config = Config::from_map(&HashMap::new()).unwrap();

        let services = Services::from_config(&config, false).unwrap();

        assert!(services.translator.is_none());
    }
}
