use crate::{
    config::LanguageCode,
    error::Result,
    media::AudioChunk,
    stt::SttResult,
    subtitle::{SpeechSegment, SubtitleDocument},
};

/// Keeps STT's timestamped result at the pipeline boundary and creates the
/// same document type used by embedded/external subtitles.
pub fn document_from_stt_result(result: SttResult) -> Result<SubtitleDocument> {
    SubtitleDocument::from_speech_segments(result.segments, result.language)
}

/// Collects local timestamped responses from overlapping audio chunks and
/// converts them into one source-timeline result. Each chunk owns only the
/// middle, non-overlapping portion of its response.
#[derive(Default)]
pub(crate) struct ChunkedSttResults {
    language: Option<LanguageCode>,
    segments: Vec<SpeechSegment>,
}

impl ChunkedSttResults {
    pub(crate) fn absorb(&mut self, result: SttResult, chunk: &AudioChunk) {
        let SttResult { language, segments } = result;
        if self.language.is_none() {
            self.language = language;
        }
        self.segments
            .extend(segments.into_iter().filter_map(|segment| {
                let start_ms = chunk.source_start_ms.saturating_add(segment.start_ms);
                let end_ms = chunk.source_start_ms.saturating_add(segment.end_ms);
                let midpoint_ms = start_ms.saturating_add(end_ms.saturating_sub(start_ms) / 2);
                chunk
                    .retains_midpoint(midpoint_ms)
                    .then_some(SpeechSegment {
                        start_ms,
                        end_ms,
                        text: segment.text,
                    })
            }));
    }

    pub(crate) fn finish(mut self) -> SttResult {
        self.segments
            .sort_by_key(|segment| (segment.start_ms, segment.end_ms));
        let mut merged: Vec<SpeechSegment> = Vec::with_capacity(self.segments.len());
        for segment in self.segments {
            if let Some(previous) = merged.last_mut()
                && previous.text == segment.text
                && segment.start_ms <= previous.end_ms
            {
                previous.end_ms = previous.end_ms.max(segment.end_ms);
            } else {
                merged.push(segment);
            }
        }
        SttResult {
            language: self.language,
            segments: merged,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio_util::sync::CancellationToken;

    use crate::{
        config::LanguageCode,
        error::Result,
        pipeline::subtitle::{TranslationContext, translate_document},
        stt::{SttInput, SttProvider, SttResult},
        subtitle::SpeechSegment,
        translator::{TranslationItem, TranslationRequest, TranslationResponse, Translator},
    };

    use super::*;

    struct MockSttProvider;

    #[async_trait]
    impl SttProvider for MockSttProvider {
        async fn transcribe(
            &self,
            _input: SttInput,
            _cancellation: &CancellationToken,
        ) -> Result<SttResult> {
            Ok(SttResult {
                language: Some(LanguageCode::parse("ja").unwrap()),
                segments: vec![SpeechSegment {
                    start_ms: 1_200,
                    end_ms: 4_700,
                    text: "こんにちは".into(),
                }],
            })
        }
    }

    struct MockTranslator;

    #[async_trait]
    impl Translator for MockTranslator {
        async fn translate(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            Ok(TranslationResponse {
                entries: request
                    .segments
                    .into_iter()
                    .map(|entry| TranslationItem {
                        id: entry.id,
                        text: format!("译: {}", entry.text),
                    })
                    .collect(),
            })
        }
    }

    #[tokio::test]
    async fn mock_stt_flows_into_translated_srt() {
        let stt = MockSttProvider;
        let result = stt
            .transcribe(
                SttInput {
                    audio_path: "unused.wav".into(),
                    language: None,
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut document = document_from_stt_result(result).unwrap();
        let (events, _receiver) = unbounded_channel();
        let cancellation = CancellationToken::new();
        let source_language = LanguageCode::parse("ja").unwrap();
        let target_language = LanguageCode::parse("zh-CN").unwrap();
        translate_document(
            &mut document,
            TranslationContext {
                source_language: &source_language,
                target_language: &target_language,
                chunk_size: 30,
                context_before: 10,
                context_after: 5,
                max_retries: 0,
                translator: &MockTranslator,
                cancellation: &cancellation,
                events: &events,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            document
                .render_with_mode(crate::subtitle::SubtitleOutputMode::Translated)
                .unwrap(),
            "1\n00:00:01,200 --> 00:00:04,700\n译: こんにちは\n"
        );
    }

    #[test]
    fn offsets_segments_and_keeps_each_boundary_once() {
        let first_chunk = AudioChunk {
            source_start_ms: 0,
            duration_ms: 602_000,
            retain_start_ms: 0,
            retain_end_ms: 600_000,
        };
        let second_chunk = AudioChunk {
            source_start_ms: 598_000,
            duration_ms: 604_000,
            retain_start_ms: 600_000,
            retain_end_ms: 1_200_000,
        };
        let mut results = ChunkedSttResults::default();
        results.absorb(
            SttResult {
                language: Some(LanguageCode::parse("ja").unwrap()),
                segments: vec![
                    SpeechSegment {
                        start_ms: 1_000,
                        end_ms: 2_000,
                        text: "first chunk".into(),
                    },
                    SpeechSegment {
                        start_ms: 599_000,
                        end_ms: 601_000,
                        text: "boundary speech".into(),
                    },
                ],
            },
            &first_chunk,
        );
        results.absorb(
            SttResult {
                language: None,
                segments: vec![SpeechSegment {
                    start_ms: 1_000,
                    end_ms: 3_000,
                    text: "boundary speech".into(),
                }],
            },
            &second_chunk,
        );

        let result = results.finish();
        assert_eq!(result.language, Some(LanguageCode::parse("ja").unwrap()));
        assert_eq!(
            result.segments,
            vec![
                SpeechSegment {
                    start_ms: 1_000,
                    end_ms: 2_000,
                    text: "first chunk".into(),
                },
                SpeechSegment {
                    start_ms: 599_000,
                    end_ms: 601_000,
                    text: "boundary speech".into(),
                },
            ]
        );
    }
}
