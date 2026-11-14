//! Embedding orchestration (M3.x): inline best-effort embedding with
//! atomic backfill jobs, shadow-space dual writes for model migrations,
//! the `embed` job handler, reindexing and coverage.
//!
//! Invariant: embedding availability may affect retrieval quality, never
//! the correctness, durability or authority of stored knowledge.

use cs_core::{AccessContext, BeliefId, MemoryId, MemoryRevisionId, Result};
use cs_memory::{
    Belief, BeliefStatus, EmbeddingRecord, Job, JobKind, MemoryDomain, MemoryRecord, MemoryStatus,
};
use cs_storage::{BeliefQuery, EmbeddingCoverage, JobCount, JobEffects, MemoryQuery};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    CognitiveEngine, EmbedError, EmbedKind, Embedder,
    embedding::embed_one,
    processing::{HandlerError, HandlerResult},
};

impl From<EmbedError> for HandlerError {
    fn from(e: EmbedError) -> Self {
        match e {
            EmbedError::Retryable(m) => Self {
                permanent: false,
                message: m,
            },
            EmbedError::Permanent(m) => Self {
                permanent: true,
                message: m,
            },
        }
    }
}

/// Which role a configured embedding space plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceRole {
    /// Serves recall and resolution.
    Active,
    /// Written alongside the active space while a migration backfills.
    Shadow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceCoverage {
    pub space: String,
    pub role: SpaceRole,
    pub coverage: EmbeddingCoverageView,
    pub ratio: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EmbeddingCoverageView {
    pub memories_total: u64,
    pub memories_embedded: u64,
    pub beliefs_total: u64,
    pub beliefs_embedded: u64,
}

impl From<EmbeddingCoverage> for EmbeddingCoverageView {
    fn from(c: EmbeddingCoverage) -> Self {
        Self {
            memories_total: c.memories_total,
            memories_embedded: c.memories_embedded,
            beliefs_total: c.beliefs_total,
            beliefs_embedded: c.beliefs_embedded,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCountView {
    pub kind: JobKind,
    pub status: cs_memory::JobStatus,
    pub count: u64,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub oldest_available_at: Option<time::OffsetDateTime>,
}

impl From<JobCount> for JobCountView {
    fn from(c: JobCount) -> Self {
        Self {
            kind: c.kind,
            status: c.status,
            count: c.count,
            oldest_available_at: c.oldest_available_at,
        }
    }
}

/// Queue and embedding health for one tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingStatus {
    pub spaces: Vec<SpaceCoverage>,
    pub jobs: Vec<JobCountView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReindexReport {
    pub space: String,
    pub selected: u64,
    pub already_embedded: u64,
    pub enqueued: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReindexRequest {
    /// Space id to backfill; defaults to the shadow space if one is
    /// configured, otherwise the active space.
    #[serde(default)]
    pub space: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum EmbedTarget {
    Memory {
        memory_id: MemoryId,
        revision_id: MemoryRevisionId,
    },
    Belief {
        lineage_id: BeliefId,
        belief_id: BeliefId,
    },
}

impl EmbedTarget {
    fn key(&self) -> String {
        match self {
            Self::Memory { revision_id, .. } => format!("memory:{revision_id}"),
            Self::Belief { belief_id, .. } => format!("belief:{belief_id}"),
        }
    }

    fn payload(&self, space: &str) -> serde_json::Value {
        match self {
            Self::Memory {
                memory_id,
                revision_id,
            } => json!({
                "target": "memory", "memory_id": memory_id, "revision_id": revision_id, "space": space,
            }),
            Self::Belief {
                lineage_id,
                belief_id,
            } => json!({
                "target": "belief", "lineage_id": lineage_id, "belief_id": belief_id, "space": space,
            }),
        }
    }
}

impl CognitiveEngine {
    /// Active embedding space id, if an embedder is configured.
    pub fn active_space(&self) -> Option<String> {
        self.embedder.as_ref().map(|e| e.space().id())
    }

    /// Shadow embedding space id, if a migration is in progress.
    pub fn shadow_space(&self) -> Option<String> {
        self.shadow.as_ref().map(|e| e.space().id())
    }

    pub(crate) fn space_key(&self) -> Option<(String, String)> {
        self.embedder
            .as_ref()
            .map(|e| (e.space().model_name(), e.space().model_version()))
    }

    fn embedder_for(&self, space: &str) -> Option<&dyn Embedder> {
        [&self.embedder, &self.shadow]
            .into_iter()
            .flatten()
            .find(|e| e.space().id() == space)
            .map(AsRef::as_ref)
    }

    fn embed_job(&self, ctx: &AccessContext, target: EmbedTarget, space: &str) -> Job {
        self.new_job(
            ctx,
            JobKind::Embed,
            format!("embed:{}@{space}", target.key()),
            target.payload(space),
        )
    }

    /// Embed a new revision inline (best effort, outside any transaction)
    /// and return the follow-up jobs that must commit with the write:
    /// a backfill for the active space if inline embedding failed or is
    /// disabled, and always one for the shadow space during a migration.
    pub(crate) async fn embedding_plan(
        &self,
        ctx: &AccessContext,
        target: EmbedTarget,
        projection: &str,
        skip: bool,
    ) -> (Option<EmbeddingRecord>, Vec<Job>) {
        let mut jobs = Vec::new();
        if skip || projection.trim().is_empty() {
            return (None, jobs);
        }
        let mut inline = None;
        if let Some(e) = &self.embedder {
            if self.processing.embed_inline {
                match embed_one(e.as_ref(), projection, EmbedKind::Document, self.now()).await {
                    Ok(v) => inline = Some(v),
                    Err(err) => {
                        tracing::warn!(error = %err, space = %e.space().id(), "inline embedding failed; queueing backfill");
                        jobs.push(self.embed_job(ctx, target, &e.space().id()));
                    }
                }
            } else {
                jobs.push(self.embed_job(ctx, target, &e.space().id()));
            }
        }
        if let Some(s) = &self.shadow {
            jobs.push(self.embed_job(ctx, target, &s.space().id()));
        }
        (inline, jobs)
    }

    pub(crate) async fn memory_embedding_plan(
        &self,
        ctx: &AccessContext,
        m: &MemoryRecord,
    ) -> (Option<EmbeddingRecord>, Vec<Job>) {
        let target = EmbedTarget::Memory {
            memory_id: m.id,
            revision_id: m.revision_id,
        };
        let (inline, mut jobs) = self
            .embedding_plan(
                ctx,
                target,
                &m.content.index_text(),
                m.status == MemoryStatus::Tombstoned,
            )
            .await;
        // Every write path plans its follow-ups here, so the entity
        // projection rides along with the revision's commit.
        jobs.extend(self.projection_job(ctx, m));
        (inline, jobs)
    }

    pub(crate) async fn belief_embedding_plan(
        &self,
        ctx: &AccessContext,
        b: &Belief,
    ) -> (Option<EmbeddingRecord>, Vec<Job>) {
        let target = EmbedTarget::Belief {
            lineage_id: b.lineage_id,
            belief_id: b.id,
        };
        self.embedding_plan(
            ctx,
            target,
            &b.proposition,
            b.status == BeliefStatus::Retracted,
        )
        .await
    }

    /// `embed` job handler: compute one vector for one exact revision.
    pub(crate) async fn embed(&self, job: &Job) -> HandlerResult {
        let space = job.payload["space"].as_str().unwrap_or_default().to_owned();
        let Some(embedder) = self.embedder_for(&space) else {
            return Err(HandlerError {
                permanent: true,
                message: format!("embedding space {space} is not configured"),
            });
        };
        let bad = |e: serde_json::Error| HandlerError {
            permanent: true,
            message: format!("bad payload: {e}"),
        };
        let now = self.now();
        let mut effects = JobEffects::default();
        match job.payload["target"].as_str() {
            Some("memory") => {
                let memory_id: MemoryId =
                    serde_json::from_value(job.payload["memory_id"].clone()).map_err(bad)?;
                let revision_id: MemoryRevisionId =
                    serde_json::from_value(job.payload["revision_id"].clone()).map_err(bad)?;
                let history = self.store.memory_history(memory_id).await?;
                let Some(rev) = history.into_iter().find(|r| r.revision_id == revision_id) else {
                    return Ok((effects, json!({ "skipped": "revision_purged" })));
                };
                let v = embed_one(
                    embedder,
                    &rev.content.index_text(),
                    EmbedKind::Document,
                    now,
                )
                .await?;
                effects.memory_embeddings.push((revision_id, v));
                // A new vector can reveal dense duplicates the structural
                // pass could not see.
                if self.processing.auto_resolve
                    && Some(&space) == self.active_space().as_ref()
                    && rev.is_current()
                {
                    effects.jobs.push(self.new_job(
                        &job.principal,
                        JobKind::Resolve,
                        format!("memory:{revision_id}"),
                        json!({ "memory_id": memory_id }),
                    ));
                }
            }
            Some("belief") => {
                let belief_id: BeliefId =
                    serde_json::from_value(job.payload["belief_id"].clone()).map_err(bad)?;
                let Some(b) = self.store.get_belief(belief_id).await? else {
                    return Ok((effects, json!({ "skipped": "belief_missing" })));
                };
                let v = embed_one(embedder, &b.proposition, EmbedKind::Document, now).await?;
                effects.belief_embeddings.push((belief_id, v));
            }
            other => {
                return Err(HandlerError::permanent_msg(format!(
                    "unknown embed target {other:?}"
                )));
            }
        }
        Ok((effects, json!({ "space": space })))
    }

    /// Enqueue backfill jobs for every current revision the caller can read
    /// that lacks a vector in `space`, newest first. Idempotent: rerunning
    /// it is the reconciliation pass for writes that raced the first run.
    pub async fn request_reembedding(
        &self,
        ctx: &AccessContext,
        req: ReindexRequest,
    ) -> Result<ReindexReport> {
        let space = req
            .space
            .or_else(|| self.shadow_space())
            .or_else(|| self.active_space())
            .ok_or_else(|| cs_core::Error::validation("no embedder is configured"))?;
        let embedder = self.embedder_for(&space).ok_or_else(|| {
            cs_core::Error::validation(format!("embedding space {space} is not configured"))
        })?;
        let key = (
            embedder.space().model_name(),
            embedder.space().model_version(),
        );
        let now = self.now();
        let mut report = ReindexReport {
            space: space.clone(),
            selected: 0,
            already_embedded: 0,
            enqueued: 0,
        };

        let memories = self
            .store
            .query_memories(&MemoryQuery {
                access: ctx.clone(),
                domains: MemoryDomain::ALL.to_vec(),
                statuses: vec![
                    MemoryStatus::Active,
                    MemoryStatus::Candidate,
                    MemoryStatus::Rejected,
                ],
                scope_filter: None,
                as_of_valid_time: now,
                as_of_transaction_time: now,
                include_superseded: false,
                ignore_valid_time: true,
                embedding_model: Some(key.clone()),
                limit: cs_memory::RecallPolicy::MAX_CANDIDATES,
            })
            .await?;
        for s in memories {
            report.selected += 1;
            if s.embedding.is_some() {
                report.already_embedded += 1;
                continue;
            }
            let target = EmbedTarget::Memory {
                memory_id: s.record.id,
                revision_id: s.record.revision_id,
            };
            self.store
                .enqueue_job(&self.embed_job(ctx, target, &space))
                .await?;
            report.enqueued += 1;
        }
        let beliefs = self
            .store
            .query_beliefs(&BeliefQuery {
                access: ctx.clone(),
                statuses: vec![BeliefStatus::Active, BeliefStatus::Disputed],
                scope_filter: None,
                as_of_valid_time: now,
                as_of_transaction_time: now,
                include_superseded: false,
                ignore_valid_time: true,
                embedding_model: Some(key),
                limit: cs_memory::RecallPolicy::MAX_CANDIDATES,
            })
            .await?;
        for s in beliefs {
            report.selected += 1;
            if s.embedding.is_some() {
                report.already_embedded += 1;
                continue;
            }
            let target = EmbedTarget::Belief {
                lineage_id: s.belief.lineage_id,
                belief_id: s.belief.id,
            };
            self.store
                .enqueue_job(&self.embed_job(ctx, target, &space))
                .await?;
            report.enqueued += 1;
        }
        Ok(report)
    }

    /// Embedding coverage and queue health for the caller's tenant.
    pub async fn processing_status(&self, ctx: &AccessContext) -> Result<ProcessingStatus> {
        let mut spaces = Vec::new();
        for (e, role) in [
            (&self.embedder, SpaceRole::Active),
            (&self.shadow, SpaceRole::Shadow),
        ] {
            if let Some(e) = e {
                let c = self
                    .store
                    .embedding_coverage(
                        Some(ctx.tenant_id),
                        &e.space().model_name(),
                        &e.space().model_version(),
                    )
                    .await?;
                spaces.push(SpaceCoverage {
                    space: e.space().id(),
                    role,
                    ratio: c.ratio(),
                    coverage: c.into(),
                });
            }
        }
        let jobs = self
            .store
            .job_counts(ctx.tenant_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(ProcessingStatus { spaces, jobs })
    }

    /// Operator gate across all tenants: the share of current revisions
    /// embedded in the active space. Used to refuse switching to a space
    /// that has not been backfilled.
    pub async fn active_space_coverage(&self) -> Result<Option<(String, EmbeddingCoverage)>> {
        let Some(e) = &self.embedder else {
            return Ok(None);
        };
        let c = self
            .store
            .embedding_coverage(None, &e.space().model_name(), &e.space().model_version())
            .await?;
        Ok(Some((e.space().id(), c)))
    }
}

impl HandlerError {
    pub(crate) fn permanent_msg(m: impl Into<String>) -> Self {
        Self {
            permanent: true,
            message: m.into(),
        }
    }
}
