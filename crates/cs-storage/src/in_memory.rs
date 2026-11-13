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
    AccessContext, BeliefId, CognitiveEvent, Error, EventDraft, EventQuery, EvidenceId, MemoryId,
    MemoryRevisionId, RecallTraceId, Result, TenantId, temporal::truncate_micros,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, EmbeddingRecord, Evidence, MemoryRecord, RecallTrace,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{CognitiveStore, IdempotencyKey, MemoryQuery, StoredMemory};

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
