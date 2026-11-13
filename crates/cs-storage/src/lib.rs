//! Storage contracts.
//!
//! Every mutating method takes the cognitive event that describes it; the
//! adapter must persist the mutation and the event atomically. Adapters
//! assign event sequence numbers and must reject attempts to supersede a
//! revision that is no longer current (optimistic concurrency).

use async_trait::async_trait;
use cs_core::{
    AccessContext, BeliefId, CognitiveEvent, EventDraft, EventQuery, EvidenceId, JobId, MemoryId,
    MemoryRevisionId, ObservationId, ProposalId, RecallTraceId, Result, ScopeFilter, TenantId,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefStatus, ClaimedJob, EmbeddingRecord, Evidence,
    ExtractionRun, Job, JobKind, JobOutcome, JobStatus, Lease, MemoryDomain, MemoryRecord,
    MemoryStatus, Observation, ProposalDecision, ProposalStatus, RecallTrace, RecordVersion,
    ResolutionProposal,
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

    // ---- beliefs for recall (M3) ---------------------------------------------

    /// Belief revisions visible at the as-of times, filtered by status and
    /// authorization inside the query. Ordered by transaction start DESC,
    /// then id ASC.
    async fn query_beliefs(&self, query: &BeliefQuery) -> Result<Vec<Belief>>;

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
    pub limit: u32,
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
    }
}

#[derive(Debug, Clone)]
pub struct MemorySupersede {
    pub current: MemoryRevisionId,
    pub closed_at: OffsetDateTime,
    pub next: MemoryRecord,
    pub embedding: Option<EmbeddingRecord>,
}

#[derive(Debug, Clone)]
pub struct BeliefSupersede {
    pub current: BeliefId,
    pub closed_at: OffsetDateTime,
    pub next: Belief,
    pub evidence: Vec<BeliefEvidence>,
    pub dependencies: Vec<BeliefDependency>,
}

#[derive(Debug, Clone, Default)]
pub struct ProposalMutations {
    pub memories: Vec<MemorySupersede>,
    pub beliefs: Vec<BeliefSupersede>,
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
