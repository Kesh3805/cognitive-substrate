//! Reference implementation of [`CognitiveStore`] held in process memory.
//!
//! Used for embedding the engine without a database, for tests, and as the
//! executable definition of storage semantics that every other adapter must
//! match (see the conformance suite).

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Mutex, MutexGuard},
};

use async_trait::async_trait;
use cs_core::{
    AccessContext, BeliefId, CognitiveEvent, Error, EventDraft, EventQuery, EvidenceId, JobId,
    MemoryId, MemoryRevisionId, ObservationId, ProposalId, RecallTraceId, Result, TenantId,
    temporal::truncate_micros,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, ClaimedJob, EmbeddingRecord, Evidence,
    ExtractionRun, Job, JobOutcome, JobStatus, Lease, MemoryRecord, Observation,
    ProposalDecision, ProposalStatus, RecallTrace, RecordVersion, ResolutionProposal,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    BeliefQuery, BeliefSupersede, CognitiveStore, IdempotencyKey, JobClaim, JobEffects,
    MemoryQuery, MemorySupersede, ProposalMutations, PurgeKind, StoredMemory,
};

#[derive(Debug, Default)]
struct State {
    memories: Vec<MemoryRecord>,
    embeddings: HashMap<MemoryRevisionId, EmbeddingRecord>,
    evidence: BTreeMap<EvidenceId, Evidence>,
    beliefs: Vec<Belief>,
    belief_evidence: Vec<(BeliefId, BeliefEvidence)>,
    dependencies: Vec<BeliefDependency>,
    events: Vec<CognitiveEvent>,
    traces: HashMap<RecallTraceId, RecallTrace>,
    idempotency: HashMap<(TenantId, &'static str, String), Uuid>,
    jobs: BTreeMap<JobId, Job>,
    observations: BTreeMap<ObservationId, Observation>,
    extraction_runs: Vec<ExtractionRun>,
    proposals: BTreeMap<ProposalId, ResolutionProposal>,
    purges: HashMap<(&'static str, Uuid), OffsetDateTime>,
}

impl State {
    fn append(&mut self, draft: EventDraft) -> CognitiveEvent {
        let sequence = self
            .events
            .iter()
            .filter(|e| e.stream_id == draft.stream_id)
            .count() as u64
            + 1;
        let event = draft.into_event(sequence, truncate_micros(OffsetDateTime::now_utc()));
        self.events.push(event.clone());
        event
    }

    fn register(&mut self, key: Option<IdempotencyKey<'_>>, id: Uuid) -> Result<()> {
        if let Some(k) = key {
            let slot = (k.tenant_id, k.kind, k.key.to_owned());
            if self.idempotency.contains_key(&slot) {
                return Err(Error::conflict("idempotency key already used"));
            }
            self.idempotency.insert(slot, id);
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct InMemoryStore {
    state: Mutex<State>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl CognitiveStore for InMemoryStore {
    async fn ping(&self) -> Result<()> {
        Ok(())
    }

    async fn idempotent_lookup(
        &self,
        tenant_id: TenantId,
        kind: &'static str,
        key: &str,
    ) -> Result<Option<Uuid>> {
        Ok(self
            .lock()
            .idempotency
            .get(&(tenant_id, kind, key.to_owned()))
            .copied())
    }

    async fn insert_memory(
        &self,
        record: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        if s.memories.iter().any(|m| m.id == record.id) {
            return Err(Error::conflict("memory already exists"));
        }
        s.register(idempotency, record.id.0)?;
        s.memories.push(record.clone());
        if let Some(e) = embedding {
            s.embeddings.insert(record.revision_id, e.clone());
        }
        Ok(s.append(event))
    }

    async fn supersede_memory(
        &self,
        current: MemoryRevisionId,
        closed_at: OffsetDateTime,
        next: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        let Some(old) = s.memories.iter_mut().find(|m| m.revision_id == current) else {
            return Err(Error::not_found("memory revision", current));
        };
        if !old.is_current() {
            return Err(Error::conflict("revision is no longer current"));
        }
        if closed_at <= old.temporal.transaction_time.start {
            return Err(Error::conflict("transaction time must advance"));
        }
        old.temporal.transaction_time.end = Some(closed_at);
        old.superseded_by = Some(next.revision_id);
        s.memories.push(next.clone());
        if let Some(e) = embedding {
            s.embeddings.insert(next.revision_id, e.clone());
        }
        Ok(s.append(event))
    }

    async fn current_memory(&self, id: MemoryId) -> Result<Option<MemoryRecord>> {
        Ok(self
            .lock()
            .memories
            .iter()
            .find(|m| m.id == id && m.is_current())
            .cloned())
    }

    async fn memory_history(&self, id: MemoryId) -> Result<Vec<MemoryRecord>> {
        let mut h: Vec<_> = self
            .lock()
            .memories
            .iter()
            .filter(|m| m.id == id)
            .cloned()
            .collect();
        h.sort_by_key(|m| (m.temporal.transaction_time.start, m.revision_number));
        Ok(h)
    }

    async fn query_memories(&self, q: &MemoryQuery) -> Result<Vec<StoredMemory>> {
        let s = self.lock();
        let mut out: Vec<StoredMemory> = s
            .memories
            .iter()
            .filter(|m| {
                let tx = m.temporal.transaction_time;
                q.access.can_read(&m.scope)
                    && q.domains.contains(&m.domain())
                    && q.statuses.contains(&m.status)
                    && q.scope_filter.as_ref().is_none_or(|f| f.matches(&m.scope))
                    && tx.start <= q.as_of_transaction_time
                    && (q.include_superseded || tx.contains(q.as_of_transaction_time))
                    && m.temporal.valid_time.contains(q.as_of_valid_time)
                    && !m.is_expired(q.as_of_transaction_time)
            })
            .map(|m| StoredMemory {
                record: m.clone(),
                embedding: q.embedding_model.as_ref().and_then(|(name, version)| {
                    s.embeddings
                        .get(&m.revision_id)
                        .filter(|e| e.same_model(name, version))
                        .map(|e| e.vector.clone())
                }),
            })
            .collect();
        out.sort_by(|a, b| {
            b.record
                .temporal
                .transaction_time
                .start
                .cmp(&a.record.temporal.transaction_time.start)
                .then_with(|| a.record.revision_id.cmp(&b.record.revision_id))
        });
        out.truncate(q.limit as usize);
        Ok(out)
    }

    async fn purge_memory(&self, id: MemoryId, event: EventDraft) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        let revisions: Vec<MemoryRevisionId> = s
            .memories
            .iter()
            .filter(|m| m.id == id)
            .map(|m| m.revision_id)
            .collect();
        if revisions.is_empty() {
            return Err(Error::not_found("memory", id));
        }
        s.memories.retain(|m| m.id != id);
        for r in revisions {
            s.embeddings.remove(&r);
        }
        s.purges.insert((PurgeKind::Memory.as_str(), id.0), truncate_micros(OffsetDateTime::now_utc()));
        Ok(s.append(event))
    }

    async fn insert_evidence(
        &self,
        evidence: &Evidence,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        if s.evidence.contains_key(&evidence.id) {
            return Err(Error::conflict("evidence already exists"));
        }
        s.evidence.insert(evidence.id, evidence.clone());
        Ok(s.append(event))
    }

    async fn get_evidence(&self, ids: &[EvidenceId]) -> Result<Vec<Evidence>> {
        let s = self.lock();
        Ok(ids
            .iter()
            .filter_map(|id| s.evidence.get(id).cloned())
            .collect())
    }

    async fn insert_belief(
        &self,
        belief: &Belief,
        evidence: &[BeliefEvidence],
        dependencies: &[BeliefDependency],
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        check_links(&s, belief, evidence, dependencies)?;
        s.register(idempotency, belief.id.0)?;
        s.beliefs.push(belief.clone());
        s.belief_evidence
            .extend(evidence.iter().map(|e| (belief.id, e.clone())));
        s.dependencies.extend(dependencies.iter().cloned());
        Ok(s.append(event))
    }

    async fn supersede_belief(
        &self,
        current: BeliefId,
        closed_at: OffsetDateTime,
        next: &Belief,
        evidence: &[BeliefEvidence],
        dependencies: &[BeliefDependency],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        check_links(&s, next, evidence, dependencies)?;
        let Some(old) = s.beliefs.iter_mut().find(|b| b.id == current) else {
            return Err(Error::not_found("belief", current));
        };
        if !old.is_current() {
            return Err(Error::conflict("belief revision is no longer current"));
        }
        if closed_at <= old.temporal.transaction_time.start {
            return Err(Error::conflict("transaction time must advance"));
        }
        old.temporal.transaction_time.end = Some(closed_at);
        old.superseded_by = Some(next.id);
        s.beliefs.push(next.clone());
        s.belief_evidence
            .extend(evidence.iter().map(|e| (next.id, e.clone())));
        s.dependencies.extend(dependencies.iter().cloned());
        Ok(s.append(event))
    }

    async fn get_belief(&self, id: BeliefId) -> Result<Option<Belief>> {
        Ok(self.lock().beliefs.iter().find(|b| b.id == id).cloned())
    }

    async fn current_belief(&self, lineage_id: BeliefId) -> Result<Option<Belief>> {
        Ok(self
            .lock()
            .beliefs
            .iter()
            .find(|b| b.lineage_id == lineage_id && b.is_current())
            .cloned())
    }

    async fn belief_lineage(&self, lineage_id: BeliefId) -> Result<Vec<Belief>> {
        let mut v: Vec<_> = self
            .lock()
            .beliefs
            .iter()
            .filter(|b| b.lineage_id == lineage_id)
            .cloned()
            .collect();
        v.sort_by_key(|b| b.revision_number);
        Ok(v)
    }

    async fn belief_evidence(&self, id: BeliefId) -> Result<Vec<BeliefEvidence>> {
        Ok(self
            .lock()
            .belief_evidence
            .iter()
            .filter(|(b, _)| *b == id)
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn insert_dependency(
        &self,
        dependency: &BeliefDependency,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        for id in [dependency.from, dependency.to] {
            if !s.beliefs.iter().any(|b| b.id == id) {
                return Err(Error::not_found("belief", id));
            }
        }
        if s.dependencies.iter().any(|d| {
            d.from == dependency.from && d.to == dependency.to && d.relation == dependency.relation
        }) {
            return Err(Error::conflict("dependency already exists"));
        }
        s.dependencies.push(dependency.clone());
        Ok(s.append(event))
    }

    async fn dependencies_of(&self, id: BeliefId) -> Result<Vec<BeliefDependency>> {
        Ok(self
            .lock()
            .dependencies
            .iter()
            .filter(|d| d.to == id)
            .cloned()
            .collect())
    }

    async fn dependents_of(&self, id: BeliefId) -> Result<Vec<BeliefDependency>> {
        Ok(self
            .lock()
            .dependencies
            .iter()
            .filter(|d| d.from == id)
            .cloned()
            .collect())
    }

    async fn beliefs_for(&self, access: &AccessContext) -> Result<Vec<Belief>> {
        Ok(self
            .lock()
            .beliefs
            .iter()
            .filter(|b| access.can_read(&b.scope))
            .cloned()
            .collect())
    }

    async fn evidence_links(&self, ids: &[BeliefId]) -> Result<Vec<(BeliefId, BeliefEvidence)>> {
        Ok(self
            .lock()
            .belief_evidence
            .iter()
            .filter(|(b, _)| ids.contains(b))
            .cloned()
            .collect())
    }

    async fn dependencies_among(&self, ids: &[BeliefId]) -> Result<Vec<BeliefDependency>> {
        Ok(self
            .lock()
            .dependencies
            .iter()
            .filter(|d| ids.contains(&d.from) && ids.contains(&d.to))
            .cloned()
            .collect())
    }

    async fn append_event(&self, event: EventDraft) -> Result<CognitiveEvent> {
        Ok(self.lock().append(event))
    }

    async fn list_events(
        &self,
        access: &AccessContext,
        q: &EventQuery,
    ) -> Result<Vec<CognitiveEvent>> {
        let limit = q.limit.unwrap_or(100).min(1000) as usize;
        let mut v: Vec<_> = self
            .lock()
            .events
            .iter()
            .filter(|e| {
                access.can_read(&e.scope)
                    && q.stream_id.is_none_or(|s| e.stream_id == s)
                    && q.event_type.is_none_or(|t| e.event_type == t)
                    && q.after_sequence.is_none_or(|n| e.sequence > n)
            })
            .cloned()
            .collect();
        v.sort_by_key(|e| e.id);
        v.truncate(limit);
        Ok(v)
    }

    async fn insert_trace(&self, trace: &RecallTrace, event: EventDraft) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        s.traces.insert(trace.id, trace.clone());
        Ok(s.append(event))
    }

    async fn get_trace(&self, id: RecallTraceId) -> Result<Option<RecallTrace>> {
        Ok(self.lock().traces.get(&id).cloned())
    }

    // ---- M3 ------------------------------------------------------------------

    async fn query_beliefs(&self, q: &BeliefQuery) -> Result<Vec<Belief>> {
        let s = self.lock();
        let mut out: Vec<Belief> = s
            .beliefs
            .iter()
            .filter(|b| {
                let tx = b.temporal.transaction_time;
                q.access.can_read(&b.scope)
                    && q.statuses.contains(&b.status)
                    && q.scope_filter.as_ref().is_none_or(|f| f.matches(&b.scope))
                    && tx.start <= q.as_of_transaction_time
                    && (q.include_superseded || tx.contains(q.as_of_transaction_time))
                    && b.temporal.valid_time.contains(q.as_of_valid_time)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.temporal
                .transaction_time
                .start
                .cmp(&a.temporal.transaction_time.start)
                .then_with(|| a.id.cmp(&b.id))
        });
        out.truncate(q.limit as usize);
        Ok(out)
    }

    async fn enqueue_job(&self, job: &Job) -> Result<Job> {
        let mut s = self.lock();
        Ok(enqueue(&mut s, job))
    }

    async fn claim_jobs(&self, c: &JobClaim) -> Result<Vec<ClaimedJob>> {
        let mut s = self.lock();
        let mut due: Vec<&mut Job> = s
            .jobs
            .values_mut()
            .filter(|j| c.kinds.contains(&j.kind) && j.is_claimable(c.now))
            .collect();
        due.sort_by_key(|j| (j.available_at, j.created_at, j.id));
        Ok(due
            .into_iter()
            .take(c.limit as usize)
            .map(|j| {
                j.status = JobStatus::Leased;
                j.lease_generation += 1;
                j.attempts += 1;
                j.lease_owner = Some(c.owner.clone());
                j.lease_until = Some(c.lease_until);
                j.updated_at = c.now;
                ClaimedJob {
                    lease: Lease { job_id: j.id, generation: j.lease_generation },
                    job: j.clone(),
                }
            })
            .collect())
    }

    async fn heartbeat_job(&self, lease: Lease, until: OffsetDateTime) -> Result<()> {
        let mut s = self.lock();
        let job = fenced(&mut s, lease)?;
        job.lease_until = Some(until);
        Ok(())
    }

    async fn finish_job(
        &self,
        lease: Lease,
        now: OffsetDateTime,
        outcome: JobOutcome,
        effects: JobEffects,
    ) -> Result<Job> {
        let mut s = self.lock();
        fenced(&mut s, lease)?;
        // Read-skew guards: drop the effects if the world moved on.
        if let Some(reason) = precondition_failure(&s, &effects) {
            let job = fenced(&mut s, lease)?;
            apply_outcome(
                job,
                now,
                JobOutcome::Succeeded { result: serde_json::json!({ "discarded": reason }) },
            );
            return Ok(job.clone());
        }
        // Validate effects before touching anything (atomicity).
        for (m, _) in &effects.memories {
            if s.memories.iter().any(|x| x.id == m.id) {
                return Err(Error::conflict("memory already exists"));
            }
        }
        if let Some(run) = &effects.extraction_run
            && s.extraction_runs.iter().any(|r| {
                r.observation_id == run.observation_id
                    && r.extractor == run.extractor
                    && r.extractor_version == run.extractor_version
            })
        {
            return Err(Error::conflict("extraction run already recorded"));
        }
        for (m, e) in effects.memories {
            if let Some(e) = e {
                s.embeddings.insert(m.revision_id, e);
            }
            s.memories.push(m);
        }
        if let Some(run) = effects.extraction_run {
            s.extraction_runs.push(run);
        }
        for p in effects.proposals {
            let dup = s.proposals.values().any(|x| x.fingerprint == p.fingerprint);
            if !dup {
                s.proposals.insert(p.id, p);
            }
        }
        for e in effects.events {
            s.append(e);
        }
        let job = fenced(&mut s, lease)?;
        apply_outcome(job, now, outcome);
        Ok(job.clone())
    }

    async fn release_job(&self, lease: Lease, now: OffsetDateTime) -> Result<()> {
        let mut s = self.lock();
        let job = fenced(&mut s, lease)?;
        job.status = JobStatus::Queued;
        job.attempts = job.attempts.saturating_sub(1);
        job.lease_owner = None;
        job.lease_until = None;
        job.available_at = now;
        job.updated_at = now;
        Ok(())
    }

    async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        Ok(self.lock().jobs.get(&id).cloned())
    }

    async fn list_jobs(
        &self,
        tenant_id: TenantId,
        status: Option<JobStatus>,
        limit: u32,
    ) -> Result<Vec<Job>> {
        Ok(self
            .lock()
            .jobs
            .values()
            .filter(|j| j.tenant_id == tenant_id && status.is_none_or(|st| j.status == st))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn insert_observation(
        &self,
        observation: &Observation,
        job: &Job,
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        if s.observations.contains_key(&observation.id) {
            return Err(Error::conflict("observation already exists"));
        }
        s.register(idempotency, observation.id.0)?;
        s.observations.insert(observation.id, observation.clone());
        enqueue(&mut s, job);
        Ok(s.append(event))
    }

    async fn get_observation(&self, id: ObservationId) -> Result<Option<Observation>> {
        Ok(self.lock().observations.get(&id).cloned())
    }

    async fn extraction_runs(&self, id: ObservationId) -> Result<Vec<ExtractionRun>> {
        Ok(self
            .lock()
            .extraction_runs
            .iter()
            .filter(|r| r.observation_id == id)
            .cloned()
            .collect())
    }

    async fn purge_observation(
        &self,
        id: ObservationId,
        now: OffsetDateTime,
        derived: &[MemoryId],
        rehome: Vec<MemorySupersede>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        if !s.observations.contains_key(&id) {
            return Err(Error::not_found("observation", id));
        }
        for m in &rehome {
            check_memory_supersede(&s, m)?;
        }
        for m in rehome {
            if let Some(old) = s.memories.iter_mut().find(|x| x.revision_id == m.current) {
                old.temporal.transaction_time.end = Some(m.closed_at);
                old.superseded_by = Some(m.next.revision_id);
            }
            if let Some(e) = m.embedding {
                s.embeddings.insert(m.next.revision_id, e);
            }
            s.memories.push(m.next);
        }
        if let Some(obs) = s.observations.get_mut(&id) {
            obs.content = None;
            obs.purged_at = Some(now);
        }
        let revisions: Vec<MemoryRevisionId> = s
            .memories
            .iter()
            .filter(|m| derived.contains(&m.id))
            .map(|m| m.revision_id)
            .collect();
        s.memories.retain(|m| !derived.contains(&m.id));
        for r in revisions {
            s.embeddings.remove(&r);
        }
        for m in derived {
            s.purges.insert((PurgeKind::Memory.as_str(), m.0), now);
        }
        s.purges.insert((PurgeKind::Observation.as_str(), id.0), now);
        Ok(s.append(event))
    }

    async fn get_proposal(&self, id: ProposalId) -> Result<Option<ResolutionProposal>> {
        Ok(self.lock().proposals.get(&id).cloned())
    }

    async fn list_proposals(
        &self,
        access: &AccessContext,
        status: Option<ProposalStatus>,
        limit: u32,
    ) -> Result<Vec<ResolutionProposal>> {
        Ok(self
            .lock()
            .proposals
            .values()
            .filter(|p| access.can_read(&p.scope) && status.is_none_or(|st| p.status == st))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn decide_proposal(
        &self,
        id: ProposalId,
        decision: &ProposalDecision,
        mutations: ProposalMutations,
        events: Vec<EventDraft>,
    ) -> Result<ResolutionProposal> {
        let mut s = self.lock();
        match s.proposals.get(&id) {
            None => return Err(Error::not_found("proposal", id)),
            Some(p) if p.status != ProposalStatus::Pending => {
                return Err(Error::conflict("proposal is no longer pending"));
            }
            Some(_) => {}
        }
        // Validate every mutation first.
        for m in &mutations.memories {
            check_memory_supersede(&s, m)?;
        }
        for b in &mutations.beliefs {
            check_belief_supersede(&s, b)?;
        }
        for m in mutations.memories {
            if let Some(old) = s.memories.iter_mut().find(|x| x.revision_id == m.current) {
                old.temporal.transaction_time.end = Some(m.closed_at);
                old.superseded_by = Some(m.next.revision_id);
            }
            if let Some(e) = m.embedding {
                s.embeddings.insert(m.next.revision_id, e);
            }
            s.memories.push(m.next);
        }
        for b in mutations.beliefs {
            if let Some(old) = s.beliefs.iter_mut().find(|x| x.id == b.current) {
                old.temporal.transaction_time.end = Some(b.closed_at);
                old.superseded_by = Some(b.next.id);
            }
            let next_id = b.next.id;
            s.belief_evidence.extend(b.evidence.into_iter().map(|e| (next_id, e)));
            s.dependencies.extend(b.dependencies);
            s.beliefs.push(b.next);
        }
        for e in events {
            s.append(e);
        }
        let p = s
            .proposals
            .get_mut(&id)
            .ok_or_else(|| Error::not_found("proposal", id))?;
        p.status = decision.status;
        p.decided_at = Some(decision.decided_at);
        p.decided_by = Some(decision.decided_by.clone());
        p.decision_reason = Some(decision.reason.clone());
        Ok(p.clone())
    }

    async fn purged_at(&self, kind: PurgeKind, id: Uuid) -> Result<Option<OffsetDateTime>> {
        Ok(self.lock().purges.get(&(kind.as_str(), id)).copied())
    }

    async fn purge_evidence(
        &self,
        id: EvidenceId,
        now: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Vec<BeliefId>> {
        let mut s = self.lock();
        if s.evidence.remove(&id).is_none() {
            return Err(Error::not_found("evidence", id));
        }
        let mut affected: Vec<BeliefId> = s
            .belief_evidence
            .iter()
            .filter(|(_, l)| l.evidence_id == id)
            .map(|(b, _)| *b)
            .collect();
        affected.sort();
        affected.dedup();
        s.belief_evidence.retain(|(_, l)| l.evidence_id != id);
        s.purges.insert((PurgeKind::Evidence.as_str(), id.0), now);
        s.append(event);
        Ok(affected)
    }
}

fn enqueue(s: &mut State, job: &Job) -> Job {
    if let Some(existing) = s.jobs.values().find(|j| {
        j.tenant_id == job.tenant_id
            && j.kind == job.kind
            && j.dedupe_key == job.dedupe_key
            && !j.status.is_terminal()
    }) {
        return existing.clone();
    }
    s.jobs.insert(job.id, job.clone());
    job.clone()
}

/// The job, if `lease` is still the current, held lease.
fn fenced(s: &mut State, lease: Lease) -> Result<&mut Job> {
    match s.jobs.get_mut(&lease.job_id) {
        Some(j) if j.status == JobStatus::Leased && j.lease_generation == lease.generation => Ok(j),
        Some(_) => Err(Error::conflict("lease is no longer held")),
        None => Err(Error::not_found("job", lease.job_id)),
    }
}

fn apply_outcome(job: &mut Job, now: OffsetDateTime, outcome: JobOutcome) {
    job.lease_owner = None;
    job.lease_until = None;
    job.updated_at = now;
    match outcome {
        JobOutcome::Succeeded { result } => {
            job.status = JobStatus::Succeeded;
            job.result = Some(result);
        }
        JobOutcome::Retry { at, error } => {
            job.status = JobStatus::Queued;
            job.available_at = at;
            job.last_error = Some(error);
        }
        JobOutcome::DeadLetter { error } => {
            job.status = JobStatus::DeadLettered;
            job.last_error = Some(error);
        }
    }
}

fn precondition_failure(s: &State, effects: &JobEffects) -> Option<&'static str> {
    if let Some(id) = effects.require_unpurged_observation {
        match s.observations.get(&id) {
            Some(o) if !o.is_purged() => {}
            _ => return Some("observation_purged"),
        }
    }
    for v in &effects.require_current {
        let current = match *v {
            RecordVersion::Memory { id, revision_id } => s
                .memories
                .iter()
                .any(|m| m.id == id && m.revision_id == revision_id && m.is_current()),
            RecordVersion::Belief { lineage_id, revision_id } => s
                .beliefs
                .iter()
                .any(|b| b.lineage_id == lineage_id && b.id == revision_id && b.is_current()),
        };
        if !current {
            return Some("target_changed");
        }
    }
    None
}

fn check_memory_supersede(s: &State, m: &MemorySupersede) -> Result<()> {
    match s.memories.iter().find(|x| x.revision_id == m.current) {
        Some(old) if old.is_current() && m.closed_at > old.temporal.transaction_time.start => Ok(()),
        Some(_) => Err(Error::conflict("revision is no longer current")),
        None => Err(Error::not_found("memory revision", m.current)),
    }
}

fn check_belief_supersede(s: &State, b: &BeliefSupersede) -> Result<()> {
    match s.beliefs.iter().find(|x| x.id == b.current) {
        Some(old) if old.is_current() && b.closed_at > old.temporal.transaction_time.start => Ok(()),
        Some(_) => Err(Error::conflict("belief revision is no longer current")),
        None => Err(Error::not_found("belief", b.current)),
    }
}

fn check_links(
    s: &State,
    belief: &Belief,
    evidence: &[BeliefEvidence],
    dependencies: &[BeliefDependency],
) -> Result<()> {
    if s.beliefs.iter().any(|b| b.id == belief.id) {
        return Err(Error::conflict("belief revision already exists"));
    }
    for link in evidence {
        match s.evidence.get(&link.evidence_id) {
            Some(e) if e.tenant_id == belief.scope.tenant_id => {}
            _ => return Err(Error::not_found("evidence", link.evidence_id)),
        }
    }
    for d in dependencies {
        for id in [d.from, d.to] {
            if id != belief.id && !s.beliefs.iter().any(|b| b.id == id) {
                return Err(Error::not_found("belief", id));
            }
        }
    }
    Ok(())
}
