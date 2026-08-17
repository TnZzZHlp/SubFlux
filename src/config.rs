use std::{collections::HashMap, env, fmt, time::Duration};

use crate::error::{AppError, Result};

#[derive(Clone, Eq, PartialEq, Hash)]
pub struct LanguageCode(String);

impl LanguageCode {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.eq_ignore_ascii_case("auto") {
            return Ok(Self("auto".into()));
        }

        let mut parts = value.split('-');
        let Some(primary) = parts.next() else {
            return Err(AppError::InvalidConfig(
                "language code cannot be empty".into(),
            ));
        };
        if !(2..=3).contains(&primary.len()) || !primary.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(AppError::InvalidConfig(format!(
                "invalid BCP 47 language code: {value}"
            )));
        }
        if !parts.all(|part| {
            (2..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
        }) {
            return Err(AppError::InvalidConfig(format!(
                "invalid BCP 47 language code: {value}"
            )));
        }
        Ok(Self(value))
    }

    pub fn auto() -> Self {
        Self("auto".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn display_name(&self) -> &'static str {
        match self.0.as_str() {
            "auto" => "自动检测",
            "zh-CN" => "简体中文",
            "zh-TW" => "繁体中文",
            "ja" => "日语",
            "en" => "英语",
            "ko" => "韩语",
            "fr" => "法语",
            "de" => "德语",
            "es" => "西班牙语",
            _ => "自定义语言",
        }
    }
}

impl fmt::Debug for LanguageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LanguageCode").field(&self.0).finish()
    }
}

impl fmt::Display for LanguageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn is_configured(&self) -> bool {
        !self.0.trim().is_empty()
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    pub fn masked(&self) -> String {
        let value = self.0.trim();
        if value.is_empty() {
            return "<未配置>".into();
        }
        let head = value.chars().take(4).collect::<String>();
        let tail: String = value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{head}****{tail}")
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApiKey").field(&self.masked()).finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslatorApiFormat {
    OpenAi,
    Anthropic,
}

impl TranslatorApiFormat {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "openai" | "openai-compatible" => Ok(Self::OpenAi),
            "anthropic" | "anthropic-compatible" => Ok(Self::Anthropic),
            other => Err(AppError::InvalidConfig(format!(
                "TRANSLATOR_API_FORMAT must be openai or anthropic, got {other}"
            ))),
        }
    }
}

impl fmt::Display for TranslatorApiFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OpenAi => "OpenAI 兼容接口",
            Self::Anthropic => "Anthropic 兼容接口",
        })
    }
}

#[derive(Clone, Debug)]
pub struct TranslatorConfig {
    pub provider: String,
    pub api_format: TranslatorApiFormat,
    pub base_url: String,
    pub api_key: ApiKey,
    pub model: String,
    pub chunk_size: usize,
    pub context_before: usize,
    pub context_after: usize,
    pub max_retries: usize,
}

#[derive(Clone, Debug)]
pub struct SttConfig {
    pub provider: String,
    pub base_url: String,
    pub api_key: ApiKey,
    pub model: String,
    pub language: LanguageCode,
    /// Maximum non-overlapping source duration in each STT request.
    pub chunk_seconds: u64,
    /// Context retained on both sides of each STT chunk boundary.
    pub chunk_overlap_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub translator: TranslatorConfig,
    pub stt: SttConfig,
    pub source_language: LanguageCode,
    pub target_language: LanguageCode,
    pub http_timeout: Duration,
    pub output_overwrite: bool,
}

