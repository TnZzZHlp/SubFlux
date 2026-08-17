pub mod model;
pub mod openai;
pub mod provider;

use std::sync::Arc;

use crate::{
    config::Config,
    error::{AppError, Result},
};

pub use model::{SttInput, SttResult};
pub use provider::SttProvider;

pub type DynSttProvider = Arc<dyn SttProvider>;

pub fn build_stt_provider(config: &Config) -> Result<DynSttProvider> {
    if !config.stt.provider.eq_ignore_ascii_case("openai")
        && !config
            .stt
            .provider
            .eq_ignore_ascii_case("openai-compatible")
    {
        return Err(AppError::InvalidConfig(format!(
            "STT_PROVIDER={} is not supported; use openai-compatible",
            config.stt.provider
        )));
    }
    Ok(Arc::new(openai::OpenAiCompatibleStt::new(
        &config.stt,
        config.http_timeout,
    )?))
}
