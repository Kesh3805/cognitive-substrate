//! Reference implementation of [`CognitiveStore`] held in process memory.
//!
//! Used for embedding the engine without a database, for tests, and as the
//! executable definition of storage semantics that every other adapter must
//! match (see the conformance suite).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{Mutex, MutexGuard},
};

use async_trait::async_trait;
use cs_core::{
    AccessContext, BeliefId, CognitiveEvent, EdgeId, EntityId, EntityMentionId, Error, EventDraft,
    EventQuery, EvidenceId, JobId, MemoryId, MemoryRevisionId, ObservationId, ProposalId,
    RecallTraceId, Result, TenantId, DerivationId, temporal::truncate_micros,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefStatus, ClaimedJob, EmbeddingRecord, Evidence,
    ExtractionRun, Job, JobError, JobOutcome, JobStatus, Lease, MemoryRecord, MemoryStatus, Observation,
    ProposalDecision, ProposalStatus, RecallTrace, RecordVersion, ResolutionProposal,
    derivation::{Derivation, DerivationStatus},
    entity::{
        AliasSource, ENTITY_NORMALIZATION_VERSION, Entity, EntityAlias, EntityKind, EntityMention,
        EntityStatus, KnowledgeEdge, Projection, entity_id_for, partition_scope,
    },
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    BeliefQuery, BeliefSupersede, CognitiveStore, EdgeDirection, EdgeQuery, EmbeddingCoverage,
    EntityMerge, IdempotencyKey, JobClaim, JobCount, JobEffects, MemoryQuery, MemorySupersede,
    ProposalMutations, PurgeKind, StoredBelief, StoredMemory,
};

#[derive(Debug, Default)]
struct State {
    memories: Vec<MemoryRecord>,
    /// Keyed by (revision, embedding space): a revision may hold vectors
    /// from several spaces during a model migration.
    embeddings: HashMap<(MemoryRevisionId, String), EmbeddingRecord>,
    belief_embeddings: HashMap<(BeliefId, String), EmbeddingRecord>,
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
    entities: BTreeMap<EntityId, Entity>,
    /// Keyed by (partition, key): one entity per name per partition.
    aliases: BTreeMap<(String, String), EntityAlias>,
    mentions: Vec<EntityMention>,
    edges: Vec<KnowledgeEdge>,
    projected: HashSet<MemoryRevisionId>,
    derivations: BTreeMap<DerivationId, Derivation>,
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

    fn put_embedding(&mut self, revision: MemoryRevisionId, e: EmbeddingRecord) {
        self.embeddings.insert((revision, e.space_id()), e);
    }

    fn drop_embeddings(&mut self, revision: MemoryRevisionId) {
        self.embeddings.retain(|(r, _), _| *r != revision);
    }

    fn put_belief_embedding(&mut self, id: BeliefId, e: EmbeddingRecord) {
        self.belief_embeddings.insert((id, e.space_id()), e);
    }

    /// Resolve a name in a partition to its entity, creating it (with a
    /// deterministic id) if the name is new. Follows merge redirects.
    fn resolve_entity(
        &mut self,
        p: &Projection,
        surface: &str,
        key: &str,
        now: OffsetDateTime,
    ) -> EntityId {
        let slot = (p.partition.clone(), key.to_owned());
        let id = match self.aliases.get(&slot) {
            Some(a) => a.entity_id,
            None => {
                let id = entity_id_for(&p.partition, key);
                self.entities.entry(id).or_insert_with(|| Entity {
                    id,
                    scope: partition_scope(&p.scope),
                    partition: p.partition.clone(),
                    kind: EntityKind::Unknown,
                    canonical_name: surface.to_owned(),
                    key: key.to_owned(),
                    status: EntityStatus::Active,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                });
                self.aliases.insert(
                    slot,
                    EntityAlias {
                        entity_id: id,
                        surface: surface.to_owned(),
                        key: key.to_owned(),
                        normalization_version: ENTITY_NORMALIZATION_VERSION,
                        source: AliasSource::Canonical,
                        created_at: now,
                    },
                );
                id
            }
        };
        self.canonical_entity(id)
    }

    fn canonical_entity(&self, mut id: EntityId) -> EntityId {
        for _ in 0..64 {
            match self.entities.get(&id).map(|e| e.status) {
                Some(EntityStatus::Merged { into, .. }) => id = into,
                _ => break,
            }
        }
        id
    }

