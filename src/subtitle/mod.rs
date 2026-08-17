//! Subtitle parsing and rendering.
//!
//! Parsers retain the complete original document and record only the byte range
//! containing each translatable payload.  Rendering therefore replaces those
//! ranges in reverse order and leaves headers, styles, attachments, comments,
//! timing lines, and unknown sections untouched.

pub mod ass;
pub(crate) mod lines;
pub mod model;
pub mod source;
pub mod srt;
pub mod token;
pub mod vtt;
pub mod writer;

use std::path::Path;

use crate::error::{AppError, Result};
pub use model::{
    ByteRange, OriginalDocument, RawSubtitleEntry, SpeechSegment, SubtitleDocument, SubtitleEntry,
    SubtitleFormat, SubtitleId, SubtitleMetadata,
};
pub use source::{EmbeddedSubtitleSource, ExternalSubtitleSource, SubtitleSource};
pub use writer::{FileSubtitleWriter, SubtitleWriter};

pub fn parse(format: SubtitleFormat, input: &str) -> Result<SubtitleDocument> {
    match format {
        SubtitleFormat::Srt => srt::parse(input),
        SubtitleFormat::Ass | SubtitleFormat::Ssa => ass::parse(format, input),
        SubtitleFormat::Vtt => vtt::parse(input),
    }
}

pub fn parse_file(path: &Path, input: &str) -> Result<SubtitleDocument> {
    let format = SubtitleFormat::from_path(path).ok_or_else(|| {
        AppError::UnsupportedSubtitleFormat(
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("<no extension>")
                .to_owned(),
        )
    })?;
    let mut document = parse(format, input)?;
    document.metadata.source_path = Some(path.to_path_buf());
    Ok(document)
}
