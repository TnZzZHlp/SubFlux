use crate::{
    error::{AppError, Result},
    subtitle::SubtitleDocument,
};

use super::model::TranslationItem;

#[derive(Clone, Debug)]
pub struct TranslationChunk {
    pub index: usize,
    /// Source-language subtitles immediately before `segments`. They provide
    /// read-only context and are never included in a translation response.
    pub previous_context: Vec<TranslationItem>,
    /// The only subtitles this request is allowed to translate and write back.
    pub segments: Vec<TranslationItem>,
    /// Source-language subtitles immediately after `segments`. They provide
    /// read-only context and are never included in a translation response.
    pub next_context: Vec<TranslationItem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranslationChunkConfig {
    /// Maximum number of subtitles that a single request must translate.
    pub chunk_size: usize,
    /// Number of earlier source subtitles to include as read-only context.
    pub context_before: usize,
    /// Number of later source subtitles to include as read-only context.
    pub context_after: usize,
}

impl TranslationChunkConfig {
    fn validate(self) -> Result<()> {
        if self.chunk_size == 0 {
            return Err(AppError::InvalidConfig(
                "SUBFLUX_TRANSLATOR_CHUNK_SIZE must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Builds independent, source-language requests. Empty/style-only lines are
/// never sent to an LLM, including as context.
pub fn build_chunks(
    document: &SubtitleDocument,
    config: TranslationChunkConfig,
) -> Result<Vec<TranslationChunk>> {
    config.validate()?;
    let entries: Vec<_> = document
        .entries
        .iter()
        .filter(|entry| !entry.translatable_text.trim().is_empty())
        .map(|entry| TranslationItem {
            id: entry.id.0,
            text: entry.translatable_text.clone(),
        })
        .collect();

    let mut chunks = Vec::new();
    for (index, segment_start) in (0..entries.len()).step_by(config.chunk_size).enumerate() {
        let segment_end = segment_start
            .saturating_add(config.chunk_size)
            .min(entries.len());
        let previous_start = segment_start.saturating_sub(config.context_before);
        let next_end = segment_end
            .saturating_add(config.context_after)
            .min(entries.len());
        chunks.push(TranslationChunk {
            index,
            previous_context: entries[previous_start..segment_start].to_vec(),
            segments: entries[segment_start..segment_end].to_vec(),
            next_context: entries[segment_end..next_end].to_vec(),
        });
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use crate::{
        config::LanguageCode,
        subtitle::{RawSubtitleEntry, SpeechSegment, SubtitleDocument},
    };

    use super::*;

    fn document_with_entries(total: usize) -> SubtitleDocument {
        SubtitleDocument::from_speech_segments(
            (1..=total)
                .map(|id| SpeechSegment {
                    start_ms: id as u64,
                    end_ms: id as u64 + 1,
                    text: format!("line {id}"),
                })
                .collect(),
            Some(LanguageCode::parse("en").unwrap()),
        )
        .unwrap()
    }

    fn ids(entries: &[TranslationItem]) -> Vec<u64> {
        entries.iter().map(|entry| entry.id).collect()
    }

    fn chunk_config(
        chunk_size: usize,
        context_before: usize,
        context_after: usize,
    ) -> TranslationChunkConfig {
        TranslationChunkConfig {
            chunk_size,
            context_before,
            context_after,
        }
    }

    #[test]
    fn standard_windows_have_expected_ranges() {
        let chunks = build_chunks(&document_with_entries(100), chunk_config(30, 10, 5)).unwrap();

        assert_eq!(chunks.len(), 4);
        assert_eq!(ids(&chunks[0].previous_context), Vec::<u64>::new());
        assert_eq!(ids(&chunks[0].segments), (1..=30).collect::<Vec<_>>());
        assert_eq!(ids(&chunks[0].next_context), (31..=35).collect::<Vec<_>>());

        assert_eq!(
            ids(&chunks[1].previous_context),
            (21..=30).collect::<Vec<_>>()
        );
        assert_eq!(ids(&chunks[1].segments), (31..=60).collect::<Vec<_>>());
        assert_eq!(ids(&chunks[1].next_context), (61..=65).collect::<Vec<_>>());

        assert_eq!(
            ids(&chunks[2].previous_context),
            (51..=60).collect::<Vec<_>>()
        );
        assert_eq!(ids(&chunks[2].segments), (61..=90).collect::<Vec<_>>());
        assert_eq!(ids(&chunks[2].next_context), (91..=95).collect::<Vec<_>>());

        assert_eq!(
            ids(&chunks[3].previous_context),
            (81..=90).collect::<Vec<_>>()
        );
        assert_eq!(ids(&chunks[3].segments), (91..=100).collect::<Vec<_>>());
        assert_eq!(ids(&chunks[3].next_context), Vec::<u64>::new());
    }

    #[test]
    fn short_subtitles_and_zero_context_have_no_context_entries() {
        let short = build_chunks(&document_with_entries(10), chunk_config(30, 10, 5)).unwrap();
        assert_eq!(short.len(), 1);
        assert_eq!(ids(&short[0].previous_context), Vec::<u64>::new());
        assert_eq!(ids(&short[0].segments), (1..=10).collect::<Vec<_>>());
        assert_eq!(ids(&short[0].next_context), Vec::<u64>::new());

        let without_context =
            build_chunks(&document_with_entries(4), chunk_config(2, 0, 0)).unwrap();
        assert_eq!(without_context.len(), 2);
        assert!(
            without_context
                .iter()
                .all(|chunk| chunk.previous_context.is_empty() && chunk.next_context.is_empty())
        );
        assert_eq!(ids(&without_context[0].segments), vec![1, 2]);
        assert_eq!(ids(&without_context[1].segments), vec![3, 4]);
    }

    #[test]
    fn segments_are_unique_while_context_can_overlap_them() {
        let chunks = build_chunks(&document_with_entries(8), chunk_config(3, 2, 2)).unwrap();
        let translated: Vec<_> = chunks
            .iter()
            .flat_map(|chunk| ids(&chunk.segments))
            .collect();
        assert_eq!(translated, (1..=8).collect::<Vec<_>>());

        assert_eq!(ids(&chunks[0].next_context), vec![4, 5]);
        assert_eq!(ids(&chunks[1].previous_context), vec![2, 3]);
        assert!(chunks[0].next_context.iter().all(|entry| {
            chunks[1]
                .segments
                .iter()
                .any(|segment| segment.id == entry.id)
        }));
    }

    #[test]
    fn chunks_skip_empty_entries_without_losing_context_boundaries() {
        let document = SubtitleDocument::from_speech_segments(
            vec![
                SpeechSegment {
                    start_ms: 0,
                    end_ms: 1,
                    text: "one".into(),
                },
                SpeechSegment {
                    start_ms: 1,
                    end_ms: 2,
                    text: String::new(),
                },
                SpeechSegment {
                    start_ms: 2,
                    end_ms: 3,
                    text: "three".into(),
                },
            ],
            Some(LanguageCode::parse("en").unwrap()),
        )
        .unwrap();
        assert!(matches!(
            document.entries[0].raw,
            RawSubtitleEntry::Generated
        ));
        let chunks = build_chunks(&document, chunk_config(1, 1, 1)).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].segments[0].id, 1);
        assert_eq!(chunks[0].next_context[0].id, 3);
        assert_eq!(chunks[1].previous_context[0].id, 1);
        assert_eq!(chunks[1].segments[0].id, 3);
    }

    #[test]
    fn rejects_zero_chunk_size_without_panicking() {
        assert!(matches!(
            build_chunks(&document_with_entries(1), chunk_config(0, 0, 0)),
            Err(AppError::InvalidConfig(_))
        ));
    }
}