    fn apply_projection(&mut self, p: &Projection, now: OffsetDateTime) {
        if self.projected.contains(&p.revision_id)
            || !self.memories.iter().any(|m| m.revision_id == p.revision_id)
        {
            return;
        }
        for m in &p.mentions {
            let entity_id = self.resolve_entity(p, &m.surface, &m.key, now);
            if !self
                .mentions
                .iter()
                .any(|x| x.revision_id == p.revision_id && x.key == m.key && x.role == m.role)
            {
                self.mentions.push(EntityMention {
                    id: EntityMentionId::new_v7(),
                    entity_id,
                    surface: m.surface.clone(),
                    key: m.key.clone(),
                    role: m.role,
                    memory_id: p.memory_id,
                    revision_id: p.revision_id,
                });
            }
        }
        for e in &p.edges {
            let from = self.resolve_entity(p, &e.from.surface, &e.from.key, now);
            let to = self.resolve_entity(p, &e.to.surface, &e.to.key, now);
            let dup = self.edges.iter().any(|x| {
                x.revision_id == p.revision_id
                    && x.from == from
                    && x.to == to
                    && x.relation == e.relation
            });
            if from != to && !dup {
                self.edges.push(KnowledgeEdge {
                    id: EdgeId::new_v7(),
                    scope: p.scope.clone(),
                    from,
                    relation: e.relation.clone(),
                    to,
                    memory_id: p.memory_id,
                    revision_id: p.revision_id,
                    valid_time: p.valid_time,
                });
            }
        }
        self.projected.insert(p.revision_id);
    }

    /// Projections are derived data: they go with their revisions. An
    /// entity left with no mentions or edges that was never curated
    /// (version 1: no alias, kind or merge decision) goes too, so a purged
    /// name does not survive in identity rows.
    fn drop_projections_of_memories(&mut self, ids: &[MemoryId]) {
        let touched: BTreeSet<EntityId> = self
            .mentions
            .iter()
            .filter(|m| ids.contains(&m.memory_id))
            .map(|m| m.entity_id)
            .collect();
        self.mentions.retain(|m| !ids.contains(&m.memory_id));
        self.edges.retain(|e| !ids.contains(&e.memory_id));
        let live: HashSet<MemoryRevisionId> = self.memories.iter().map(|m| m.revision_id).collect();
        self.projected.retain(|r| live.contains(r));
        for id in touched {
            let orphan = self.entities.get(&id).is_some_and(|e| e.version == 1)
                && !self.mentions.iter().any(|m| m.entity_id == id)
                && !self.edges.iter().any(|e| e.from == id || e.to == id);
            if orphan {
                self.entities.remove(&id);
                self.aliases.retain(|_, a| a.entity_id != id);
            }
        }
    }

    /// A purge reaches derivations atomically: a derivation of a purged
    /// memory goes with it, and one citing a purged root is flagged for
    /// revalidation so it cannot silently keep support it lost.
    fn flag_derivations(&mut self, ids: &[MemoryId], at: OffsetDateTime) {
        self.derivations
            .retain(|_, d| !ids.contains(&d.derived_memory_id));
        for d in self.derivations.values_mut() {
            if d.roots.iter().any(|r| ids.contains(r)) {
                d.status = DerivationStatus::RevalidationRequired;
                d.status_reason = Some("a supporting root was purged".into());
                d.version += 1;
                d.updated_at = at;
            }
        }
    }

    /// Record the redirect. Mentions, aliases and edges are left where
    /// they are: reads resolve through the redirect as of their
    /// transaction time, so history before the merge is unchanged.
    fn merge_entities(&mut self, m: &EntityMerge) {
        if let Some(e) = self.entities.get_mut(&m.merged) {
            e.status = EntityStatus::Merged {
                into: m.survivor,
                at: m.at,
            };
            e.version += 1;
            e.updated_at = m.at;
        }
        if let Some(e) = self.entities.get_mut(&m.survivor) {
            e.version += 1;
            e.updated_at = m.at;
        }
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
        follow_up: &[Job],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        if s.memories.iter().any(|m| m.id == record.id) {
            return Err(Error::conflict("memory already exists"));
        }
        s.register(idempotency, record.id.0)?;
        s.memories.push(record.clone());
        if let Some(e) = embedding {
            s.put_embedding(record.revision_id, e.clone());
        }
        for j in follow_up {
            enqueue(&mut s, j);
        }
        Ok(s.append(event))
    }

