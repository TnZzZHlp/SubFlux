use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{
    config::LanguageCode,
    error::{AppError, Result},
    stt::SttResult,
    subtitle::SpeechSegment,
    translator::{TranslationItem, TranslationResponse},
};

const CHECKPOINT_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputFingerprint {
    path: PathBuf,
    length: u64,
    modified_seconds: u64,
    modified_nanos: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SttCheckpointSettings {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub chunk_seconds: u64,
    pub chunk_overlap_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranslatorCheckpointSettings {
    pub provider: String,
    pub api_format: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub chunk_size: usize,
    pub context_before: usize,
    pub context_after: usize,
    pub max_retries: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckpointIdentity {
    pub inputs: Vec<InputFingerprint>,
    pub requested_input: String,
    pub resolved_input: String,
    pub track_index: Option<u32>,
    pub format: String,
    pub source_language: String,
    pub target_language: String,
    pub http_timeout_seconds: u64,
    pub stt: SttCheckpointSettings,
    pub translator: TranslatorCheckpointSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedCheckpoint {
    version: u8,
    identity: CheckpointIdentity,
    #[serde(default)]
    stt_chunks: Vec<SavedSttChunk>,
    #[serde(default)]
    translation_chunks: Vec<SavedTranslationChunk>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedSttChunk {
    index: usize,
    language: Option<String>,
    segments: Vec<SavedSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedSegment {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SavedTranslationChunk {
    index: usize,
    entries: Vec<TranslationItem>,
}

pub struct CheckpointStore {
    path: PathBuf,
    data: PersistedCheckpoint,
}

impl CheckpointStore {
    pub(crate) async fn load(output: &Path, identity: CheckpointIdentity) -> Result<Self> {
        let path = checkpoint_path(output, &identity)?;
        let data = match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice::<PersistedCheckpoint>(&bytes) {
                Ok(data)
                    if data.version == CHECKPOINT_VERSION
                        && data.identity == identity
                        && data.is_structurally_valid() =>
                {
                    data
                }
                Ok(_) | Err(_) => {
                    debug!(checkpoint = %path.display(), "ignoring invalid or incompatible checkpoint");
                    PersistedCheckpoint::new(identity)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistedCheckpoint::new(identity)
            }
            Err(_) => {
                return Err(checkpoint_error(
                    "could not read checkpoint; check output directory permissions",
                ));
            }
        };
        Ok(Self { path, data })
    }

    pub(crate) const fn stt_len(&self) -> usize {
        self.data.stt_chunks.len()
    }

    pub(crate) const fn has_translations(&self) -> bool {
        !self.data.translation_chunks.is_empty()
    }

    pub(crate) const fn translation_len(&self) -> usize {
        self.data.translation_chunks.len()
    }

    pub(crate) fn stt_results(&self) -> Result<Vec<SttResult>> {
        self.data
            .stt_chunks
            .iter()
            .map(|chunk| {
                Ok(SttResult {
                    language: chunk
                        .language
                        .as_deref()
                        .map(|language| LanguageCode::parse(language.to_owned()))
                        .transpose()?,
                    segments: chunk
                        .segments
                        .iter()
                        .map(|segment| SpeechSegment {
                            start_ms: segment.start_ms,
                            end_ms: segment.end_ms,
                            text: segment.text.clone(),
                        })
                        .collect(),
                })
            })
            .collect()
    }

    pub(crate) fn translation_response(&self, index: usize) -> Option<TranslationResponse> {
        self.data
            .translation_chunks
            .get(index)
            .filter(|chunk| chunk.index == index)
            .map(|chunk| TranslationResponse {
                entries: chunk.entries.clone(),
            })
    }

    pub(crate) async fn record_stt(&mut self, index: usize, result: &SttResult) -> Result<()> {
        if index != self.data.stt_chunks.len() {
            return Err(checkpoint_error("checkpoint STT chunks are not contiguous"));
        }
        self.data.stt_chunks.push(SavedSttChunk {
            index,
            language: result.language.as_ref().map(ToString::to_string),
            segments: result
                .segments
                .iter()
                .map(|segment| SavedSegment {
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                    text: segment.text.clone(),
                })
                .collect(),
        });
        self.persist().await
    }

    pub(crate) async fn record_translation(
        &mut self,
        index: usize,
        response: &TranslationResponse,
    ) -> Result<()> {
        if index != self.data.translation_chunks.len() {
            return Err(checkpoint_error(
                "checkpoint translation chunks are not contiguous",
            ));
        }
        self.data.translation_chunks.push(SavedTranslationChunk {
            index,
            entries: response.entries.clone(),
        });
        self.persist().await
    }

    pub(crate) async fn clear(&mut self) -> Result<()> {
        self.data.stt_chunks.clear();
        self.data.translation_chunks.clear();
        self.persist().await
    }

    pub(crate) async fn remove_after_success(&self) -> Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(checkpoint_error(
                "could not remove completed checkpoint; it can be deleted manually",
            )),
        }
    }

    // ponytail: rewrites one JSON file per completed chunk; use append-only records if very large checkpoints become a measured bottleneck.
    async fn persist(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| checkpoint_error("could not create checkpoint directory"))?;
        let content = serde_json::to_vec(&self.data)
            .map_err(|_| checkpoint_error("could not serialize checkpoint"))?;
        let temporary = tempfile::Builder::new()
            .prefix(".checkpoint-")
            .suffix(".json")
            .tempfile_in(parent)
            .map_err(|_| checkpoint_error("could not create checkpoint file"))?
            .into_temp_path();
        let temporary_path: &Path = temporary.as_ref();
        tokio::fs::write(temporary_path, content)
            .await
            .map_err(|_| checkpoint_error("could not write checkpoint"))?;
        temporary
            .persist(&self.path)
            .map_err(|_| checkpoint_error("could not save checkpoint"))?;
        Ok(())
    }
}

impl PersistedCheckpoint {
    const fn new(identity: CheckpointIdentity) -> Self {
        Self {
            version: CHECKPOINT_VERSION,
            identity,
            stt_chunks: Vec::new(),
            translation_chunks: Vec::new(),
        }
    }

    fn is_structurally_valid(&self) -> bool {
        self.stt_chunks.iter().enumerate().all(|(index, chunk)| {
            chunk.index == index
                && chunk
                    .language
                    .as_deref()
                    .is_none_or(|language| LanguageCode::parse(language.to_owned()).is_ok())
                && chunk
                    .segments
                    .iter()
                    .all(|segment| segment.end_ms >= segment.start_ms)
        }) && self
            .translation_chunks
            .iter()
            .enumerate()
            .all(|(index, chunk)| chunk.index == index)
    }
}

pub fn fingerprint(path: &Path) -> Result<InputFingerprint> {
    let path = path.canonicalize()?;
    let metadata = std::fs::metadata(&path)?;
    let modified = metadata
        .modified()
        .map_err(|_| checkpoint_error("could not inspect input metadata"))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| checkpoint_error("could not inspect input metadata"))?;
    Ok(InputFingerprint {
        path,
        length: metadata.len(),
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
    })
}

fn checkpoint_path(output: &Path, identity: &CheckpointIdentity) -> Result<PathBuf> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut filename = output
        .file_name()
        .ok_or_else(|| checkpoint_error("output path has no filename"))?
        .to_os_string();
    filename.push(format!(".{:016x}.checkpoint.json", source_hash(identity)?));
    Ok(parent.join(".subflux").join(filename))
}

fn source_hash(identity: &CheckpointIdentity) -> Result<u64> {
    let paths: Vec<_> = identity.inputs.iter().map(|input| &input.path).collect();
    let bytes = serde_json::to_vec(&paths)
        .map_err(|_| checkpoint_error("could not serialize checkpoint identity"))?;
    Ok(bytes
        .into_iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        }))
}

