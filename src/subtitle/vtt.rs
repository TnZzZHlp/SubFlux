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
        let first = block[0].text(input).trim_start_matches('\u{feff}').trim();
        if first == "WEBVTT"
            || first.starts_with("WEBVTT ")
            || first.starts_with("NOTE")
            || first.starts_with("STYLE")
            || first.starts_with("REGION")
        {
            continue;
        }
        let Some(timing_index) = block
            .iter()
            .position(|line| line.text(input).contains("-->"))
        else {
            continue;
        };
        let timing = block[timing_index];
        let (start_ms, end_ms) = parse_timing(timing.text(input))?;
        if end_ms < start_ms {
            return Err(AppError::SubtitleParseError(
                "WebVTT cue ends before it starts".into(),
            ));
        }
        let text_start = timing.end_with_ending;
        let text_end = block
            .last()
            .map_or(text_start, |line| line.end)
            .max(text_start);
        let raw_text = &input[text_start..text_end];
        let template = TextTemplate::with_markup(raw_text);
        entries.push(SubtitleEntry {
            id: SubtitleId(next_id),
            start_ms,
            end_ms,
            translatable_text: template.plain_text().to_owned(),
            raw: RawSubtitleEntry::Vtt {
                text_range: ByteRange::new(text_start, text_end),
                template,
            },
            translated_text: None,
        });
        next_id += 1;
    }
    Ok(SubtitleDocument {
        format: SubtitleFormat::Vtt,
        entries,
        metadata: SubtitleMetadata::default(),
        original: OriginalDocument {
            content: input.into(),
            generated: false,
        },
    })
}

fn parse_timing(value: &str) -> Result<(u64, u64)> {
    let Some((start, end_and_settings)) = value.split_once("-->") else {
        return Err(AppError::SubtitleParseError(
            "WebVTT cue is missing -->".into(),
        ));
    };
    let end = end_and_settings
        .split_whitespace()
        .next()
        .unwrap_or_default();
    Ok((
        parse_vtt_timestamp(start.trim())?,
        parse_vtt_timestamp(end)?,
    ))
}

fn parse_vtt_timestamp(value: &str) -> Result<u64> {
    let (clock, fraction) = value.rsplit_once('.').unwrap_or((value, "0"));
    if fraction.is_empty()
        || fraction.len() > 3
        || !fraction.chars().all(|value| value.is_ascii_digit())
    {
        return Err(AppError::SubtitleParseError(format!(
            "invalid WebVTT timestamp: {value}"
        )));
    }
    let units: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match units.as_slice() {
        [minutes, seconds] => (0, parse_unit(minutes, value)?, parse_unit(seconds, value)?),
        [hours, minutes, seconds] => (
            parse_unit(hours, value)?,
            parse_unit(minutes, value)?,
            parse_unit(seconds, value)?,
        ),
        _ => {
            return Err(AppError::SubtitleParseError(format!(
                "invalid WebVTT timestamp: {value}"
            )));
        }
    };
    if minutes >= 60 || seconds >= 60 {
        return Err(AppError::SubtitleParseError(format!(
            "invalid WebVTT timestamp: {value}"
        )));
    }
    let millis = match fraction.len() {
        1 => fraction.parse::<u64>().unwrap_or(0) * 100,
        2 => fraction.parse::<u64>().unwrap_or(0) * 10,
        _ => fraction.parse::<u64>().unwrap_or(0),
    };
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1_000 + millis)
}

fn parse_unit(value: &str, original: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| AppError::SubtitleParseError(format!("invalid WebVTT timestamp: {original}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_vtt_header_settings_and_note() {
        let input = "WEBVTT - example\n\nNOTE untouched\nhello\n\ncue-7\n00:01.000 --> 00:03.000 line:90%\n<c.red>Hello</c>\n";
        let mut document = parse(input).unwrap();
        document
            .apply_translation(SubtitleId(1), "你好".into())
            .unwrap();
        let output = document
            .render_with_mode(crate::subtitle::SubtitleOutputMode::Translated)
            .unwrap();
        assert!(output.starts_with("WEBVTT - example\n\nNOTE untouched\nhello\n"));
        assert!(output.contains("00:01.000 --> 00:03.000 line:90%\n<c.red>你好</c>"));
    }
}