impl Config {
    /// Loads exactly the current working directory's `.env` without mutating
    /// process environment variables. This makes Settings reload reliable and
    /// lets an already-set system variable override the local file.
    pub fn load() -> Result<Self> {
        let dotenv_values = match dotenvy::from_path_iter(".env") {
            Ok(iterator) => {
                let mut values = HashMap::new();
                for item in iterator {
                    let (name, value) = item.map_err(|error| {
                        AppError::InvalidConfig(format!("could not load .env: {error}"))
                    })?;
                    // Match dotenvy's normal loading behavior: the first
                    // declaration wins when a key appears more than once.
                    values.entry(name).or_insert(value);
                }
                values
            }
            Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                HashMap::new()
            }
            Err(error) => {
                return Err(AppError::InvalidConfig(format!(
                    "could not load .env: {error}"
                )));
            }
        };
        Self::from_getter(|name| {
            env::var(name)
                .ok()
                .or_else(|| dotenv_values.get(name).cloned())
        })
    }

    pub fn from_map(values: &HashMap<String, String>) -> Result<Self> {
        Self::from_getter(|name| values.get(name).cloned())
    }

    fn from_getter(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get_or = |name: &str, default: &str| get(name).unwrap_or_else(|| default.into());
        let provider = get_or("TRANSLATOR_PROVIDER", "openai");
        let api_format = TranslatorApiFormat::parse(&get_or(
            "TRANSLATOR_API_FORMAT",
            if provider.eq_ignore_ascii_case("anthropic") {
                "anthropic"
            } else {
                "openai"
            },
        ))?;
        let (chunk_size_name, chunk_size_value) = get("TRANSLATOR_CHUNK_SIZE")
            .map(|value| ("TRANSLATOR_CHUNK_SIZE", value))
            .or_else(|| {
                get("TRANSLATOR_MAX_SEGMENTS_PER_REQUEST")
                    .map(|value| ("TRANSLATOR_MAX_SEGMENTS_PER_REQUEST", value))
            })
            .unwrap_or_else(|| ("TRANSLATOR_CHUNK_SIZE", "30".into()));
        let chunk_size = parse_positive_usize(chunk_size_name, &chunk_size_value)?;
        let context_before = parse_usize(
            "TRANSLATOR_CONTEXT_BEFORE",
            &get_or("TRANSLATOR_CONTEXT_BEFORE", "10"),
        )?;
        let context_after = parse_usize(
            "TRANSLATOR_CONTEXT_AFTER",
            &get_or("TRANSLATOR_CONTEXT_AFTER", "5"),
        )?;
        let max_retries = parse_usize(
            "TRANSLATOR_MAX_RETRIES",
            &get_or("TRANSLATOR_MAX_RETRIES", "3"),
        )?;
        let stt_chunk_seconds =
            parse_positive_u64("STT_CHUNK_SECONDS", &get_or("STT_CHUNK_SECONDS", "600"))?;
        let stt_chunk_overlap_seconds = parse_u64(
            "STT_CHUNK_OVERLAP_SECONDS",
            &get_or("STT_CHUNK_OVERLAP_SECONDS", "2"),
        )?;
        if stt_chunk_overlap_seconds >= stt_chunk_seconds {
            return Err(AppError::InvalidConfig(
                "STT_CHUNK_OVERLAP_SECONDS must be less than STT_CHUNK_SECONDS".into(),
            ));
        }
        let timeout_seconds = parse_positive_u64(
            "HTTP_TIMEOUT_SECONDS",
            &get_or("HTTP_TIMEOUT_SECONDS", "120"),
        )?;
        let output_overwrite =
            parse_bool("OUTPUT_OVERWRITE", &get_or("OUTPUT_OVERWRITE", "false"))?;

        let target_language = LanguageCode::parse(get_or("TARGET_LANGUAGE", "zh-CN"))?;
        if target_language == LanguageCode::auto() {
            return Err(AppError::InvalidConfig(
                "TARGET_LANGUAGE cannot be auto; choose a BCP 47 code".into(),
            ));
        }

        Ok(Self {
            translator: TranslatorConfig {
                provider,
                api_format,
                base_url: get_or("TRANSLATOR_BASE_URL", "https://api.openai.com/v1"),
                api_key: ApiKey::new(get_or("TRANSLATOR_API_KEY", "")),
                model: get_or("TRANSLATOR_MODEL", "gpt-4o-mini"),
                chunk_size,
                context_before,
                context_after,
                max_retries,
            },
            stt: SttConfig {
                provider: get_or("STT_PROVIDER", "openai"),
                base_url: get_or("STT_BASE_URL", "https://api.openai.com/v1"),
                api_key: ApiKey::new(get_or("STT_API_KEY", "")),
                model: get_or("STT_MODEL", "whisper-1"),
                language: LanguageCode::parse(get_or("STT_LANGUAGE", "auto"))?,
                chunk_seconds: stt_chunk_seconds,
                chunk_overlap_seconds: stt_chunk_overlap_seconds,
            },
            source_language: LanguageCode::parse(get_or("SOURCE_LANGUAGE", "auto"))?,
            target_language,
            http_timeout: Duration::from_secs(timeout_seconds),
            output_overwrite,
        })
    }
}

