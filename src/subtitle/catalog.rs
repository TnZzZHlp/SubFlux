use std::path::{Path, PathBuf};

use crate::{
    config::LanguageCode,
    error::Result,
    media::{MediaProbe, SubtitleTrack, probe_media},
    output::path::strip_trailing_language,
};

use super::SubtitleFormat;

#[derive(Clone, Debug)]
pub struct SubtitleFile {
    pub path: PathBuf,
    pub language: Option<String>,
}

impl SubtitleFile {
    pub fn display_label(&self) -> String {
        format!(
            "外部  {}  {}",
            self.path.file_name().unwrap_or_default().to_string_lossy(),
            self.language.as_deref().unwrap_or("未知")
        )
    }
}

pub fn preferred_track<'a>(
    probe: &'a MediaProbe,
    target: &LanguageCode,
) -> Option<&'a SubtitleTrack> {
    let mut source_probe = probe.clone();
    source_probe.subtitle_tracks.retain(|track| {
        !track
            .language
            .as_deref()
            .is_some_and(|language| language_matches(language, target))
    });
    source_probe
        .auto_track()
        .and_then(|track| probe.track(track.index))
        .or_else(|| probe.auto_track())
}

pub async fn probe_subtitles(video: &Path) -> Result<(MediaProbe, Vec<SubtitleFile>)> {
    let (probe, files) = tokio::try_join!(probe_media(video), discover_subtitles(video))?;
    Ok((probe, files))
}

pub async fn discover_subtitles(video: &Path) -> Result<Vec<SubtitleFile>> {
    let Some(stem) = video.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(Vec::new());
    };
    discover_subtitles_for_stem(video, stem).await
}

pub async fn discover_related_subtitles(source: &Path) -> Result<Vec<SubtitleFile>> {
    let Some(stem) = source.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(Vec::new());
    };
    discover_subtitles_for_stem(source, strip_trailing_language(stem)).await
}

async fn discover_subtitles_for_stem(source: &Path, stem: &str) -> Result<Vec<SubtitleFile>> {
    let directory = source
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if SubtitleFormat::from_path(&path).is_none() {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if name != stem
            && !name
                .strip_prefix(stem)
                .is_some_and(|suffix| suffix.starts_with('.'))
        {
            continue;
        }
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_file() {
            files.push(SubtitleFile {
                language: filename_language(&path),
                path,
            });
        }
    }
    files.sort_by_key(|file| {
        let same_stem = file.path.file_stem().and_then(|name| name.to_str()) == Some(stem);
        let format = match SubtitleFormat::from_path(&file.path) {
            Some(SubtitleFormat::Srt) => 0,
            Some(SubtitleFormat::Ass) => 1,
            Some(SubtitleFormat::Ssa) => 2,
            Some(SubtitleFormat::Vtt) | None => 3,
        };
        (!same_stem, format, file.path.clone())
    });
    Ok(files)
}

pub fn filename_language(path: &Path) -> Option<String> {
    path.file_stem()?
        .to_str()?
        .split_once('.')?
        .1
        .rsplit('.')
        .find_map(|part| {
            let normalized = normalize_language(part);
            let primary = normalized.split('-').next()?;
            (!matches!(primary, "sdh" | "cc" | "sub")
                && LanguageCode::parse(normalized.clone()).is_ok())
            .then_some(normalized)
        })
}

pub fn language_matches(language: &str, target: &LanguageCode) -> bool {
    let language = normalize_language(language);
    let target = normalize_language(target.as_str());
    if matches!(language.as_str(), "auto" | "und" | "unknown") {
        return false;
    }
    language == target
        || ((!language.contains('-') || !target.contains('-'))
            && language.split('-').next() == target.split('-').next())
}

fn normalize_language(language: &str) -> String {
    let value = language.trim().to_ascii_lowercase().replace('_', "-");
    let (primary, suffix) = value.split_once('-').unwrap_or((&value, ""));
    let primary = match primary {
        "eng" => "en",
        "jpn" => "ja",
        "kor" => "ko",
        "chi" | "zho" => "zh",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "rus" => "ru",
        "ara" => "ar",
        "hin" => "hi",
        "tha" => "th",
        "vie" => "vi",
        "ind" => "id",
        "dut" | "nld" => "nl",
        other => other,
    };
    match (primary, suffix) {
        ("zh", "hans" | "cn" | "sg") => "zh-hans".into(),
        ("zh", "hant" | "tw" | "hk" | "mo") => "zh-hant".into(),
        (_, "") => primary.into(),
        _ => format!("{primary}-{suffix}"),
    }
}
