//! Raw observations and the extraction runs derived from them.
//!
//! An observation is immutable source material. Extraction projects it into
//! zero or more *candidate* memories; an extraction run records which
//! extractor (and version) produced which candidates, so reprocessing with
//! the same extractor version is a no-op and a new version never overwrites
//! an older result.

use cs_core::{ActorRef, Error, MemoryId, ObservationId, Result, Scope, SourceRef};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::MemoryContent;

pub const MAX_OBSERVATION_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub id: ObservationId,
    pub scope: Scope,
    /// `None` once purged.
    #[serde(default)]
    pub content: Option<String>,
    /// SHA-256 of the original content; retained after purge.
    pub content_hash: String,
    #[serde(default)]
    pub media_type: Option<String>,
    pub source: SourceRef,
    pub actor: ActorRef,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub purged_at: Option<OffsetDateTime>,
}

impl Observation {
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        match &self.content {
            Some(c) if c.trim().is_empty() => {
                Err(Error::validation("observation content is empty"))
            }
            Some(c) if c.len() > MAX_OBSERVATION_BYTES => {
                Err(Error::validation("observation content exceeds 64 KiB"))
            }
            _ => Ok(()),
        }
    }

    pub fn is_purged(&self) -> bool {
        self.purged_at.is_some()
    }
}

/// One extracted candidate, as produced by an extractor (before storage).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedCandidate {
    pub content: MemoryContent,
    /// Extractor-assigned confidence, a heuristic in `[0, 1]`.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Which span or rule produced it (for inspection).
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractionRun {
    pub observation_id: ObservationId,
    pub extractor: String,
    pub extractor_version: String,
    pub candidate_ids: Vec<MemoryId>,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
}
