//! Knowledge compilation (M4.3): recurring episodes → semantic candidates.
//!
//! Operator `recurring-episode/1`. Episodes carrying the same structured
//! triple (subject entity, relation, object entity, with identity resolved
//! through merges as of now) are compiled into one semantic memory once
//! independent support meets the thresholds. The compiled memory starts as
//! `candidate`: compilation proposes knowledge, it never trusts itself.
//!
//! Recompiling a signature from scratch is also how a derivation is
//! revalidated, so the "compile" and "revalidate" paths cannot drift.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cs_core::{
    AccessContext, ActorRef, Bitemporal, CognitiveEventType, DerivationId, EntityId, Error,
    ExtractionInfo, MemoryId, MemoryRevisionId, Provenance, Result, SourceRef, TimeInterval,
};
use cs_memory::{
    Job, JobKind, MemoryContent, MemoryRecord, MemoryStatus, SemanticMemory,
    derivation::{
        Derivation, DerivationStatus, RECURRING_EPISODE_OPERATOR, Support, Witness, bucket_of,
        origin, origin_key, signature, unit_key,
    },
    entity::{Entity, partition_key, partition_scope},
};
use cs_storage::{DerivationWrite, EdgeDirection, EdgeQuery, JobEffects, MemorySupersede};
use serde_json::json;
use time::OffsetDateTime;

use crate::{
    CognitiveEngine,
    processing::{HandlerError, HandlerResult},
};

/// Edges scanned per signature. A signature with more supporting edges
/// than this is compiled from the first ones in deterministic order.
const MAX_ROOT_EDGES: u32 = 5_000;

/// One root episode, classified.
struct Root {
    memory: MemoryRecord,
    unit: uuid::Uuid,
    origin: uuid::Uuid,
    bucket: i64,
}

impl CognitiveEngine {
    pub(crate) fn compile_job(&self, ctx: &AccessContext, m: &MemoryRecord) -> Job {
        self.new_job(
            ctx,
            JobKind::Compile,
            format!("compile:{}", m.revision_id),
            json!({ "memory_id": m.id, "revision_id": m.revision_id }),
        )
    }

    pub(crate) fn revalidation_job(&self, ctx: &AccessContext, d: &Derivation) -> Job {
        self.new_job(
            ctx,
            JobKind::Compile,
            format!("derivation:{}@{}", d.id, d.version),
            json!({ "derivation_id": d.id }),
        )
    }

    /// Jobs that revalidate every derivation citing `memories` (planned
    /// before a purge, enqueued after it).
    pub(crate) async fn revalidation_jobs(
        &self,
        ctx: &AccessContext,
        memories: &[MemoryId],
    ) -> Result<Vec<Job>> {
        let mut seen = BTreeSet::new();
        let mut jobs = Vec::new();
        for m in memories {
            for d in self.store.derivations_citing(*m).await? {
                if seen.insert(d.id) {
                    // The purge bumps the version; key on the next one.
                    let mut next = d.clone();
                    next.version += 1;
                    jobs.push(self.revalidation_job(ctx, &next));
                }
            }
        }
        Ok(jobs)
    }

    /// `compile` job handler.
    pub(crate) async fn compile(&self, job: &Job) -> HandlerResult {
        let ctx = &job.principal;
        let mut effects = JobEffects::default();
        let mut touched = Vec::new();
        if let Some(id) = job.payload.get("derivation_id") {
            let id: DerivationId = serde_json::from_value(id.clone()).map_err(bad)?;
            if let Some(d) = self.store.get_derivation(id).await? {
                touched.push(self.recompile(ctx, d, &mut effects).await?);
            }
        } else {
            let memory_id: MemoryId =
                serde_json::from_value(job.payload["memory_id"].clone()).map_err(bad)?;
            // Any derivation citing this memory may have lost (or changed)
            // a root: an earlier revision was superseded or tombstoned.
            for d in self.store.derivations_citing(memory_id).await? {
                touched.push(self.recompile(ctx, d, &mut effects).await?);
            }
            if let Some(m) = self.store.current_memory(memory_id).await?
                && let Some((s, rel, o)) = self.signature_of(ctx, &m).await?
            {
                let partition = partition_key(&m.scope);
                let existing = self
                    .store
                    .derivation_for(&partition, RECURRING_EPISODE_OPERATOR, &signature(s, &rel, o))
                    .await?;
                if !existing
                    .as_ref()
                    .is_some_and(|d| effects.derivations.iter().any(|w| w.derivation.id == d.id))
                {
                    touched.push(
                        self.compile_signature(ctx, &m.scope, s, &rel, o, existing, &mut effects)
                            .await?,
                    );
                }
            }
        }
        let touched: Vec<_> = touched.into_iter().flatten().collect();
        Ok((effects, json!({ "derivations": touched })))
    }

