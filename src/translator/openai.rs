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
    let content = escape_unescaped_string_controls(strip_code_fence(trim_json_whitespace(content)));
    let value: Value = serde_json::from_str(&content)
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

fn escape_unescaped_string_controls(content: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut escaped = false;
    let mut in_string = false;
    let mut normalized = String::with_capacity(content.len());
    for character in content.chars() {
        if in_string && !escaped && character <= '\u{001F}' {
            let value = character as u8;
            normalized.push_str("\\u00");
            normalized.push(char::from(HEX[usize::from(value >> 4)]));
            normalized.push(char::from(HEX[usize::from(value & 0x0F)]));
        } else {
            normalized.push(character);
        }

        if in_string {
            if character == '\\' {
                escaped = !escaped;
            } else {
                if character == '"' && !escaped {
                    in_string = false;
                }
                escaped = false;
            }
        } else if character == '"' {
            in_string = true;
        }
    }
    normalized
}

fn strip_code_fence(content: &str) -> &str {
    content
        .strip_prefix("```json")
        .or_else(|| content.strip_prefix("```JSON"))
        .or_else(|| content.strip_prefix("```"))
        .map_or(content, strip_code_fence_content)
}

fn trim_json_whitespace(content: &str) -> &str {
    content.trim_matches([' ', '\t', '\n', '\r'])
}

fn strip_code_fence_content(inner: &str) -> &str {
    let inner = trim_json_whitespace(inner);
    trim_json_whitespace(inner.strip_suffix("```").unwrap_or(inner))
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

    #[test]
    fn repairs_unescaped_controls_inside_translation_text() {
        let raw_controls = format!("NUL:\0 LF:\n CR:\r TAB:\t SOH:{}", '\u{0001}');
        let response = format!("```json\n[{{\"id\":1,\"text\":\"{raw_controls}\"}}]\n```");

        assert_eq!(
            parse_translation_json(&response).unwrap().entries[0].text,
            raw_controls
        );
    }

    #[test]
    fn preserves_valid_escapes_while_repairing_raw_controls() {
        let response = r#"[{"id":1,"text":"escaped quote: \" and slash: \\; raw:
"}]"#;

        assert_eq!(
            parse_translation_json(response).unwrap().entries[0].text,
            "escaped quote: \" and slash: \\; raw:\n"
        );
    }

    #[test]
    fn rejects_invalid_controls_outside_json_strings() {
        for control in ['\0', '\u{000B}', '\u{000C}'] {
            let response = format!("{control}[{{\"id\":1,\"text\":\"ok\"}}]{control}");
            assert!(matches!(
                parse_translation_json(&response),
                Err(AppError::InvalidApiResponse(_))
            ));
        }
    }

    #[test]
    fn parses_streamed_translation_with_raw_newline() {
        let translation = "[{\"id\":1,\"text\":\"first\nsecond\"}]";
        let data = serde_json::json!({
            "choices": [{"delta": {"content": translation}}],
        })
        .to_string();
        let content = stream_content(&[SseEvent { event: None, data }]).unwrap();

        assert_eq!(
            parse_translation_json(&content).unwrap().entries[0].text,
            "first\nsecond"
        );
    }
}
