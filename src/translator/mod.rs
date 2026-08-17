pub mod anthropic;
pub mod chunk;
pub mod model;
pub mod openai;
pub(crate) mod prompt;
pub mod provider;

use std::sync::Arc;

use crate::{
    config::{Config, TranslatorApiFormat},
    error::Result,
};

pub use chunk::{TranslationChunk, TranslationChunkConfig, build_chunks};
pub use model::{TranslationItem, TranslationRequest, TranslationResponse};
pub use provider::Translator;

pub type DynTranslator = Arc<dyn Translator>;

pub fn build_translator(config: &Config) -> Result<DynTranslator> {
    match config.translator.api_format {
        TranslatorApiFormat::OpenAi => Ok(Arc::new(openai::OpenAiCompatibleTranslator::new(
            &config.translator,
            config.http_timeout,
        )?)),
        TranslatorApiFormat::Anthropic => Ok(Arc::new(
            anthropic::AnthropicCompatibleTranslator::new(&config.translator, config.http_timeout)?,
        )),
    }
}
