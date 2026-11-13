//! The embeddable Cognitive Substrate engine.
//!
//! [`CognitiveEngine`] is the single place where cognitive operations are
//! implemented. The HTTP service is a thin adapter over it; an application can
//! embed it directly with any [`CognitiveStore`] implementation.
//!
//! Every authoritative mutation:
//! 1. is authorized against the caller's [`AccessContext`] first,
//! 2. preserves history (revisions close transaction time, never overwrite),
//! 3. is persisted atomically with a cognitive event.

pub mod embedding;
pub mod requests;

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

use cs_core::{
    AccessContext, ActorRef, BeliefId, Bitemporal, Clock, CognitiveEvent, CognitiveEventType,
    Error, EventDraft, EventId, EventQuery, EventStreamId, EvidenceId, MemoryId, MemoryRevisionId,
    Provenance, RecallTraceId, Result, Scope, SystemClock, TimeInterval, temporal::truncate_micros,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefStatus, EmbeddingRecord, Evidence,
    EvidenceDirection, MemoryDomain, MemoryRecord, MemoryStatus, RecallCandidate, RecallPolicy,
    RecallTrace, recall::SCORING_VERSION,
};
pub use cs_policy::{CognitiveDebtReport, DebtConfig};
use cs_policy::{DebtInputs, ScoringInput};
use cs_storage::{CognitiveStore, IdempotencyKey, MemoryQuery};
pub use embedding::{Embedder, HashingEmbedder};
pub use requests::*;
use serde_json::json;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const MAX_QUERY_CHARS: usize = 8_192;
const MAX_IDEMPOTENCY_KEY: usize = 255;

#[derive(Debug, Clone)]
pub struct CognitiveEngine {
    store: Arc<dyn CognitiveStore>,
    embedder: Option<Arc<dyn Embedder>>,
    clock: Arc<dyn Clock>,
    debt_config: DebtConfig,
}

/// Builder for [`CognitiveEngine`].
#[derive(Debug)]
pub struct EngineBuilder {
    store: Arc<dyn CognitiveStore>,
    embedder: Option<Arc<dyn Embedder>>,
    clock: Arc<dyn Clock>,
    debt_config: DebtConfig,
}

impl EngineBuilder {
    pub fn embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    pub fn without_embedder(mut self) -> Self {
        self.embedder = None;
        self
    }

    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn debt_config(mut self, config: DebtConfig) -> Self {
        self.debt_config = config;
        self
    }

    pub fn build(self) -> CognitiveEngine {
        CognitiveEngine {
            store: self.store,
            embedder: self.embedder,
            clock: self.clock,
            debt_config: self.debt_config,
        }
    }
}

