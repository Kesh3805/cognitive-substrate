//! Governed cognitive processing (M3): observations, perception, resolution
//! proposals, and purge workflows.
//!
//! Background handlers return their writes as [`JobEffects`]; the worker
//! commits them together with the fenced job transition, so a worker that
//! lost its lease can never apply effects.

use cs_core::{
    AccessContext, ActorRef, BeliefId, Bitemporal, CognitiveEventType, Error, EvidenceId,
    ExtractionInfo, JobId, MemoryId, MemoryRevisionId, ObservationId, ProposalId, Provenance,
    Result, Scope, SourceRef, TimeInterval, temporal::truncate_micros,
};
use cs_memory::{
    Belief, BeliefStatus, ExtractionRun, Job, JobKind, JobStatus, MemoryDomain, MemoryRecord,
    MemoryStatus, Observation, ProposalDecision, ProposalStatus, RecordVersion,
    ResolutionOperation, ResolutionProposal, ResolutionRationale, RetryPolicy,
    observation::MAX_OBSERVATION_BYTES, proposal::fingerprint,
};
use cs_policy::{ProposalDraft, ResolutionConfig, resolve::DETECTOR_VERSION};
use cs_storage::{
    BeliefQuery, BeliefSupersede, IdempotencyKey, JobEffects, MemoryQuery, MemorySupersede,
    ProposalMutations, PurgeKind,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    CognitiveEngine, Decision, ExtractError, Extractor, NewObservation, ObservationView,
    PurgeReport, actor,
};

/// Knobs for background processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessingConfig {
    /// Enqueue a resolution job whenever a memory or belief revision is
    /// written.
    pub auto_resolve: bool,
    pub resolution: ResolutionConfig,
    pub retry: RetryPolicy,
    /// Embed new revisions inline (best effort) before writing. When false,
    /// every write defers its vector to a backfill job (recommended for
    /// remote embedders with significant latency).
    pub embed_inline: bool,
    /// Candidates compared per resolution job.
    pub resolution_window: u32,
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            auto_resolve: false,
            resolution: ResolutionConfig::default(),
            retry: RetryPolicy::default(),
            embed_inline: true,
            resolution_window: 500,
        }
    }
}

/// A handler failure, classified for retry.
#[derive(Debug, Clone)]
pub struct HandlerError {
    pub permanent: bool,
    pub message: String,
}

impl HandlerError {
    fn permanent(m: impl Into<String>) -> Self {
        Self {
            permanent: true,
            message: m.into(),
        }
    }
}

impl From<Error> for HandlerError {
    fn from(e: Error) -> Self {
        // Storage failures may be transient; everything else is not.
        Self {
            permanent: !matches!(e, Error::Storage(_)),
            message: e.to_string(),
        }
    }
}

impl From<ExtractError> for HandlerError {
    fn from(e: ExtractError) -> Self {
        match e {
            ExtractError::Retryable(m) => Self {
                permanent: false,
                message: m,
            },
            ExtractError::Permanent(m) => Self {
                permanent: true,
                message: m,
            },
        }
    }
}

pub(crate) type HandlerResult = std::result::Result<(JobEffects, serde_json::Value), HandlerError>;

impl CognitiveEngine {
    pub fn processing_config(&self) -> &ProcessingConfig {
        &self.processing
    }

