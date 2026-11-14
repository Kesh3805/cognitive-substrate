//! Resolution proposals: reviewable recommendations, never mutations.
//!
//! A detector may notice that two memories duplicate each other, that a
//! newer fact supersedes an older one, or that two facts or beliefs
//! contradict. It records a [`ResolutionProposal`] bound to the exact
//! revisions it saw. Nothing changes until an authorized principal accepts
//! it; acceptance re-checks those revisions and applies the operation
//! atomically, or marks the proposal stale.

use cs_core::{
    ActorRef, BeliefId, EntityId, Error, MemoryId, MemoryRevisionId, ProposalId, Result, Scope,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    Duplicate,
    Supersession,
    Contradiction,
    /// Two entities in one partition name the same thing.
    EntityMerge,
}

impl ResolutionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Supersession => "supersession",
            Self::Contradiction => "contradiction",
            Self::EntityMerge => "entity_merge",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "duplicate" => Self::Duplicate,
            "supersession" => Self::Supersession,
            "contradiction" => Self::Contradiction,
            "entity_merge" => Self::EntityMerge,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Rejected,
    /// A target changed after detection. Never applied implicitly.
    Stale,
}

impl ProposalStatus {
    pub const ALL: [Self; 4] = [Self::Pending, Self::Accepted, Self::Rejected, Self::Stale];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Stale => "stale",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == v)
    }
}

/// An exact revision a proposal was computed against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordVersion {
    Memory {
        id: MemoryId,
        revision_id: MemoryRevisionId,
    },
    /// Beliefs: `lineage_id` is the logical identity, `revision_id` the
    /// specific revision.
    Belief {
        lineage_id: BeliefId,
        revision_id: BeliefId,
    },
    /// Entities: identity changes bump `version`.
    Entity { id: EntityId, version: u32 },
}

/// What acceptance will do. Explicit by design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolutionOperation {
    /// Keep `canonical`; tombstone `duplicate`, recording the merge.
    Merge {
        canonical: MemoryId,
        duplicate: MemoryId,
    },
    /// Close the older fact's valid time where the newer one begins.
    /// History is preserved; this is not deletion.
    Supersede { older: MemoryId, newer: MemoryId },
    /// Return both facts to `candidate` until someone validates one.
    Quarantine { memories: Vec<MemoryId> },
    /// Mark every belief `disputed`. Never picks a winner.
    Dispute { beliefs: Vec<BeliefId> },
    /// Redirect `merged` into `survivor`: aliases, mentions and edges move;
    /// `merged` remains as a tombstone that resolves to `survivor`.
    MergeEntities {
        survivor: EntityId,
        merged: EntityId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolutionSignal {
    NormalizedTextExact,
    StructuredFieldsExact,
    DenseSimilarity {
        cosine: f64,
        threshold: f64,
    },
    TemporalOverlap,
    ScopeCompatible,
    PredicateConflict {
        field: String,
        left: String,
        right: String,
    },
    OppositePolarity,
    ValidTimeOrdered,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionRationale {
    pub explanation: String,
    pub signals: Vec<ResolutionSignal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectionMetadata {
    pub detector_version: String,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(with = "time::serde::rfc3339")]
    pub detected_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionProposal {
    pub id: ProposalId,
    pub scope: Scope,
    pub kind: ResolutionKind,
    pub status: ProposalStatus,
    pub operation: ResolutionOperation,
    pub rationale: ResolutionRationale,
    pub detection: DetectionMetadata,
    /// Revisions the proposal is bound to, sorted.
    pub based_on: Vec<RecordVersion>,
    /// Identity of "this finding about these exact revisions". Unique
    /// across all proposals.
    pub fingerprint: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub decided_by: Option<ActorRef>,
    #[serde(default)]
    pub decision_reason: Option<String>,
}

impl ResolutionProposal {
    pub fn validate(&self) -> Result<()> {
        if self.based_on.len() < 2 {
            return Err(Error::validation(
                "a proposal concerns at least two records",
            ));
        }
        if self.rationale.explanation.trim().is_empty() {
            return Err(Error::validation("a proposal needs an explanation"));
        }
        Ok(())
    }
}

/// Fingerprint: kind plus the sorted *exact revisions*. Because any real
/// change to a target produces a new revision (and so a new fingerprint),
/// a fingerprint is never proposed twice, whatever the earlier proposal's
/// outcome. A rejected finding therefore stays rejected until the data
/// changes, instead of being re-detected on every pass.
pub fn fingerprint(kind: ResolutionKind, versions: &[RecordVersion]) -> String {
    let mut ids: Vec<String> = versions
        .iter()
        .map(|v| match v {
            RecordVersion::Memory { id, revision_id } => format!("m:{id}@{revision_id}"),
            RecordVersion::Belief {
                lineage_id,
                revision_id,
            } => {
                format!("b:{lineage_id}@{revision_id}")
            }
            RecordVersion::Entity { id, version } => format!("e:{id}@v{version}"),
        })
        .collect();
    ids.sort();
    format!("{}|{}", kind.as_str(), ids.join(","))
}

/// A decision recorded on a proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalDecision {
    pub status: ProposalStatus,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
    pub decided_by: ActorRef,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_order_but_binds_revisions() {
        let (a, b) = (MemoryId::new_v7(), MemoryId::new_v7());
        let (ra, rb) = (MemoryRevisionId::new_v7(), MemoryRevisionId::new_v7());
        let v = |id, r| RecordVersion::Memory { id, revision_id: r };
        let f1 = fingerprint(ResolutionKind::Duplicate, &[v(a, ra), v(b, rb)]);
        let f2 = fingerprint(ResolutionKind::Duplicate, &[v(b, rb), v(a, ra)]);
        assert_eq!(f1, f2);
        let f3 = fingerprint(
            ResolutionKind::Duplicate,
            &[v(a, ra), v(b, MemoryRevisionId::new_v7())],
        );
        assert_ne!(f1, f3);
    }

    #[test]
    fn operation_serializes_tagged() {
        let op = ResolutionOperation::Merge {
            canonical: MemoryId::new_v7(),
            duplicate: MemoryId::new_v7(),
        };
        assert_eq!(serde_json::to_value(&op).unwrap()["type"], "merge");
    }
}
