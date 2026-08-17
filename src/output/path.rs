use std::path::{Path, PathBuf};

use crate::{
    config::LanguageCode,
    error::{AppError, Result},
    subtitle::SubtitleFormat,
};

/// Computes the sole allowed output name. A related video always wins over the
/// external subtitle's name, preventing `movie.ja.zh-CN.ass` style outputs.
pub fn build_output_path(
    video: Option<&Path>,
    subtitle_input: Option<&Path>,
    target_language: &LanguageCode,
    format: SubtitleFormat,
) -> Result<PathBuf> {
    let naming_source = video.or(subtitle_input).ok_or_else(|| {
        AppError::SubtitleWriteError("an output path needs a video or subtitle source".into())
    })?;
    let directory = naming_source.parent().unwrap_or_else(|| Path::new("."));
    let stem = naming_source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::SubtitleWriteError("source filename has no usable stem".into()))?;
    let base_name = if video.is_some() {
        stem
    } else {
        strip_trailing_language(stem)
    };
    Ok(directory.join(format!(
        "{base_name}.{target_language}.{}",
        format.extension()
    )))
}

fn strip_trailing_language(stem: &str) -> &str {
    let Some((base, candidate)) = stem.rsplit_once('.') else {
        return stem;
    };
    if !base.is_empty() && LanguageCode::parse(candidate.to_owned()).is_ok() {
        base
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language(value: &str) -> LanguageCode {
        LanguageCode::parse(value).unwrap()
    }

    #[test]
    fn srt_output_uses_video_stem_and_target_code() {
        assert_eq!(
            build_output_path(
                Some(Path::new("/movie/movie.mkv")),
                None,
                &language("zh-CN"),
                SubtitleFormat::Srt,
            )
            .unwrap(),
            Path::new("/movie/movie.zh-CN.srt")
        );
    }

    #[test]
    fn video_name_never_includes_its_extension() {
        assert_eq!(
            build_output_path(
                Some(Path::new("/movie/Anime EP01.mkv")),
                None,
                &language("ja"),
                SubtitleFormat::Ass,
            )
            .unwrap(),
            Path::new("/movie/Anime EP01.ja.ass")
        );
    }

    #[test]
    fn external_subtitle_uses_video_base_name() {
        assert_eq!(
            build_output_path(
                Some(Path::new("/movie/movie.mkv")),
                Some(Path::new("/movie/movie.ja.ass")),
                &language("zh-CN"),
                SubtitleFormat::Ass,
            )
            .unwrap(),
            Path::new("/movie/movie.zh-CN.ass")
        );
    }

    #[test]
    fn standalone_subtitle_drops_its_language_suffix() {
        assert_eq!(
            build_output_path(
                None,
                Some(Path::new("movie.ja.ass")),
                &language("zh-CN"),
                SubtitleFormat::Ass,
            )
            .unwrap(),
            Path::new("movie.zh-CN.ass")
        );
    }
}
