use std::{error::Error as StdError, path::PathBuf};

use reqwest::header::HeaderMap;
use thiserror::Error;

pub type Result<T, E = AppError> = std::result::Result<T, E>;

/// Response headers that commonly carry an upstream request id, covering
/// OpenAI (`x-request-id`, `openai-request-id`), Anthropic (`request-id`), and
/// OpenAI-compatible gateways.
const REQUEST_ID_HEADERS: [&str; 4] = [
    "x-request-id",
    "request-id",
    "x-client-request-id",
    "openai-request-id",
];

/// The upstream request id from the first recognised response header, if any.
pub(crate) fn upstream_request_id(headers: &HeaderMap) -> Option<String> {
    REQUEST_ID_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    })
}

/// Errors crossing module boundaries.  The variants intentionally describe the
/// operation that failed instead of collapsing the whole application into
/// opaque strings.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("ffmpeg was not found on PATH")]
    FfmpegNotFound,
    #[error("ffprobe was not found on PATH")]
    FfprobeNotFound,
    #[error("media probe failed: {0}")]
    ProbeFailed(String),
    #[error("audio extraction failed: {0}")]
    AudioExtractionFailed(String),
    #[error("media has no audio stream; speech recognition needs an audio track")]
    NoAudioStream,
    #[error("subtitle extraction failed: {0}")]
    SubtitleExtractionFailed(String),
    #[error("unsupported subtitle codec: {0}")]
    UnsupportedSubtitleCodec(String),
    #[error("unsupported subtitle format: {0}")]
    UnsupportedSubtitleFormat(String),
    #[error("subtitle parse error: {0}")]
    SubtitleParseError(String),
    #[error("subtitle write error: {0}")]
    SubtitleWriteError(String),
    #[error("HTTP request failed")]
    Http(#[source] reqwest::Error),
    #[error("{message}")]
    ApiError { message: String },
    #[error("invalid API response: {0}")]
    InvalidApiResponse(String),
    #[error("speech recognition failed: {0}")]
    SttError(String),
    #[error("translation failed: {0}")]
    TranslationError(String),
    #[error("checkpoint error: {0}")]
    CheckpointError(String),
    #[error("output already exists: {}", .0.display())]
    OutputExists(PathBuf),
    #[error("output skipped: {}", .0.display())]
    Skipped(PathBuf),
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("missing configuration: {0}")]
    MissingConfiguration(&'static str),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl AppError {
    /// The message shown in the TUI and written to the log. Failures keep
    /// useful diagnostics without exposing configured request URLs or
    /// provider response bodies.
    pub fn safe_message(&self) -> String {
        match self {
            Self::Http(error) => http_error_chain(error),
            Self::ApiError { message } => message.clone(),
            Self::ProbeFailed(message)
            | Self::AudioExtractionFailed(message)
            | Self::SubtitleExtractionFailed(message)
            | Self::SubtitleParseError(message)
            | Self::SubtitleWriteError(message)
            | Self::InvalidApiResponse(message)
            | Self::CheckpointError(message)
            | Self::InvalidConfig(message) => limit(message, 500),
            _ => self.to_string(),
        }
    }
}

/// Formats an error together with every cause below it, replacing reqwest's
/// URL-bearing top-level message with a safe classification.
fn http_error_chain(error: &reqwest::Error) -> String {
    let mut parts = vec![http_error_kind(error).to_owned()];
    let mut source = error.source();
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

fn http_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_builder() {
        "HTTP request builder failed"
    } else if error.is_timeout() {
        "HTTP request timed out"
    } else if error.is_connect() {
        "HTTP connection failed"
    } else if error.is_body() {
        "HTTP body error"
    } else if error.is_decode() {
        "HTTP response decode failed"
    } else if error.is_redirect() {
        "HTTP redirect failed"
    } else if error.is_upgrade() {
        "HTTP protocol upgrade failed"
    } else if error.is_status() {
        "HTTP status error"
    } else {
        "HTTP request failed"
    }
}

/// Builds the user-visible API error for a rejected request: the HTTP status
/// and upstream request id when the provider returned one. The response body
/// is deliberately omitted because it may contain credentials or subtitle
/// content.
pub(crate) fn api_response_error(
    status: u16,
    request_id: Option<&str>,
    _response_body: &[u8],
    secret: &str,
) -> AppError {
    let request_id = request_id.map_or_else(String::new, |id| {
        let id = if secret.trim().is_empty() {
            id.to_owned()
        } else {
            id.replace(secret, "[REDACTED]")
        };
        format!("request id: {id}\n")
    });
    AppError::ApiError {
        message: format!(
            "API 请求失败（HTTP {status}）\n{request_id}请求失败；请检查服务端日志或稍后重试。"
        ),
    }
}

fn limit(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let shortened: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_upstream_request_id_from_common_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("request-id", "req_abc".parse().expect("header value"));

        assert_eq!(upstream_request_id(&headers).as_deref(), Some("req_abc"));
    }

    #[test]
    fn api_errors_redact_secrets_from_request_ids_and_keep_benign_ids() {
        let body = r#"{"error":{"message":"subtitle text","key":"body-secret"}}"#;
        let secret = "sk-secret";

        let message =
            api_response_error(429, Some("req_sk-secret"), body.as_bytes(), secret).safe_message();

        assert!(message.contains("HTTP 429"));
        assert!(message.contains("[REDACTED]"));
        assert!(!message.contains("req_sk-secret"));
        assert!(!message.contains("subtitle text"));
        assert!(!message.contains("body-secret"));

        let benign = api_response_error(429, Some("req_123"), &[], secret).safe_message();
        assert!(benign.contains("req_123"));
    }

    #[tokio::test]
    async fn http_errors_do_not_expose_attached_urls() {
        let error = reqwest::Client::new()
            .get("ftp://user:sk-secret@example.com/path?api_key=sk-secret")
            .send()
            .await
            .expect_err("unsupported schemes fail before network access");

        let message = AppError::Http(error).safe_message();

        assert!(message.contains("URL scheme is not allowed"));
        assert!(!message.contains("ftp://user:sk-secret@example.com"));
        assert!(!message.contains("api_key=sk-secret"));
    }
}
