use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    error::Result,
    media::Ffmpeg,
    stt::{DynSttProvider, SttInput, SttProvider, SttResult, build_stt_provider},
    subtitle::{FileSubtitleWriter, SubtitleWriter},
    translator::{
        DynTranslator, TranslationRequest, TranslationResponse, Translator, build_translator,
    },
};

/// The only provider-instantiation boundary. Pipeline code receives traits and
/// never branches on a provider name.
#[derive(Clone)]
pub struct Services {
    /// Omitted when translation is not requested or no translator key is
    /// configured. Construction is deferred until the first request.
    pub translator: Option<DynTranslator>,
    /// Optional for subtitle-only input and deferred until STT is actually
    /// required, so an existing output can be skipped first.
    pub stt: Option<DynSttProvider>,
    pub subtitle_writer: Arc<dyn SubtitleWriter>,
    pub ffmpeg: Arc<Ffmpeg>,
}

impl Services {
    pub fn from_config(config: &Config, needs_translation: bool) -> Result<Self> {
        Ok(Self {
            translator: (needs_translation && config.translator.api_key.is_configured())
                .then(|| Arc::new(LazyTranslator::new(config.clone())) as DynTranslator),
            stt: config
                .stt
                .api_key
                .is_configured()
                .then(|| Arc::new(LazyStt::new(config.clone())) as DynSttProvider),
            subtitle_writer: Arc::new(FileSubtitleWriter),
            ffmpeg: Arc::new(Ffmpeg),
        })
    }
}

struct LazyTranslator {
    config: Config,
    provider: OnceCell<DynTranslator>,
}

impl LazyTranslator {
    fn new(config: Config) -> Self {
        Self {
            config,
            provider: OnceCell::new(),
        }
    }

    async fn provider(&self) -> Result<&DynTranslator> {
        self.provider
            .get_or_try_init(|| async { build_translator(&self.config) })
            .await
    }
}

#[async_trait]
impl Translator for LazyTranslator {
    async fn translate(
        &self,
        request: TranslationRequest,
        cancellation: &CancellationToken,
    ) -> Result<TranslationResponse> {
        self.provider()
            .await?
            .translate(request, cancellation)
            .await
    }

    async fn translate_correction(
        &self,
        request: TranslationRequest,
        cancellation: &CancellationToken,
    ) -> Result<TranslationResponse> {
        self.provider()
            .await?
            .translate_correction(request, cancellation)
            .await
    }
}

struct LazyStt {
    config: Config,
    provider: OnceCell<DynSttProvider>,
}

impl LazyStt {
    fn new(config: Config) -> Self {
        Self {
            config,
            provider: OnceCell::new(),
        }
    }

    async fn provider(&self) -> Result<&DynSttProvider> {
        self.provider
            .get_or_try_init(|| async { build_stt_provider(&self.config) })
            .await
    }
}

#[async_trait]
impl SttProvider for LazyStt {
    async fn transcribe(
        &self,
        input: SttInput,
        cancellation: &CancellationToken,
    ) -> Result<SttResult> {
        self.provider().await?.transcribe(input, cancellation).await
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

    #[test]
    fn translator_provider_construction_is_deferred() {
        let config = Config::from_map(&HashMap::from([
            ("SUBFLUX_TRANSLATOR_API_KEY".into(), "key".into()),
            ("SUBFLUX_TRANSLATOR_BASE_URL".into(), "not a URL".into()),
        ]))
        .unwrap();

        let services = Services::from_config(&config, true).unwrap();

        assert!(services.translator.is_some());
    }
}