    pub(crate) fn new_job(
        &self,
        ctx: &AccessContext,
        kind: JobKind,
        dedupe_key: String,
        payload: serde_json::Value,
    ) -> Job {
        let now = self.now();
        Job {
            id: JobId::new_v7(),
            tenant_id: ctx.tenant_id,
            kind,
            principal: ctx.clone(),
            payload,
            dedupe_key,
            status: JobStatus::Queued,
            attempts: 0,
            max_attempts: self.processing.retry.max_attempts,
            available_at: now,
            lease_owner: None,
            lease_until: None,
            lease_generation: 0,
            last_error: None,
            result: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// A perception job for an observation (used for explicit reprocessing).
    pub fn new_perceive_job(
        &self,
        ctx: &AccessContext,
        id: ObservationId,
        dedupe_key: &str,
    ) -> Job {
        self.new_job(
            ctx,
            JobKind::Perceive,
            dedupe_key.to_owned(),
            json!({ "observation_id": id }),
        )
    }

    // ======================================================================
    // Observations & perception
    // ======================================================================

    /// Store an immutable observation and enqueue its perception job in one
    /// transaction.
    pub async fn submit_observation(
        &self,
        ctx: &AccessContext,
        req: NewObservation,
    ) -> Result<ObservationView> {
        ctx.ensure_write(&req.scope)?;
        if req.content.len() > MAX_OBSERVATION_BYTES {
            return Err(Error::validation("observation content exceeds 64 KiB"));
        }
        if let Some(key) = &req.idempotency_key {
            if key.is_empty() || key.len() > 255 {
                return Err(Error::validation("idempotency_key must be 1..=255 bytes"));
            }
            if let Some(existing) = self
                .store
                .idempotent_lookup(ctx.tenant_id, "observation", key)
                .await?
            {
                return self.get_observation(ctx, ObservationId(existing)).await;
            }
        }
        let now = self.now();
        let observation = Observation {
            id: ObservationId::new_v7(),
            scope: req.scope,
            content_hash: cs_policy::query_hash(&req.content),
            content: Some(req.content),
            media_type: req.media_type,
            source: req.source,
            actor: actor(ctx),
            observed_at: req.observed_at.map_or(now, truncate_micros),
            recorded_at: now,
            purged_at: None,
        };
        observation.validate()?;
        let job = self.new_job(
            ctx,
            JobKind::Perceive,
            format!("observation:{}", observation.id),
            json!({ "observation_id": observation.id }),
        );
        job.validate()?;
        let event = self.draft(
            ctx,
            observation.id.0,
            &observation.scope,
            CognitiveEventType::ObservationRecorded,
            json!({
                "observation_id": observation.id,
                "content_hash": observation.content_hash,
                "job_id": job.id,
            }),
            now,
        );
        let key = req.idempotency_key.as_deref().map(|key| IdempotencyKey {
            tenant_id: ctx.tenant_id,
            kind: "observation",
            key,
        });
        match self
            .store
            .insert_observation(&observation, &job, key, event)
            .await
        {
            Ok(_) => {}
            Err(Error::Conflict(_)) if key.is_some() => {
                let k = key.map(|k| k.key).unwrap_or_default();
                if let Some(id) = self
                    .store
                    .idempotent_lookup(ctx.tenant_id, "observation", k)
                    .await?
                {
                    return self.get_observation(ctx, ObservationId(id)).await;
                }
                return Err(Error::conflict("concurrent observation submit"));
            }
            Err(e) => return Err(e),
        }
        Ok(ObservationView {
            observation,
            job: Some(job),
            extraction_runs: Vec::new(),
        })
    }

    pub async fn get_observation(
        &self,
        ctx: &AccessContext,
        id: ObservationId,
    ) -> Result<ObservationView> {
        let observation = self
            .store
            .get_observation(id)
            .await?
            .filter(|o| ctx.can_read(&o.scope))
            .ok_or_else(|| Error::not_found("observation", id))?;
        let jobs = self.store.list_jobs(ctx.tenant_id, None, 10_000).await?;
        let key = format!("observation:{id}");
        let job = jobs
            .into_iter()
            .find(|j| j.kind == JobKind::Perceive && j.dedupe_key == key);
        Ok(ObservationView {
            extraction_runs: self.store.extraction_runs(id).await?,
            observation,
            job,
        })
    }

    /// Perception handler: extract candidate memories from an observation.
    pub(crate) async fn perceive(&self, job: &Job, extractor: &dyn Extractor) -> HandlerResult {
        let id: ObservationId = serde_json::from_value(job.payload["observation_id"].clone())
            .map_err(|e| HandlerError::permanent(format!("bad payload: {e}")))?;
        let ctx = &job.principal;
        let Some(observation) = self.store.get_observation(id).await? else {
            return Ok((
                JobEffects::default(),
                json!({ "skipped": "observation_missing" }),
            ));
        };
        if !ctx.can_read(&observation.scope) {
            return Err(HandlerError::permanent(
                "principal can no longer read the observation",
            ));
        }
        if observation.is_purged() {
            return Ok((
                JobEffects::default(),
                json!({ "skipped": "observation_purged" }),
            ));
        }
        // Idempotent per (observation, extractor, version).
        let runs = self.store.extraction_runs(id).await?;
        if let Some(run) = runs
            .iter()
            .find(|r| r.extractor == extractor.name() && r.extractor_version == extractor.version())
        {
            return Ok((
                JobEffects::default(),
                json!({ "skipped": "already_extracted", "candidates": run.candidate_ids }),
            ));
        }

        let extracted = extractor.extract(&observation).await?;
        let now = self.now();
        let mut effects = JobEffects {
            // The observation may be purged while a slow extractor runs.
            require_unpurged_observation: Some(id),
            ..JobEffects::default()
        };
        let mut ids = Vec::new();
        for c in extracted {
            if c.content.domain() == MemoryDomain::Working {
                return Err(HandlerError::permanent(
                    "extractors may not produce working memory",
                ));
            }
            let record = MemoryRecord {
                id: MemoryId::new_v7(),
                revision_id: MemoryRevisionId::new_v7(),
                revision_number: 1,
                scope: observation.scope.clone(),
                content: c.content,
                provenance: Provenance {
                    source: SourceRef::Observation { observation_id: id },
                    actor: ActorRef::System {
                        component: format!("perception:{}", extractor.name()),
                    },
                    observed_at: observation.observed_at,
                    recorded_at: now,
                    extraction: Some(ExtractionInfo {
                        model: extractor.model().map(str::to_owned),
                        model_version: None,
                        prompt_version: None,
                        extractor_version: format!("{}@{}", extractor.name(), extractor.version()),
                    }),
                    parent_evidence: Vec::new(),
                    corroborations: Vec::new(),
                },
                temporal: Bitemporal {
                    valid_time: TimeInterval::from(observation.observed_at),
                    transaction_time: TimeInterval::from(now),
                },
                status: MemoryStatus::Candidate,
                status_reason: c.rationale,
                supersedes: None,
                superseded_by: None,
                created_at: now,
                expires_at: None,
            };
            // Untrusted output: one invalid candidate rejects the run, so
            // nothing partial is ever written.
            record
                .validate()
                .map_err(|e| HandlerError::permanent(format!("invalid extraction: {e}")))?;
            effects.events.push(self.draft(
                ctx,
                record.id.0,
                &record.scope,
                CognitiveEventType::MemoryCreated,
                json!({
                    "memory_id": record.id,
                    "revision_id": record.revision_id,
                    "domain": record.domain(),
                    "status": record.status,
                    "observation_id": id,
                }),
                now,
            ));
            ids.push(record.id);
            if self.processing.auto_resolve {
                effects.jobs.push(self.new_job(
                    ctx,
                    JobKind::Resolve,
                    format!("memory:{}", record.revision_id),
                    json!({ "memory_id": record.id }),
                ));
            }
            let (embedding, follow_up) = self.memory_embedding_plan(ctx, &record).await;
            effects.jobs.extend(follow_up);
            effects.memories.push((record, embedding));
        }
        effects.extraction_run = Some(ExtractionRun {
            observation_id: id,
            extractor: extractor.name().to_owned(),
            extractor_version: extractor.version().to_owned(),
            candidate_ids: ids.clone(),
            completed_at: now,
        });
        effects.events.push(self.draft(
            ctx,
            id.0,
            &observation.scope,
            CognitiveEventType::ExtractionCompleted,
            json!({
                "observation_id": id,
                "extractor": extractor.name(),
                "extractor_version": extractor.version(),
                "candidates": ids,
            }),
            now,
        ));
        Ok((effects, json!({ "candidates": ids })))
    }

    // ======================================================================
    // Resolution
    // ======================================================================

    /// Queue resolution for a memory's current revision.
    pub async fn request_memory_resolution(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
    ) -> Result<Job> {
        let m = self.get_memory(ctx, id).await?;
        let job = self.new_job(
            ctx,
            JobKind::Resolve,
            format!("memory:{}", m.revision_id),
            json!({ "memory_id": id }),
        );
        self.store.enqueue_job(&job).await
    }

    /// Queue resolution for a belief's current revision.
    pub async fn request_belief_resolution(
        &self,
        ctx: &AccessContext,
        id: BeliefId,
    ) -> Result<Job> {
        let b = self.current_of(ctx, id).await?;
        let job = self.new_job(
            ctx,
            JobKind::Resolve,
            format!("belief:{}", b.id),
            json!({ "belief_id": b.lineage_id }),
        );
        self.store.enqueue_job(&job).await
    }

    pub(crate) async fn auto_resolve_memory(&self, ctx: &AccessContext, m: &MemoryRecord) {
        if self.processing.auto_resolve
            && let Err(e) = self.request_memory_resolution(ctx, m.id).await
        {
            tracing::warn!(error = %e, memory = %m.id, "failed to enqueue resolution");
        }
    }

    pub(crate) async fn auto_resolve_belief(&self, ctx: &AccessContext, b: &Belief) {
        if self.processing.auto_resolve
            && let Err(e) = self.request_belief_resolution(ctx, b.lineage_id).await
        {
            tracing::warn!(error = %e, belief = %b.id, "failed to enqueue resolution");
        }
    }

    fn proposal_from(&self, scope: &Scope, d: ProposalDraft) -> ResolutionProposal {
        let now = self.now();
        ResolutionProposal {
            id: ProposalId::new_v7(),
            scope: scope.clone(),
            kind: d.kind,
            status: ProposalStatus::Pending,
            fingerprint: fingerprint(d.kind, &d.based_on),
            operation: d.operation,
            rationale: ResolutionRationale {
                explanation: d.explanation,
                signals: d.signals,
            },
            detection: cs_memory::DetectionMetadata {
                detector_version: DETECTOR_VERSION.to_owned(),
                score: d.score,
                detected_at: now,
            },
            based_on: d.based_on,
            created_at: now,
            decided_at: None,
            decided_by: None,
            decision_reason: None,
        }
    }

    /// Resolution handler: detect and record proposals. Never mutates.
    pub(crate) async fn resolve(&self, job: &Job) -> HandlerResult {
        let ctx = &job.principal;
        let now = self.now();
        let drafts: Vec<(Scope, ProposalDraft)> = if let Some(v) = job.payload.get("memory_id") {
            let id: MemoryId = serde_json::from_value(v.clone())
                .map_err(|e| HandlerError::permanent(format!("bad payload: {e}")))?;
            let Some(target) = self
                .store
                .current_memory(id)
                .await?
                .filter(|m| ctx.can_read(&m.scope))
            else {
                return Ok((
                    JobEffects::default(),
                    json!({ "skipped": "memory_missing" }),
                ));
            };
            let model = self.space_key();
            // Compare across all valid time: the detector itself decides
            // overlap (duplicates, contradictions) or ordering
            // (supersession).
            let window = self
                .store
                .query_memories(&MemoryQuery {
                    access: ctx.clone(),
                    domains: vec![target.domain()],
                    statuses: vec![MemoryStatus::Active, MemoryStatus::Candidate],
                    scope_filter: None,
                    as_of_valid_time: now,
                    as_of_transaction_time: now,
                    include_superseded: false,
                    ignore_valid_time: true,
                    embedding_model: model,
                    limit: self.processing.resolution_window,
                })
                .await?;
            // The target's own stored vector in the active space, if any;
            // without one, only the structural rules apply (the embed job
            // re-queues resolution once the vector lands).
            let target_embedding: Option<Vec<f32>> = window
                .iter()
                .find(|s| s.record.revision_id == target.revision_id)
                .and_then(|s| s.embedding.clone());
            let others: Vec<cs_policy::resolve::Candidate<'_>> = window
                .iter()
                .map(|s| cs_policy::resolve::Candidate {
                    record: &s.record,
                    embedding: s.embedding.as_deref(),
                })
                .collect();
            let model_key = self.active_space();
            cs_policy::detect_memory(
                &self.processing.resolution,
                model_key.as_deref(),
                cs_policy::resolve::Candidate {
                    record: &target,
                    embedding: target_embedding.as_deref(),
                },
                &others,
            )
            .into_iter()
            .map(|d| (target.scope.clone(), d))
            .collect()
        } else if let Some(v) = job.payload.get("belief_id") {
            let lineage: BeliefId = serde_json::from_value(v.clone())
                .map_err(|e| HandlerError::permanent(format!("bad payload: {e}")))?;
            let Some(target) = self
                .store
                .current_belief(lineage)
                .await?
                .filter(|b| ctx.can_read(&b.scope))
            else {
                return Ok((
                    JobEffects::default(),
                    json!({ "skipped": "belief_missing" }),
                ));
            };
            let others = self
                .store
                .query_beliefs(&BeliefQuery {
                    access: ctx.clone(),
                    statuses: vec![BeliefStatus::Active, BeliefStatus::Disputed],
                    scope_filter: None,
                    as_of_valid_time: now,
                    as_of_transaction_time: now,
                    include_superseded: false,
                    ignore_valid_time: true,
                    embedding_model: None,
                    limit: self.processing.resolution_window,
                })
                .await?
                .into_iter()
                .map(|s| s.belief)
                .collect::<Vec<_>>();
            let disputed = |v: &RecordVersion| {
                others.iter().chain(std::iter::once(&target)).any(|b| {
                    *v == RecordVersion::Belief {
                        lineage_id: b.lineage_id,
                        revision_id: b.id,
                    } && b.status == BeliefStatus::Disputed
                })
            };
            cs_policy::detect_beliefs(&target, &others)
                .into_iter()
                // Nothing to propose if every belief is already disputed.
                .filter(|d| !d.based_on.iter().all(disputed))
                .map(|d| (target.scope.clone(), d))
                .collect()
        } else {
            return Err(HandlerError::permanent(
                "resolve job needs memory_id or belief_id",
            ));
        };

        let mut effects = JobEffects::default();
        let mut ids = Vec::new();
        for (scope, d) in drafts {
            let p = self.proposal_from(&scope, d);
            p.validate()?;
            effects.events.push(self.draft(
                ctx,
                p.id.0,
                &p.scope,
                CognitiveEventType::ProposalCreated,
                json!({
                    "proposal_id": p.id,
                    "kind": p.kind,
                    "fingerprint": p.fingerprint,
                    "based_on": p.based_on,
                }),
                now,
            ));
            ids.push(p.id);
            for v in &p.based_on {
                if !effects.require_current.contains(v) {
                    effects.require_current.push(*v);
                }
            }
            effects.proposals.push(p);
        }
        Ok((effects, json!({ "proposals": ids })))
    }

    pub async fn get_proposal(
        &self,
        ctx: &AccessContext,
        id: ProposalId,
    ) -> Result<ResolutionProposal> {
        self.store
            .get_proposal(id)
            .await?
            .filter(|p| ctx.can_read(&p.scope))
            .ok_or_else(|| Error::not_found("proposal", id))
    }

    pub async fn list_proposals(
        &self,
        ctx: &AccessContext,
        status: Option<ProposalStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<ResolutionProposal>> {
        self.store
            .list_proposals(ctx, status, limit.unwrap_or(100).min(1000))
            .await
    }

    fn decision(
        &self,
        ctx: &AccessContext,
        status: ProposalStatus,
        reason: &str,
    ) -> Result<ProposalDecision> {
        if reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        Ok(ProposalDecision {
            status,
            decided_at: self.now(),
            decided_by: actor(ctx),
            reason: reason.to_owned(),
        })
    }

    /// Reject a proposal. Never touches its targets.
    pub async fn reject_proposal(
        &self,
        ctx: &AccessContext,
        id: ProposalId,
        req: Decision,
    ) -> Result<ResolutionProposal> {
        let p = self.get_proposal(ctx, id).await?;
        ctx.ensure_write(&p.scope)?;
        if p.status == ProposalStatus::Rejected {
            return Ok(p); // idempotent
        }
        let decision = self.decision(ctx, ProposalStatus::Rejected, &req.reason)?;
        let event = self.draft(
            ctx,
            id.0,
            &p.scope,
            CognitiveEventType::ProposalRejected,
            json!({ "proposal_id": id, "reason": req.reason }),
            decision.decided_at,
        );
        self.store
            .decide_proposal(id, &decision, ProposalMutations::default(), vec![event])
            .await
    }

    /// Accept a proposal: verify its target revisions are still current,
    /// then apply its operation and record the decision atomically. If any
    /// target changed, the proposal becomes `stale` and nothing is applied.
    pub async fn accept_proposal(
        &self,
        ctx: &AccessContext,
        id: ProposalId,
        req: Decision,
    ) -> Result<ResolutionProposal> {
        let p = self.get_proposal(ctx, id).await?;
        ctx.ensure_write(&p.scope)?;
        match p.status {
            ProposalStatus::Accepted => return Ok(p), // idempotent
            ProposalStatus::Pending => {}
            other => {
                return Err(Error::conflict(format!("proposal is {}", other.as_str())));
            }
        }

        // Re-check every revision the proposal was computed against.
        let mut memories = std::collections::BTreeMap::new();
        let mut beliefs = std::collections::BTreeMap::new();
        let mut stale = false;
        for v in &p.based_on {
            match *v {
                RecordVersion::Memory { id, revision_id } => {
                    match self.store.current_memory(id).await? {
                        Some(m) if m.revision_id == revision_id => {
                            ctx.ensure_write(&m.scope)?;
                            memories.insert(id, m);
                        }
                        _ => stale = true,
                    }
                }
                RecordVersion::Belief {
                    lineage_id,
                    revision_id,
                } => match self.store.current_belief(lineage_id).await? {
                    Some(b) if b.id == revision_id => {
                        ctx.ensure_write(&b.scope)?;
                        beliefs.insert(lineage_id, b);
                    }
                    _ => stale = true,
                },
            }
        }
        if stale {
            let decision = self.decision(ctx, ProposalStatus::Stale, &req.reason)?;
            let event = self.draft(
                ctx,
                id.0,
                &p.scope,
                CognitiveEventType::ProposalStale,
                json!({ "proposal_id": id }),
                decision.decided_at,
            );
            self.store
                .decide_proposal(id, &decision, ProposalMutations::default(), vec![event])
                .await?;
            return Err(Error::conflict(
                "proposal is stale: a target changed after detection",
            ));
        }

        let decision = self.decision(ctx, ProposalStatus::Accepted, &req.reason)?;
        let at = decision.decided_at;
        let mut mutations = ProposalMutations::default();
        let mut events = Vec::new();
        let reason = format!("proposal {id}: {}", req.reason);

        let mut revise = |m: &MemoryRecord,
                          status: MemoryStatus,
                          valid_end: Option<time::OffsetDateTime>,
                          provenance: Option<Provenance>,
                          event_type: CognitiveEventType|
         -> Result<()> {
            let closed_at = self.next_tx(m.temporal.transaction_time.start).max(at);
            let mut next = m.clone();
            next.revision_id = MemoryRevisionId::new_v7();
            next.revision_number = m.revision_number + 1;
            next.supersedes = Some(m.revision_id);
            next.superseded_by = None;
            next.status = status;
            next.status_reason = Some(reason.clone());
            if let Some(end) = valid_end {
                next.temporal.valid_time.end = Some(end);
            }
            if let Some(p) = provenance {
                next.provenance = p;
            }
            next.temporal.transaction_time = TimeInterval::from(closed_at);
            next.created_at = closed_at;
            next.validate()?;
            events.push(self.draft(
                ctx,
                m.id.0,
                &m.scope,
                event_type,
                json!({
                    "memory_id": m.id,
                    "revision_id": next.revision_id,
                    "supersedes": m.revision_id,
                    "status": next.status,
                    "proposal_id": id,
                }),
                closed_at,
            ));
            mutations.memories.push(MemorySupersede {
                current: m.revision_id,
                closed_at,
                embedding: None,
                follow_up: Vec::new(),
                next,
            });
            Ok(())
        };

        let get = |mid: &MemoryId| {
            memories
                .get(mid)
                .ok_or_else(|| Error::validation("operation names a record outside based_on"))
        };
        match &p.operation {
            ResolutionOperation::Merge {
                canonical,
                duplicate,
            } => {
                // Provenance union: the canonical record absorbs the
                // duplicate's sources and evidence, and its recency is
                // refreshed, so purging either source alone loses nothing
                // the other corroborates.
                let (c, d) = (get(canonical)?, get(duplicate)?);
                let mut p = c.provenance.clone();
                p.observed_at = p.observed_at.max(d.provenance.observed_at);
                for e in &d.provenance.parent_evidence {
                    if !p.parent_evidence.contains(e) {
                        p.parent_evidence.push(*e);
                    }
                }
                for s in std::iter::once(&d.provenance.source).chain(&d.provenance.corroborations) {
                    if *s != p.source && !p.corroborations.contains(s) {
                        p.corroborations.push(s.clone());
                    }
                }
                revise(
                    c,
                    c.status,
                    None,
                    Some(p),
                    CognitiveEventType::MemoryRevised,
                )?;
                revise(
                    d,
                    MemoryStatus::Tombstoned,
                    None,
                    None,
                    CognitiveEventType::MemoryTombstoned,
                )?;
            }
            ResolutionOperation::Supersede { older, newer } => {
                let (o, n) = (get(older)?, get(newer)?);
                let end = n.temporal.valid_time.start;
                if end <= o.temporal.valid_time.start {
                    return Err(Error::validation(
                        "newer fact does not start after the older one",
                    ));
                }
                revise(
                    o,
                    o.status,
                    Some(end),
                    None,
                    CognitiveEventType::MemoryRevised,
                )?;
            }
            ResolutionOperation::Quarantine { memories: ids } => {
                for mid in ids {
                    revise(
                        get(mid)?,
                        MemoryStatus::Candidate,
                        None,
                        None,
                        CognitiveEventType::MemoryRevised,
                    )?;
                }
            }
            ResolutionOperation::Dispute { beliefs: lineages } => {
                for l in lineages {
                    let b = beliefs.get(l).ok_or_else(|| {
                        Error::validation("operation names a belief outside based_on")
                    })?;
                    if b.status == BeliefStatus::Disputed {
                        continue;
                    }
                    let closed_at = self.next_tx(b.temporal.transaction_time.start).max(at);
                    let mut next = b.clone();
                    next.id = BeliefId::new_v7();
                    next.revision_number = b.revision_number + 1;
                    next.supersedes = Some(b.id);
                    next.superseded_by = None;
                    next.status = BeliefStatus::Disputed;
                    next.revision_reason = reason.clone();
                    next.temporal.transaction_time = TimeInterval::from(closed_at);
                    next.created_at = closed_at;
                    next.validate()?;
                    let evidence = self.store.belief_evidence(b.id).await?;
                    let dependencies = self
                        .store
                        .dependencies_of(b.id)
                        .await?
                        .into_iter()
                        .map(|d| cs_memory::BeliefDependency {
                            to: next.id,
                            created_at: closed_at,
                            ..d
                        })
                        .collect();
                    events.push(self.draft(
                        ctx,
                        b.lineage_id.0,
                        &b.scope,
                        CognitiveEventType::BeliefDisputed,
                        json!({ "belief_id": next.id, "supersedes": b.id, "proposal_id": id }),
                        closed_at,
                    ));
                    mutations.beliefs.push(BeliefSupersede {
                        current: b.id,
                        closed_at,
                        next,
                        evidence,
                        dependencies,
                        embedding: None,
                        follow_up: Vec::new(),
                    });
                }
            }
        }
        events.push(self.draft(
            ctx,
            id.0,
            &p.scope,
            CognitiveEventType::ProposalAccepted,
            json!({ "proposal_id": id, "operation": p.operation }),
            at,
        ));
        // Embed the new revisions outside the transaction; failures become
        // backfill jobs committed with the decision.
        for m in &mut mutations.memories {
            let (e, jobs) = self.memory_embedding_plan(ctx, &m.next).await;
            m.embedding = e;
            m.follow_up = jobs;
        }
        for b in &mut mutations.beliefs {
            let (e, jobs) = self.belief_embedding_plan(ctx, &b.next).await;
            b.embedding = e;
            b.follow_up = jobs;
        }
        self.store
            .decide_proposal(id, &decision, mutations, events)
            .await
    }

    // ======================================================================
    // Governance: purge workflows
    // ======================================================================

    /// Erase an evidence record and its belief links. Idempotent. Live
    /// beliefs that lose evidence are flagged for re-evaluation (they will
    /// also surface as cognitive debt if left unsupported).
    pub async fn purge_evidence(
        &self,
        ctx: &AccessContext,
        id: EvidenceId,
        req: Decision,
    ) -> Result<PurgeReport> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        if let Some(at) = self.store.purged_at(PurgeKind::Evidence, id.0).await? {
            return Ok(PurgeReport {
                id: id.0,
                already_purged: true,
                purged_at: at,
                purged_memories: Vec::new(),
                beliefs_needing_reevaluation: Vec::new(),
            });
        }
        let evidence = self
            .store
            .get_evidence(&[id])
            .await?
            .into_iter()
            .find(|e| e.tenant_id == ctx.tenant_id)
            .ok_or_else(|| Error::not_found("evidence", id))?;
        let now = self.now();
        let event = self.draft(
            ctx,
            evidence.id.0,
            &Scope::tenant(ctx.tenant_id),
            CognitiveEventType::EvidencePurged,
            json!({ "evidence_id": id, "reason": req.reason }),
            now,
        );
        let affected = self.store.purge_evidence(id, now, event).await?;
        let mut live = Vec::new();
        for b in affected {
            if let Some(b) = self.store.get_belief(b).await?
                && b.is_current()
                && b.status.is_live()
            {
                live.push(b.id);
                self.store
                    .append_event(self.draft(
                        ctx,
                        b.lineage_id.0,
                        &b.scope,
                        CognitiveEventType::BeliefReevaluationRequested,
                        json!({ "belief_id": b.id, "cause": "evidence_purged", "evidence_id": id }),
                        now,
                    ))
                    .await?;
            }
        }
        Ok(PurgeReport {
            id: id.0,
            already_purged: false,
            purged_at: now,
            purged_memories: Vec::new(),
            beliefs_needing_reevaluation: live,
        })
    }

    /// Is a provenance source still standing (not a purged observation)?
    async fn source_alive(&self, s: &SourceRef) -> Result<bool> {
        Ok(match s {
            SourceRef::Observation { observation_id } => self
                .store
                .get_observation(*observation_id)
                .await?
                .is_some_and(|o| !o.is_purged()),
            _ => true,
        })
    }

    /// Erase an observation's content and every memory extracted from it,    /// whatever its status. Idempotent.
    pub async fn purge_observation(
        &self,
        ctx: &AccessContext,
        id: ObservationId,
        req: Decision,
    ) -> Result<PurgeReport> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        let observation = self
            .store
            .get_observation(id)
            .await?
            .filter(|o| ctx.can_read(&o.scope))
            .ok_or_else(|| Error::not_found("observation", id))?;
        ctx.ensure_write(&observation.scope)?;
        if let Some(at) = observation.purged_at {
            return Ok(PurgeReport {
                id: id.0,
                already_purged: true,
                purged_at: at,
                purged_memories: Vec::new(),
                beliefs_needing_reevaluation: Vec::new(),
            });
        }
        let mut derived: Vec<MemoryId> = self
            .store
            .extraction_runs(id)
            .await?
            .into_iter()
            .flat_map(|r| r.candidate_ids)
            .collect();
        let now = self.now();
        let purged_source = SourceRef::Observation { observation_id: id };
        // Memories re-homed onto this observation after an earlier purge are
        // derived from it too, though another observation's run created them.
        let all = self
            .store
            .query_memories(&MemoryQuery {
                access: ctx.clone(),
                domains: MemoryDomain::ALL.to_vec(),
                statuses: vec![
                    MemoryStatus::Candidate,
                    MemoryStatus::Active,
                    MemoryStatus::Rejected,
                    MemoryStatus::Tombstoned,
                ],
                scope_filter: None,
                as_of_valid_time: now,
                as_of_transaction_time: now,
                include_superseded: false,
                ignore_valid_time: false,
                embedding_model: None,
                limit: cs_memory::RecallPolicy::MAX_CANDIDATES,
            })
            .await?;
        derived.extend(all.into_iter().filter_map(|s| {
            (s.record.provenance.source == purged_source
                || s.record.provenance.corroborations.contains(&purged_source))
            .then_some(s.record.id)
        }));
        derived.sort();
        derived.dedup();

        // A derived memory survives only if a source other than this
        // observation still stands behind it. Survivors get a revision that
        // drops the purged source (re-homing their primary source if
        // needed); the rest are erased with all their revisions.
        let mut delete = Vec::new();
        let mut rehome = Vec::new();
        let mut rehomed = Vec::new();
        for mid in derived {
            let Some(current) = self.store.current_memory(mid).await? else {
                delete.push(mid);
                continue;
            };
            let p = &current.provenance;
            if p.source != purged_source && !p.corroborations.contains(&purged_source) {
                continue;
            }
            let primary = if p.source != purged_source && self.source_alive(&p.source).await? {
                Some(p.source.clone())
            } else {
                let mut found = None;
                for s in p.corroborations.iter().filter(|s| **s != purged_source) {
                    if self.source_alive(s).await? {
                        found = Some(s.clone());
                        break;
                    }
                }
                found
            };
            let Some(primary) = primary else {
                delete.push(mid);
                continue;
            };
            let closed_at = self.next_tx(current.temporal.transaction_time.start);
            let mut next = current.clone();
            next.revision_id = MemoryRevisionId::new_v7();
            next.revision_number = current.revision_number + 1;
            next.supersedes = Some(current.revision_id);
            next.superseded_by = None;
            next.provenance
                .corroborations
                .retain(|s| *s != primary && *s != purged_source);
            next.provenance.source = primary;
            next.status_reason = Some(format!("source observation {id} purged"));
            next.temporal.transaction_time = TimeInterval::from(closed_at);
            next.created_at = closed_at;
            rehome.push(MemorySupersede {
                current: current.revision_id,
                closed_at,
                embedding: None,
                follow_up: Vec::new(),
                next,
            });
            rehomed.push(mid);
        }
        let event = self.draft(
            ctx,
            id.0,
            &observation.scope,
            CognitiveEventType::ObservationPurged,
            json!({
                "observation_id": id,
                "memories_purged": delete,
                "memories_rehomed": rehomed,
                "reason": req.reason,
            }),
            now,
        );
        let mut rehome = rehome;
        for m in &mut rehome {
            let (e, jobs) = self.memory_embedding_plan(ctx, &m.next).await;
            m.embedding = e;
            m.follow_up = jobs;
        }
        self.store
            .purge_observation(id, now, &delete, rehome, event)
            .await?;
        Ok(PurgeReport {
            id: id.0,
            already_purged: false,
            purged_at: now,
            purged_memories: delete,
            beliefs_needing_reevaluation: Vec::new(),
        })
    }

    // ======================================================================
    // Jobs
    // ======================================================================

    pub async fn get_job(&self, ctx: &AccessContext, id: JobId) -> Result<Job> {
        self.store
            .get_job(id)
            .await?
            .filter(|j| j.tenant_id == ctx.tenant_id && ctx.can_read_principal(&j.principal))
            .ok_or_else(|| Error::not_found("job", id))
    }

    pub async fn list_jobs(
        &self,
        ctx: &AccessContext,
        status: Option<JobStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<Job>> {
        Ok(self
            .store
            .list_jobs(ctx.tenant_id, status, limit.unwrap_or(100).min(1000))
            .await?
            .into_iter()
            .filter(|j| ctx.can_read_principal(&j.principal))
            .collect())
    }
}

/// Jobs are visible to the principal that submitted them.
trait PrincipalAccess {
    fn can_read_principal(&self, other: &AccessContext) -> bool;
}

impl PrincipalAccess for AccessContext {
    fn can_read_principal(&self, other: &AccessContext) -> bool {
        self.tenant_id == other.tenant_id
            && self.user_id == other.user_id
            && self.agent_id == other.agent_id
    }
}
