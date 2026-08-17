use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::Result;

use super::{SttInput, SttResult};

#[async_trait]
pub trait SttProvider: Send + Sync {
    async fn transcribe(
        &self,
        input: SttInput,
        cancellation: &CancellationToken,
    ) -> Result<SttResult>;
}
