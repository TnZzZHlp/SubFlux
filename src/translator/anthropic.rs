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
    sse::{SseEvent, read_response},
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
            return Err(AppError::MissingConfiguration("SUBFLUX_TRANSLATOR_API_KEY"));
        }
        if config.model.trim().is_empty() {
            return Err(AppError::MissingConfiguration("SUBFLUX_TRANSLATOR_MODEL"));
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
            stream: true,
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
        if !status.is_success() {
            let bytes = response.bytes().await.map_err(AppError::Http)?;
            return Err(api_error(status.as_u16(), &bytes, &self.api_key));
        }
        let events = read_response(response, cancellation).await?;
        let content = stream_content(&events)?;
        debug!(
            model = %self.model,
            segments = request.segments.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "Anthropic-compatible streaming translation response received"
        );
        let translated = parse_translation_json(&content)?;
        translated.validate_for(&request)?;
        Ok(translated)
    }
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    system: String,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    delta: Option<AnthropicDelta>,
}

#[derive(Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

fn stream_content(events: &[SseEvent]) -> Result<String> {
    let mut content = String::new();
    for event in events {
        let response: AnthropicStreamEvent =
            serde_json::from_str(&event.data).map_err(|error| {
                AppError::InvalidApiResponse(format!("invalid messages SSE JSON: {error}"))
            })?;
        if response.kind == "content_block_delta"
            && response
                .delta
                .as_ref()
                .and_then(|delta| delta.kind.as_deref())
                == Some("text_delta")
            && let Some(text) = response.delta.and_then(|delta| delta.text)
        {
            content.push_str(&text);
        }
    }
    if content.is_empty() {
        Err(AppError::InvalidApiResponse(
            "messages stream had no text content".into(),
        ))
    } else {
        Ok(content)
    }
}

fn api_error(status: u16, bytes: &[u8], api_key: &str) -> AppError {
    warn!(
        status,
        "Anthropic-compatible provider rejected translation request"
    );
    api_response_error(status, bytes, api_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_anthropic_sse_text_deltas() {
        let content = stream_content(&[
            SseEvent {
                event: Some("content_block_delta".into()),
                data: r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"["}}"#
                    .into(),
            },
            SseEvent {
                event: Some("content_block_delta".into()),
                data: r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"{}"}}"#
                    .into(),
            },
            SseEvent {
                event: Some("message_stop".into()),
                data: r#"{"type":"message_stop"}"#.into(),
            },
        ])
        .unwrap();
        assert_eq!(content, "[{}");
    }
}