fn parse_usize(name: &str, value: &str) -> Result<usize> {
    value
        .parse()
        .map_err(|_| AppError::InvalidConfig(format!("{name} must be an integer")))
}

fn parse_positive_usize(name: &str, value: &str) -> Result<usize> {
    let parsed = parse_usize(name, value)?;
    if parsed == 0 {
        Err(AppError::InvalidConfig(format!(
            "{name} must be greater than zero"
        )))
    } else {
        Ok(parsed)
    }
}

fn parse_u64(name: &str, value: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| AppError::InvalidConfig(format!("{name} must be an integer")))
}

fn parse_positive_u64(name: &str, value: &str) -> Result<u64> {
    let parsed = parse_u64(name, value)?;
    if parsed == 0 {
        Err(AppError::InvalidConfig(format!(
            "{name} must be greater than zero"
        )))
    } else {
        Ok(parsed)
    }
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(AppError::InvalidConfig(format!(
            "{name} must be true or false"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_secret_without_exposing_it() {
        assert_eq!(ApiKey::new("sk-12345678").masked(), "sk-1****5678");
        assert_eq!(ApiKey::new("").masked(), "<未配置>");
    }

    #[test]
    fn validates_target_language() {
        let mut values = HashMap::new();
        values.insert("TARGET_LANGUAGE".into(), "auto".into());
        assert!(matches!(
            Config::from_map(&values),
            Err(AppError::InvalidConfig(_))
        ));
    }

    #[test]
    fn loads_context_window_and_accepts_zero_context() {
        let values = HashMap::from([
            ("TRANSLATOR_CHUNK_SIZE".into(), "12".into()),
            ("TRANSLATOR_CONTEXT_BEFORE".into(), "0".into()),
            ("TRANSLATOR_CONTEXT_AFTER".into(), "0".into()),
        ]);
        let config = Config::from_map(&values).unwrap();
        assert_eq!(config.translator.chunk_size, 12);
        assert_eq!(config.translator.context_before, 0);
        assert_eq!(config.translator.context_after, 0);
    }

    #[test]
    fn supports_legacy_chunk_size_and_rejects_zero_new_chunk_size() {
        let legacy = HashMap::from([("TRANSLATOR_MAX_SEGMENTS_PER_REQUEST".into(), "7".into())]);
        assert_eq!(Config::from_map(&legacy).unwrap().translator.chunk_size, 7);

        let zero = HashMap::from([("TRANSLATOR_CHUNK_SIZE".into(), "0".into())]);
        assert!(matches!(
            Config::from_map(&zero),
            Err(AppError::InvalidConfig(_))
        ));
    }

    #[test]
    fn loads_stt_chunk_configuration_and_rejects_an_invalid_overlap() {
        let values = HashMap::from([
            ("STT_CHUNK_SECONDS".into(), "480".into()),
            ("STT_CHUNK_OVERLAP_SECONDS".into(), "3".into()),
        ]);
        let config = Config::from_map(&values).unwrap();
        assert_eq!(config.stt.chunk_seconds, 480);
        assert_eq!(config.stt.chunk_overlap_seconds, 3);

        let invalid = HashMap::from([
            ("STT_CHUNK_SECONDS".into(), "10".into()),
            ("STT_CHUNK_OVERLAP_SECONDS".into(), "10".into()),
        ]);
        assert!(matches!(
            Config::from_map(&invalid),
            Err(AppError::InvalidConfig(_))
        ));
    }
}
