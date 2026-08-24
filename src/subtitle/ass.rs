use std::collections::HashMap;

use crate::error::{AppError, Result};

use super::{
    lines::split,
    model::{
        ByteRange, OriginalDocument, RawSubtitleEntry, SubtitleDocument, SubtitleEntry,
        SubtitleFormat, SubtitleId, SubtitleMetadata,
    },
    token::TextTemplate,
};

const DEFAULT_EVENT_FORMAT: &str = "Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text";

pub fn parse(format: SubtitleFormat, input: &str) -> Result<SubtitleDocument> {
    debug_assert!(matches!(format, SubtitleFormat::Ass | SubtitleFormat::Ssa));
    let lines = split(input);
    let mut entries = Vec::new();
    let mut in_events = false;
    let mut event_format = parse_event_format(DEFAULT_EVENT_FORMAT);
    let mut next_id = 1_u64;

    for line in lines {
        let text = line.text(input);
        let trimmed = text.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_events = trimmed.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some((prefix_len, rest)) = split_prefix_case_insensitive(text, "Format:") {
            let _ = prefix_len;
            event_format = parse_event_format(rest);
            continue;
        }
        let Some((prefix_len, fields_text)) = split_prefix_case_insensitive(text, "Dialogue:")
        else {
            continue;
        };
        let Some(text_index) = event_format.get("text").copied() else {
            return Err(AppError::SubtitleParseError(
                "ASS event Format has no Text field".into(),
            ));
        };
        if text_index + 1 != event_format.len() {
            return Err(AppError::SubtitleParseError(
                "ASS event Format must have Text as the final field to preserve commas safely"
                    .into(),
            ));
        }
        let fields: Vec<&str> = fields_text.splitn(event_format.len(), ',').collect();
        if fields.len() != event_format.len() {
            return Err(AppError::SubtitleParseError(
                "ASS Dialogue has fewer fields than its Event Format".into(),
            ));
        }
        let start_index = *event_format.get("start").ok_or_else(|| {
            AppError::SubtitleParseError("ASS event Format has no Start field".into())
        })?;
        let end_index = *event_format.get("end").ok_or_else(|| {
            AppError::SubtitleParseError("ASS event Format has no End field".into())
        })?;
        let start_ms = parse_timestamp(fields[start_index].trim())?;
        let end_ms = parse_timestamp(fields[end_index].trim())?;
        if end_ms < start_ms {
            return Err(AppError::SubtitleParseError(
                "ASS Dialogue ends before it starts".into(),
            ));
        }
        let text_start = line.start + prefix_len + offset_after_commas(fields_text, text_index)?;
        let text_end = line.end;
        let raw_text = &input[text_start..text_end];
        let template = TextTemplate::with_ass_overrides(raw_text);
        entries.push(SubtitleEntry {
            id: SubtitleId(next_id),
            start_ms,
            end_ms,
            translatable_text: template.plain_text().to_owned(),
            raw: RawSubtitleEntry::Ass {
                text_range: ByteRange::new(text_start, text_end),
                template,
            },
            translated_text: None,
        });
        next_id += 1;
    }

    Ok(SubtitleDocument {
        format,
        entries,
        metadata: SubtitleMetadata::default(),
        original: OriginalDocument {
            content: input.into(),
            generated: false,
        },
    })
}

fn split_prefix_case_insensitive<'a>(line: &'a str, prefix: &str) -> Option<(usize, &'a str)> {
    let whitespace = line.len() - line.trim_start().len();
    let candidate = &line[whitespace..];
    candidate
        .get(..prefix.len())
        .filter(|part| part.eq_ignore_ascii_case(prefix))
        .map(|_| (whitespace + prefix.len(), &candidate[prefix.len()..]))
}

fn parse_event_format(value: &str) -> HashMap<String, usize> {
    value
        .split(',')
        .enumerate()
        .map(|(index, name)| (name.trim().to_ascii_lowercase(), index))
        .collect()
}

fn offset_after_commas(value: &str, commas: usize) -> Result<usize> {
    if commas == 0 {
        return Ok(0);
    }
    let mut seen = 0;
    for (index, byte) in value.bytes().enumerate() {
        if byte == b',' {
            seen += 1;
            if seen == commas {
                return Ok(index + 1);
            }
        }
    }
    Err(AppError::SubtitleParseError(
        "ASS Dialogue is missing a field separator".into(),
    ))
}

pub(crate) fn parse_timestamp(value: &str) -> Result<u64> {
    let Some((clock, centiseconds)) = value.trim().rsplit_once('.') else {
        return Err(AppError::SubtitleParseError(format!(
            "invalid ASS timestamp: {value}"
        )));
    };
    if centiseconds.is_empty()
        || centiseconds.len() > 2
        || !centiseconds
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err(AppError::SubtitleParseError(format!(
            "invalid ASS timestamp: {value}"
        )));
    }
    let values: Vec<&str> = clock.split(':').collect();
    if values.len() != 3 {
        return Err(AppError::SubtitleParseError(format!(
            "invalid ASS timestamp: {value}"
        )));
    }
    let hours = values[0]
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid ASS timestamp: {value}")))?;
    let minutes = values[1]
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid ASS timestamp: {value}")))?;
    let seconds = values[2]
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid ASS timestamp: {value}")))?;
    if minutes >= 60 || seconds >= 60 {
        return Err(AppError::SubtitleParseError(format!(
            "invalid ASS timestamp: {value}"
        )));
    }
    let raw = centiseconds
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid ASS timestamp: {value}")))?;
    let millis = if centiseconds.len() == 1 {
        raw * 100
    } else {
        raw * 10
    };
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1_000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_everything_except_dialogue_text() {
        let source = concat!(
            "[Script Info]\nTitle: Keep me\n\n[V4+ Styles]\n",
            "Style: Default,Arial,42,&H00FFFFFF\n\n[Events]\n",
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Comment: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,unchanged\n",
            "Dialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,{\\an8}こんにちは{\\c&H00FFFF&}Alice{\\c}です\n",
            "[Fonts]\nfontname: untouched\n"
        );
        let mut document = parse(SubtitleFormat::Ass, source).unwrap();
        assert_eq!(document.entries[0].translatable_text, "こんにちはAliceです");
        document
            .apply_translation(SubtitleId(1), "你好 Alice".into())
            .unwrap();
        let output = document
            .render_with_mode(crate::subtitle::SubtitleOutputMode::Translated)
            .unwrap();
        assert!(output.contains("Title: Keep me"));
        assert!(output.contains("Style: Default,Arial,42,&H00FFFFFF"));
        assert!(output.contains("Comment: 0,0:00:00.00"));
        assert!(output.contains("{\\an8}"));
        assert!(output.contains("{\\c&H00FFFF&}"));
        assert!(output.contains("{\\c}"));
        assert!(output.contains("[Fonts]\nfontname: untouched"));
    }

    #[test]
    fn keeps_ssa_as_ssa() {
        let source = concat!(
            "[Events]\n",
            "Format: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Dialogue: Marked=0,0:00:01.00,0:00:02.00,Default,,0000,0000,0000,,hello\n"
        );
        let mut document = parse(SubtitleFormat::Ssa, source).unwrap();
        document
            .apply_translation(SubtitleId(1), "translated".into())
            .unwrap();
        assert_eq!(document.format, SubtitleFormat::Ssa);
        assert!(
            document
                .render_with_mode(crate::subtitle::SubtitleOutputMode::Translated)
                .unwrap()
                .contains(
                    "Dialogue: Marked=0,0:00:01.00,0:00:02.00,Default,,0000,0000,0000,,translated"
                )
        );
    }
}