    /// The canonical (subject, relation, object) of an active episode with
    /// a structured triple, if its names have been projected.
    async fn signature_of(
        &self,
        ctx: &AccessContext,
        m: &MemoryRecord,
    ) -> Result<Option<(EntityId, String, EntityId)>> {
        let MemoryContent::Episodic(e) = &m.content else {
            return Ok(None);
        };
        let Some((s, p, o)) = e.triple() else {
            return Ok(None);
        };
        if m.status != MemoryStatus::Active || !m.is_current() {
            return Ok(None);
        }
        let partition = partition_key(&m.scope);
        let now = self.now();
        let find = |name: &str| {
            let name = name.to_owned();
            let partition = partition.clone();
            async move {
                let Some(key) = cs_memory::entity::normalize_entity_name(&name) else {
                    return Ok::<_, Error>(None);
                };
                Ok(self
                    .store
                    .entities_by_key(ctx, &key)
                    .await?
                    .into_iter()
                    .find(|x| x.partition == partition)
                    .map(|x| x.id))
            }
        };
        let (Some(s), Some(o), Some(rel)) = (
            find(s).await?,
            find(o).await?,
            cs_memory::entity::normalize_relation(p),
        ) else {
            return Ok(None);
        };
        let (s, o) = (
            self.canonical_as_of(s, now).await?,
            self.canonical_as_of(o, now).await?,
        );
        Ok((s != o).then_some((s, rel, o)))
    }

    /// Revalidate one derivation: recompile its signature with identity
    /// resolved as of now. A derivation whose names were merged into an
    /// already-compiled signature is retired in favour of that one.
    async fn recompile(
        &self,
        ctx: &AccessContext,
        d: Derivation,
        effects: &mut JobEffects,
    ) -> Result<Option<DerivationId>> {
        if effects.derivations.iter().any(|w| w.derivation.id == d.id) {
            return Ok(None);
        }
        let now = self.now();
        let s = self.canonical_as_of(d.subject, now).await?;
        let o = self.canonical_as_of(d.object, now).await?;
        let sig = signature(s, &d.relation, o);
        if sig != d.signature
            && let Some(other) = self
                .store
                .derivation_for(&d.partition, &d.operator, &sig)
                .await?
        {
            self.retire(ctx, d, "superseded by a derivation of the merged names", effects)
                .await?;
            if effects.derivations.iter().any(|w| w.derivation.id == other.id) {
                return Ok(None);
            }
            let scope = other.scope.clone();
            let relation = other.relation.clone();
            return self
                .compile_signature(ctx, &scope, s, &relation, o, Some(other), effects)
                .await;
        }
        if s == o {
            let id = d.id;
            self.retire(ctx, d, "subject and object were merged", effects)
                .await?;
            return Ok(Some(id));
        }
        let scope = d.scope.clone();
        let relation = d.relation.clone();
        self.compile_signature(ctx, &scope, s, &relation, o, Some(d), effects)
            .await
    }

    /// Mark a derivation invalidated and demote its memory.
    async fn retire(
        &self,
        ctx: &AccessContext,
        mut d: Derivation,
        reason: &str,
        effects: &mut JobEffects,
    ) -> Result<()> {
        let expected = d.version;
        d.status = DerivationStatus::Invalidated;
        d.status_reason = Some(reason.to_owned());
        d.version += 1;
        d.updated_at = self.now();
        self.demote(ctx, &d, effects).await?;
        self.derivation_event(ctx, &d, CognitiveEventType::DerivationUpdated, effects);
        effects.derivations.push(DerivationWrite {
            derivation: d,
            expected_version: Some(expected),
        });
        Ok(())
    }

