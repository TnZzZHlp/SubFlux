use std::{fmt, path::PathBuf};

use crate::{
    config::LanguageCode,
    error::{AppError, Result},
};

use super::token::TextTemplate;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SubtitleId(pub u64);

impl fmt::Display for SubtitleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubtitleFormat {
    Srt,
    Ass,
    Ssa,
    Vtt,
}

impl SubtitleFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
            Self::Ssa => "ssa",
            Self::Vtt => "vtt",
        }
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "srt" | "subrip" => Some(Self::Srt),
            "ass" => Some(Self::Ass),
            "ssa" => Some(Self::Ssa),
            "vtt" | "webvtt" => Some(Self::Vtt),
            _ => None,
        }
    }

    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|value| value.to_str())
            .and_then(Self::from_extension)
    }

    pub fn from_codec(codec: &str) -> Option<Self> {
        match codec.to_ascii_lowercase().as_str() {
            "subrip" | "srt" | "mov_text" | "text" => Some(Self::Srt),
            "ass" => Some(Self::Ass),
            "ssa" => Some(Self::Ssa),
            "webvtt" | "vtt" => Some(Self::Vtt),
            _ => None,
        }
    }
}

impl fmt::Display for SubtitleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Srt => "SRT",
            Self::Ass => "ASS",
            Self::Ssa => "SSA",
            Self::Vtt => "WebVTT",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SubtitleMetadata {
    pub language: Option<LanguageCode>,
    pub source_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct OriginalDocument {
    pub content: String,
    pub generated: bool,
}

#[derive(Clone, Debug)]
pub enum RawSubtitleEntry {
    Srt {
        text_range: ByteRange,
        template: TextTemplate,
    },
    Ass {
        text_range: ByteRange,
        template: TextTemplate,
    },
    Vtt {
        text_range: ByteRange,
        template: TextTemplate,
    },
    Generated,
}

impl RawSubtitleEntry {
    pub const fn text_range(&self) -> Option<ByteRange> {
        match self {
            Self::Srt { text_range, .. }
            | Self::Ass { text_range, .. }
            | Self::Vtt { text_range, .. } => Some(*text_range),
            Self::Generated => None,
        }
    }

    pub fn render_translation(&self, translated: &str) -> String {
        match self {
            Self::Srt { template, .. } | Self::Vtt { template, .. } => template.render(translated),
            Self::Ass { template, .. } => template.render_ass(translated),
            Self::Generated => translated.to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SubtitleEntry {
    pub id: SubtitleId,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Plain visible text sent to a translator. Tags and control codes remain
    /// in `raw`, never in the provider request.
    pub translatable_text: String,
    pub raw: RawSubtitleEntry,
    pub translated_text: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SubtitleDocument {
    pub format: SubtitleFormat,
    pub entries: Vec<SubtitleEntry>,
    pub metadata: SubtitleMetadata,
    pub original: OriginalDocument,
}

impl SubtitleDocument {
    pub fn from_speech_segments(
        segments: Vec<SpeechSegment>,
        language: Option<LanguageCode>,
    ) -> Result<Self> {
        let mut entries = Vec::with_capacity(segments.len());
        for (index, segment) in segments.into_iter().enumerate() {
            if segment.end_ms < segment.start_ms {
                return Err(AppError::SttError(format!(
                    "segment {} ends before it starts",
                    index + 1
                )));
            }
            entries.push(SubtitleEntry {
                id: SubtitleId((index + 1) as u64),
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                translatable_text: segment.text,
                raw: RawSubtitleEntry::Generated,
                translated_text: None,
            });
        }
        Ok(Self {
            format: SubtitleFormat::Srt,
            entries,
            metadata: SubtitleMetadata {
                language,
                source_path: None,
            },
            original: OriginalDocument {
                content: String::new(),
                generated: true,
            },
        })
    }

    pub fn apply_translation(&mut self, id: SubtitleId, text: String) -> Result<()> {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return Err(AppError::TranslationError(format!(
                "unknown subtitle id {id}"
            )));
        };
        entry.translated_text = Some(text);
        Ok(())
    }

    pub fn render(&self) -> Result<String> {
        if self.original.generated {
            return Ok(render_generated_srt(&self.entries));
        }

        let mut replacements: Vec<(ByteRange, String)> = self
            .entries
            .iter()
            .filter_map(|entry| {
                entry.translated_text.as_ref().and_then(|translation| {
                    entry
                        .raw
                        .text_range()
                        .map(|range| (range, entry.raw.render_translation(translation)))
                })
            })
            .collect();
        replacements.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));

        let mut rendered = self.original.content.clone();
        let mut previous_start = rendered.len() + 1;
        for (range, replacement) in replacements {
            if range.start > range.end
                || range.end > rendered.len()
                || range.end > previous_start
                || !rendered.is_char_boundary(range.start)
                || !rendered.is_char_boundary(range.end)
            {
                return Err(AppError::SubtitleWriteError(
                    "subtitle parser supplied invalid or overlapping text ranges".into(),
                ));
            }
            rendered.replace_range(range.start..range.end, &replacement);
            previous_start = range.start;
        }
        Ok(rendered)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

pub fn format_srt_timestamp(milliseconds: u64) -> String {
    let hours = milliseconds / 3_600_000;
    let minutes = (milliseconds / 60_000) % 60;
    let seconds = (milliseconds / 1_000) % 60;
    let millis = milliseconds % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

fn render_generated_srt(entries: &[SubtitleEntry]) -> String {
    let mut result = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            result.push('\n');
        }
        result.push_str(&(index + 1).to_string());
        result.push('\n');
        result.push_str(&format_srt_timestamp(entry.start_ms));
        result.push_str(" --> ");
        result.push_str(&format_srt_timestamp(entry.end_ms));
        result.push('\n');
        result.push_str(
            entry
                .translated_text
                .as_deref()
                .unwrap_or(&entry.translatable_text),
        );
        result.push('\n');
    }
    result
}
