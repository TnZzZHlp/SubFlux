use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::TranslatorConfig,
    error::{AppError, Result, api_response_error},
};

use super::{
    TranslationRequest, TranslationResponse,
    openai::{endpoint, parse_translation_json},
    prompt::{system_prompt, user_payload},
    provider::Translator,
};

#[derive(Clone)]
pub struct AnthropicCompatibleTranslator {
    client: Client,
    endpoint: Url,
    api_key: String,
    model: String,
}

impl AnthropicCompatibleTranslator {
    pub fn new(config: &TranslatorConfig, timeout: Duration) -> Result<Self> {
        if !config.api_key.is_configured() {
            return Err(AppError::MissingConfiguration("TRANSLATOR_API_KEY"));
        }
        if config.model.trim().is_empty() {
            return Err(AppError::MissingConfiguration("TRANSLATOR_MODEL"));
        }
        Ok(Self {
            client: Client::builder()
                .timeout(timeout)
                .build()
                .map_err(AppError::Http)?,
            endpoint: endpoint(&config.base_url, "messages")?,
            api_key: config.api_key.expose().to_owned(),
            model: config.model.clone(),
        })
    }
}

#[async_trait]
impl Translator for AnthropicCompatibleTranslator {
    async fn translate(
        &self,
        request: TranslationRequest,
        cancellation: &CancellationToken,
    ) -> Result<TranslationResponse> {
        let input = user_payload(&request)?;
        let payload = AnthropicRequest {
            model: &self.model,
            max_tokens: 4_096,
            system: system_prompt(&request),
            messages: vec![AnthropicMessage {
                role: "user",
                content: input,
            }],
        };
        debug!(model = %self.model, segments = request.segments.len(), "sending Anthropic-compatible translation request");
        let started = Instant::now();
        let send = self
            .client
            .post(self.endpoint.clone())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send();
        let response = tokio::select! {
            result = send => result.map_err(AppError::Http)?,
            () = cancellation.cancelled() => return Err(AppError::Cancelled),
        };
        let status = response.status();
        let bytes = response.bytes().await.map_err(AppError::Http)?;
        debug!(
            model = %self.model,
            segments = request.segments.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "Anthropic-compatible translation response received"
        );
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &bytes, &self.api_key));
        }
        let response: AnthropicResponse = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::InvalidApiResponse(format!("invalid messages response JSON: {error}"))
        })?;
        let content: String = response
            .content
            .into_iter()
            .filter(|part| part.kind == "text")
            .filter_map(|part| part.text)
            .collect();
        if content.is_empty() {
            return Err(AppError::InvalidApiResponse(
                "messages response had no text content".into(),
            ));
        }
        let translated = parse_translation_json(&content)?;
        translated.validate_for(&request)?;
        Ok(translated)
    }
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

fn api_error(status: u16, bytes: &[u8], api_key: &str) -> AppError {
    warn!(
        status,
        "Anthropic-compatible provider rejected translation request"
    );
    api_response_error(status, bytes, api_key)
}