    /// Return a compiled memory that lost its support to `candidate`.
    async fn demote(
        &self,
        ctx: &AccessContext,
        d: &Derivation,
        effects: &mut JobEffects,
    ) -> Result<()> {
        let Some(m) = self.store.current_memory(d.derived_memory_id).await? else {
            return Ok(());
        };
        if m.status != MemoryStatus::Active {
            return Ok(());
        }
        let closed_at = self.next_tx(m.temporal.transaction_time.start);
        let mut next = m.clone();
        next.revision_id = MemoryRevisionId::new_v7();
        next.revision_number = m.revision_number + 1;
        next.supersedes = Some(m.revision_id);
        next.superseded_by = None;
        next.status = MemoryStatus::Candidate;
        next.status_reason = Some(format!("derivation {} lost its support", d.id));
        next.temporal.transaction_time = TimeInterval::from(closed_at);
        next.created_at = closed_at;
        next.validate()?;
        effects.events.push(self.draft(
            ctx,
            m.id.0,
            &m.scope,
            CognitiveEventType::MemoryRevised,
            json!({
                "memory_id": m.id,
                "revision_id": next.revision_id,
                "supersedes": m.revision_id,
                "status": next.status,
                "derivation_id": d.id,
            }),
            closed_at,
        ));
        let (embedding, follow_up) = self.memory_embedding_plan(ctx, &next).await;
        effects.supersedes.push(MemorySupersede {
            current: m.revision_id,
            closed_at,
            next,
            embedding,
            follow_up,
        });
        Ok(())
    }

    fn derivation_event(
        &self,
        ctx: &AccessContext,
        d: &Derivation,
        kind: CognitiveEventType,
        effects: &mut JobEffects,
    ) {
        // Ids and counts only: no names, no content.
        effects.events.push(self.draft(
            ctx,
            d.id.0,
            &d.scope,
            kind,
            json!({
                "derivation_id": d.id,
                "operator": d.operator,
                "derived_memory_id": d.derived_memory_id,
                "status": d.status,
                "support": d.support,
                "witnesses": d.witnesses.iter().map(|w| w.root_revision_id).collect::<Vec<_>>(),
                "version": d.version,
            }),
            d.updated_at,
        ));
    }

    /// Where a root's information came from. Observation sources resolve
    /// to the observation's own source.
    async fn root_origin(&self, m: &MemoryRecord) -> Result<Option<String>> {
        if let SourceRef::Observation { observation_id } = &m.provenance.source {
            return Ok(match self.store.get_observation(*observation_id).await? {
                Some(o) => origin(&o.source),
                None => None,
            });
        }
        Ok(origin(&m.provenance.source))
    }

    /// Gather supporting and counterexample roots for `subject —relation→`.
    async fn roots(
        &self,
        ctx: &AccessContext,
        subject: EntityId,
        relation: &str,
        object: EntityId,
    ) -> Result<(Vec<Root>, Vec<Root>)> {
        let now = self.now();
        let t = self.processing.compilation;
        let members = self.members(subject, Some(now)).await?;
        let edges = self
            .store
            .entity_edges(&EdgeQuery {
                access: ctx.clone(),
                entity_ids: members,
                direction: EdgeDirection::Out,
                relations: Some(vec![relation.to_owned()]),
                as_of_transaction_time: now,
                as_of_valid_time: None,
                include_candidates: false,
                limit: MAX_ROOT_EDGES,
            })
            .await?;
        let mut canon: HashMap<EntityId, EntityId> = HashMap::new();
        let (mut support, mut counter) = (Vec::new(), Vec::new());
        let mut seen = BTreeSet::new();
        for e in edges {
            if !seen.insert(e.revision_id) {
                continue;
            }
            let Some(m) = self.store.current_memory(e.memory_id).await? else {
                continue;
            };
            // Only validated episodes, and never anything derived.
            if m.revision_id != e.revision_id
                || m.status != MemoryStatus::Active
                || !matches!(m.content, MemoryContent::Episodic(_))
            {
                continue;
            }
            let Some(origin) = self.root_origin(&m).await? else {
                continue;
            };
            let to = match canon.get(&e.to) {
                Some(c) => *c,
                None => {
                    let c = self.canonical_as_of(e.to, now).await?;
                    canon.insert(e.to, c);
                    c
                }
            };
            let actor: &ActorRef = &m.provenance.actor;
            let bucket = bucket_of(m.temporal.valid_time.start, t.bucket_seconds);
            let root = Root {
                unit: unit_key(actor, &origin, bucket),
                origin: origin_key(actor, &origin),
                bucket,
                memory: m,
            };
            if to == object {
                support.push(root);
            } else if to != subject {
                counter.push(root);
            }
        }
        Ok((support, counter))
    }

