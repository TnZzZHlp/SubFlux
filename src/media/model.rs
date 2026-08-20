use std::fmt;

use crate::subtitle::SubtitleFormat;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TrackIndex(pub u32);

impl fmt::Display for TrackIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubtitleTrackKind {
    Text(SubtitleFormat),
    Image,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubtitleTrack {
    pub index: TrackIndex,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub kind: SubtitleTrackKind,
}

impl SubtitleTrack {
    pub const fn format(&self) -> Option<SubtitleFormat> {
        match self.kind {
            SubtitleTrackKind::Text(format) => Some(format),
            SubtitleTrackKind::Image | SubtitleTrackKind::Unsupported => None,
        }
    }

    pub const fn is_text(&self) -> bool {
        matches!(self.kind, SubtitleTrackKind::Text(_))
    }

    fn is_sdh(&self) -> bool {
        self.title
            .as_deref()
            .is_some_and(|title| title.to_ascii_lowercase().contains("sdh"))
    }

    pub fn display_label(&self) -> String {
        let language = self.language.as_deref().unwrap_or("未知");
        let title = self.title.as_deref().filter(|title| !title.is_empty());
        let flags = match (self.default, self.forced) {
            (true, true) => " 默认 强制",
            (true, false) => " 默认",
            (false, true) => " 强制",
            (false, false) => "",
        };
        let type_label = match self.kind {
            SubtitleTrackKind::Text(format) => format.to_string(),
            SubtitleTrackKind::Image => "图像字幕（请使用语音识别）".into(),
            SubtitleTrackKind::Unsupported => "不支持的字幕".into(),
        };
        title.map_or_else(
            || format!("#{}  {type_label}  {language}{flags}", self.index),
            |title| format!("#{}  {type_label}  {language}  {title}{flags}", self.index),
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaProbe {
    pub subtitle_tracks: Vec<SubtitleTrack>,
}

impl MediaProbe {
    /// Auto selection is deliberately limited to textual tracks. SDH tracks
    /// take priority, followed by the container default. Image tracks are
    /// surfaced to the UI but never fed into the text subtitle pipeline.
    pub fn auto_track(&self) -> Option<&SubtitleTrack> {
        self.subtitle_tracks
            .iter()
            .filter(|track| track.is_text())
            .min_by_key(|track| (!track.is_sdh(), !track.default))
    }

    pub fn track(&self, index: TrackIndex) -> Option<&SubtitleTrack> {
        self.subtitle_tracks
            .iter()
            .find(|track| track.index == index)
    }
}

pub fn classify_codec(codec: &str) -> SubtitleTrackKind {
    SubtitleFormat::from_codec(codec).map_or_else(
        || {
            if matches!(
                codec.to_ascii_lowercase().as_str(),
                "hdmv_pgs_subtitle"
                    | "pgssub"
                    | "dvd_subtitle"
                    | "vobsub"
                    | "xsub"
                    | "dvb_subtitle"
            ) {
                SubtitleTrackKind::Image
            } else {
                SubtitleTrackKind::Unsupported
            }
        },
        SubtitleTrackKind::Text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_ignores_image_subtitle() {
        let probe = MediaProbe {
            subtitle_tracks: vec![
                SubtitleTrack {
                    index: TrackIndex(2),
                    codec: "hdmv_pgs_subtitle".into(),
                    language: Some("zh".into()),
                    title: None,
                    default: true,
                    forced: false,
                    kind: SubtitleTrackKind::Image,
                },
                SubtitleTrack {
                    index: TrackIndex(3),
                    codec: "ass".into(),
                    language: Some("ja".into()),
                    title: None,
                    default: false,
                    forced: false,
                    kind: SubtitleTrackKind::Text(SubtitleFormat::Ass),
                },
            ],
        };
        assert_eq!(probe.auto_track().unwrap().index, TrackIndex(3));
    }

    #[test]
    fn auto_prefers_sdh_before_default_text_subtitles() {
        let probe = MediaProbe {
            subtitle_tracks: vec![
                SubtitleTrack {
                    index: TrackIndex(1),
                    codec: "subrip".into(),
                    language: Some("en".into()),
                    title: Some("English".into()),
                    default: true,
                    forced: false,
                    kind: SubtitleTrackKind::Text(SubtitleFormat::Srt),
                },
                SubtitleTrack {
                    index: TrackIndex(2),
                    codec: "subrip".into(),
                    language: Some("en".into()),
                    title: Some("English SDH".into()),
                    default: false,
                    forced: false,
                    kind: SubtitleTrackKind::Text(SubtitleFormat::Srt),
                },
            ],
        };

        assert_eq!(probe.auto_track().unwrap().index, TrackIndex(2));
    }
}
