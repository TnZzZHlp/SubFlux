use std::{
    ffi::OsStr,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use reqwest::{Client, Url, multipart};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    config::{LanguageCode, SttConfig},
    error::{AppError, Result, api_response_error},
    subtitle::SpeechSegment,
    translator::openai::endpoint,
};

use super::{SttInput, SttResult, provider::SttProvider};

#[derive(Clone)]
pub struct OpenAiCompatibleStt {
    client: Client,
    endpoint: Url,
    api_key: String,
    model: String,
}

impl OpenAiCompatibleStt {
    pub fn new(config: &SttConfig, timeout: Duration) -> Result<Self> {
        if !config.api_key.is_configured() {
            return Err(AppError::MissingConfiguration("STT_API_KEY"));
        }
        if config.model.trim().is_empty() {
            return Err(AppError::MissingConfiguration("STT_MODEL"));
        }
        Ok(Self {
            client: Client::builder()
                .timeout(timeout)
                .build()
                .map_err(AppError::Http)?,
            endpoint: endpoint(&config.base_url, "audio/transcriptions")?,
            api_key: config.api_key.expose().to_owned(),
            model: config.model.clone(),
        })
    }
}

#[async_trait]
impl SttProvider for OpenAiCompatibleStt {
    async fn transcribe(
        &self,
        input: SttInput,
        cancellation: &CancellationToken,
    ) -> Result<SttResult> {
        if cancellation.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        let filename = input
            .audio_path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("audio.flac")
            .to_owned();
        let audio = multipart::Part::file(&input.audio_path)
            .await
            .map_err(|error| {
                AppError::SttError(format!("could not attach extracted audio: {error}"))
            })?
            .file_name(filename);
        let mut form = multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "verbose_json")
            .part("file", audio);
        if let Some(language) = input
            .language
            .filter(|language| language != &LanguageCode::auto())
        {
            form = form.text("language", language.to_string());
        }
        debug!(model = %self.model, "sending OpenAI-compatible STT request");
        let started = Instant::now();
        let send = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send();
        let response = tokio::select! {
            result = send => result.map_err(AppError::Http)?,
            () = cancellation.cancelled() => return Err(AppError::Cancelled),
        };
        let status = response.status();
        let bytes = response.bytes().await.map_err(AppError::Http)?;
        debug!(
            model = %self.model,
            elapsed_ms = started.elapsed().as_millis(),
            "OpenAI-compatible STT response received"
        );
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &bytes, &self.api_key));
        }
        parse_verbose_json(&bytes)
    }
}

fn api_error(status: u16, bytes: &[u8], api_key: &str) -> AppError {
    warn!(status, "OpenAI-compatible STT provider rejected request");
    api_response_error(status, bytes, api_key)
}

pub(crate) fn parse_verbose_json(bytes: &[u8]) -> Result<SttResult> {
    let response: VerboseResponse = serde_json::from_slice(bytes).map_err(|error| {
        AppError::InvalidApiResponse(format!("invalid STT verbose_json response: {error}"))
    })?;
    if response.segments.is_empty() {
        return Err(AppError::SttError(
            "STT response has no timestamped segments; verbose_json segments are required".into(),
        ));
    }
    let language = response
        .language
        .and_then(|value| LanguageCode::parse(value).ok());
    let mut segments = Vec::with_capacity(response.segments.len());
    for (index, segment) in response.segments.into_iter().enumerate() {
        let start_ms = seconds_to_ms(segment.start, index, "start")?;
        let end_ms = seconds_to_ms(segment.end, index, "end")?;
        if end_ms < start_ms {
            return Err(AppError::SttError(format!(
                "STT segment {} ends before it starts",
                index + 1
            )));
        }
        segments.push(SpeechSegment {
            start_ms,
            end_ms,
            text: segment.text,
        });
    }
    Ok(SttResult { language, segments })
}

fn seconds_to_ms(value: f64, index: usize, field: &str) -> Result<u64> {
    if !value.is_finite() || value < 0.0 {
        return Err(AppError::SttError(format!(
            "STT segment {} has invalid {field} time",
            index + 1
        )));
    }
    let duration = Duration::try_from_secs_f64(value).map_err(|_| {
        AppError::SttError(format!(
            "STT segment {} has invalid {field} time",
            index + 1
        ))
    })?;
    let rounded = duration
        .checked_add(Duration::from_micros(500))
        .ok_or_else(|| {
            AppError::SttError(format!(
                "STT segment {} has invalid {field} time",
                index + 1
            ))
        })?;
    u64::try_from(rounded.as_millis()).map_err(|_| {
        AppError::SttError(format!(
            "STT segment {} has invalid {field} time",
            index + 1
        ))
    })
}

#[derive(Deserialize)]
struct VerboseResponse {
    language: Option<String>,
    #[serde(default)]
    segments: Vec<VerboseSegment>,
}

#[derive(Deserialize)]
struct VerboseSegment {
    start: f64,
    end: f64,
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_segment_timestamps() {
        let result = parse_verbose_json(br#"{"text":"complete text only"}"#);
        assert!(matches!(result, Err(AppError::SttError(_))));
    }

    #[test]
    fn converts_timestamped_segments() {
        let result = parse_verbose_json(
            br#"{"language":"ja","segments":[{"start":1.2,"end":4.7,"text":"hello"}]}"#,
        )
        .unwrap();
        assert_eq!(result.segments[0].start_ms, 1200);
        assert_eq!(result.segments[0].end_ms, 4700);
    }
}
