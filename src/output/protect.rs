use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

pub async fn protect_source_output(
    output: PathBuf,
    source: Option<&Path>,
) -> Result<(PathBuf, bool)> {
    let Some(source) = source else {
        return Ok((output, false));
    };
    if !same_file(&output, source).await? {
        return Ok((output, false));
    }
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::SubtitleWriteError("output filename has no usable stem".into()))?;
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("srt");
    for index in 1_u64.. {
        let suffix = if index == 1 {
            String::new()
        } else {
            format!(".{index}")
        };
        let candidate = output.with_file_name(format!("{stem}.translated{suffix}.{extension}"));
        if !tokio::fs::try_exists(&candidate).await? {
            return Ok((candidate, true));
        }
    }
    Err(AppError::SubtitleWriteError(
        "no unused output filename available".into(),
    ))
}

pub(crate) async fn same_file(left: &Path, right: &Path) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    if !tokio::fs::try_exists(left).await? || !tokio::fs::try_exists(right).await? {
        return Ok(false);
    }
    if tokio::fs::canonicalize(left).await? == tokio::fs::canonicalize(right).await? {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left = tokio::fs::metadata(left).await?;
        let right = tokio::fs::metadata(right).await?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    Ok(false)
}
