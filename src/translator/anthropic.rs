use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
    structured_output::{rejects_structured_output_field, translation_schema},
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

    async fn send_request(
        &self,
        payload: &AnthropicRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<reqwest::Response> {
        let send = self
            .client
            .post(self.endpoint.clone())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::CONTENT_TYPE, "application/json")
            .json(payload)
            .send();
        tokio::select! {
            result = send => result.map_err(AppError::Http),
            () = cancellation.cancelled() => Err(AppError::Cancelled),
        }
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
        let mut payload = AnthropicRequest {
            model: &self.model,
            max_tokens: 4_096,
            stream: true,
            system: system_prompt(&request),
            messages: vec![AnthropicMessage {
                role: "user",
                content: input,
            }],
            output_config: Some(output_config(&request)),
        };
        debug!(model = %self.model, segments = request.segments.len(), "sending Anthropic-compatible translation request");
        let started = Instant::now();
        let mut response = self.send_request(&payload, cancellation).await?;
        if !response.status().is_success() {
            let status = response.status();
            let bytes = response.bytes().await.map_err(AppError::Http)?;
            if !rejects_structured_output_field(status.as_u16(), &bytes, "output_config") {
                return Err(api_error(status.as_u16(), &bytes, &self.api_key));
            }
            warn!(
                status = status.as_u16(),
                "Anthropic-compatible provider rejected output_config; retrying without structured output"
            );
            payload.output_config = None;
            response = self.send_request(&payload, cancellation).await?;
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<Value>,
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

fn output_config(request: &TranslationRequest) -> Value {
    json!({
        "format": {
            "type": "json_schema",
            "schema": translation_schema(request),
        },
    })
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

    #[test]
    fn parses_streamed_translation_with_raw_newline() {
        let translation = "[{\"id\":1,\"text\":\"first\nsecond\"}]";
        let data = serde_json::json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": translation},
        })
        .to_string();
        let content = stream_content(&[SseEvent {
            event: Some("content_block_delta".into()),
            data,
        }])
        .unwrap();

        assert_eq!(
            parse_translation_json(&content).unwrap().entries[0].text,
            "first\nsecond"
        );
    }

    #[test]
    fn serializes_structured_output_config() {
        let request = TranslationRequest {
            source_language: crate::config::LanguageCode::parse("ja").unwrap(),
            target_language: crate::config::LanguageCode::parse("zh-CN").unwrap(),
            previous_context: Vec::new(),
            segments: vec![crate::translator::TranslationItem {
                id: 101,
                text: "one".into(),
            }],
            next_context: Vec::new(),
        };
        let payload = AnthropicRequest {
            model: "model",
            max_tokens: 4_096,
            stream: true,
            system: String::new(),
            messages: Vec::new(),
            output_config: Some(output_config(&request)),
        };
        let body = serde_json::to_value(payload).unwrap();

        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        let translations = &body["output_config"]["format"]["schema"]["properties"]["translations"];
        assert!(translations.get("minItems").is_none());
        assert!(translations.get("maxItems").is_none());
        assert_eq!(
            translations["items"]["properties"]["id"]["enum"],
            serde_json::json!([101])
        );
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn omits_output_config_for_fallback_requests() {
        let payload = AnthropicRequest {
            model: "model",
            max_tokens: 4_096,
            stream: true,
            system: String::new(),
            messages: Vec::new(),
            output_config: None,
        };

        assert!(
            serde_json::to_value(payload)
                .unwrap()
                .get("output_config")
                .is_none()
        );
    }
}
