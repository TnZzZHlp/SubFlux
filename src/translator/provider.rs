use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::Result;

use super::{TranslationRequest, TranslationResponse};

#[async_trait]
pub trait Translator: Send + Sync {
    async fn translate(
        &self,
        request: TranslationRequest,
        cancellation: &CancellationToken,
    ) -> Result<TranslationResponse>;
}
