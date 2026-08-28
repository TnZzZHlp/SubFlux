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
            return Err(AppError::MissingConfiguration("SUBFLUX_STT_API_KEY"));
        }
        if config.model.trim().is_empty() {
            return Err(AppError::MissingConfiguration("SUBFLUX_STT_MODEL"));
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
            .text("timestamp_granularities[]", "segment")
            .text("timestamp_granularities[]", "word")
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
    let VerboseResponse {
        language,
        segments: response_segments,
        words,
    } = response;
    if response_segments.is_empty() {
        return Err(AppError::SttError(
            "STT response has no timestamped segments; verbose_json segments are required".into(),
        ));
    }
    let word_starts = words
        .into_iter()
        .enumerate()
        .map(|(index, word)| seconds_to_ms(word.start, index, "word start"))
        .collect::<Result<Vec<_>>>()?;
    let language = language.and_then(|value| LanguageCode::parse(value).ok());
    let mut segments = Vec::with_capacity(response_segments.len());
    for (index, segment) in response_segments.into_iter().enumerate() {
        let segment_start_ms = seconds_to_ms(segment.start, index, "start")?;
        // Cap the end of a segment so it can never precede its start. Speech
        // recognition can emit sub-millisecond or boundary-skipping timestamps
        // at tight transitions; rounding a near-zero-duration segment to the
        // same or lower ms should not discard the whole transcript.
        let end_ms =
            seconds_to_ms(segment.end, index, "end")?.max(segment_start_ms.saturating_add(1));
        let start_ms = word_starts
            .iter()
            .copied()
            .find(|word_start_ms| *word_start_ms >= segment_start_ms && *word_start_ms <= end_ms)
            .unwrap_or(segment_start_ms);
        segments.push(SpeechSegment {
            start_ms,
            end_ms: end_ms.max(start_ms.saturating_add(1)),
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
    #[serde(default)]
    words: Vec<VerboseWord>,
}

#[derive(Deserialize)]
struct VerboseWord {
    start: f64,
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

    #[test]
    fn uses_word_timestamp_to_remove_leading_silence() {
        let result = parse_verbose_json(
            br#"{"language":"en","words":[{"word":"hello","start":4.2,"end":4.7}],"segments":[{"start":0.0,"end":5.0,"text":"hello"}]}"#,
        )
        .unwrap();

        assert_eq!(result.segments[0].start_ms, 4_200);
        assert_eq!(result.segments[0].end_ms, 5_000);
    }

    #[test]
    fn ignores_word_timestamps_outside_a_segment() {
        let result = parse_verbose_json(
            br#"{"words":[{"word":"later","start":5.1,"end":5.5}],"segments":[{"start":0.0,"end":5.0,"text":"hello"}]}"#,
        )
        .unwrap();

        assert_eq!(result.segments[0].start_ms, 0);
    }

    #[test]
    fn tolerates_a_segment_whose_end_precedes_its_start() {
        // Whisper-style STT can emit a tight transition where the rounded end
        // lands at or below the rounded start; that must not abort the whole
        // transcript. The end is clamped to start + 1ms instead.
        let result = parse_verbose_json(
            br#"{"language":"en","segments":[{"start":12.0006,"end":11.9998,"text":"tight"}]}"#,
        )
        .unwrap();
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].start_ms, 12001);
        assert_eq!(result.segments[0].end_ms, 12002);
        assert_eq!(result.segments[0].text, "tight");
    }
}
