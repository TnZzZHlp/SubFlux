use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::TranslatorConfig,
    error::{AppError, Result, api_response_error},
};

use super::{
    TranslationRequest, TranslationResponse,
    prompt::{system_prompt, user_payload},
    provider::Translator,
    sse::{SseEvent, read_response},
};

#[derive(Clone)]
pub struct OpenAiCompatibleTranslator {
    client: Client,
    endpoint: Url,
    api_key: String,
    model: String,
}

impl OpenAiCompatibleTranslator {
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
            endpoint: endpoint(&config.base_url, "chat/completions")?,
            api_key: config.api_key.expose().to_owned(),
            model: config.model.clone(),
        })
    }
}

#[async_trait]
impl Translator for OpenAiCompatibleTranslator {
    async fn translate(
        &self,
        request: TranslationRequest,
        cancellation: &CancellationToken,
    ) -> Result<TranslationResponse> {
        let payload = OpenAiRequest {
            model: &self.model,
            temperature: 0.2,
            stream: true,
            messages: vec![
                Message {
                    role: "system",
                    content: system_prompt(&request),
                },
                Message {
                    role: "user",
                    content: user_payload(&request)?,
                },
            ],
        };
        debug!(model = %self.model, segments = request.segments.len(), "sending OpenAI-compatible translation request");
        let started = Instant::now();
        let send = self
            .client
            .post(self.endpoint.clone())
            .header(header::AUTHORIZATION, format!("Bearer {}", self.api_key))
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
            "OpenAI-compatible streaming translation response received"
        );
        let translated = parse_translation_json(&content)?;
        translated.validate_for(&request)?;
        Ok(translated)
    }
}

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    temperature: f32,
    stream: bool,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct OpenAiStreamResponse {
    choices: Vec<OpenAiStreamChoice>,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
}

#[derive(Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}

fn stream_content(events: &[SseEvent]) -> Result<String> {
    let mut content = String::new();
    for event in events {
        if event.data.trim() == "[DONE]" {
            break;
        }
        let response: OpenAiStreamResponse =
            serde_json::from_str(&event.data).map_err(|error| {
                AppError::InvalidApiResponse(format!("invalid chat completion SSE JSON: {error}"))
            })?;
        if let Some(text) = response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.delta.content)
        {
            content.push_str(&text);
        }
    }
    if content.is_empty() {
        Err(AppError::InvalidApiResponse(
            "chat completion stream had no text content".into(),
        ))
    } else {
        Ok(content)
    }
}

pub(crate) fn endpoint(base_url: &str, suffix: &str) -> Result<Url> {
    let normalized = base_url.trim_end_matches('/');
    let complete = if normalized.ends_with(suffix) {
        normalized.to_owned()
    } else {
        format!("{normalized}/{suffix}")
    };
    Url::parse(&complete)
        .map_err(|error| AppError::InvalidConfig(format!("invalid provider base URL: {error}")))
}

pub(crate) fn parse_translation_json(content: &str) -> Result<TranslationResponse> {
    let content = strip_code_fence(content.trim());
    let value: Value = serde_json::from_str(content)
        .map_err(|error| AppError::InvalidApiResponse(format!("response was not JSON: {error}")))?;
    let entries_value = match value {
        Value::Array(_) => value,
        Value::Object(mut object) => object
            .remove("translations")
            .ok_or_else(|| AppError::InvalidApiResponse("JSON object lacks translations".into()))?,
        _ => {
            return Err(AppError::InvalidApiResponse(
                "translation JSON must be an array or translations object".into(),
            ));
        }
    };
    let entries = serde_json::from_value(entries_value).map_err(|error| {
        AppError::InvalidApiResponse(format!("invalid translation entries: {error}"))
    })?;
    Ok(TranslationResponse { entries })
}

fn strip_code_fence(content: &str) -> &str {
    content
        .strip_prefix("```json")
        .or_else(|| content.strip_prefix("```JSON"))
        .or_else(|| content.strip_prefix("```"))
        .map_or(content, strip_code_fence_content)
}

fn strip_code_fence_content(inner: &str) -> &str {
    inner.trim().strip_suffix("```").unwrap_or(inner).trim()
}

fn api_error(status: u16, bytes: &[u8], api_key: &str) -> AppError {
    warn!(
        status,
        "OpenAI-compatible provider rejected translation request"
    );
    api_response_error(status, bytes, api_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_openai_sse_text_deltas() {
        let content = stream_content(&[
            SseEvent {
                event: None,
                data: r#"{"choices":[{"delta":{"content":"["}}]}"#.into(),
            },
            SseEvent {
                event: None,
                data: r#"{"choices":[{"delta":{"content":"{}"}}]}"#.into(),
            },
            SseEvent {
                event: None,
                data: "[DONE]".into(),
            },
        ])
        .unwrap();
        assert_eq!(content, "[{}");
    }

    #[test]
    fn reads_object_or_array_translation_json() {
        assert_eq!(
            parse_translation_json(r#"{"translations":[{"id":1,"text":"ok"}]}"#)
                .unwrap()
                .entries[0]
                .text,
            "ok"
        );
        assert_eq!(
            parse_translation_json(r#"[{"id":1,"text":"ok"}]"#)
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}
