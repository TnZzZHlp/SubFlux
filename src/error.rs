use std::path::PathBuf;

use thiserror::Error;

pub type Result<T, E = AppError> = std::result::Result<T, E>;

/// API responses should never normally contain a credential, but gateways can
/// reflect malformed authorization data. Remove the configured secret before a
/// response reaches an error, log, or TUI message.
pub fn redact_secret(value: String, secret: &str) -> String {
    if secret.trim().is_empty() {
        value
    } else {
        value.replace(secret, "[REDACTED]")
    }
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
    /// A message safe to put in the TUI or a log.  In particular, a reqwest
    /// error is not formatted because request URLs can theoretically contain a
    /// user-supplied secret.
    pub fn safe_message(&self) -> String {
        match self {
            Self::Http(_) => {
                "HTTP request failed; check the endpoint and network connection".into()
            }
            Self::ApiError { message } => message.clone(),
            Self::ProbeFailed(message)
            | Self::AudioExtractionFailed(message)
            | Self::SubtitleExtractionFailed(message)
            | Self::SubtitleParseError(message)
            | Self::SubtitleWriteError(message)
            | Self::InvalidApiResponse(message)
            | Self::SttError(message)
            | Self::TranslationError(message)
            | Self::InvalidConfig(message) => limit(message, 500),
            _ => self.to_string(),
        }
    }
}

/// Builds a safe, user-visible API error. Provider bodies are deliberately
/// excluded because gateways may echo credentials or subtitle request data.
pub(crate) fn api_response_error(status: u16, _response_body: &[u8], _secret: &str) -> AppError {
    AppError::ApiError {
        message: format!("API 请求失败（HTTP {status}）；请检查服务端日志或稍后重试。"),
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
    fn redacts_reflected_credentials() {
        assert_eq!(
            redact_secret("gateway rejected sk-secret".into(), "sk-secret"),
            "gateway rejected [REDACTED]"
        );
    }

    #[test]
    fn api_errors_do_not_expose_provider_response_bodies() {
        let body = r#"{"error":{"message":"subtitle body","key":"sk-secret"}}"#;

        let message = api_response_error(403, body.as_bytes(), "sk-secret").safe_message();

        assert!(message.contains("HTTP 403"));
        assert!(!message.contains("subtitle body"));
        assert!(!message.contains("sk-secret"));
    }
}
