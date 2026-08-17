use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    config::LanguageCode,
    error::{AppError, Result},
    subtitle::SubtitleId,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranslationItem {
    pub id: u64,
    pub text: String,
}

impl From<(SubtitleId, String)> for TranslationItem {
    fn from((id, text): (SubtitleId, String)) -> Self {
        Self { id: id.0, text }
    }
}

#[derive(Clone, Debug)]
pub struct TranslationRequest {
    pub source_language: LanguageCode,
    pub target_language: LanguageCode,
    /// Source-language entries immediately before `segments`; they are
    /// read-only context and must never be returned by a provider.
    pub previous_context: Vec<TranslationItem>,
    /// The only entries that may be translated and written back.
    pub segments: Vec<TranslationItem>,
    /// Source-language entries immediately after `segments`; they are
    /// read-only context and must never be returned by a provider.
    pub next_context: Vec<TranslationItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationResponse {
    pub entries: Vec<TranslationItem>,
}

impl TranslationResponse {
    /// Providers can be creative even with an explicit prompt.  Rejecting an
    /// incomplete/reordered/duplicated response is safer than silently putting
    /// the wrong line on a video's timecode.
    pub fn validate_for(&self, request: &TranslationRequest) -> Result<()> {
        let expected: HashSet<u64> = request.segments.iter().map(|entry| entry.id).collect();
        if expected.len() != request.segments.len() {
            return Err(AppError::TranslationError(
                "translation request had duplicate subtitle IDs".into(),
            ));
        }
        if self.entries.len() != request.segments.len() {
            return Err(AppError::InvalidApiResponse(format!(
                "expected {} translations but received {}",
                request.segments.len(),
                self.entries.len()
            )));
        }
        let mut actual = HashSet::with_capacity(self.entries.len());
        for (position, entry) in self.entries.iter().enumerate() {
            if entry.text.trim().is_empty() {
                return Err(AppError::InvalidApiResponse(format!(
                    "translation response contains an empty translation for id {}",
                    entry.id
                )));
            }
            if !actual.insert(entry.id) {
                return Err(AppError::InvalidApiResponse(format!(
                    "translation response contains duplicate id {}",
                    entry.id
                )));
            }
            if !expected.contains(&entry.id) {
                return Err(AppError::InvalidApiResponse(format!(
                    "translation response contains unknown id {}",
                    entry.id
                )));
            }
            if entry.id != request.segments[position].id {
                return Err(AppError::InvalidApiResponse(format!(
                    "translation response changed subtitle order at position {}",
                    position + 1
                )));
            }
        }
        if actual != expected {
            return Err(AppError::InvalidApiResponse(
                "translation response omitted one or more subtitle IDs".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> TranslationRequest {
        TranslationRequest {
            source_language: LanguageCode::parse("ja").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            previous_context: vec![TranslationItem {
                id: 99,
                text: "before".into(),
            }],
            segments: vec![
                TranslationItem {
                    id: 101,
                    text: "one".into(),
                },
                TranslationItem {
                    id: 102,
                    text: "two".into(),
                },
            ],
            next_context: vec![TranslationItem {
                id: 103,
                text: "after".into(),
            }],
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_ids() {
        let request = request();
        for response in [
            TranslationResponse {
                entries: vec![TranslationItem {
                    id: 101,
                    text: "x".into(),
                }],
            },
            TranslationResponse {
                entries: vec![
                    TranslationItem {
                        id: 101,
                        text: "x".into(),
                    },
                    TranslationItem {
                        id: 101,
                        text: "y".into(),
                    },
                ],
            },
            TranslationResponse {
                entries: vec![
                    TranslationItem {
                        id: 101,
                        text: "x".into(),
                    },
                    TranslationItem {
                        id: 9,
                        text: "y".into(),
                    },
                ],
            },
        ] {
            assert!(matches!(
                response.validate_for(&request),
                Err(AppError::InvalidApiResponse(_))
            ));
        }
    }

    #[test]
    fn rejects_context_ids_empty_translations_and_reordered_entries() {
        let request = request();
        for response in [
            TranslationResponse {
                entries: vec![
                    TranslationItem {
                        id: 99,
                        text: "context".into(),
                    },
                    TranslationItem {
                        id: 101,
                        text: "x".into(),
                    },
                ],
            },
            TranslationResponse {
                entries: vec![
                    TranslationItem {
                        id: 101,
                        text: " ".into(),
                    },
                    TranslationItem {
                        id: 102,
                        text: "x".into(),
                    },
                ],
            },
            TranslationResponse {
                entries: vec![
                    TranslationItem {
                        id: 102,
                        text: "x".into(),
                    },
                    TranslationItem {
                        id: 101,
                        text: "y".into(),
                    },
                ],
            },
        ] {
            assert!(matches!(
                response.validate_for(&request),
                Err(AppError::InvalidApiResponse(_))
            ));
        }
    }

    #[test]
    fn accepts_exact_segment_ids_without_returning_context() {
        let request = request();
        TranslationResponse {
            entries: vec![
                TranslationItem {
                    id: 101,
                    text: "one translated".into(),
                },
                TranslationItem {
                    id: 102,
                    text: "two translated".into(),
                },
            ],
        }
        .validate_for(&request)
        .unwrap();
    }
}
