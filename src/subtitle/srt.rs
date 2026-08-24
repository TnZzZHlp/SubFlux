use crate::error::{AppError, Result};

use super::{
    lines::split,
    model::{
        ByteRange, OriginalDocument, RawSubtitleEntry, SubtitleDocument, SubtitleEntry,
        SubtitleFormat, SubtitleId, SubtitleMetadata,
    },
    token::TextTemplate,
};

pub fn parse(input: &str) -> Result<SubtitleDocument> {
    let lines = split(input);
    let mut entries = Vec::new();
    let mut cursor = 0;
    let mut next_id = 1_u64;

    while cursor < lines.len() {
        while cursor < lines.len() && lines[cursor].is_blank(input) {
            cursor += 1;
        }
        let block_start = cursor;
        while cursor < lines.len() && !lines[cursor].is_blank(input) {
            cursor += 1;
        }
        if block_start == cursor {
            continue;
        }
        let block = &lines[block_start..cursor];
        let Some(timing_index) = block
            .iter()
            .position(|line| line.text(input).contains("-->"))
        else {
            // A non-cue block is retained verbatim. This is friendlier to
            // vendor metadata than rejecting an otherwise usable SRT file.
            continue;
        };
        let timing = block[timing_index];
        let (start_ms, end_ms) = parse_timing(timing.text(input))?;
        if end_ms < start_ms {
            return Err(AppError::SubtitleParseError(
                "SRT cue ends before it starts".into(),
            ));
        }
        let text_start = timing.end_with_ending;
        let text_end = block
            .last()
            .map_or(text_start, |line| line.end)
            .max(text_start);
        let raw_text = &input[text_start..text_end];
        entries.push(SubtitleEntry {
            id: SubtitleId(next_id),
            start_ms,
            end_ms,
            translatable_text: TextTemplate::with_markup(raw_text).plain_text().to_owned(),
            raw: RawSubtitleEntry::Srt {
                text_range: ByteRange::new(text_start, text_end),
                template: TextTemplate::with_markup(raw_text),
            },
            translated_text: None,
        });
        next_id += 1;
    }

    Ok(SubtitleDocument {
        format: SubtitleFormat::Srt,
        entries,
        metadata: SubtitleMetadata::default(),
        original: OriginalDocument {
            content: input.into(),
            generated: false,
        },
    })
}

pub(crate) fn parse_timing(value: &str) -> Result<(u64, u64)> {
    let Some((start, end_and_settings)) = value.split_once("-->") else {
        return Err(AppError::SubtitleParseError(
            "SRT cue is missing -->".into(),
        ));
    };
    let end = end_and_settings
        .split_whitespace()
        .next()
        .unwrap_or_default();
    Ok((parse_timestamp(start.trim())?, parse_timestamp(end)?))
}

pub(crate) fn parse_timestamp(value: &str) -> Result<u64> {
    let normalized = value.trim().replace(',', ".");
    let (time, millis) = normalized
        .rsplit_once('.')
        .unwrap_or((normalized.as_str(), "0"));
    let mut values = time.split(':');
    let hours = values
        .next()
        .ok_or_else(|| AppError::SubtitleParseError(format!("invalid SRT timestamp: {value}")))?
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid SRT timestamp: {value}")))?;
    let minutes = values
        .next()
        .ok_or_else(|| AppError::SubtitleParseError(format!("invalid SRT timestamp: {value}")))?
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid SRT timestamp: {value}")))?;
    let seconds = values
        .next()
        .ok_or_else(|| AppError::SubtitleParseError(format!("invalid SRT timestamp: {value}")))?
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid SRT timestamp: {value}")))?;
    if values.next().is_some() || minutes >= 60 || seconds >= 60 {
        return Err(AppError::SubtitleParseError(format!(
            "invalid SRT timestamp: {value}"
        )));
    }
    let millis = parse_millis(millis, value)?;
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1_000 + millis)
}

fn parse_millis(value: &str, original: &str) -> Result<u64> {
    if value.is_empty() || value.len() > 3 || !value.chars().all(|char| char.is_ascii_digit()) {
        return Err(AppError::SubtitleParseError(format!(
            "invalid SRT timestamp: {original}"
        )));
    }
    let raw = value
        .parse::<u64>()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid SRT timestamp: {original}")))?;
    Ok(match value.len() {
        1 => raw * 100,
        2 => raw * 10,
        _ => raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_keeps_srt_timing_byte_for_byte() {
        let source = "1\r\n00:00:01,250 --> 00:00:03,000\r\n<i>Hello</i>\r\n\r\n";
        let mut document = parse(source).unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();
        assert_eq!(
            document
                .render_with_mode(crate::subtitle::SubtitleOutputMode::Translated)
                .unwrap(),
            "1\r\n00:00:01,250 --> 00:00:03,000\r\n<i>你好</i>\r\n\r\n"
        );
    }
}
