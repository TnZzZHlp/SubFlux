use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::error::{AppError, Result};

const VIDEO_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "asf", "avi", "divx", "f4v", "flv", "m2ts", "m4v", "mk3d", "mkv", "mov", "mp4",
    "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "qt", "rm", "rmvb", "ts", "vob", "webm", "wmv",
];

/// Finds one video file or recursively discovers video files below a directory.
///
/// A directory's symlink children are not followed, which prevents recursive
/// loops while scanning a media library.
pub fn discover_videos(input: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let input = input.as_ref();
    let metadata = fs::metadata(input)?;
    if metadata.is_file() {
        return Ok(is_video_path(input)
            .then(|| input.to_path_buf())
            .into_iter()
            .collect());
    }
    if !metadata.is_dir() {
        return Err(AppError::InvalidConfig(format!(
            "startup path is neither a file nor a directory: {}",
            input.display()
        )));
    }

    let mut pending_directories = vec![input.to_path_buf()];
    let mut videos = Vec::new();
    while let Some(directory) = pending_directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                pending_directories.push(path);
            } else if file_type.is_file() && is_video_path(&path) {
                videos.push(path);
            }
        }
    }
    videos.sort();
    Ok(videos)
}

fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            VIDEO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        fs::write(path, []).unwrap();
    }

    #[test]
    fn recursively_discovers_common_video_extensions_in_stable_order() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        let deeper = nested.join("deeper");
        fs::create_dir_all(&deeper).unwrap();
        touch(&directory.path().join("episode.mkv"));
        touch(&nested.join("clip.MP4"));
        touch(&deeper.join("movie.webm"));
        touch(&nested.join("notes.txt"));

        let videos = discover_videos(directory.path()).unwrap();
        let relative: Vec<_> = videos
            .iter()
            .map(|path| path.strip_prefix(directory.path()).unwrap().to_path_buf())
            .collect();
        assert_eq!(
            relative,
            vec![
                PathBuf::from("episode.mkv"),
                PathBuf::from("nested/clip.MP4"),
                PathBuf::from("nested/deeper/movie.webm"),
            ]
        );
    }

    #[test]
    fn accepts_a_single_video_file_and_ignores_other_files() {
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("feature.mov");
        let text = directory.path().join("readme.md");
        touch(&video);
        touch(&text);

        assert_eq!(discover_videos(&video).unwrap(), vec![video]);
        assert_eq!(
            discover_videos(&text).unwrap(),
            [] as [std::path::PathBuf; 0]
        );
    }
}
