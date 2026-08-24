use std::collections::BTreeSet;

use tokio::{
    sync::mpsc::UnboundedSender,
    time::{Duration, sleep},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::checkpoint::CheckpointStore;

use crate::{
    config::LanguageCode,
    error::{AppError, Result},
    event::{CheckpointPhase, TaskEvent},
    subtitle::{SubtitleDocument, SubtitleId},
    translator::{
        TranslationChunkConfig, TranslationRequest, TranslationResponse, Translator, build_chunks,
    },
};

/// In-memory recovery state.
///
/// Completed chunks remain applied to the document. If a later request
/// exhausts retries, callers can report which chunk failed without pretending
/// earlier work disappeared.
#[derive(Debug, Default)]
pub struct TranslationWorkState {
    pub completed_chunks: BTreeSet<usize>,
    pub pending_chunks: BTreeSet<usize>,
    pub failed_chunks: BTreeSet<usize>,
}

pub struct TranslationContext<'a> {
    pub source_language: &'a LanguageCode,
    pub target_language: &'a LanguageCode,
    pub chunk_size: usize,
    pub context_before: usize,
    pub context_after: usize,
    pub max_retries: usize,
    pub translator: &'a dyn Translator,
    pub cancellation: &'a CancellationToken,
    pub events: &'a UnboundedSender<TaskEvent>,
}

pub(crate) fn checkpoint_translations_valid(
    document: &SubtitleDocument,
    source_language: &LanguageCode,
    target_language: &LanguageCode,
    chunk_size: usize,
    context_before: usize,
    context_after: usize,
    checkpoint: &CheckpointStore,
) -> Result<bool> {
    let chunks = build_chunks(
        document,
        TranslationChunkConfig {
            chunk_size,
            context_before,
            context_after,
        },
    )?;
    if checkpoint.translation_len() > chunks.len() {
        return Ok(false);
    }
    Ok(chunks
        .iter()
        .take(checkpoint.translation_len())
        .all(|chunk| {
            checkpoint
                .translation_response(chunk.index)
                .is_some_and(|response| {
                    response
                        .validate_for(&translation_request(
                            chunk,
                            source_language,
                            target_language,
                        ))
                        .is_ok()
                })
        }))
}

pub async fn translate_document(
    document: &mut SubtitleDocument,
    context: TranslationContext<'_>,
) -> Result<TranslationWorkState> {
    translate_document_inner(document, context, None).await
}

pub(crate) async fn translate_document_with_checkpoint(
    document: &mut SubtitleDocument,
    context: TranslationContext<'_>,
    checkpoint: &mut CheckpointStore,
) -> Result<TranslationWorkState> {
    translate_document_inner(document, context, Some(checkpoint)).await
}

async fn translate_document_inner(
    document: &mut SubtitleDocument,
    context: TranslationContext<'_>,
    mut checkpoint: Option<&mut CheckpointStore>,
) -> Result<TranslationWorkState> {
    let chunks = build_chunks(
        document,
        TranslationChunkConfig {
            chunk_size: context.chunk_size,
            context_before: context.context_before,
            context_after: context.context_after,
        },
    )?;
    let total = chunks.iter().map(|chunk| chunk.segments.len()).sum();
    debug!(
        chunks = chunks.len(),
        entries = total,
        "starting batched subtitle translation"
    );
    let _ = context.events.send(TaskEvent::TranslationStarted { total });
    let mut state = TranslationWorkState {
        pending_chunks: chunks.iter().map(|chunk| chunk.index).collect(),
        ..TranslationWorkState::default()
    };
    let resumed = checkpoint
        .as_deref()
        .map_or(0, CheckpointStore::translation_len);
    if resumed > chunks.len() {
        return Err(AppError::CheckpointError(
            "checkpoint has too many translation chunks".into(),
        ));
    }
    let mut completed = 0;

    for chunk in chunks.iter().take(resumed) {
        let request = translation_request(chunk, context.source_language, context.target_language);
        let response = checkpoint
            .as_deref()
            .and_then(|checkpoint| checkpoint.translation_response(chunk.index))
            .ok_or_else(|| {
                AppError::CheckpointError("checkpoint translation chunk is missing".into())
            })?;
        response.validate_for(&request)?;
        apply_response(document, response)?;
        completed += chunk.segments.len();
        state.pending_chunks.remove(&chunk.index);
        state.completed_chunks.insert(chunk.index);
    }
    if completed > 0 {
        let _ = context.events.send(TaskEvent::CheckpointResumed {
            phase: CheckpointPhase::Translation,
            completed,
            total,
        });
    }

    for chunk in chunks.into_iter().skip(resumed) {
        check_cancelled(context.cancellation)?;
        let request = translation_request(&chunk, context.source_language, context.target_language);
        let response = translate_chunk_with_retry(
            &request,
            chunk.index,
            context.max_retries,
            context.translator,
            context.cancellation,
        )
        .await;
        match response {
            Ok(response) => {
                if let Some(checkpoint) = checkpoint.as_deref_mut() {
                    checkpoint
                        .record_translation(chunk.index, &response)
                        .await?;
                }
                apply_response(document, response)?;
                completed += chunk.segments.len();
                state.pending_chunks.remove(&chunk.index);
                state.completed_chunks.insert(chunk.index);
                let _ = context.events.send(TaskEvent::TranslationProgress {
                    completed,
                    total,
                    request: chunk.index + 1,
                });
            }
            Err(error) => {
                state.pending_chunks.remove(&chunk.index);
                state.failed_chunks.insert(chunk.index);
                return Err(error);
            }
        }
    }
    Ok(state)
}

