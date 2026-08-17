use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;

use crate::error::{AppError, Result};

use super::SubtitleDocument;

#[async_trait]
pub trait SubtitleWriter: Send + Sync {
    async fn write(
        &self,
        document: &SubtitleDocument,
        output: &Path,
        overwrite: bool,
    ) -> Result<PathBuf>;
}

#[derive(Clone, Debug, Default)]
pub struct FileSubtitleWriter;

#[async_trait]
impl SubtitleWriter for FileSubtitleWriter {
    async fn write(
        &self,
        document: &SubtitleDocument,
        output: &Path,
        overwrite: bool,
    ) -> Result<PathBuf> {
        if output.exists() && !overwrite {
            return Err(AppError::OutputExists(output.to_path_buf()));
        }
        let content = document.render()?;
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temp = tempfile::Builder::new()
            .prefix(".subtitle-translator-")
            .tempfile_in(parent)
            .map_err(|error| AppError::SubtitleWriteError(error.to_string()))?
            .into_temp_path();
        let temp_path = temp.to_path_buf();
        tokio::fs::write(&temp_path, content)
            .await
            .map_err(|error| AppError::SubtitleWriteError(error.to_string()))?;
        let persisted = if overwrite {
            temp.persist(output)
        } else {
            temp.persist_noclobber(output)
        };
        persisted.map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                AppError::OutputExists(output.to_path_buf())
            } else {
                AppError::SubtitleWriteError(error.error.to_string())
            }
        })?;
        Ok(output.to_path_buf())
    }
}

pub type DynSubtitleWriter = Arc<dyn SubtitleWriter>;

#[cfg(test)]
mod tests {
    use crate::{
        error::AppError,
        subtitle::{SpeechSegment, SubtitleDocument},
    };

    use super::*;

    #[tokio::test]
    async fn writes_rendered_document_and_refuses_unapproved_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("movie.zh-CN.srt");
        let mut document = SubtitleDocument::from_speech_segments(
            vec![SpeechSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
            }],
            None,
        )
        .unwrap();
        document.entries[0].translated_text = Some("你好".into());
        let writer = FileSubtitleWriter;
        writer.write(&document, &output, false).await.unwrap();
        assert!(
            tokio::fs::read_to_string(&output)
                .await
                .unwrap()
                .contains("你好")
        );
        assert!(matches!(
            writer.write(&document, &output, false).await,
            Err(AppError::OutputExists(_))
        ));
    }
}