fn checkpoint_error(message: &str) -> AppError {
    AppError::CheckpointError(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> CheckpointIdentity {
        CheckpointIdentity {
            inputs: Vec::new(),
            requested_input: "external".into(),
            resolved_input: "external".into(),
            track_index: None,
            format: "srt".into(),
            source_language: "en".into(),
            target_language: "zh-CN".into(),
            http_timeout_seconds: 120,
            stt: SttCheckpointSettings {
                provider: "openai".into(),
                base_url: "https://example.invalid/v1".into(),
                model: "stt".into(),
                language: "auto".into(),
                chunk_seconds: 600,
                chunk_overlap_seconds: 2,
            },
            translator: TranslatorCheckpointSettings {
                provider: "openai".into(),
                api_format: "OpenAi".into(),
                base_url: "https://example.invalid/v1".into(),
                model: "translator".into(),
                reasoning_effort: None,
                chunk_size: 30,
                context_before: 10,
                context_after: 5,
                max_retries: 3,
            },
        }
    }

    #[test]
    fn separate_sources_use_separate_checkpoint_paths() {
        let output = Path::new("movie.zh-CN.srt");
        let mut first = identity();
        first.inputs = vec![InputFingerprint {
            path: PathBuf::from("first.mkv"),
            length: 1,
            modified_seconds: 1,
            modified_nanos: 0,
        }];
        let mut second = first.clone();
        second.inputs[0].path = PathBuf::from("second.mkv");

        assert_ne!(
            checkpoint_path(output, &first).unwrap(),
            checkpoint_path(output, &second).unwrap()
        );
    }

    #[tokio::test]
    async fn persists_valid_state_and_ignores_incompatible_or_invalid_files() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("movie.zh-CN.srt");
        let mut expected = identity();
        expected.inputs = vec![InputFingerprint {
            path: PathBuf::from("source.mkv"),
            length: 1,
            modified_seconds: 1,
            modified_nanos: 0,
        }];
        let mut checkpoint = CheckpointStore::load(&output, expected.clone())
            .await
            .unwrap();
        checkpoint
            .record_stt(
                0,
                &SttResult {
                    language: Some(LanguageCode::parse("ja").unwrap()),
                    segments: vec![SpeechSegment {
                        start_ms: 0,
                        end_ms: 1_000,
                        text: "source".into(),
                    }],
                },
            )
            .await
            .unwrap();
        let path = checkpoint_path(&output, &expected).unwrap();
        assert!(path.exists());

        let loaded = CheckpointStore::load(&output, expected.clone())
            .await
            .unwrap();
        assert_eq!(loaded.stt_len(), 1);
        assert_eq!(loaded.stt_results().unwrap()[0].segments[0].text, "source");

        let mut legacy = serde_json::to_value(&checkpoint.data).unwrap();
        legacy["identity"]["translator"]
            .as_object_mut()
            .unwrap()
            .remove("reasoning_effort");
        tokio::fs::write(&path, serde_json::to_vec(&legacy).unwrap())
            .await
            .unwrap();
        assert_eq!(
            CheckpointStore::load(&output, expected.clone())
                .await
                .unwrap()
                .stt_len(),
            1
        );

        let mut metadata_mismatch = expected.clone();
        metadata_mismatch.inputs[0].length += 1;
        assert_eq!(
            CheckpointStore::load(&output, metadata_mismatch)
                .await
                .unwrap()
                .stt_len(),
            0
        );

        let mut settings_mismatch = expected.clone();
        settings_mismatch.translator.max_retries += 1;
        assert_eq!(
            CheckpointStore::load(&output, settings_mismatch)
                .await
                .unwrap()
                .stt_len(),
            0
        );

        let mut reasoning_mismatch = expected.clone();
        reasoning_mismatch.translator.reasoning_effort = Some("high".into());
        assert_eq!(
            CheckpointStore::load(&output, reasoning_mismatch)
                .await
                .unwrap()
                .stt_len(),
            0
        );

        tokio::fs::write(&path, "not json").await.unwrap();
        assert_eq!(
            CheckpointStore::load(&output, expected)
                .await
                .unwrap()
                .stt_len(),
            0
        );
    }
}