    /// Compile (or revalidate) one signature, adding the writes to
    /// `effects`. Returns the derivation written, if any.
    #[allow(clippy::too_many_arguments)]
    async fn compile_signature(
        &self,
        ctx: &AccessContext,
        scope: &cs_core::Scope,
        subject: EntityId,
        relation: &str,
        object: EntityId,
        existing: Option<Derivation>,
        effects: &mut JobEffects,
    ) -> Result<Option<DerivationId>> {
        let now = self.now();
        let t = self.processing.compilation;
        let (support_roots, counter_roots) = self.roots(ctx, subject, relation, object).await?;

        // One representative per unit: the lowest revision id.
        let mut units: BTreeMap<uuid::Uuid, &Root> = BTreeMap::new();
        for r in &support_roots {
            units
                .entry(r.unit)
                .and_modify(|cur| {
                    if r.memory.revision_id < cur.memory.revision_id {
                        *cur = r;
                    }
                })
                .or_insert(r);
        }
        let mut witnesses: Vec<Witness> = units
            .values()
            .map(|r| Witness {
                root_memory_id: r.memory.id,
                root_revision_id: r.memory.revision_id,
                unit_key: r.unit,
                origin_key: r.origin,
                bucket: r.bucket,
            })
            .collect();
        witnesses.sort_by(|a, b| (a.bucket, a.unit_key).cmp(&(b.bucket, b.unit_key)));
        let counter_units: BTreeSet<uuid::Uuid> = counter_roots.iter().map(|r| r.unit).collect();
        let support = Support::of(
            &witnesses,
            u32::try_from(counter_units.len()).unwrap_or(u32::MAX),
        );
        let mut roots: Vec<MemoryId> = support_roots.iter().map(|r| r.memory.id).collect();
        roots.sort();
        roots.dedup();
        let meets = support.meets(&t);
        let sig = signature(subject, relation, object);

        match existing {
            None if !meets => Ok(None),
            None => {
                let (s, o) = (self.entity(subject).await?, self.entity(object).await?);
                let id = DerivationId::new_v7();
                let valid_from = support_roots
                    .iter()
                    .map(|r| r.memory.temporal.valid_time.start)
                    .min()
                    .unwrap_or(now);
                let memory =
                    self.compiled_memory(id, scope, &s, relation, &o, valid_from, &support, now)?;
                let d = Derivation {
                    id,
                    scope: partition_scope(scope),
                    partition: partition_key(scope),
                    operator: RECURRING_EPISODE_OPERATOR.to_owned(),
                    signature: sig,
                    subject,
                    relation: relation.to_owned(),
                    object,
                    derived_memory_id: memory.id,
                    thresholds: t,
                    support,
                    witnesses,
                    roots,
                    status: DerivationStatus::Active,
                    status_reason: None,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                };
                effects.events.push(self.draft(
                    ctx,
                    memory.id.0,
                    &memory.scope,
                    CognitiveEventType::MemoryCreated,
                    json!({
                        "memory_id": memory.id,
                        "revision_id": memory.revision_id,
                        "domain": memory.domain(),
                        "status": memory.status,
                        "derivation_id": id,
                    }),
                    now,
                ));
                let (embedding, jobs) = self.memory_embedding_plan(ctx, &memory).await;
                effects.jobs.extend(jobs);
                effects.memories.push((memory, embedding));
                self.derivation_event(ctx, &d, CognitiveEventType::DerivationRecorded, effects);
                effects.derivations.push(DerivationWrite {
                    derivation: d,
                    expected_version: None,
                });
                Ok(Some(id))
            }
            Some(mut d) => {
                let (status, reason) = if meets {
                    (DerivationStatus::Active, None)
                } else if support.counter_ratio() > t.max_counter_ratio {
                    (
                        DerivationStatus::Invalidated,
                        Some("counterexamples exceed the tolerated share".to_owned()),
                    )
                } else {
                    (
                        DerivationStatus::Invalidated,
                        Some("independent support fell below the thresholds".to_owned()),
                    )
                };
                let unchanged = d.status == status
                    && d.signature == sig
                    && d.support == support
                    && d.witnesses == witnesses
                    && d.roots == roots;
                if unchanged {
                    return Ok(None);
                }
                let expected = d.version;
                d.signature = sig;
                d.subject = subject;
                d.object = object;
                d.support = support;
                d.witnesses = witnesses;
                d.roots = roots;
                d.status = status;
                d.status_reason = reason;
                d.version += 1;
                d.updated_at = now;
                if status == DerivationStatus::Invalidated {
                    self.demote(ctx, &d, effects).await?;
                }
                self.derivation_event(ctx, &d, CognitiveEventType::DerivationUpdated, effects);
                let id = d.id;
                effects.derivations.push(DerivationWrite {
                    derivation: d,
                    expected_version: Some(expected),
                });
                Ok(Some(id))
            }
        }
    }

