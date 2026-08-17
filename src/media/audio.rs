use std::{path::Path, time::Duration};

use tempfile::TempPath;

/// Owns one temporary audio fragment. Dropping it removes the file even when
/// the pipeline returns early with an error or cancellation.
pub struct TemporaryAudio {
    path: TempPath,
}

/// A source-time range for one STT request. The extracted range includes
/// overlap around the retained range so recognition at a boundary has context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioChunk {
    /// Absolute source time represented by the start of the temporary file.
    pub source_start_ms: u64,
    /// Length of the temporary file's source-time range.
    pub duration_ms: u64,
    /// Inclusive start of the non-overlapping result range.
    pub retain_start_ms: u64,
    /// Exclusive end of the non-overlapping result range.
    pub retain_end_ms: u64,
}

impl AudioChunk {
    pub const fn source_end_ms(&self) -> u64 {
        self.source_start_ms.saturating_add(self.duration_ms)
    }

    pub const fn retains_midpoint(&self, timestamp_ms: u64) -> bool {
        timestamp_ms >= self.retain_start_ms && timestamp_ms < self.retain_end_ms
    }
}

/// Builds contiguous source windows with context on both sides of each boundary.
///
/// A zero duration produces no work; configuration validation keeps the chunk
/// duration non-zero and overlap smaller than a chunk.
pub fn plan_audio_chunks(
    duration: Duration,
    chunk_seconds: u64,
    overlap_seconds: u64,
) -> Vec<AudioChunk> {
    let total_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let chunk_ms = chunk_seconds.saturating_mul(1_000);
    let overlap_ms = overlap_seconds.saturating_mul(1_000);
    if total_ms == 0 || chunk_ms == 0 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut retain_start_ms = 0;
    while retain_start_ms < total_ms {
        let retain_end_ms = retain_start_ms.saturating_add(chunk_ms).min(total_ms);
        let source_start_ms = retain_start_ms.saturating_sub(overlap_ms);
        let source_end_ms = retain_end_ms.saturating_add(overlap_ms).min(total_ms);
        chunks.push(AudioChunk {
            source_start_ms,
            duration_ms: source_end_ms.saturating_sub(source_start_ms),
            retain_start_ms,
            retain_end_ms,
        });
        retain_start_ms = retain_end_ms;
    }
    chunks
}

impl TemporaryAudio {
    pub(crate) const fn new(path: TempPath) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        self.path.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_center_owned_chunks_with_boundary_context() {
        let chunks = plan_audio_chunks(Duration::from_secs(1_250), 600, 2);
        assert_eq!(
            chunks,
            vec![
                AudioChunk {
                    source_start_ms: 0,
                    duration_ms: 602_000,
                    retain_start_ms: 0,
                    retain_end_ms: 600_000,
                },
                AudioChunk {
                    source_start_ms: 598_000,
                    duration_ms: 604_000,
                    retain_start_ms: 600_000,
                    retain_end_ms: 1_200_000,
                },
                AudioChunk {
                    source_start_ms: 1_198_000,
                    duration_ms: 52_000,
                    retain_start_ms: 1_200_000,
                    retain_end_ms: 1_250_000,
                },
            ]
        );
    }

    #[test]
    fn plans_one_chunk_for_short_audio() {
        let chunks = plan_audio_chunks(Duration::from_secs(12), 600, 2);
        assert_eq!(
            chunks,
            vec![AudioChunk {
                source_start_ms: 0,
                duration_ms: 12_000,
                retain_start_ms: 0,
                retain_end_ms: 12_000,
            }]
        );
    }
}
