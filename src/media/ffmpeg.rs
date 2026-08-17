use std::{path::Path, process::Stdio};

use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    error::{AppError, Result},
    subtitle::SubtitleFormat,
};

use super::{
    audio::{AudioChunk, TemporaryAudio},
    model::{SubtitleTrack, SubtitleTrackKind},
};

#[derive(Clone, Debug)]
pub struct ExtractedSubtitle {
    pub format: SubtitleFormat,
    pub content: String,
}

#[derive(Clone, Debug, Default)]
pub struct Ffmpeg;

impl Ffmpeg {
    pub async fn extract_subtitle(
        &self,
        video: &Path,
        track: &SubtitleTrack,
        cancellation: &CancellationToken,
    ) -> Result<ExtractedSubtitle> {
        let SubtitleTrackKind::Text(format) = track.kind else {
            return Err(AppError::UnsupportedSubtitleCodec(track.codec.clone()));
        };
        let output = tempfile::Builder::new()
            .prefix("subtitle-translator-")
            .suffix(&format!(".{}", format.extension()))
            .tempfile()?
            .into_temp_path();
        let output_path = output.to_path_buf();
        let codec = extraction_codec(format);
        debug!(
            input = %video.display(),
            track = track.index.0,
            output_format = %format,
            ffmpeg_command = %format!(
                "ffmpeg -nostdin -y -v error -i {} -map 0:{} -c:s {} {}",
                video.display(), track.index.0, codec, output_path.display()
            ),
            "extracting embedded subtitle with ffmpeg"
        );
        let mut command = Command::new("ffmpeg");
        command
            .args(["-nostdin", "-y", "-v", "error", "-i"])
            .arg(video)
            .args(["-map", &format!("0:{}", track.index.0), "-c:s", codec])
            .arg(&output_path);
        run_ffmpeg(command, cancellation, AppError::SubtitleExtractionFailed).await?;
        let bytes = tokio::fs::read(&output_path).await?;
        let content = String::from_utf8(bytes).map_err(|_| {
            AppError::SubtitleParseError("extracted subtitle is not valid UTF-8".into())
        })?;
        Ok(ExtractedSubtitle { format, content })
    }

    pub async fn extract_audio(
        &self,
        video: &Path,
        cancellation: &CancellationToken,
    ) -> Result<TemporaryAudio> {
        let output = tempfile::Builder::new()
            .prefix("subtitle-translator-")
            .suffix(".flac")
            .tempfile()?
            .into_temp_path();
        let output_path = output.to_path_buf();
        debug!(
            input = %video.display(),
            ffmpeg_command = %format!(
                "ffmpeg -nostdin -y -v error -i {} -vn -ac 1 -ar 16000 -c:a flac {}",
                video.display(), output_path.display()
            ),
            "extracting 16kHz mono FLAC for speech recognition"
        );
        let mut command = Command::new("ffmpeg");
        command
            .args(["-nostdin", "-y", "-v", "error", "-i"])
            .arg(video)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "flac"])
            .arg(&output_path);
        run_ffmpeg(command, cancellation, AppError::AudioExtractionFailed).await?;
        Ok(TemporaryAudio::new(output))
    }

    /// Extracts one bounded FLAC fragment for an STT upload. Source-time
    /// coordinates are preserved separately by [`AudioChunk`], so a provider's
    /// local timestamps can be shifted back after transcription.
    pub async fn extract_audio_chunk(
        &self,
        video: &Path,
        chunk: &AudioChunk,
        cancellation: &CancellationToken,
    ) -> Result<TemporaryAudio> {
        if chunk.duration_ms == 0 {
            return Err(AppError::AudioExtractionFailed(
                "audio chunk duration must be greater than zero".into(),
            ));
        }
        let output = tempfile::Builder::new()
            .prefix("subtitle-translator-")
            .suffix(".flac")
            .tempfile()?
            .into_temp_path();
        let output_path = output.to_path_buf();
        let start = format_time_ms(chunk.source_start_ms);
        let duration = format_time_ms(chunk.duration_ms);
        debug!(
            input = %video.display(),
            source_start_ms = chunk.source_start_ms,
            duration_ms = chunk.duration_ms,
            ffmpeg_command = %format!(
                "ffmpeg -nostdin -y -v error -ss {start} -i {} -t {duration} -vn -ac 1 -ar 16000 -c:a flac {}",
                video.display(), output_path.display()
            ),
            "extracting bounded FLAC fragment for speech recognition"
        );
        let mut command = Command::new("ffmpeg");
        command
            .args(["-nostdin", "-y", "-v", "error", "-ss", &start, "-i"])
            .arg(video)
            .args([
                "-t", &duration, "-vn", "-ac", "1", "-ar", "16000", "-c:a", "flac",
            ])
            .arg(&output_path);
        run_ffmpeg(command, cancellation, AppError::AudioExtractionFailed).await?;
        Ok(TemporaryAudio::new(output))
    }
}

