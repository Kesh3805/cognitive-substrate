//! Storage contracts.
//!
//! Every mutating method takes the cognitive event that describes it; the
//! adapter must persist the mutation and the event atomically. Adapters
//! assign event sequence numbers and must reject attempts to supersede a
//! revision that is no longer current (optimistic concurrency).

use async_trait::async_trait;
use cs_core::{
    AccessContext, BeliefId, CognitiveEvent, EventDraft, EventQuery, EvidenceId, MemoryId,
    MemoryRevisionId, RecallTraceId, Result, ScopeFilter, TenantId,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, EmbeddingRecord, Evidence, MemoryDomain,
    MemoryRecord, MemoryStatus, RecallTrace,
};
use time::OffsetDateTime;
use uuid::Uuid;

#[cfg(feature = "in-memory")]
pub mod in_memory;

#[cfg(feature = "in-memory")]
pub use in_memory::InMemoryStore;

/// Candidate retrieval filter. Authorization (`access`) is applied by the
/// adapter *inside* the query, before any record leaves storage.
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    pub access: AccessContext,
    pub domains: Vec<MemoryDomain>,
    pub statuses: Vec<MemoryStatus>,
    pub scope_filter: Option<ScopeFilter>,
    pub as_of_valid_time: OffsetDateTime,
    pub as_of_transaction_time: OffsetDateTime,
    /// Include revisions already closed at `as_of_transaction_time`.
    pub include_superseded: bool,
    /// Embedding model whose vectors should be returned.
    pub embedding_model: Option<(String, String)>,
    pub limit: u32,
}

/// Deterministic order: transaction start DESC, then revision id ASC.
#[derive(Debug, Clone)]
pub struct StoredMemory {
    pub record: MemoryRecord,
    pub embedding: Option<Vec<f32>>,
}

/// Idempotency registration accompanying a create.
#[derive(Debug, Clone, Copy)]
pub struct IdempotencyKey<'a> {
    pub tenant_id: TenantId,
    pub kind: &'static str,
    pub key: &'a str,
}

#[async_trait]
pub trait CognitiveStore: Send + Sync + std::fmt::Debug + 'static {
    async fn ping(&self) -> Result<()>;

    // ---- idempotency -------------------------------------------------------

    async fn idempotent_lookup(
        &self,
        tenant_id: TenantId,
        kind: &'static str,
        key: &str,
    ) -> Result<Option<Uuid>>;

    // ---- memories ----------------------------------------------------------

    async fn insert_memory(
        &self,
        record: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    /// Atomically close `current` at `closed_at` and insert `next`.
    /// Fails with `Conflict` if `current` is not the open revision.
    async fn supersede_memory(
        &self,
        current: MemoryRevisionId,
        closed_at: OffsetDateTime,
        next: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    async fn current_memory(&self, id: MemoryId) -> Result<Option<MemoryRecord>>;

    /// All revisions ordered by transaction start ascending.
    async fn memory_history(&self, id: MemoryId) -> Result<Vec<MemoryRecord>>;

    async fn query_memories(&self, query: &MemoryQuery) -> Result<Vec<StoredMemory>>;

    /// Governance hard delete of every revision and derived embedding.
    async fn purge_memory(&self, id: MemoryId, event: EventDraft) -> Result<CognitiveEvent>;

    // ---- evidence & beliefs ------------------------------------------------

    async fn insert_evidence(
        &self,
        evidence: &Evidence,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    async fn get_evidence(&self, ids: &[EvidenceId]) -> Result<Vec<Evidence>>;

    async fn insert_belief(
        &self,
        belief: &Belief,
        evidence: &[BeliefEvidence],
        dependencies: &[BeliefDependency],
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    /// Atomically close `current` and insert `next` with its links.
    async fn supersede_belief(
        &self,
        current: BeliefId,
        closed_at: OffsetDateTime,
        next: &Belief,
        evidence: &[BeliefEvidence],
        dependencies: &[BeliefDependency],
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    async fn get_belief(&self, id: BeliefId) -> Result<Option<Belief>>;

    async fn current_belief(&self, lineage_id: BeliefId) -> Result<Option<Belief>>;

    async fn belief_lineage(&self, lineage_id: BeliefId) -> Result<Vec<Belief>>;

    async fn belief_evidence(&self, id: BeliefId) -> Result<Vec<BeliefEvidence>>;

    async fn insert_dependency(
        &self,
        dependency: &BeliefDependency,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    /// Edges where `to == id` (what `id` relies on).
    async fn dependencies_of(&self, id: BeliefId) -> Result<Vec<BeliefDependency>>;

    /// Edges where `from == id` (what relies on `id`).
    async fn dependents_of(&self, id: BeliefId) -> Result<Vec<BeliefDependency>>;

    /// All belief revisions (current and historical) readable by `access`.
    async fn beliefs_for(&self, access: &AccessContext) -> Result<Vec<Belief>>;

    async fn evidence_links(&self, ids: &[BeliefId]) -> Result<Vec<(BeliefId, BeliefEvidence)>>;

    async fn dependencies_among(&self, ids: &[BeliefId]) -> Result<Vec<BeliefDependency>>;

    // ---- events ------------------------------------------------------------

    async fn append_event(&self, event: EventDraft) -> Result<CognitiveEvent>;

    /// Events readable by `access`, ordered by event id (time-ordered v7).
    async fn list_events(
        &self,
        access: &AccessContext,
        query: &EventQuery,
    ) -> Result<Vec<CognitiveEvent>>;

    // ---- recall traces -----------------------------------------------------

    async fn insert_trace(&self, trace: &RecallTrace, event: EventDraft) -> Result<CognitiveEvent>;

    async fn get_trace(&self, id: RecallTraceId) -> Result<Option<RecallTrace>>;
}
