use std::{path::Path, process::Command, time::Duration};

use serde::Deserialize;
use tokio::process::Command as TokioCommand;
use tracing::debug;

use crate::error::{AppError, Result};

use super::model::{MediaProbe, SubtitleTrack, TrackIndex, classify_codec};

#[derive(Clone, Debug, Default)]
pub struct ToolStatus {
    pub ffmpeg: Option<String>,
    pub ffprobe: Option<String>,
}

impl ToolStatus {
    pub const fn is_ready(&self) -> bool {
        self.ffmpeg.is_none() && self.ffprobe.is_none()
    }

    pub fn problems(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if let Some(problem) = &self.ffmpeg {
            issues.push(format!("ffmpeg: {problem}"));
        }
        if let Some(problem) = &self.ffprobe {
            issues.push(format!("ffprobe: {problem}"));
        }
        issues
    }

    pub fn into_result(self) -> Result<()> {
        if self.ffmpeg.is_some() {
            Err(AppError::FfmpegNotFound)
        } else if self.ffprobe.is_some() {
            Err(AppError::FfprobeNotFound)
        } else {
            Ok(())
        }
    }
}

pub fn check_tools() -> ToolStatus {
    ToolStatus {
        ffmpeg: check_binary("ffmpeg"),
        ffprobe: check_binary("ffprobe"),
    }
}

fn check_binary(binary: &str) -> Option<String> {
    match Command::new(binary).arg("-version").output() {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!("已找到，但退出码为 {}", output.status)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Some("未在 PATH 中找到".into())
        }
        Err(error) => Some(format!("无法执行（{error}）")),
    }
}

pub async fn probe_media(path: &Path) -> Result<MediaProbe> {
    debug!(path = %path.display(), "probing media streams with ffprobe");
    let output = TokioCommand::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams"])
        .arg(path)
        .output()
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AppError::FfprobeNotFound
            } else {
                AppError::ProbeFailed(error.to_string())
            }
        })?;
    if !output.status.success() {
        return Err(AppError::ProbeFailed(command_failure(&output.stderr)));
    }
    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout).map_err(|error| {
        AppError::ProbeFailed(format!("ffprobe returned invalid JSON: {error}"))
    })?;
    let subtitle_tracks = parsed
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("subtitle"))
        .map(|stream| {
            let codec = stream
                .codec_name
                .clone()
                .unwrap_or_else(|| "unknown".into());
            SubtitleTrack {
                index: TrackIndex(stream.index),
                kind: classify_codec(&codec),
                codec,
                language: stream
                    .tags
                    .as_ref()
                    .and_then(|tags| tags.language.clone()),
                title: stream.tags.as_ref().and_then(|tags| tags.title.clone()),
                default: stream.disposition.default != 0,
                forced: stream.disposition.forced != 0,
            }
        })
        .collect();
    let has_audio = parsed
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("audio"));
    Ok(MediaProbe {
        subtitle_tracks,
        has_audio,
    })
}

/// Reads the container duration once so STT can plan bounded upload chunks
/// before extracting any temporary audio.
pub async fn probe_duration(path: &Path) -> Result<Duration> {
    let output = TokioCommand::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AppError::FfprobeNotFound
            } else {
                AppError::ProbeFailed(error.to_string())
            }
        })?;
    if !output.status.success() {
        return Err(AppError::ProbeFailed(command_failure(&output.stderr)));
    }
    parse_duration(&output.stdout)
}

fn parse_duration(value: &[u8]) -> Result<Duration> {
    let value = String::from_utf8_lossy(value);
    let seconds: f64 = value.trim().parse().map_err(|_| {
        AppError::ProbeFailed(format!(
            "ffprobe returned an invalid duration: {}",
            value.trim()
        ))
    })?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(AppError::ProbeFailed(format!(
            "ffprobe returned an invalid duration: {}",
            value.trim()
        )));
    }
    Duration::try_from_secs_f64(seconds).map_err(|_| {
        AppError::ProbeFailed(format!(
            "ffprobe returned an invalid duration: {}",
            value.trim()
        ))
    })
}

fn command_failure(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        "ffprobe 执行失败".into()
    } else {
        text.chars().take(1_000).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_positive_media_duration() {
        assert_eq!(
            parse_duration(b"12.345\n").unwrap(),
            Duration::from_millis(12_345)
        );
    }

    #[test]
    fn rejects_missing_or_zero_media_duration() {
        assert!(matches!(
            parse_duration(b"N/A\n"),
            Err(AppError::ProbeFailed(_))
        ));
        assert!(matches!(
            parse_duration(b"0\n"),
            Err(AppError::ProbeFailed(_))
        ));
    }
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    index: u32,
    codec_name: Option<String>,
    codec_type: Option<String>,
    #[serde(default)]
    tags: Option<FfprobeTags>,
    #[serde(default)]
    disposition: FfprobeDisposition,
}

#[derive(Debug, Deserialize)]
struct FfprobeTags {
    language: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FfprobeDisposition {
    #[serde(default)]
    default: i32,
    #[serde(default)]
    forced: i32,
}
