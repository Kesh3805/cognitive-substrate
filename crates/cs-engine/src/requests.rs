//! Request and response contracts for the engine API. The HTTP service
//! (de)serializes exactly these types.

use cs_core::{
    BeliefId, EvidenceId, ExtractionInfo, MemoryId, MemoryRevisionId, RecallTraceId, Scope,
    SourceRef, TimeInterval,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefPolarity, BeliefStatus, Confidence,
    DependencyRelation, Evidence, MemoryContent, MemoryRecord, RecallPolicy, RecallTrace,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceInput {
    pub source: SourceRef,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub extraction: Option<ExtractionInfo>,
    #[serde(default)]
    pub parent_evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMemory {
    pub scope: Scope,
    pub content: MemoryContent,
    pub provenance: ProvenanceInput,
    /// Defaults to `[now, ∞)`.
    #[serde(default)]
    pub valid_time: Option<TimeInterval>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviseMemory {
    #[serde(default)]
    pub content: Option<MemoryContent>,
    #[serde(default)]
    pub valid_time: Option<TimeInterval>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub provenance: Option<ProvenanceInput>,
    pub reason: String,
    /// Optimistic concurrency: fail if this is not the current revision.
    #[serde(default)]
    pub expected_revision: Option<MemoryRevisionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusChange {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHistory {
    pub memory_id: MemoryId,
    pub revisions: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvidence {
    pub source: SourceRef,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub observed_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub extraction: Option<ExtractionInfo>,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyInput {
    /// Any revision of the belief relied upon; resolved to its current revision.
    pub belief_id: BeliefId,
    pub relation: DependencyRelation,
    #[serde(default)]
    pub strength: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewBelief {
    pub scope: Scope,
    pub proposition: String,
    #[serde(default = "default_polarity")]
    pub polarity: BeliefPolarity,
    pub confidence: Confidence,
    #[serde(default)]
    pub evidence: Vec<BeliefEvidence>,
    #[serde(default)]
    pub depends_on: Vec<DependencyInput>,
    #[serde(default)]
    pub valid_time: Option<TimeInterval>,
    #[serde(default)]
    pub revalidation_interval_days: Option<u32>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

const fn default_polarity() -> BeliefPolarity {
    BeliefPolarity::Supports
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviseBelief {
    #[serde(default)]
    pub proposition: Option<String>,
    #[serde(default)]
    pub polarity: Option<BeliefPolarity>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
    /// Added to (or replacing, by evidence id) the carried-forward links.
    #[serde(default)]
    pub add_evidence: Vec<BeliefEvidence>,
    #[serde(default)]
    pub valid_time: Option<TimeInterval>,
    /// Explicit status. Without it, refuting evidence yields `disputed`.
    #[serde(default)]
    pub status: Option<BeliefStatus>,
    #[serde(default)]
    pub revalidation_interval_days: Option<u32>,
    pub reason: String,
    #[serde(default)]
    pub expected_revision: Option<BeliefId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDependency {
    pub from: BeliefId,
    pub to: BeliefId,
    pub relation: DependencyRelation,
    #[serde(default)]
    pub strength: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceView {
    pub link: BeliefEvidence,
    pub evidence: Option<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefView {
    pub belief: Belief,
    pub current_revision_id: Option<BeliefId>,
    pub evidence: Vec<EvidenceView>,
    /// What this revision relies on.
    pub depends_on: Vec<BeliefDependency>,
    /// What relies on this revision.
    pub dependents: Vec<BeliefDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefRevisionOutcome {
    pub belief: Belief,
    /// Live beliefs (transitively) relying on the previous revision that must
    /// be re-evaluated. They are flagged, never automatically retracted.
    pub needs_reevaluation: Vec<BeliefId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallRequest {
    pub query: String,
    #[serde(default)]
    pub policy: RecallPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecalledMemory {
    pub rank: u32,
    pub score: f64,
    pub memory: MemoryRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallSummary {
    pub policy_version: u32,
    pub scoring_version: u32,
    pub candidates_considered: usize,
    pub selected_count: usize,
    pub data_snapshot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResponse {
    pub trace_id: RecallTraceId,
    pub results: Vec<RecalledMemory>,
    pub trace: RecallSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplayRequest {
    /// Counterfactual replay: substitute policy parameters. The original
    /// as-of times are kept unless the override sets them.
    #[serde(default)]
    pub policy_override: Option<RecallPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayReport {
    pub trace_id: RecallTraceId,
    pub counterfactual: bool,
    /// The replay selected exactly the same revisions in the same order.
    pub identical: bool,
    pub original_selected: Vec<(MemoryId, MemoryRevisionId)>,
    pub replayed_selected: Vec<(MemoryId, MemoryRevisionId)>,
    pub added: Vec<MemoryId>,
    pub removed: Vec<MemoryId>,
    pub replay: RecallTrace,
}
