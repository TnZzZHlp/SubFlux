use reqwest::Response;
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=newline).collect::<Vec<_>>();
            self.handle_line(&line[..line.len() - 1], &mut events)?;
        }
        Ok(events)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<SseEvent>> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.handle_line(&line, &mut events)?;
        }
        self.dispatch(&mut events);
        Ok(events)
    }

    fn handle_line(&mut self, line: &[u8], events: &mut Vec<SseEvent>) -> Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            self.dispatch(events);
        } else if line[0] == b':' {
            return Ok(());
        } else if let Some(value) = line.strip_prefix(b"event:") {
            self.event = Some(decode(value)?);
        } else if let Some(value) = line.strip_prefix(b"data:") {
            self.data.push(decode(value)?);
        }
        Ok(())
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        let event = self.event.take();
        if self.data.is_empty() {
            return;
        }
        events.push(SseEvent {
            event,
            data: self.data.join("\n"),
        });
        self.data.clear();
    }
}

pub(crate) async fn read_response(
    mut response: Response,
    cancellation: &CancellationToken,
) -> Result<Vec<SseEvent>> {
    let mut decoder = SseDecoder::default();
    let mut events = Vec::new();
    loop {
        let chunk = tokio::select! {
            chunk = response.chunk() => chunk.map_err(AppError::Http)?,
            () = cancellation.cancelled() => return Err(AppError::Cancelled),
        };
        let Some(chunk) = chunk else {
            break;
        };
        events.extend(decoder.push(&chunk)?);
    }
    events.extend(decoder.finish()?);
    Ok(events)
}

fn decode(value: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(value).map_err(|error| {
        AppError::InvalidApiResponse(format!("invalid SSE event text: {error}"))
    })?;
    Ok(value.strip_prefix(' ').unwrap_or(value).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_events_split_across_chunks() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(b"event: content_block_delta\ndata: {\"text\":")
                .unwrap()
                .is_empty()
        );
        let mut events = decoder.push(b"\"ok\"}\n\n").unwrap();
        events.extend(decoder.finish().unwrap());

        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("content_block_delta".into()),
                data: r#"{"text":"ok"}"#.into(),
            }]
        );
    }

    #[test]
    fn joins_multiple_data_lines_and_ignores_comments() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push(b": keepalive\ndata: first\ndata: second\n\n")
            .unwrap();

        assert_eq!(events[0].data, "first\nsecond");
    }
}