    async fn supersede_memory(
        &self,
        current: MemoryRevisionId,
        closed_at: OffsetDateTime,
        next: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        follow_up: &[Job],
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
            s.put_embedding(next.revision_id, e.clone());
        }
        for j in follow_up {
            enqueue(&mut s, j);
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
                    && (q.ignore_valid_time || m.temporal.valid_time.contains(q.as_of_valid_time))
                    && !m.is_expired(q.as_of_transaction_time)
            })
            .map(|m| StoredMemory {
                record: m.clone(),
                embedding: q.embedding_model.as_ref().and_then(|(name, version)| {
                    s.embeddings
                        .get(&(m.revision_id, format!("{name}@{version}")))
                        .filter(|e| e.embedded_at.is_none_or(|t| t <= q.as_of_transaction_time))
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
        s.drop_projections_of_memories(&[id]);
        s.flag_derivations(&[id], event.occurred_at);
        for r in revisions {
            s.drop_embeddings(r);
        }
        s.purges.insert(
            (PurgeKind::Memory.as_str(), id.0),
            truncate_micros(OffsetDateTime::now_utc()),
        );
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
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        follow_up: &[Job],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut s = self.lock();
        check_links(&s, belief, evidence, dependencies)?;
        s.register(idempotency, belief.id.0)?;
        if let Some(e) = embedding {
            s.put_belief_embedding(belief.id, e.clone());
        }
        for j in follow_up {
            enqueue(&mut s, j);
        }
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
        embedding: Option<&EmbeddingRecord>,
        follow_up: &[Job],
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
        if let Some(e) = embedding {
            s.put_belief_embedding(next.id, e.clone());
        }
        for j in follow_up {
            enqueue(&mut s, j);
        }
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

    async fn query_beliefs(&self, q: &BeliefQuery) -> Result<Vec<StoredBelief>> {
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
                    && (q.ignore_valid_time || b.temporal.valid_time.contains(q.as_of_valid_time))
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
        Ok(out
            .into_iter()
            .map(|b| StoredBelief {
                embedding: q.embedding_model.as_ref().and_then(|(name, version)| {
                    s.belief_embeddings
                        .get(&(b.id, format!("{name}@{version}")))
                        .filter(|e| e.embedded_at.is_none_or(|t| t <= q.as_of_transaction_time))
                        .map(|e| e.vector.clone())
                }),
                belief: b,
            })
            .collect())
    }

    async fn job_counts(&self, tenant_id: TenantId) -> Result<Vec<JobCount>> {
        let s = self.lock();
        let mut groups: BTreeMap<(&'static str, &'static str), JobCount> = BTreeMap::new();
        for j in s.jobs.values().filter(|j| j.tenant_id == tenant_id) {
            let g = groups
                .entry((j.kind.as_str(), j.status.as_str()))
                .or_insert(JobCount {
                    kind: j.kind,
                    status: j.status,
                    count: 0,
                    oldest_available_at: None,
                });
            g.count += 1;
            g.oldest_available_at = Some(
                g.oldest_available_at
                    .map_or(j.available_at, |o| o.min(j.available_at)),
            );
        }
        Ok(groups.into_values().collect())
    }

    async fn embedding_coverage(
        &self,
        tenant: Option<TenantId>,
        model_name: &str,
        model_version: &str,
    ) -> Result<EmbeddingCoverage> {
        let s = self.lock();
        let space = format!("{model_name}@{model_version}");
        let in_tenant = |t: TenantId| tenant.is_none_or(|x| x == t);
        let mut c = EmbeddingCoverage::default();
        for m in s.memories.iter().filter(|m| {
            m.is_current() && m.status != MemoryStatus::Tombstoned && in_tenant(m.scope.tenant_id)
        }) {
            c.memories_total += 1;
            if s.embeddings.contains_key(&(m.revision_id, space.clone())) {
                c.memories_embedded += 1;
            }
        }
        for b in s.beliefs.iter().filter(|b| {
            b.is_current() && b.status != BeliefStatus::Retracted && in_tenant(b.scope.tenant_id)
        }) {
            c.beliefs_total += 1;
            if s.belief_embeddings.contains_key(&(b.id, space.clone())) {
                c.beliefs_embedded += 1;
            }
        }
        Ok(c)
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
                    lease: Lease {
                        job_id: j.id,
                        generation: j.lease_generation,
                    },
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
                JobOutcome::Succeeded {
                    result: serde_json::json!({ "discarded": reason }),
                },
            );
            return Ok(job.clone());
        }
        // Contention: another job got there first. Retry rather than fail.
        if let Some(reason) = contention(&s, &effects) {
            let job = fenced(&mut s, lease)?;
            let attempt = job.attempts;
            apply_outcome(
                job,
                now,
                JobOutcome::Retry {
                    at: now + time::Duration::seconds(1),
                    error: JobError {
                        permanent: false,
                        message: reason.to_owned(),
                        attempt,
                    },
                },
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
                s.put_embedding(m.revision_id, e);
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
        for j in &effects.jobs {
            enqueue(&mut s, j);
        }
        for (rev, e) in effects.memory_embeddings {
            // Purged revisions are skipped, never resurrected.
            if s.memories.iter().any(|m| m.revision_id == rev) {
                s.put_embedding(rev, e);
            }
        }
        for (id, e) in effects.belief_embeddings {
            if s.beliefs.iter().any(|b| b.id == id) {
                s.put_belief_embedding(id, e);
            }
        }
        if let Some(p) = effects.projection {
            s.apply_projection(&p, now);
        }
        for m in effects.supersedes {
            if let Some(old) = s.memories.iter_mut().find(|x| x.revision_id == m.current) {
                old.temporal.transaction_time.end = Some(m.closed_at);
                old.superseded_by = Some(m.next.revision_id);
            }
            if let Some(e) = m.embedding {
                s.put_embedding(m.next.revision_id, e);
            }
            for j in &m.follow_up {
                enqueue(&mut s, j);
            }
            s.memories.push(m.next);
        }
        for w in effects.derivations {
            s.derivations.insert(w.derivation.id, w.derivation);
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
                s.put_embedding(m.next.revision_id, e);
            }
            for j in &m.follow_up {
                enqueue(&mut s, j);
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
        s.drop_projections_of_memories(derived);
        s.flag_derivations(derived, now);
        for r in revisions {
            s.drop_embeddings(r);
        }
        for m in derived {
            s.purges.insert((PurgeKind::Memory.as_str(), m.0), now);
        }
        s.purges
            .insert((PurgeKind::Observation.as_str(), id.0), now);
        Ok(s.append(event))
    }

    async fn get_proposal(&self, id: ProposalId) -> Result<Option<ResolutionProposal>> {
        Ok(self.lock().proposals.get(&id).cloned())
    }

    async fn insert_proposal(
        &self,
        proposal: &ResolutionProposal,
        event: EventDraft,
    ) -> Result<ResolutionProposal> {
        let mut s = self.lock();
        if let Some(existing) = s.proposals.values().find(|x| {
            x.scope.tenant_id == proposal.scope.tenant_id && x.fingerprint == proposal.fingerprint
        }) {
            return Ok(existing.clone());
        }
        s.proposals.insert(proposal.id, proposal.clone());
        s.append(event);
        Ok(proposal.clone())
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
        for m in &mutations.entity_merges {
            check_entity_merge(&s, m)?;
        }
        for m in &mutations.entity_merges {
            s.merge_entities(m);
        }
        for m in mutations.memories {
            if let Some(old) = s.memories.iter_mut().find(|x| x.revision_id == m.current) {
                old.temporal.transaction_time.end = Some(m.closed_at);
                old.superseded_by = Some(m.next.revision_id);
            }
            if let Some(e) = m.embedding {
                s.put_embedding(m.next.revision_id, e);
            }
            for j in &m.follow_up {
                enqueue(&mut s, j);
            }
            s.memories.push(m.next);
        }
        for b in mutations.beliefs {
            if let Some(old) = s.beliefs.iter_mut().find(|x| x.id == b.current) {
                old.temporal.transaction_time.end = Some(b.closed_at);
                old.superseded_by = Some(b.next.id);
            }
            let next_id = b.next.id;
            s.belief_evidence
                .extend(b.evidence.into_iter().map(|e| (next_id, e)));
            s.dependencies.extend(b.dependencies);
            if let Some(e) = b.embedding {
                s.put_belief_embedding(next_id, e);
            }
            for j in &b.follow_up {
                enqueue(&mut s, j);
            }
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

    async fn get_entity(&self, id: EntityId) -> Result<Option<Entity>> {
        Ok(self.lock().entities.get(&id).cloned())
    }

    async fn entities_by_key(&self, access: &AccessContext, key: &str) -> Result<Vec<Entity>> {
        let s = self.lock();
        let ids: BTreeSet<EntityId> = s
            .aliases
            .values()
            .filter(|a| a.key == key)
            .map(|a| s.canonical_entity(a.entity_id))
            .collect();
        Ok(ids
            .into_iter()
            .filter_map(|id| s.entities.get(&id))
            .filter(|e| access.can_read(&e.scope))
            .cloned()
            .collect())
    }

    async fn entity_aliases(&self, id: EntityId) -> Result<Vec<EntityAlias>> {
        let s = self.lock();
        let mut v: Vec<EntityAlias> = s
            .aliases
            .values()
            .filter(|a| a.entity_id == id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(v)
    }

    async fn entity_mentions(&self, id: EntityId, limit: u32) -> Result<Vec<EntityMention>> {
        let s = self.lock();
        let mut v: Vec<EntityMention> = s
            .mentions
            .iter()
            .filter(|m| m.entity_id == id)
            .cloned()
            .collect();
        v.sort_by_key(|m| m.id);
        v.truncate(limit as usize);
        Ok(v)
    }

    async fn add_entity_alias(
        &self,
        id: EntityId,
        expected_version: u32,
        alias: &EntityAlias,
        event: EventDraft,
    ) -> Result<Entity> {
        let mut s = self.lock();
        let e = s
            .entities
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::not_found("entity", id))?;
        if e.version != expected_version || e.status != EntityStatus::Active {
            return Err(Error::conflict("entity changed; reload it"));
        }
        let slot = (e.partition.clone(), alias.key.clone());
        if let Some(existing) = s.aliases.get(&slot) {
            if s.canonical_entity(existing.entity_id) == id {
                return Ok(e); // idempotent
            }
            return Err(Error::conflict(
                "that name already identifies another entity in this partition",
            ));
        }
        s.aliases.insert(
            slot,
            EntityAlias {
                entity_id: id,
                ..alias.clone()
            },
        );
        let at = alias.created_at;
        let entity = s
            .entities
            .get_mut(&id)
            .ok_or_else(|| Error::not_found("entity", id))?;
        entity.version += 1;
        entity.updated_at = at;
        let out = entity.clone();
        s.append(event);
        Ok(out)
    }

    async fn set_entity_kind(
        &self,
        id: EntityId,
        expected_version: u32,
        kind: EntityKind,
        at: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Entity> {
        let mut s = self.lock();
        let entity = s
            .entities
            .get_mut(&id)
            .ok_or_else(|| Error::not_found("entity", id))?;
        if entity.version != expected_version || entity.status != EntityStatus::Active {
            return Err(Error::conflict("entity changed; reload it"));
        }
        entity.kind = kind;
        entity.version += 1;
        entity.updated_at = at;
        let out = entity.clone();
        s.append(event);
        Ok(out)
    }

    async fn get_derivation(&self, id: DerivationId) -> Result<Option<Derivation>> {
        Ok(self.lock().derivations.get(&id).cloned())
    }

    async fn derivation_for(
        &self,
        partition: &str,
        operator: &str,
        signature: &str,
    ) -> Result<Option<Derivation>> {
        Ok(self
            .lock()
            .derivations
            .values()
            .find(|d| d.partition == partition && d.operator == operator && d.signature == signature)
            .cloned())
    }

    async fn derivations_citing(&self, memory: MemoryId) -> Result<Vec<Derivation>> {
        Ok(self
            .lock()
            .derivations
            .values()
            .filter(|d| d.roots.contains(&memory))
            .cloned()
            .collect())
    }

    async fn list_derivations(
        &self,
        access: &AccessContext,
        status: Option<DerivationStatus>,
        limit: u32,
    ) -> Result<Vec<Derivation>> {
        let s = self.lock();
        let mut v: Vec<Derivation> = s
            .derivations
            .values()
            .filter(|d| access.can_read(&d.scope) && status.is_none_or(|x| d.status == x))
            .cloned()
            .collect();
        v.sort_by(|a, b| (b.created_at, b.id).cmp(&(a.created_at, a.id)));
        v.truncate(limit as usize);
        Ok(v)
    }

    async fn merged_into(&self, ids: &[EntityId]) -> Result<Vec<Entity>> {
        Ok(self
            .lock()
            .entities
            .values()
            .filter(
                |e| matches!(e.status, EntityStatus::Merged { into, .. } if ids.contains(&into)),
            )
            .cloned()
            .collect())
    }

    async fn partition_entities(&self, partition: &str, limit: u32) -> Result<Vec<Entity>> {
        Ok(self
            .lock()
            .entities
            .values()
            .filter(|e| e.partition == partition)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn entity_edges(&self, q: &EdgeQuery) -> Result<Vec<KnowledgeEdge>> {
        let s = self.lock();
        let ids: HashSet<EntityId> = q.entity_ids.iter().copied().collect();
        let mut out: Vec<(String, EntityId, KnowledgeEdge)> = s
            .edges
            .iter()
            .filter_map(|e| {
                let other = match q.direction {
                    EdgeDirection::Out if ids.contains(&e.from) => e.to,
                    EdgeDirection::In if ids.contains(&e.to) => e.from,
                    EdgeDirection::Both if ids.contains(&e.from) => e.to,
                    EdgeDirection::Both if ids.contains(&e.to) => e.from,
                    _ => return None,
                };
                let visible = q.access.can_read(&e.scope)
                    && q.relations.as_ref().is_none_or(|r| r.contains(&e.relation))
                    && q.as_of_valid_time.is_none_or(|t| e.valid_time.contains(t))
                    && s.memories.iter().any(|m| {
                        m.revision_id == e.revision_id
                            && m.temporal
                                .transaction_time
                                .contains(q.as_of_transaction_time)
                            && (m.status == MemoryStatus::Active
                                || (q.include_candidates && m.status == MemoryStatus::Candidate))
                    });
                visible.then(|| (e.relation.clone(), other, e.clone()))
            })
            .collect();
        out.sort_by(|a, b| (&a.0, a.1, a.2.id).cmp(&(&b.0, b.1, b.2.id)));
        out.truncate(q.limit as usize);
        Ok(out.into_iter().map(|(_, _, e)| e).collect())
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

fn contention(s: &State, effects: &JobEffects) -> Option<&'static str> {
    for w in &effects.derivations {
        let current = s.derivations.get(&w.derivation.id).map(|d| d.version);
        let taken = w.expected_version.is_none()
            && s.derivations.values().any(|d| {
                d.id != w.derivation.id
                    && d.partition == w.derivation.partition
                    && d.operator == w.derivation.operator
                    && d.signature == w.derivation.signature
            });
        if current != w.expected_version || taken {
            return Some("derivation changed concurrently");
        }
    }
    for m in &effects.supersedes {
        if check_memory_supersede(s, m).is_err() {
            return Some("revision changed concurrently");
        }
    }
    None
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
            RecordVersion::Belief {
                lineage_id,
                revision_id,
            } => s
                .beliefs
                .iter()
                .any(|b| b.lineage_id == lineage_id && b.id == revision_id && b.is_current()),
            RecordVersion::Entity { id, version } => s
                .entities
                .get(&id)
                .is_some_and(|e| e.version == version && e.status == EntityStatus::Active),
        };
        if !current {
            return Some("target_changed");
        }
    }
    None
}

fn check_entity_merge(s: &State, m: &EntityMerge) -> Result<()> {
    let get = |id: EntityId| {
        s.entities
            .get(&id)
            .ok_or_else(|| Error::not_found("entity", id))
    };
    let (a, b) = (get(m.survivor)?, get(m.merged)?);
    if m.survivor == m.merged {
        return Err(Error::validation("an entity cannot be merged into itself"));
    }
    if a.partition != b.partition {
        return Err(Error::validation(
            "entities in different partitions cannot be merged",
        ));
    }
    if a.version != m.survivor_version
        || b.version != m.merged_version
        || a.status != EntityStatus::Active
        || b.status != EntityStatus::Active
    {
        return Err(Error::conflict("entity changed since the proposal"));
    }
    Ok(())
}

fn check_memory_supersede(s: &State, m: &MemorySupersede) -> Result<()> {
    match s.memories.iter().find(|x| x.revision_id == m.current) {
        Some(old) if old.is_current() && m.closed_at > old.temporal.transaction_time.start => {
            Ok(())
        }
        Some(_) => Err(Error::conflict("revision is no longer current")),
        None => Err(Error::not_found("memory revision", m.current)),
    }
}

fn check_belief_supersede(s: &State, b: &BeliefSupersede) -> Result<()> {
    match s.beliefs.iter().find(|x| x.id == b.current) {
        Some(old) if old.is_current() && b.closed_at > old.temporal.transaction_time.start => {
            Ok(())
        }
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