    async fn entity(&self, id: EntityId) -> Result<Entity> {
        self.store
            .get_entity(id)
            .await?
            .ok_or_else(|| Error::not_found("entity", id))
    }

    #[allow(clippy::too_many_arguments)]
    fn compiled_memory(
        &self,
        derivation: DerivationId,
        scope: &cs_core::Scope,
        subject: &Entity,
        relation: &str,
        object: &Entity,
        valid_from: OffsetDateTime,
        support: &Support,
        now: OffsetDateTime,
    ) -> Result<MemoryRecord> {
        let predicate = relation.replace('_', " ");
        let m = MemoryRecord {
            id: MemoryId::new_v7(),
            revision_id: MemoryRevisionId::new_v7(),
            revision_number: 1,
            scope: partition_scope(scope),
            content: MemoryContent::Semantic(SemanticMemory {
                proposition: format!(
                    "{} {predicate} {}",
                    subject.canonical_name, object.canonical_name
                ),
                subject: Some(subject.canonical_name.clone()),
                predicate: Some(predicate),
                object: Some(object.canonical_name.clone()),
            }),
            provenance: Provenance {
                source: SourceRef::SystemDerived {
                    derivation: format!("{RECURRING_EPISODE_OPERATOR}:{derivation}"),
                },
                actor: ActorRef::System {
                    component: "compiler".into(),
                },
                observed_at: now,
                recorded_at: now,
                extraction: Some(ExtractionInfo {
                    model: None,
                    model_version: None,
                    prompt_version: None,
                    extractor_version: RECURRING_EPISODE_OPERATOR.to_owned(),
                }),
                parent_evidence: Vec::new(),
                corroborations: Vec::new(),
            },
            temporal: Bitemporal {
                valid_time: TimeInterval {
                    start: valid_from,
                    end: None,
                },
                transaction_time: TimeInterval::from(now),
            },
            // Compilation proposes; promotion is an explicit decision.
            status: MemoryStatus::Candidate,
            status_reason: Some(format!(
                "compiled from {} independent episodes ({} origins)",
                support.units, support.origins
            )),
            supersedes: None,
            superseded_by: None,
            created_at: now,
            expires_at: None,
        };
        m.validate()?;
        Ok(m)
    }

    // ---- API ---------------------------------------------------------------

    pub async fn get_derivation(&self, ctx: &AccessContext, id: DerivationId) -> Result<Derivation> {
        self.store
            .get_derivation(id)
            .await?
            .filter(|d| ctx.can_read(&d.scope))
            .ok_or_else(|| Error::not_found("derivation", id))
    }

    pub async fn list_derivations(
        &self,
        ctx: &AccessContext,
        status: Option<DerivationStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<Derivation>> {
        self.store
            .list_derivations(ctx, status, limit.unwrap_or(100).min(1000))
            .await
    }

    /// Queue a revalidation of one derivation.
    pub async fn revalidate_derivation(&self, ctx: &AccessContext, id: DerivationId) -> Result<Job> {
        let d = self.get_derivation(ctx, id).await?;
        ctx.ensure_write(&d.scope)?;
        self.store
            .enqueue_job(&self.revalidation_job(ctx, &d))
            .await
    }
}

fn bad(e: serde_json::Error) -> HandlerError {
    HandlerError {
        permanent: true,
        message: format!("bad payload: {e}"),
    }
}
