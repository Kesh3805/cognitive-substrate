//! Storage contracts.
//!
//! Every mutating method takes the cognitive event that describes it; the
//! adapter must persist the mutation and the event atomically. Adapters
//! assign event sequence numbers and must reject attempts to supersede a
//! revision that is no longer current (optimistic concurrency).

use async_trait::async_trait;
use cs_core::{
    AccessContext, BeliefId, CognitiveEvent, EntityId, EventDraft, EventQuery, EvidenceId, JobId,
    MemoryId, MemoryRevisionId, ObservationId, ProposalId, RecallTraceId, Result, ScopeFilter,
    TenantId,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefStatus, ClaimedJob, EmbeddingRecord, Evidence,
    ExtractionRun, Job, JobKind, JobOutcome, JobStatus, Lease, MemoryDomain, MemoryRecord,
    MemoryStatus, Observation, ProposalDecision, ProposalStatus, RecallTrace, RecordVersion,
    ResolutionProposal,
    entity::{Entity, EntityAlias, EntityKind, EntityMention, KnowledgeEdge, Projection},
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
    /// Skip the valid-time filter (resolution compares across valid time).
    pub ignore_valid_time: bool,
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

    /// `follow_up` jobs (e.g. an embedding backfill) commit atomically with
    /// the write, so a stored record can never be left without its work.
    async fn insert_memory(
        &self,
        record: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        follow_up: &[Job],
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
        follow_up: &[Job],
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
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        follow_up: &[Job],
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
        embedding: Option<&EmbeddingRecord>,
        follow_up: &[Job],
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

    // ---- beliefs for recall (M3) ---------------------------------------------

    /// Belief revisions visible at the as-of times, filtered by status and
    /// authorization inside the query. Ordered by transaction start DESC,
    /// then id ASC.
    async fn query_beliefs(&self, query: &BeliefQuery) -> Result<Vec<StoredBelief>>;

    /// Job counts per (kind, status) with the oldest `available_at` of each
    /// group, for queue observability.
    async fn job_counts(&self, tenant_id: TenantId) -> Result<Vec<JobCount>>;

    /// How many current, embeddable revisions have a vector in a space.
    /// `tenant = None` counts across all tenants (operator gate; never
    /// exposed to API callers).
    async fn embedding_coverage(
        &self,
        tenant: Option<TenantId>,
        model_name: &str,
        model_version: &str,
    ) -> Result<EmbeddingCoverage>;

    // ---- jobs (M3) -----------------------------------------------------------

    /// Insert a job, or return the *active* (queued or leased) job with the
    /// same `(tenant, kind, dedupe_key)`. Terminal jobs never block a new
    /// submission; work-level idempotency lives in domain constraints.
    async fn enqueue_job(&self, job: &Job) -> Result<Job>;

    /// Lease up to `limit` claimable jobs of the given kinds. Each claim
    /// increments the job's lease generation and attempt count.
    async fn claim_jobs(&self, claim: &JobClaim) -> Result<Vec<ClaimedJob>>;

    /// Extend a lease. Fails with `Conflict` if the lease is no longer held.
    async fn heartbeat_job(&self, lease: Lease, until: OffsetDateTime) -> Result<()>;

    /// Record an attempt's outcome *and* its effects atomically, fenced by
    /// the lease generation. A stale lease fails with `Conflict` and nothing
    /// is written. If an effect precondition no longer holds (read skew),
    /// the effects are discarded and the job succeeds with a `discarded`
    /// result. If applying effects violates a constraint, the effects are
    /// rolled back and the job is dead-lettered.
    async fn finish_job(
        &self,
        lease: Lease,
        now: OffsetDateTime,
        outcome: JobOutcome,
        effects: JobEffects,
    ) -> Result<Job>;

    /// Return a leased job to the queue without consuming an attempt
    /// (graceful shutdown).
    async fn release_job(&self, lease: Lease, now: OffsetDateTime) -> Result<()>;

    async fn get_job(&self, id: JobId) -> Result<Option<Job>>;

    async fn list_jobs(
        &self,
        tenant_id: TenantId,
        status: Option<JobStatus>,
        limit: u32,
    ) -> Result<Vec<Job>>;

    // ---- observations (M3) ---------------------------------------------------

    /// Persist an observation and enqueue its perception job atomically.
    async fn insert_observation(
        &self,
        observation: &Observation,
        job: &Job,
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    async fn get_observation(&self, id: ObservationId) -> Result<Option<Observation>>;

    async fn extraction_runs(&self, id: ObservationId) -> Result<Vec<ExtractionRun>>;

    /// Erase an observation's content, delete the memories derived only
    /// from it (all revisions and embeddings), re-home corroborated ones to
    /// a surviving source, record the purge, append `event`.
    async fn purge_observation(
        &self,
        id: ObservationId,
        now: OffsetDateTime,
        delete: &[MemoryId],
        rehome: Vec<MemorySupersede>,
        event: EventDraft,
    ) -> Result<CognitiveEvent>;

    // ---- proposals (M3) ------------------------------------------------------

    async fn get_proposal(&self, id: ProposalId) -> Result<Option<ResolutionProposal>>;

    /// Record a proposal made outside a job (e.g. an explicit request).
    /// Deduplicated by fingerprint: if one exists in any status, it is
    /// returned unchanged and `event` is not appended.
    async fn insert_proposal(
        &self,
        proposal: &ResolutionProposal,
        event: EventDraft,
    ) -> Result<ResolutionProposal>;

    async fn list_proposals(
        &self,
        access: &AccessContext,
        status: Option<ProposalStatus>,
        limit: u32,
    ) -> Result<Vec<ResolutionProposal>>;

    /// Decide a pending proposal and apply its mutations in one transaction.
    /// Fails with `Conflict` if the proposal is no longer pending or any
    /// supersede finds its revision no longer current.
    async fn decide_proposal(
        &self,
        id: ProposalId,
        decision: &ProposalDecision,
        mutations: ProposalMutations,
        events: Vec<EventDraft>,
    ) -> Result<ResolutionProposal>;

    // ---- governance (M3) -----------------------------------------------------

    /// When (if ever) a record was purged.
    async fn purged_at(&self, kind: PurgeKind, id: Uuid) -> Result<Option<OffsetDateTime>>;

    // ---- entities & graph (M4) ---------------------------------------------

    async fn get_entity(&self, id: EntityId) -> Result<Option<Entity>>;

    /// Entities whose alias key matches, across the partitions the caller
    /// can read. Ordered by id.
    async fn entities_by_key(&self, access: &AccessContext, key: &str) -> Result<Vec<Entity>>;

    async fn entity_aliases(&self, id: EntityId) -> Result<Vec<EntityAlias>>;

    async fn entity_mentions(&self, id: EntityId, limit: u32) -> Result<Vec<EntityMention>>;

    /// Add an alias to an active entity. Fails with `Conflict` if the
    /// entity's version moved or the key already names another entity in
    /// the partition.
    async fn add_entity_alias(
        &self,
        id: EntityId,
        expected_version: u32,
        alias: &EntityAlias,
        event: EventDraft,
    ) -> Result<Entity>;

    async fn set_entity_kind(
        &self,
        id: EntityId,
        expected_version: u32,
        kind: EntityKind,
        at: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Entity>;

    /// Entities in one partition (for merge detection), ordered by id.
    async fn partition_entities(&self, partition: &str, limit: u32) -> Result<Vec<Entity>>;

    /// Entities merged directly into any of `ids` (at any time), ordered
    /// by id. Readers filter by merge time to resolve as of a point.
    async fn merged_into(&self, ids: &[EntityId]) -> Result<Vec<Entity>>;

    /// Edges touching `entity_ids`, visible to the query: the caller can
    /// read the edge's scope, its source revision is current at the as-of
    /// transaction time with an allowed status, and its valid time holds.
    /// Ordered by (relation, other entity, edge id).
    async fn entity_edges(&self, query: &EdgeQuery) -> Result<Vec<KnowledgeEdge>>;

    /// Erase evidence content and its belief links; record the purge.
    /// Returns the belief revisions that lost a link.
    async fn purge_evidence(
        &self,
        id: EvidenceId,
        now: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Vec<BeliefId>>;
}

/// Belief recall filter (see [`MemoryQuery`]).
#[derive(Debug, Clone)]
pub struct BeliefQuery {
    pub access: AccessContext,
    pub statuses: Vec<BeliefStatus>,
    pub scope_filter: Option<ScopeFilter>,
    pub as_of_valid_time: OffsetDateTime,
    pub as_of_transaction_time: OffsetDateTime,
    pub include_superseded: bool,
    pub ignore_valid_time: bool,
    /// Embedding space whose vectors should be returned.
    pub embedding_model: Option<(String, String)>,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct StoredBelief {
    pub belief: Belief,
    pub embedding: Option<Vec<f32>>,
}

/// Current revisions vs. those embedded in one space. Memories exclude
/// tombstoned revisions; beliefs exclude retracted ones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct EmbeddingCoverage {
    pub memories_total: u64,
    pub memories_embedded: u64,
    pub beliefs_total: u64,
    pub beliefs_embedded: u64,
}

impl EmbeddingCoverage {
    /// Fraction covered; 1.0 when there is nothing to embed.
    pub fn ratio(&self) -> f64 {
        let total = self.memories_total + self.beliefs_total;
        if total == 0 {
            1.0
        } else {
            (self.memories_embedded + self.beliefs_embedded) as f64 / total as f64
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCount {
    pub kind: JobKind,
    pub status: JobStatus,
    pub count: u64,
    pub oldest_available_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct JobClaim {
    pub kinds: Vec<JobKind>,
    pub owner: String,
    pub now: OffsetDateTime,
    pub lease_until: OffsetDateTime,
    pub limit: u32,
}

/// Inserts a successful attempt commits together with its job transition.
#[derive(Debug, Clone, Default)]
pub struct JobEffects {
    pub memories: Vec<(MemoryRecord, Option<EmbeddingRecord>)>,
    pub extraction_run: Option<ExtractionRun>,
    /// Proposals whose fingerprint already exists (any status) are skipped.
    pub proposals: Vec<ResolutionProposal>,
    pub events: Vec<EventDraft>,
    /// Follow-up jobs, enqueued with the usual active-only dedupe.
    pub jobs: Vec<Job>,
    /// Backfilled vectors, keyed by the exact revision they embed. Upserted
    /// idempotently; skipped if the revision no longer exists (purged).
    pub memory_embeddings: Vec<(MemoryRevisionId, EmbeddingRecord)>,
    /// An entity projection of one revision: names are resolved to entities
    /// inside the commit (creating them with deterministic ids if new), and
    /// mentions and edges are written idempotently.
    pub projection: Option<Projection>,
    pub belief_embeddings: Vec<(BeliefId, EmbeddingRecord)>,
    /// Read-skew guards checked inside the commit: the observation must
    /// still exist unpurged, and these revisions must still be current.
    pub require_unpurged_observation: Option<ObservationId>,
    pub require_current: Vec<RecordVersion>,
}

impl JobEffects {
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty()
            && self.extraction_run.is_none()
            && self.proposals.is_empty()
            && self.events.is_empty()
            && self.jobs.is_empty()
            && self.memory_embeddings.is_empty()
            && self.projection.is_none()
            && self.belief_embeddings.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct MemorySupersede {
    pub current: MemoryRevisionId,
    pub closed_at: OffsetDateTime,
    pub next: MemoryRecord,
    pub embedding: Option<EmbeddingRecord>,
    pub follow_up: Vec<Job>,
}

#[derive(Debug, Clone)]
pub struct BeliefSupersede {
    pub current: BeliefId,
    pub closed_at: OffsetDateTime,
    pub next: Belief,
    pub evidence: Vec<BeliefEvidence>,
    pub dependencies: Vec<BeliefDependency>,
    pub embedding: Option<EmbeddingRecord>,
    pub follow_up: Vec<Job>,
}

#[derive(Debug, Clone, Default)]
pub struct ProposalMutations {
    pub memories: Vec<MemorySupersede>,
    pub beliefs: Vec<BeliefSupersede>,
    pub entity_merges: Vec<EntityMerge>,
}

/// Merge `merged` into `survivor` (same partition, both active, versions
/// unchanged): aliases, mentions and edges move to the survivor and the
/// merged entity redirects.
#[derive(Debug, Clone, Copy)]
pub struct EntityMerge {
    pub survivor: EntityId,
    pub survivor_version: u32,
    pub merged: EntityId,
    pub merged_version: u32,
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeDirection {
    #[default]
    Out,
    In,
    Both,
}

#[derive(Debug, Clone)]
pub struct EdgeQuery {
    pub access: AccessContext,
    pub entity_ids: Vec<EntityId>,
    pub direction: EdgeDirection,
    pub relations: Option<Vec<String>>,
    pub as_of_transaction_time: OffsetDateTime,
    pub as_of_valid_time: OffsetDateTime,
    pub include_candidates: bool,
    pub limit: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PurgeKind {
    Memory,
    Evidence,
    Observation,
}

impl PurgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Evidence => "evidence",
            Self::Observation => "observation",
        }
    }
}