fn format_time_ms(milliseconds: u64) -> String {
    format!("{}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

const fn extraction_codec(format: SubtitleFormat) -> &'static str {
    match format {
        // Copying ASS/SSA retains override data and native SSA styles. SRT and
        // WebVTT use their muxer-compatible text codec, which also handles
        // `mov_text` embedded tracks.
        SubtitleFormat::Ass | SubtitleFormat::Ssa => "copy",
        SubtitleFormat::Srt => "srt",
        SubtitleFormat::Vtt => "webvtt",
    }
}

async fn run_ffmpeg(
    mut command: Command,
    cancellation: &CancellationToken,
    make_error: impl Fn(String) -> AppError + Copy,
) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::FfmpegNotFound
        } else {
            make_error(error.to_string())
        }
    })?;
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut bytes).await;
        }
        bytes
    });

    let status = tokio::select! {
        result = child.wait() => result?,
        () = cancellation.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stderr_task.await;
            return Err(AppError::Cancelled);
        }
    };
    let stderr = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let message: String = String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(1_000)
            .collect();
        return Err(make_error(if message.is_empty() {
            format!("ffmpeg exited with {status}")
        } else {
            message
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use crate::media::{check_tools, probe_media};

    use super::*;

    #[tokio::test]
    async fn probes_and_extracts_a_real_embedded_srt_when_ffmpeg_is_available() {
        // Developers without the required runtime tools can still run the pure
        // logic suite. CI/runtime environments with ffmpeg exercise the real
        // command boundary rather than an FFmpeg Rust FFI.
        if !check_tools().is_ready() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.srt");
        let video = directory.path().join("sample.mkv");
        tokio::fs::write(&source, "1\n00:00:00,000 --> 00:00:00,800\nembedded text\n")
            .await
            .unwrap();
        let generated = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:duration=1",
                "-i",
            ])
            .arg(&source)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-map",
                "2:0",
                "-c:v",
                "mpeg4",
                "-c:a",
                "pcm_s16le",
                "-c:s",
                "srt",
                "-shortest",
            ])
            .arg(&video)
            .output()
            .await
            .unwrap();
        assert!(
            generated.status.success(),
            "fixture generation failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let probe = probe_media(&video).await.unwrap();
        let track = probe
            .auto_track()
            .expect("fixture has a text subtitle track");
        let extracted = Ffmpeg
            .extract_subtitle(&video, track, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(extracted.format, SubtitleFormat::Srt);
        assert!(extracted.content.contains("embedded text"));
        let audio = Ffmpeg
            .extract_audio(&video, &CancellationToken::new())
            .await
            .unwrap();
        let audio_path = audio.path().to_path_buf();
        assert!(tokio::fs::metadata(&audio_path).await.unwrap().len() > 44);
        assert_eq!(
            audio_path.extension().and_then(|value| value.to_str()),
            Some("flac")
        );

        let audio_chunk = Ffmpeg
            .extract_audio_chunk(
                &video,
                &AudioChunk {
                    source_start_ms: 0,
                    duration_ms: 500,
                    retain_start_ms: 0,
                    retain_end_ms: 500,
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(tokio::fs::metadata(audio_chunk.path()).await.unwrap().len() > 0);
        drop(audio);
        assert!(
            !audio_path.exists(),
            "temporary audio should clean itself up"
        );
    }
}