fn translation_request(
    chunk: &crate::translator::TranslationChunk,
    source_language: &LanguageCode,
    target_language: &LanguageCode,
) -> TranslationRequest {
    TranslationRequest {
        source_language: source_language.clone(),
        target_language: target_language.clone(),
        previous_context: chunk.previous_context.clone(),
        segments: chunk.segments.clone(),
        next_context: chunk.next_context.clone(),
    }
}

fn apply_response(document: &mut SubtitleDocument, response: TranslationResponse) -> Result<()> {
    for item in response.entries {
        document.apply_translation(SubtitleId(item.id), item.text)?;
    }
    Ok(())
}

async fn translate_chunk_with_retry(
    request: &TranslationRequest,
    chunk_index: usize,
    max_retries: usize,
    translator: &dyn Translator,
    cancellation: &CancellationToken,
) -> Result<crate::translator::TranslationResponse> {
    let mut last_error = None;
    let mut correction = false;
    for attempt in 0..=max_retries {
        check_cancelled(cancellation)?;
        let response = if correction {
            translator
                .translate_correction(request.clone(), cancellation)
                .await
        } else {
            translator.translate(request.clone(), cancellation).await
        }
        .and_then(|response| {
            response.validate_for(request)?;
            Ok(response)
        });
        match response {
            Ok(response) => return Ok(response),
            Err(AppError::Cancelled) => return Err(AppError::Cancelled),
            Err(error) => {
                if let AppError::InvalidApiResponse(_) = &error {
                    correction = true;
                }
                last_error = Some(error);
                if attempt < max_retries {
                    warn!(
                        chunk = chunk_index + 1,
                        attempt = attempt + 1,
                        "translation request failed; retrying"
                    );
                    let delay = Duration::from_millis(250 * (1_u64 << attempt.min(4)));
                    tokio::select! {
                        () = sleep(delay) => {}
                        () = cancellation.cancelled() => return Err(AppError::Cancelled),
                    }
                }
            }
        }
    }
    let detail = last_error.map_or_else(
        || "provider did not return a result".into(),
        |error| error.safe_message(),
    );
    Err(AppError::TranslationError(format!(
        "chunk {} failed after {} attempt(s): {detail}",
        chunk_index + 1,
        max_retries + 1
    )))
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(AppError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::{
        config::LanguageCode,
        error::Result,
        subtitle::{SpeechSegment, SubtitleDocument},
        translator::{TranslationItem, TranslationRequest, TranslationResponse},
    };

    use super::*;

    struct MustNotRun;

    #[async_trait]
    impl Translator for MustNotRun {
        async fn translate(
            &self,
            _request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            panic!("cancelled translation should not call the provider")
        }
    }

    struct ContextAwareTranslator {
        calls: AtomicUsize,
        requests: Mutex<Vec<TranslationRequest>>,
    }

    #[async_trait]
    impl Translator for ContextAwareTranslator {
        async fn translate(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());

            if attempt == 0 {
                let mut entries = request.segments.clone();
                entries[0] = request.next_context[0].clone();
                return Ok(TranslationResponse { entries });
            }

            Ok(TranslationResponse {
                entries: request
                    .segments
                    .into_iter()
                    .map(|entry| TranslationItem {
                        id: entry.id,
                        text: format!("translated {}", entry.text),
                    })
                    .collect(),
            })
        }
    }

    struct CorrectiveTranslator {
        normal_calls: AtomicUsize,
        correction_calls: AtomicUsize,
    }

    #[async_trait]
    impl Translator for CorrectiveTranslator {
        async fn translate(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            self.normal_calls.fetch_add(1, Ordering::SeqCst);
            let mut entries = request.segments;
            entries.push(TranslationItem {
                id: 999,
                text: "extra".into(),
            });
            Ok(TranslationResponse { entries })
        }

        async fn translate_correction(
            &self,
            request: TranslationRequest,
            _cancellation: &CancellationToken,
        ) -> Result<TranslationResponse> {
            self.correction_calls.fetch_add(1, Ordering::SeqCst);
            Ok(TranslationResponse {
                entries: request
                    .segments
                    .into_iter()
                    .map(|item| TranslationItem {
                        id: item.id,
                        text: format!("corrected {}", item.text),
                    })
                    .collect(),
            })
        }
    }

    #[tokio::test]
    async fn cancelled_token_stops_before_a_translation_request() {
        let mut document = SubtitleDocument::from_speech_segments(
            vec![SpeechSegment {
                start_ms: 0,
                end_ms: 1,
                text: "hello".into(),
            }],
            None,
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (events, _receiver) = unbounded_channel();
        assert!(matches!(
            translate_document(
                &mut document,
                TranslationContext {
                    source_language: &LanguageCode::parse("en").unwrap(),
                    target_language: &LanguageCode::parse("zh-CN").unwrap(),
                    chunk_size: 1,
                    context_before: 0,
                    context_after: 0,
                    max_retries: 0,
                    translator: &MustNotRun,
                    cancellation: &cancellation,
                    events: &events,
                },
            )
            .await,
            Err(AppError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn context_is_source_only_and_context_ids_are_retried_not_written_back() {
        let mut document = SubtitleDocument::from_speech_segments(
            (1..=4)
                .map(|id| SpeechSegment {
                    start_ms: id,
                    end_ms: id + 1,
                    text: format!("line {id}"),
                })
                .collect(),
            None,
        )
        .unwrap();
        let translator = ContextAwareTranslator {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        };
        let cancellation = CancellationToken::new();
        let (events, _receiver) = unbounded_channel();
        let source_language = LanguageCode::parse("en").unwrap();
        let target_language = LanguageCode::parse("zh-CN").unwrap();

        let state = translate_document(
            &mut document,
            TranslationContext {
                source_language: &source_language,
                target_language: &target_language,
                chunk_size: 2,
                context_before: 1,
                context_after: 1,
                max_retries: 1,
                translator: &translator,
                cancellation: &cancellation,
                events: &events,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            state.completed_chunks.into_iter().collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(translator.calls.load(Ordering::SeqCst), 3);
        let requests = translator.requests.lock().unwrap();
        assert_eq!(
            requests[0]
                .segments
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            requests[0]
                .next_context
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(requests[0].segments, requests[1].segments);
        assert_eq!(requests[0].previous_context, requests[1].previous_context);
        assert_eq!(requests[0].next_context, requests[1].next_context);
        assert_eq!(requests[2].previous_context[0].id, 2);
        assert_eq!(requests[2].previous_context[0].text, "line 2");
        drop(requests);

        assert!(
            document
                .entries
                .iter()
                .enumerate()
                .all(|(index, entry)| entry.translated_text.as_deref()
                    == Some(&format!("translated line {}", index + 1)))
        );
    }

    #[tokio::test]
    async fn extra_response_entry_uses_correction_and_applies_only_valid_response() {
        let mut document = SubtitleDocument::from_speech_segments(
            vec![
                SpeechSegment {
                    start_ms: 0,
                    end_ms: 1,
                    text: "one".into(),
                },
                SpeechSegment {
                    start_ms: 1,
                    end_ms: 2,
                    text: "two".into(),
                },
            ],
            None,
        )
        .unwrap();
        let translator = CorrectiveTranslator {
            normal_calls: AtomicUsize::new(0),
            correction_calls: AtomicUsize::new(0),
        };
        let cancellation = CancellationToken::new();
        let (events, _receiver) = unbounded_channel();
        let source_language = LanguageCode::parse("en").unwrap();
        let target_language = LanguageCode::parse("zh-CN").unwrap();

        translate_document(
            &mut document,
            TranslationContext {
                source_language: &source_language,
                target_language: &target_language,
                chunk_size: 2,
                context_before: 0,
                context_after: 0,
                max_retries: 1,
                translator: &translator,
                cancellation: &cancellation,
                events: &events,
            },
        )
        .await
        .unwrap();

        assert_eq!(translator.normal_calls.load(Ordering::SeqCst), 1);
        assert_eq!(translator.correction_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            document
                .entries
                .iter()
                .map(|entry| entry.translated_text.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("corrected one"), Some("corrected two")]
        );
    }
}
