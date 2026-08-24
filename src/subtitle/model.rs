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

/// Controls which text is written for every subtitle entry after processing.
///
/// `Original` is also meaningful for STT input: it writes the recognised
/// source transcript without constructing or calling a translation provider.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SubtitleOutputMode {
    /// Write the translated text followed by the original text.
    #[default]
    BilingualTranslationFirst,
    /// Write the original text followed by the translated text.
    Bilingual,
    /// Write only the translated text.
    Translated,
    /// Write only the original text.
    Original,
}

impl SubtitleOutputMode {
    pub const fn needs_translation(self) -> bool {
        !matches!(self, Self::Original)
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

    fn render_bilingual(
        &self,
        original: &str,
        translated: &str,
        line_break: &str,
        translation_first: bool,
    ) -> String {
        let translated = self.render_translation(translated);
        match self {
            Self::Srt { .. } | Self::Vtt { .. } => {
                if translation_first {
                    format!("{translated}{line_break}{original}")
                } else {
                    format!("{original}{line_break}{translated}")
                }
            }
            Self::Ass { .. } => {
                if translation_first {
                    format!("{translated}\\N{original}")
                } else {
                    format!("{original}\\N{translated}")
                }
            }
            Self::Generated => {
                if translation_first {
                    format!("{translated}\n{original}")
                } else {
                    format!("{original}\n{translated}")
                }
            }
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
        self.render_with_mode(SubtitleOutputMode::default())
    }

    pub fn render_with_mode(&self, output_mode: SubtitleOutputMode) -> Result<String> {
        if output_mode == SubtitleOutputMode::Original && !self.original.generated {
            return Ok(self.original.content.clone());
        }
        if self.original.generated {
            return Ok(render_generated_srt(&self.entries, output_mode));
        }

        let line_break = if self.original.content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut replacements = Vec::new();
        for entry in &self.entries {
            let Some(translation) = entry.translated_text.as_deref() else {
                continue;
            };
            let Some(range) = entry.raw.text_range() else {
                continue;
            };
            if range.start > range.end
                || range.end > self.original.content.len()
                || !self.original.content.is_char_boundary(range.start)
                || !self.original.content.is_char_boundary(range.end)
            {
                return Err(AppError::SubtitleWriteError(
                    "subtitle parser supplied an invalid text range".into(),
                ));
            }
            let replacement = match output_mode {
                SubtitleOutputMode::Translated => entry.raw.render_translation(translation),
                SubtitleOutputMode::BilingualTranslationFirst | SubtitleOutputMode::Bilingual => {
                    let original = &self.original.content[range.start..range.end];
                    entry.raw.render_bilingual(
                        original,
                        translation,
                        line_break,
                        output_mode == SubtitleOutputMode::BilingualTranslationFirst,
                    )
                }
                SubtitleOutputMode::Original => unreachable!("handled before replacements"),
            };
            replacements.push((range, replacement));
        }
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

fn render_generated_srt(entries: &[SubtitleEntry], output_mode: SubtitleOutputMode) -> String {
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
        match output_mode {
            SubtitleOutputMode::Translated => result.push_str(
                entry
                    .translated_text
                    .as_deref()
                    .unwrap_or(&entry.translatable_text),
            ),
            SubtitleOutputMode::BilingualTranslationFirst => {
                if let Some(translation) = &entry.translated_text {
                    result.push_str(translation);
                    result.push('\n');
                }
                result.push_str(&entry.translatable_text);
            }
            SubtitleOutputMode::Bilingual => {
                result.push_str(&entry.translatable_text);
                if let Some(translation) = &entry.translated_text {
                    result.push('\n');
                    result.push_str(translation);
                }
            }
            SubtitleOutputMode::Original => result.push_str(&entry.translatable_text),
        }
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_stt_document_renders_all_output_modes() {
        let mut document = SubtitleDocument::from_speech_segments(
            vec![SpeechSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
            }],
            None,
        )
        .unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();

        assert_eq!(
            document.render().unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\n你好\nhello\n"
        );
        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::Translated)
                .unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\n你好\n"
        );
        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::BilingualTranslationFirst)
                .unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\n你好\nhello\n"
        );
        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::Bilingual)
                .unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\nhello\n你好\n"
        );
        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::Original)
                .unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\nhello\n"
        );
    }

    #[test]
    fn bilingual_ass_keeps_the_exact_source_and_uses_an_ass_line_break() {
        let source = concat!(
            "[Events]\n",
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8}hello\n"
        );
        let mut document = crate::subtitle::parse(SubtitleFormat::Ass, source).unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();

        let rendered = document
            .render_with_mode(SubtitleOutputMode::Bilingual)
            .unwrap();
        assert!(rendered.contains("{\\an8}hello\\N{\\an8}你好"));

        let rendered = document
            .render_with_mode(SubtitleOutputMode::BilingualTranslationFirst)
            .unwrap();
        assert!(rendered.contains("{\\an8}你好\\N{\\an8}hello"));
    }

    #[test]
    fn bilingual_srt_uses_the_source_line_ending() {
        let source = "1\r\n00:00:01,000 --> 00:00:02,000\r\nhello\r\n\r\n";
        let mut document = crate::subtitle::parse(SubtitleFormat::Srt, source).unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();

        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::Bilingual)
                .unwrap(),
            "1\r\n00:00:01,000 --> 00:00:02,000\r\nhello\r\n你好\r\n\r\n"
        );
        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::BilingualTranslationFirst)
                .unwrap(),
            "1\r\n00:00:01,000 --> 00:00:02,000\r\n你好\r\nhello\r\n\r\n"
        );
    }

    #[test]
    fn bilingual_vtt_preserves_markup_in_both_lines() {
        let source = "WEBVTT\n\n00:01.000 --> 00:03.000\n<c.red>Hello</c>\n";
        let mut document = crate::subtitle::parse(SubtitleFormat::Vtt, source).unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();

        let rendered = document
            .render_with_mode(SubtitleOutputMode::BilingualTranslationFirst)
            .unwrap();
        assert!(rendered.contains("<c.red>你好</c>\n<c.red>Hello</c>"));
    }

    #[test]
    fn bilingual_ssa_uses_the_ass_line_break() {
        let source = concat!(
            "[Events]\n",
            "Format: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Dialogue: Marked=0,0:00:01.00,0:00:02.00,Default,,0000,0000,0000,,hello\n"
        );
        let mut document = crate::subtitle::parse(SubtitleFormat::Ssa, source).unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();

        let rendered = document
            .render_with_mode(SubtitleOutputMode::BilingualTranslationFirst)
            .unwrap();
        assert!(rendered.contains("你好\\Nhello"));
    }

    #[test]
    fn bilingual_translation_first_falls_back_to_source_without_translation() {
        let document = SubtitleDocument::from_speech_segments(
            vec![SpeechSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
            }],
            None,
        )
        .unwrap();

        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::BilingualTranslationFirst)
                .unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\nhello\n"
        );
    }

    #[test]
    fn original_mode_returns_existing_subtitles_byte_for_byte() {
        let source = "1\r\n00:00:01,000 --> 00:00:02,000\r\n<i>Hello</i>\r\n\r\n";
        let document = crate::subtitle::parse(SubtitleFormat::Srt, source).unwrap();

        assert_eq!(
            document
                .render_with_mode(SubtitleOutputMode::Original)
                .unwrap(),
            source
        );
    }
}