fn actor(ctx: &AccessContext) -> ActorRef {
    match (ctx.agent_id, ctx.user_id) {
        (Some(agent_id), _) => ActorRef::Agent { agent_id },
        (None, Some(user_id)) => ActorRef::User { user_id },
        (None, None) => ActorRef::System {
            component: "anonymous".into(),
        },
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

fn check_idempotency_key(key: Option<&String>) -> Result<()> {
    match key {
        Some(k) if k.is_empty() || k.len() > MAX_IDEMPOTENCY_KEY => {
            Err(Error::validation("idempotency_key must be 1..=255 bytes"))
        }
        _ => Ok(()),
    }
}

impl CognitiveEngine {
    /// Engine with the deterministic hashing embedder and the system clock.
    pub fn builder(store: Arc<dyn CognitiveStore>) -> EngineBuilder {
        EngineBuilder {
            store,
            embedder: Some(Arc::new(HashingEmbedder::default())),
            clock: Arc::new(SystemClock),
            debt_config: DebtConfig::default(),
        }
    }

    pub fn new(store: Arc<dyn CognitiveStore>) -> Self {
        Self::builder(store).build()
    }

    pub fn store(&self) -> &Arc<dyn CognitiveStore> {
        &self.store
    }

    pub async fn ready(&self) -> Result<()> {
        self.store.ping().await
    }

    fn now(&self) -> OffsetDateTime {
        truncate_micros(self.clock.now())
    }

    /// Transaction time for a new revision: strictly after the current one.
    fn next_tx(&self, current_start: OffsetDateTime) -> OffsetDateTime {
        let now = self.now();
        if now > current_start {
            now
        } else {
            current_start + Duration::microseconds(1)
        }
    }

    fn draft(
        &self,
        ctx: &AccessContext,
        stream: Uuid,
        scope: &Scope,
        event_type: CognitiveEventType,
        payload: serde_json::Value,
        at: OffsetDateTime,
    ) -> EventDraft {
        EventDraft {
            id: EventId::new_v7(),
            stream_id: EventStreamId(stream),
            scope: scope.clone(),
            actor: actor(ctx),
            event_type,
            payload,
            occurred_at: at,
            causation_id: None,
            correlation_id: None,
        }
    }

    fn embed(&self, record: &MemoryRecord) -> Option<EmbeddingRecord> {
        if record.status == MemoryStatus::Tombstoned {
            return None;
        }
        self.embedder
            .as_ref()
            .map(|e| e.record(&record.content.index_text()))
    }

    fn provenance(
        &self,
        ctx: &AccessContext,
        input: ProvenanceInput,
        now: OffsetDateTime,
    ) -> Provenance {
        Provenance {
            source: input.source,
            actor: actor(ctx),
            observed_at: input.observed_at.map_or(now, truncate_micros),
            recorded_at: now,
            extraction: input.extraction,
            parent_evidence: input.parent_evidence,
        }
    }

    // ======================================================================
    // Memories
    // ======================================================================

    #[tracing::instrument(skip_all, fields(domain = ?req.content.domain()))]
    pub async fn create_memory(&self, ctx: &AccessContext, req: NewMemory) -> Result<MemoryRecord> {
        ctx.ensure_write(&req.scope)?;
        check_idempotency_key(req.idempotency_key.as_ref())?;
        if let Some(key) = &req.idempotency_key
            && let Some(existing) = self
                .store
                .idempotent_lookup(ctx.tenant_id, "memory", key)
                .await?
        {
            return self.get_memory(ctx, MemoryId(existing)).await;
        }

        let now = self.now();
        let provenance = self.provenance(ctx, req.provenance, now);
        let status = if provenance.is_model_derived() {
            MemoryStatus::Candidate
        } else {
            MemoryStatus::Active
        };
        let record = MemoryRecord {
            id: MemoryId::new_v7(),
            revision_id: MemoryRevisionId::new_v7(),
            revision_number: 1,
            scope: req.scope,
            content: req.content,
            provenance,
            temporal: Bitemporal {
                valid_time: req
                    .valid_time
                    .map_or(TimeInterval::from(now), TimeInterval::normalized),
                transaction_time: TimeInterval::from(now),
            },
            status,
            status_reason: None,
            supersedes: None,
            superseded_by: None,
            created_at: now,
            expires_at: req.expires_at.map(truncate_micros),
        };
        record.validate()?;

        let event = self.draft(
            ctx,
            record.id.0,
            &record.scope,
            CognitiveEventType::MemoryCreated,
            json!({
                "memory_id": record.id,
                "revision_id": record.revision_id,
                "domain": record.domain(),
                "status": record.status,
            }),
            now,
        );
        let key = req.idempotency_key.as_deref().map(|key| IdempotencyKey {
            tenant_id: ctx.tenant_id,
            kind: "memory",
            key,
        });
        match self
            .store
            .insert_memory(&record, self.embed(&record).as_ref(), key, event)
            .await
        {
            Ok(_) => Ok(record),
            Err(Error::Conflict(_)) if key.is_some() => {
                // Lost an idempotency race: return the winner.
                let k = key.map(|k| k.key).unwrap_or_default();
                match self
                    .store
                    .idempotent_lookup(ctx.tenant_id, "memory", k)
                    .await?
                {
                    Some(id) => self.get_memory(ctx, MemoryId(id)).await,
                    None => Err(Error::conflict("concurrent create")),
                }
            }
            Err(e) => Err(e),
        }
    }

    pub async fn get_memory(&self, ctx: &AccessContext, id: MemoryId) -> Result<MemoryRecord> {
        let record = self
            .store
            .current_memory(id)
            .await?
            .ok_or_else(|| Error::not_found("memory", id))?;
        // Unreadable records are indistinguishable from missing ones.
        if !ctx.can_read(&record.scope) {
            return Err(Error::not_found("memory", id));
        }
        Ok(record)
    }

    pub async fn memory_history(&self, ctx: &AccessContext, id: MemoryId) -> Result<MemoryHistory> {
        let revisions = self.store.memory_history(id).await?;
        match revisions.last() {
            Some(last) if ctx.can_read(&last.scope) => Ok(MemoryHistory {
                memory_id: id,
                revisions,
            }),
            _ => Err(Error::not_found("memory", id)),
        }
    }

    async fn transition<F>(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        event_type: CognitiveEventType,
        reason: &str,
        expected: Option<MemoryRevisionId>,
        build: F,
    ) -> Result<MemoryRecord>
    where
        F: FnOnce(&MemoryRecord, OffsetDateTime) -> Result<MemoryRecord>,
    {
        if reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        let current = self.get_memory(ctx, id).await?;
        ctx.ensure_write(&current.scope)?;
        if expected.is_some_and(|e| e != current.revision_id) {
            return Err(Error::conflict(
                "expected_revision is not the current revision",
            ));
        }
        if current.status == MemoryStatus::Tombstoned {
            return Err(Error::conflict("memory is tombstoned"));
        }
        let at = self.next_tx(current.temporal.transaction_time.start);
        let mut next = build(&current, at)?;
        next.revision_id = MemoryRevisionId::new_v7();
        next.revision_number = current.revision_number + 1;
        next.supersedes = Some(current.revision_id);
        next.superseded_by = None;
        next.status_reason = Some(reason.to_owned());
        next.temporal.transaction_time = TimeInterval::from(at);
        next.created_at = at;
        next.validate()?;

        let event = self.draft(
            ctx,
            id.0,
            &next.scope,
            event_type,
            json!({
                "memory_id": id,
                "revision_id": next.revision_id,
                "supersedes": current.revision_id,
                "status": next.status,
                "reason": reason,
            }),
            at,
        );
        self.store
            .supersede_memory(
                current.revision_id,
                at,
                &next,
                self.embed(&next).as_ref(),
                event,
            )
            .await?;
        Ok(next)
    }

    pub async fn revise_memory(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: ReviseMemory,
    ) -> Result<MemoryRecord> {
        let reason = req.reason.clone();
        self.transition(
            ctx,
            id,
            CognitiveEventType::MemoryRevised,
            &reason,
            req.expected_revision,
            |cur, at| {
                let mut next = cur.clone();
                if let Some(content) = req.content {
                    if content.domain() != cur.domain() {
                        return Err(Error::validation(
                            "a revision cannot change the memory domain",
                        ));
                    }
                    next.content = content;
                }
                if let Some(v) = req.valid_time {
                    next.temporal.valid_time = v.normalized();
                }
                if let Some(e) = req.expires_at {
                    next.expires_at = Some(truncate_micros(e));
                }
                if let Some(p) = req.provenance {
                    next.provenance = self.provenance(ctx, p, at);
                }
                next.status = if next.provenance.is_model_derived() {
                    MemoryStatus::Candidate
                } else {
                    MemoryStatus::Active
                };
                Ok(next)
            },
        )
        .await
    }

    /// Validate a candidate memory, making it trusted knowledge.
    pub async fn promote_memory(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: StatusChange,
    ) -> Result<MemoryRecord> {
        self.transition(
            ctx,
            id,
            CognitiveEventType::MemoryPromoted,
            &req.reason,
            None,
            |cur, _| {
                if cur.status != MemoryStatus::Candidate {
                    return Err(Error::conflict("only candidate memories can be promoted"));
                }
                Ok(MemoryRecord {
                    status: MemoryStatus::Active,
                    ..cur.clone()
                })
            },
        )
        .await
    }

    pub async fn reject_memory(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: StatusChange,
    ) -> Result<MemoryRecord> {
        self.transition(
            ctx,
            id,
            CognitiveEventType::MemoryRejected,
            &req.reason,
            None,
            |cur, _| {
                if cur.status != MemoryStatus::Candidate {
                    return Err(Error::conflict("only candidate memories can be rejected"));
                }
                Ok(MemoryRecord {
                    status: MemoryStatus::Rejected,
                    ..cur.clone()
                })
            },
        )
        .await
    }

    /// Logical deletion: excluded from recall, history preserved.
    pub async fn tombstone_memory(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: StatusChange,
    ) -> Result<MemoryRecord> {
        self.transition(
            ctx,
            id,
            CognitiveEventType::MemoryTombstoned,
            &req.reason,
            None,
            |cur, _| {
                Ok(MemoryRecord {
                    status: MemoryStatus::Tombstoned,
                    ..cur.clone()
                })
            },
        )
        .await
    }

    /// Governance hard delete: removes every revision and derived embedding.
    /// The event log keeps only identifiers.
    pub async fn purge_memory(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: StatusChange,
    ) -> Result<CognitiveEvent> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        let history = self.store.memory_history(id).await?;
        let Some(last) = history.last() else {
            return Err(Error::not_found("memory", id));
        };
        if !ctx.can_read(&last.scope) {
            return Err(Error::not_found("memory", id));
        }
        ctx.ensure_write(&last.scope)?;
        let event = self.draft(
            ctx,
            id.0,
            &last.scope,
            CognitiveEventType::MemoryPurged,
            json!({ "memory_id": id, "revisions_purged": history.len(), "reason": req.reason }),
            self.now(),
        );
        self.store.purge_memory(id, event).await
    }

    // ======================================================================
    // Recall
    // ======================================================================

    fn resolve_policy(&self, mut policy: RecallPolicy) -> Result<RecallPolicy> {
        policy.validate()?;
        let now = self.now();
        let tx = policy.as_of_transaction_time.map_or(now, truncate_micros);
        if tx > now {
            return Err(Error::validation(
                "as_of_transaction_time cannot be in the future",
            ));
        }
        policy.as_of_transaction_time = Some(tx);
        policy.as_of_valid_time = Some(policy.as_of_valid_time.map_or(tx, truncate_micros));
        let mut domains = policy.allowed_domains.clone();
        domains.sort();
        domains.dedup();
        policy.allowed_domains = domains;
        Ok(policy)
    }

    async fn execute_recall(
        &self,
        ctx: &AccessContext,
        query: &str,
        policy: &RecallPolicy,
    ) -> Result<(
        Vec<RecallCandidate>,
        BTreeMap<MemoryRevisionId, MemoryRecord>,
    )> {
        let as_of_tx = policy.as_of_transaction_time.unwrap_or_else(|| self.now());
        let mut statuses = vec![MemoryStatus::Active];
        if policy.include_candidates {
            statuses.push(MemoryStatus::Candidate);
        }
        let stored = self
            .store
            .query_memories(&MemoryQuery {
                access: ctx.clone(),
                domains: policy.allowed_domains.clone(),
                statuses,
                scope_filter: policy.scope_filter.clone(),
                as_of_valid_time: policy.as_of_valid_time.unwrap_or(as_of_tx),
                as_of_transaction_time: as_of_tx,
                include_superseded: policy.include_superseded,
                embedding_model: self
                    .embedder
                    .as_ref()
                    .map(|e| (e.model_name().to_owned(), e.model_version().to_owned())),
                limit: policy.max_candidates,
            })
            .await?;

        let texts: Vec<String> = stored
            .iter()
            .map(|s| s.record.content.index_text())
            .collect();
        let inputs: Vec<ScoringInput<'_>> = stored
            .iter()
            .zip(&texts)
            .map(|(s, text)| ScoringInput {
                memory_id: s.record.id,
                revision_id: s.record.revision_id,
                text,
                embedding: s.embedding.as_deref(),
                observed_at: s.record.provenance.observed_at,
            })
            .collect();
        let query_embedding = self.embedder.as_ref().map(|e| e.embed(query));
        let candidates = cs_policy::score_candidates(
            policy,
            query,
            query_embedding.as_deref(),
            as_of_tx,
            &inputs,
        );
        let records = stored
            .into_iter()
            .map(|s| (s.record.revision_id, s.record))
            .collect();
        Ok((candidates, records))
    }

    fn snapshot(&self, policy: &RecallPolicy) -> String {
        let embedder = self.embedder.as_ref().map_or_else(
            || "none".to_owned(),
            |e| format!("{}@{}", e.model_name(), e.model_version()),
        );
        format!(
            "tx={};valid={};scoring=v{};embedder={}",
            policy
                .as_of_transaction_time
                .map(rfc3339)
                .unwrap_or_default(),
            policy.as_of_valid_time.map(rfc3339).unwrap_or_default(),
            SCORING_VERSION,
            embedder
        )
    }

    fn build_trace(
        &self,
        ctx: &AccessContext,
        id: RecallTraceId,
        query: &str,
        policy: RecallPolicy,
        candidates: Vec<RecallCandidate>,
    ) -> RecallTrace {
        RecallTrace {
            id,
            tenant_id: ctx.tenant_id,
            data_snapshot: self.snapshot(&policy),
            query_text: policy.retain_query_text.then(|| query.to_owned()),
            policy,
            scoring_version: SCORING_VERSION,
            query_hash: cs_policy::query_hash(query),
            embedding_model: self.embedder.as_ref().map(|e| e.model_name().to_owned()),
            embedding_version: self.embedder.as_ref().map(|e| e.model_version().to_owned()),
            candidates,
            executed_at: self.now(),
        }
    }

    /// Policy-controlled, traceable, reproducible recall.
    #[tracing::instrument(skip_all)]
    pub async fn recall(&self, ctx: &AccessContext, req: RecallRequest) -> Result<RecallResponse> {
        let query = req.query.trim();
        if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
            return Err(Error::validation("query must be 1..=8192 characters"));
        }
        let policy = self.resolve_policy(req.policy)?;
        let (candidates, records) = self.execute_recall(ctx, query, &policy).await?;
        let trace = self.build_trace(ctx, RecallTraceId::new_v7(), query, policy, candidates);

        let results: Vec<RecalledMemory> = trace
            .selected()
            .filter_map(|c| {
                records.get(&c.revision_id).map(|m| RecalledMemory {
                    rank: c.rank,
                    score: c.score,
                    memory: m.clone(),
                })
            })
            .collect();
        let summary = RecallSummary {
            policy_version: trace.policy.version,
            scoring_version: trace.scoring_version,
            candidates_considered: trace.candidates.len(),
            selected_count: results.len(),
            data_snapshot: trace.data_snapshot.clone(),
        };
        let event = self.draft(
            ctx,
            trace.id.0,
            &Scope::tenant(ctx.tenant_id),
            CognitiveEventType::RecallExecuted,
            json!({
                "trace_id": trace.id,
                "query_hash": trace.query_hash,
                "candidates": summary.candidates_considered,
                "selected": summary.selected_count,
            }),
            trace.executed_at,
        );
        // Recall events are visible only to the caller's own principal.
        let mut event = event;
        event.scope = match (ctx.user_id, ctx.agent_id) {
            (Some(u), _) => Scope::private(ctx.tenant_id, u),
            (None, Some(a)) => Scope::tenant(ctx.tenant_id)
                .with_agent(a)
                .with_visibility(cs_core::Visibility::Agent),
            (None, None) => Scope::tenant(ctx.tenant_id),
        };
        self.store.insert_trace(&trace, event).await?;
        Ok(RecallResponse {
            trace_id: trace.id,
            results,
            trace: summary,
        })
    }

    pub async fn get_trace(&self, ctx: &AccessContext, id: RecallTraceId) -> Result<RecallTrace> {
        match self.store.get_trace(id).await? {
            Some(t) if t.tenant_id == ctx.tenant_id => Ok(t),
            _ => Err(Error::not_found("recall trace", id)),
        }
    }

    /// Re-execute a recorded recall against the same snapshot (exact replay)
    /// or with substituted policy parameters (counterfactual replay).
    pub async fn replay_recall(
        &self,
        ctx: &AccessContext,
        id: RecallTraceId,
        req: ReplayRequest,
    ) -> Result<ReplayReport> {
        let original = self.get_trace(ctx, id).await?;
        let query = original
            .query_text
            .clone()
            .ok_or_else(|| Error::validation("trace did not retain its query text"))?;
        let counterfactual = req.policy_override.is_some();
        let mut policy = req
            .policy_override
            .unwrap_or_else(|| original.policy.clone());
        if policy.as_of_transaction_time.is_none() {
            policy.as_of_transaction_time = original.policy.as_of_transaction_time;
        }
        if policy.as_of_valid_time.is_none() {
            policy.as_of_valid_time = original.policy.as_of_valid_time;
        }
        let policy = self.resolve_policy(policy)?;
        let (candidates, _) = self.execute_recall(ctx, &query, &policy).await?;
        let replay = self.build_trace(ctx, RecallTraceId::new_v7(), &query, policy, candidates);

        let pick = |t: &RecallTrace| -> Vec<(MemoryId, MemoryRevisionId)> {
            t.selected().map(|c| (c.memory_id, c.revision_id)).collect()
        };
        let original_selected = pick(&original);
        let replayed_selected = pick(&replay);
        let before: BTreeSet<MemoryId> = original_selected.iter().map(|(m, _)| *m).collect();
        let after: BTreeSet<MemoryId> = replayed_selected.iter().map(|(m, _)| *m).collect();
        Ok(ReplayReport {
            trace_id: id,
            counterfactual,
            identical: original_selected == replayed_selected,
            added: after.difference(&before).copied().collect(),
            removed: before.difference(&after).copied().collect(),
            original_selected,
            replayed_selected,
            replay,
        })
    }

    // ======================================================================
    // Evidence & beliefs
    // ======================================================================

    pub async fn record_evidence(&self, ctx: &AccessContext, req: NewEvidence) -> Result<Evidence> {
        let now = self.now();
        let evidence = Evidence {
            id: EvidenceId::new_v7(),
            tenant_id: ctx.tenant_id,
            source: req.source,
            actor: actor(ctx),
            observed_at: req.observed_at.map_or(now, truncate_micros),
            recorded_at: now,
            extraction: req.extraction,
            content_hash: req.content_hash,
            summary: req.summary,
        };
        evidence.validate()?;
        let event = self.draft(
            ctx,
            evidence.id.0,
            &Scope::tenant(ctx.tenant_id),
            CognitiveEventType::EvidenceRecorded,
            json!({ "evidence_id": evidence.id, "source_type": evidence.source.kind() }),
            now,
        );
        self.store.insert_evidence(&evidence, event).await?;
        Ok(evidence)
    }

    /// Resolve any revision id to the current revision of its lineage,
    /// enforcing read access.
    async fn current_of(&self, ctx: &AccessContext, id: BeliefId) -> Result<Belief> {
        let any = self
            .store
            .get_belief(id)
            .await?
            .ok_or_else(|| Error::not_found("belief", id))?;
        if !ctx.can_read(&any.scope) {
            return Err(Error::not_found("belief", id));
        }
        self.store
            .current_belief(any.lineage_id)
            .await?
            .ok_or_else(|| Error::not_found("belief", id))
    }

    fn validate_links(links: &[BeliefEvidence]) -> Result<()> {
        let mut seen = BTreeSet::new();
        for l in links {
            l.validate()?;
            if !seen.insert(l.evidence_id) {
                return Err(Error::validation("duplicate evidence link"));
            }
        }
        Ok(())
    }

    fn derived_status(links: &[BeliefEvidence]) -> BeliefStatus {
        if links
            .iter()
            .any(|l| l.direction == EvidenceDirection::Refutes)
        {
            BeliefStatus::Disputed
        } else {
            BeliefStatus::Active
        }
    }

    pub async fn create_belief(&self, ctx: &AccessContext, req: NewBelief) -> Result<Belief> {
        ctx.ensure_write(&req.scope)?;
        check_idempotency_key(req.idempotency_key.as_ref())?;
        if let Some(key) = &req.idempotency_key
            && let Some(existing) = self
                .store
                .idempotent_lookup(ctx.tenant_id, "belief", key)
                .await?
        {
            return self
                .get_belief(ctx, BeliefId(existing))
                .await
                .map(|v| v.belief);
        }
        Self::validate_links(&req.evidence)?;

        let now = self.now();
        let id = BeliefId::new_v7();
        let mut dependencies = Vec::new();
        for d in &req.depends_on {
            let source = self.current_of(ctx, d.belief_id).await?;
            if source.scope.tenant_id != req.scope.tenant_id {
                return Err(Error::forbidden("dependencies cannot cross tenants"));
            }
            dependencies.push(BeliefDependency {
                from: source.id,
                to: id,
                relation: d.relation,
                strength: d.strength,
                created_at: now,
            });
        }
        for d in &dependencies {
            d.validate()?;
        }

        let belief = Belief {
            id,
            lineage_id: id,
            scope: req.scope,
            proposition: req.proposition,
            polarity: req.polarity,
            confidence: req.confidence,
            revision_number: 1,
            supersedes: None,
            superseded_by: None,
            revision_reason: req.reason.unwrap_or_else(|| "initial assertion".to_owned()),
            status: Self::derived_status(&req.evidence),
            revalidation_interval_days: req.revalidation_interval_days,
            temporal: Bitemporal {
                valid_time: req
                    .valid_time
                    .map_or(TimeInterval::from(now), TimeInterval::normalized),
                transaction_time: TimeInterval::from(now),
            },
            created_at: now,
        };
        belief.validate()?;

        let event = self.draft(
            ctx,
            id.0,
            &belief.scope,
            CognitiveEventType::BeliefCreated,
            json!({
                "belief_id": id,
                "status": belief.status,
                "evidence": req.evidence.len(),
                "dependencies": dependencies.len(),
            }),
            now,
        );
        let key = req.idempotency_key.as_deref().map(|key| IdempotencyKey {
            tenant_id: ctx.tenant_id,
            kind: "belief",
            key,
        });
        self.store
            .insert_belief(&belief, &req.evidence, &dependencies, key, event)
            .await?;
        Ok(belief)
    }

    pub async fn get_belief(&self, ctx: &AccessContext, id: BeliefId) -> Result<BeliefView> {
        let belief = self
            .store
            .get_belief(id)
            .await?
            .ok_or_else(|| Error::not_found("belief", id))?;
        if !ctx.can_read(&belief.scope) {
            return Err(Error::not_found("belief", id));
        }
        let links = self.store.belief_evidence(id).await?;
        let ids: Vec<EvidenceId> = links.iter().map(|l| l.evidence_id).collect();
        let evidence: BTreeMap<EvidenceId, Evidence> = self
            .store
            .get_evidence(&ids)
            .await?
            .into_iter()
            .filter(|e| e.tenant_id == ctx.tenant_id)
            .map(|e| (e.id, e))
            .collect();
        let current = self.store.current_belief(belief.lineage_id).await?;
        Ok(BeliefView {
            current_revision_id: current.map(|c| c.id),
            evidence: links
                .into_iter()
                .map(|link| EvidenceView {
                    evidence: evidence.get(&link.evidence_id).cloned(),
                    link,
                })
                .collect(),
            depends_on: self.store.dependencies_of(id).await?,
            dependents: self.store.dependents_of(id).await?,
            belief,
        })
    }

    pub async fn belief_history(&self, ctx: &AccessContext, id: BeliefId) -> Result<Vec<Belief>> {
        let current = self.current_of(ctx, id).await?;
        self.store.belief_lineage(current.lineage_id).await
    }

    /// Live beliefs transitively relying on `root`.
    async fn reliant_closure(&self, root: BeliefId) -> Result<Vec<BeliefId>> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::from([root]);
        let mut visited = BTreeSet::from([root]);
        while let Some(id) = queue.pop_front() {
            for edge in self.store.dependents_of(id).await? {
                if !edge.relation.is_reliance() || !visited.insert(edge.to) {
                    continue;
                }
                if let Some(b) = self.store.get_belief(edge.to).await?
                    && b.is_current()
                    && b.status.is_live()
                {
                    out.insert(b.id);
                    queue.push_back(b.id);
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    pub async fn revise_belief(
        &self,
        ctx: &AccessContext,
        id: BeliefId,
        req: ReviseBelief,
    ) -> Result<BeliefRevisionOutcome> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        let current = self.current_of(ctx, id).await?;
        ctx.ensure_write(&current.scope)?;
        if req.expected_revision.is_some_and(|e| e != current.id) {
            return Err(Error::conflict(
                "expected_revision is not the current revision",
            ));
        }
        Self::validate_links(&req.add_evidence)?;

        // Carry evidence forward; new links replace old ones per evidence id.
        let mut links: BTreeMap<EvidenceId, BeliefEvidence> = self
            .store
            .belief_evidence(current.id)
            .await?
            .into_iter()
            .map(|l| (l.evidence_id, l))
            .collect();
        for l in req.add_evidence {
            links.insert(l.evidence_id, l);
        }
        let links: Vec<BeliefEvidence> = links.into_values().collect();

        let at = self.next_tx(current.temporal.transaction_time.start);
        let next_id = BeliefId::new_v7();
        let status = req.status.unwrap_or_else(|| Self::derived_status(&links));
        let next = Belief {
            id: next_id,
            lineage_id: current.lineage_id,
            scope: current.scope.clone(),
            proposition: req
                .proposition
                .unwrap_or_else(|| current.proposition.clone()),
            polarity: req.polarity.unwrap_or(current.polarity),
            confidence: req.confidence.unwrap_or_else(|| current.confidence.clone()),
            revision_number: current.revision_number + 1,
            supersedes: Some(current.id),
            superseded_by: None,
            revision_reason: req.reason.clone(),
            status,
            revalidation_interval_days: req
                .revalidation_interval_days
                .or(current.revalidation_interval_days),
            temporal: Bitemporal {
                valid_time: req
                    .valid_time
                    .map_or(current.temporal.valid_time, TimeInterval::normalized),
                transaction_time: TimeInterval::from(at),
            },
            created_at: at,
        };
        next.validate()?;

        // The new revision still relies on whatever the old one relied on.
        let inherited: Vec<BeliefDependency> = self
            .store
            .dependencies_of(current.id)
            .await?
            .into_iter()
            .map(|d| BeliefDependency {
                to: next_id,
                created_at: at,
                ..d
            })
            .collect();

        let event_type = match status {
            BeliefStatus::Retracted => CognitiveEventType::BeliefRetracted,
            BeliefStatus::Disputed if current.status != BeliefStatus::Disputed => {
                CognitiveEventType::BeliefDisputed
            }
            _ => CognitiveEventType::BeliefRevised,
        };
        let event = self.draft(
            ctx,
            current.lineage_id.0,
            &next.scope,
            event_type,
            json!({
                "belief_id": next_id,
                "supersedes": current.id,
                "status": status,
                "reason": req.reason,
            }),
            at,
        );
        self.store
            .supersede_belief(current.id, at, &next, &links, &inherited, event)
            .await?;

        let needs_reevaluation = self.reliant_closure(current.id).await?;
        if !needs_reevaluation.is_empty() {
            let event = self.draft(
                ctx,
                current.lineage_id.0,
                &next.scope,
                CognitiveEventType::BeliefReevaluationRequested,
                json!({ "changed": next_id, "dependents": needs_reevaluation }),
                at,
            );
            self.store.append_event(event).await?;
        }
        Ok(BeliefRevisionOutcome {
            belief: next,
            needs_reevaluation,
        })
    }

    pub async fn retract_belief(
        &self,
        ctx: &AccessContext,
        id: BeliefId,
        req: StatusChange,
    ) -> Result<BeliefRevisionOutcome> {
        self.revise_belief(
            ctx,
            id,
            ReviseBelief {
                status: Some(BeliefStatus::Retracted),
                reason: req.reason,
                ..ReviseBelief::default()
            },
        )
        .await
    }

    /// Add (or re-affirm after a revision) a dependency edge between the
    /// current revisions of two beliefs.
    pub async fn add_dependency(
        &self,
        ctx: &AccessContext,
        req: NewDependency,
    ) -> Result<BeliefDependency> {
        let from = self.current_of(ctx, req.from).await?;
        let to = self.current_of(ctx, req.to).await?;
        ctx.ensure_write(&to.scope)?;
        if from.scope.tenant_id != to.scope.tenant_id {
            return Err(Error::forbidden("dependencies cannot cross tenants"));
        }
        let now = self.now();
        let dep = BeliefDependency {
            from: from.id,
            to: to.id,
            relation: req.relation,
            strength: req.strength,
            created_at: now,
        };
        dep.validate()?;
        let event = self.draft(
            ctx,
            to.lineage_id.0,
            &to.scope,
            CognitiveEventType::DependencyCreated,
            json!({ "from": dep.from, "to": dep.to, "relation": dep.relation }),
            now,
        );
        self.store.insert_dependency(&dep, event).await?;
        Ok(dep)
    }

    // ======================================================================
    // Inspection
    // ======================================================================

    pub async fn events(
        &self,
        ctx: &AccessContext,
        query: EventQuery,
    ) -> Result<Vec<CognitiveEvent>> {
        if query.after_sequence.is_some() && query.stream_id.is_none() {
            return Err(Error::validation("after_sequence requires stream_id"));
        }
        let query = EventQuery {
            tenant_id: Some(ctx.tenant_id),
            ..query
        };
        self.store.list_events(ctx, &query).await
    }

    /// Evaluate the Cognitive Debt Index over everything the caller can read.
    pub async fn cognitive_debt(&self, ctx: &AccessContext) -> Result<CognitiveDebtReport> {
        let now = self.now();
        let beliefs = self.store.beliefs_for(ctx).await?;
        let ids: Vec<BeliefId> = beliefs.iter().map(|b| b.id).collect();
        let mut evidence: BTreeMap<BeliefId, Vec<BeliefEvidence>> = BTreeMap::new();
        for (id, link) in self.store.evidence_links(&ids).await? {
            evidence.entry(id).or_default().push(link);
        }
        let dependencies = self.store.dependencies_among(&ids).await?;
        let procedures: Vec<MemoryRecord> = self
            .store
            .query_memories(&MemoryQuery {
                access: ctx.clone(),
                domains: vec![MemoryDomain::Procedural],
                statuses: vec![MemoryStatus::Active],
                scope_filter: None,
                as_of_valid_time: now,
                as_of_transaction_time: now,
                include_superseded: false,
                embedding_model: None,
                limit: RecallPolicy::MAX_CANDIDATES,
            })
            .await?
            .into_iter()
            .map(|s| s.record)
            .collect();
        Ok(cs_policy::evaluate_debt(
            &self.debt_config,
            DebtInputs {
                now,
                beliefs: &beliefs,
                evidence: &evidence,
                dependencies: &dependencies,
                procedures: &procedures,
            },
        ))
    }
}
