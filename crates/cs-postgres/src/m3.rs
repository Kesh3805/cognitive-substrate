//! M3 persistence: belief recall, durable jobs, observations, resolution
//! proposals and purge workflows.
//!
//! Lock order (deadlock avoidance): processing_jobs → observations →
//! memory_records (by revision id) → beliefs (by id) → resolution_proposals
//! → cognitive_events.

use cs_core::{
    AccessContext, BeliefId, CognitiveEvent, Error, EventDraft, EvidenceId, JobId, MemoryId,
    ObservationId, ProposalId, Result, TenantId,
};
use cs_memory::{
    Belief, ClaimedJob, ExtractionRun, Job, JobError, JobKind, JobOutcome,
    JobStatus, Lease, Observation, ProposalDecision, ProposalStatus, RecordVersion,
    ResolutionKind, ResolutionProposal,
};
use cs_storage::{
    BeliefQuery, BeliefSupersede, IdempotencyKey, JobClaim, JobEffects, MemorySupersede,
    ProposalMutations, PurgeKind,
};
use sqlx::{Row, postgres::PgRow, types::Json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    BELIEF_COLS, PgStore, TABLE_ACCESS_COLS, Tx, access_sql, append_event, belief_from_row,
    db_err, insert_belief_rows, insert_memory_row, parse, project_ids, register_key,
    scope_from_row,
};

// ---- rows ---------------------------------------------------------------------

const JOB_COLS: &str = "id, tenant_id, kind, principal, payload, dedupe_key, status::text AS status, \
    attempts, max_attempts, available_at, lease_owner, lease_until, lease_generation, last_error, \
    result, created_at, updated_at";

fn job_from_row(r: &PgRow) -> Result<Job> {
    let kind: String = r.try_get("kind").map_err(db_err)?;
    let status: String = r.try_get("status").map_err(db_err)?;
    let Json(principal): Json<AccessContext> = r.try_get("principal").map_err(db_err)?;
    let last_error: Option<Json<JobError>> = r.try_get("last_error").map_err(db_err)?;
    Ok(Job {
        id: JobId(r.try_get("id").map_err(db_err)?),
        tenant_id: TenantId(r.try_get("tenant_id").map_err(db_err)?),
        kind: parse(&kind, JobKind::parse, "job kind")?,
        principal,
        payload: r.try_get("payload").map_err(db_err)?,
        dedupe_key: r.try_get("dedupe_key").map_err(db_err)?,
        status: parse(&status, JobStatus::parse, "job status")?,
        attempts: u32::try_from(r.try_get::<i32, _>("attempts").map_err(db_err)?).unwrap_or(0),
        max_attempts: u32::try_from(r.try_get::<i32, _>("max_attempts").map_err(db_err)?)
            .unwrap_or(1),
        available_at: r.try_get("available_at").map_err(db_err)?,
        lease_owner: r.try_get("lease_owner").map_err(db_err)?,
        lease_until: r.try_get("lease_until").map_err(db_err)?,
        lease_generation: u64::try_from(r.try_get::<i64, _>("lease_generation").map_err(db_err)?)
            .unwrap_or(0),
        last_error: last_error.map(|j| j.0),
        result: r.try_get("result").map_err(db_err)?,
        created_at: r.try_get("created_at").map_err(db_err)?,
        updated_at: r.try_get("updated_at").map_err(db_err)?,
    })
}

fn observation_from_row(r: &PgRow) -> Result<Observation> {
    let Json(source) = r.try_get("source").map_err(db_err)?;
    let Json(actor) = r.try_get("actor").map_err(db_err)?;
    Ok(Observation {
        id: ObservationId(r.try_get("id").map_err(db_err)?),
        scope: scope_from_row(r)?,
        content: r.try_get("content").map_err(db_err)?,
        content_hash: r.try_get("content_hash").map_err(db_err)?,
        media_type: r.try_get("media_type").map_err(db_err)?,
        source,
        actor,
        observed_at: r.try_get("observed_at").map_err(db_err)?,
        recorded_at: r.try_get("recorded_at").map_err(db_err)?,
        purged_at: r.try_get("purged_at").map_err(db_err)?,
    })
}

const PROPOSAL_COLS: &str = "id, tenant_id, owner_user_id, agent_id, project_id, conversation_id, \
    visibility::text AS visibility, kind, status::text AS status, operation, rationale, detection, \
    based_on, fingerprint, created_at, decided_at, decided_by, decision_reason";

fn proposal_from_row(r: &PgRow) -> Result<ResolutionProposal> {
    let kind: String = r.try_get("kind").map_err(db_err)?;
    let status: String = r.try_get("status").map_err(db_err)?;
    let Json(operation) = r.try_get("operation").map_err(db_err)?;
    let Json(rationale) = r.try_get("rationale").map_err(db_err)?;
    let Json(detection) = r.try_get("detection").map_err(db_err)?;
    let Json(based_on) = r.try_get("based_on").map_err(db_err)?;
    let decided_by: Option<Json<cs_core::ActorRef>> = r.try_get("decided_by").map_err(db_err)?;
    Ok(ResolutionProposal {
        id: ProposalId(r.try_get("id").map_err(db_err)?),
        scope: scope_from_row(r)?,
        kind: parse(&kind, ResolutionKind::parse, "proposal kind")?,
        status: parse(&status, ProposalStatus::parse, "proposal status")?,
        operation,
        rationale,
        detection,
        based_on,
        fingerprint: r.try_get("fingerprint").map_err(db_err)?,
        created_at: r.try_get("created_at").map_err(db_err)?,
        decided_at: r.try_get("decided_at").map_err(db_err)?,
        decided_by: decided_by.map(|j| j.0),
        decision_reason: r.try_get("decision_reason").map_err(db_err)?,
    })
}

// ---- helpers ------------------------------------------------------------------

async fn insert_job(tx: &mut Tx<'_>, j: &Job) -> Result<Job> {
    // Return the active job with the same dedupe key, if any.
    let existing = sqlx::query(&format!(
        "SELECT {JOB_COLS} FROM processing_jobs \
         WHERE tenant_id = $1 AND kind = $2 AND dedupe_key = $3 AND status IN ('queued', 'leased')"
    ))
    .bind(j.tenant_id.0)
    .bind(j.kind.as_str())
    .bind(&j.dedupe_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;
    if let Some(row) = existing {
        return job_from_row(&row);
    }
    let inserted = sqlx::query(
        "INSERT INTO processing_jobs (id, tenant_id, kind, principal, payload, dedupe_key, status, \
         attempts, max_attempts, available_at, lease_generation, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'queued', 0, $7, $8, 0, $9, $9) \
         ON CONFLICT (tenant_id, kind, dedupe_key) WHERE status IN ('queued', 'leased') DO NOTHING",
    )
    .bind(j.id.0)
    .bind(j.tenant_id.0)
    .bind(j.kind.as_str())
    .bind(Json(&j.principal))
    .bind(&j.payload)
    .bind(&j.dedupe_key)
    .bind(i32::try_from(j.max_attempts).unwrap_or(5))
    .bind(j.available_at)
    .bind(j.created_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    if inserted.rows_affected() == 0 {
        // Lost a race with a concurrent enqueue: return the winner.
        let row = sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM processing_jobs \
             WHERE tenant_id = $1 AND kind = $2 AND dedupe_key = $3 AND status IN ('queued', 'leased')"
        ))
        .bind(j.tenant_id.0)
        .bind(j.kind.as_str())
        .bind(&j.dedupe_key)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_err)?;
        return job_from_row(&row);
    }
    Ok(j.clone())
}

/// Lock the job row iff `lease` is still held.
async fn lock_fenced(tx: &mut Tx<'_>, lease: Lease) -> Result<()> {
    let found = sqlx::query(
        "SELECT 1 FROM processing_jobs WHERE id = $1 AND status = 'leased' \
         AND lease_generation = $2 FOR UPDATE",
    )
    .bind(lease.job_id.0)
    .bind(i64::try_from(lease.generation).unwrap_or(i64::MAX))
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;
    if found.is_none() {
        return Err(Error::conflict("lease is no longer held"));
    }
    Ok(())
}

async fn write_outcome(
    tx: &mut Tx<'_>,
    id: JobId,
    now: OffsetDateTime,
    outcome: &JobOutcome,
) -> Result<Job> {
    let (status, available_at, error, result): (
        &str,
        Option<OffsetDateTime>,
        Option<&JobError>,
        Option<&serde_json::Value>,
    ) = match outcome {
        JobOutcome::Succeeded { result } => ("succeeded", None, None, Some(result)),
        JobOutcome::Retry { at, error } => ("queued", Some(*at), Some(error), None),
        JobOutcome::DeadLetter { error } => ("dead_lettered", None, Some(error), None),
    };
    let row = sqlx::query(&format!(
        "UPDATE processing_jobs SET status = $2::job_status, \
             available_at = COALESCE($3, available_at), \
             last_error = COALESCE($4, last_error), result = COALESCE($5, result), \
             lease_owner = NULL, lease_until = NULL, updated_at = $6 \
         WHERE id = $1 RETURNING {JOB_COLS}"
    ))
    .bind(id.0)
    .bind(status)
    .bind(available_at)
    .bind(error.map(Json))
    .bind(result)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_err)?;
    job_from_row(&row)
}

async fn precondition_failure(tx: &mut Tx<'_>, e: &JobEffects) -> Result<Option<&'static str>> {
    if let Some(id) = e.require_unpurged_observation {
        let ok: Option<bool> = sqlx::query_scalar(
            "SELECT purged_at IS NULL FROM observations WHERE id = $1 FOR SHARE",
        )
        .bind(id.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;
        if ok != Some(true) {
            return Ok(Some("observation_purged"));
        }
    }
    let mut versions = e.require_current.clone();
    versions.sort();
    for v in versions {
        let current: Option<i32> = match v {
            RecordVersion::Memory { id, revision_id } => sqlx::query_scalar(
                "SELECT 1 FROM memory_records WHERE memory_id = $1 AND revision_id = $2 \
                 AND upper_inf(transaction_time) FOR SHARE",
            )
            .bind(id.0)
            .bind(revision_id.0),
            RecordVersion::Belief { lineage_id, revision_id } => sqlx::query_scalar(
                "SELECT 1 FROM beliefs WHERE lineage_id = $1 AND id = $2 \
                 AND upper_inf(transaction_time) FOR SHARE",
            )
            .bind(lineage_id.0)
            .bind(revision_id.0),
        }
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;
        if current.is_none() {
            return Ok(Some("target_changed"));
        }
    }
    Ok(None)
}

async fn apply_effects(tx: &mut Tx<'_>, e: JobEffects) -> Result<()> {
    let mut memories = e.memories;
    memories.sort_by_key(|(m, _)| m.revision_id);
    for (m, emb) in &memories {
        insert_memory_row(tx, m, emb.as_ref()).await?;
    }
    if let Some(run) = &e.extraction_run {
        sqlx::query(
            "INSERT INTO extraction_runs (observation_id, extractor, extractor_version, \
             candidate_ids, completed_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(run.observation_id.0)
        .bind(&run.extractor)
        .bind(&run.extractor_version)
        .bind(run.candidate_ids.iter().map(|m| m.0).collect::<Vec<_>>())
        .bind(run.completed_at)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    let mut proposals = e.proposals;
    proposals.sort_by_key(|p| p.id);
    let mut inserted = std::collections::BTreeSet::new();
    for p in &proposals {
        let r = sqlx::query(
            "INSERT INTO resolution_proposals (id, tenant_id, owner_user_id, agent_id, project_id, \
             conversation_id, visibility, kind, status, operation, rationale, detection, based_on, \
             fingerprint, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::visibility, $8, 'pending', $9, $10, $11, $12, $13, $14) \
             ON CONFLICT (tenant_id, fingerprint) DO NOTHING",
        )
        .bind(p.id.0)
        .bind(p.scope.tenant_id.0)
        .bind(p.scope.owner_user_id.map(|u| u.0))
        .bind(p.scope.agent_id.map(|u| u.0))
        .bind(p.scope.project_id.map(|u| u.0))
        .bind(p.scope.conversation_id.map(|u| u.0))
        .bind(p.scope.visibility.as_str())
        .bind(p.kind.as_str())
        .bind(Json(&p.operation))
        .bind(Json(&p.rationale))
        .bind(Json(&p.detection))
        .bind(Json(&p.based_on))
        .bind(&p.fingerprint)
        .bind(p.created_at)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
        if r.rows_affected() == 1 {
            inserted.insert(p.id);
        }
    }
    for ev in e.events {
        // Skip the creation event of a proposal that was deduplicated away.
        if ev.event_type == cs_core::CognitiveEventType::ProposalCreated
            && proposals.iter().any(|p| p.id.0 == ev.stream_id.0 && !inserted.contains(&p.id))
        {
            continue;
        }
        append_event(tx, ev).await?;
    }
    Ok(())
}

async fn supersede_memory_in(tx: &mut Tx<'_>, m: &MemorySupersede) -> Result<()> {
    let closed = sqlx::query(
        "UPDATE memory_records \
         SET transaction_time = tstzrange(lower(transaction_time), $2, '[)'), \
             superseded_by_revision_id = $3 \
         WHERE revision_id = $1 AND upper_inf(transaction_time) AND lower(transaction_time) < $2",
    )
    .bind(m.current.0)
    .bind(m.closed_at)
    .bind(m.next.revision_id.0)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    if closed.rows_affected() != 1 {
        return Err(Error::conflict("revision is no longer current"));
    }
    insert_memory_row(tx, &m.next, m.embedding.as_ref()).await
}

async fn supersede_belief_in(tx: &mut Tx<'_>, b: &BeliefSupersede) -> Result<()> {
    let closed = sqlx::query(
        "UPDATE beliefs \
         SET transaction_time = tstzrange(lower(transaction_time), $2, '[)'), \
             superseded_by_belief_id = $3 \
         WHERE id = $1 AND upper_inf(transaction_time) AND lower(transaction_time) < $2",
    )
    .bind(b.current.0)
    .bind(b.closed_at)
    .bind(b.next.id.0)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    if closed.rows_affected() != 1 {
        return Err(Error::conflict("belief revision is no longer current"));
    }
    insert_belief_rows(tx, &b.next, &b.evidence, &b.dependencies).await
}

async fn ledger(tx: &mut Tx<'_>, kind: PurgeKind, id: Uuid, at: OffsetDateTime) -> Result<()> {
    sqlx::query(
        "INSERT INTO purge_ledger (kind, record_id, purged_at) VALUES ($1, $2, $3) \
         ON CONFLICT (kind, record_id) DO NOTHING",
    )
    .bind(kind.as_str())
    .bind(id)
    .bind(at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

// ---- operations (called from the CognitiveStore impl) --------------------------

impl PgStore {
    pub(crate) async fn m3_query_beliefs(&self, q: &BeliefQuery) -> Result<Vec<Belief>> {
        let clause = access_sql(TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        let filter = q.scope_filter.clone().unwrap_or_default();
        let statuses: Vec<&str> = q.statuses.iter().map(|s| s.as_str()).collect();
        sqlx::query(&format!(
            "SELECT {BELIEF_COLS} FROM beliefs WHERE {clause} \
               AND status::text = ANY($5) \
               AND lower(transaction_time) <= $6 \
               AND ($7 OR transaction_time @> $6) \
               AND valid_time @> $8 \
               AND ($9::uuid IS NULL OR project_id = $9) \
               AND ($10::uuid IS NULL OR conversation_id = $10) \
               AND ($11::uuid IS NULL OR agent_id = $11) \
             ORDER BY lower(transaction_time) DESC, id ASC LIMIT $12"
        ))
        .bind(q.access.tenant_id.0)
        .bind(q.access.user_id.map(|u| u.0))
        .bind(q.access.agent_id.map(|a| a.0))
        .bind(project_ids(&q.access))
        .bind(statuses)
        .bind(q.as_of_transaction_time)
        .bind(q.include_superseded)
        .bind(q.as_of_valid_time)
        .bind(filter.project_id.map(|p| p.0))
        .bind(filter.conversation_id.map(|c| c.0))
        .bind(filter.agent_id.map(|a| a.0))
        .bind(i64::from(q.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(belief_from_row)
        .collect()
    }

    pub(crate) async fn m3_enqueue_job(&self, job: &Job) -> Result<Job> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let j = insert_job(&mut tx, job).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(j)
    }

    pub(crate) async fn m3_claim_jobs(&self, c: &JobClaim) -> Result<Vec<ClaimedJob>> {
        let kinds: Vec<&str> = c.kinds.iter().map(|k| k.as_str()).collect();
        let rows = sqlx::query(&format!(
            "UPDATE processing_jobs SET status = 'leased', \
                 lease_generation = lease_generation + 1, attempts = attempts + 1, \
                 lease_owner = $3, lease_until = $4, updated_at = $2 \
             WHERE id IN ( \
                 SELECT id FROM processing_jobs \
                 WHERE kind = ANY($1) AND ( \
                     (status = 'queued' AND available_at <= $2) \
                     OR (status = 'leased' AND lease_until <= $2)) \
                 ORDER BY available_at, created_at, id \
                 LIMIT $5 FOR UPDATE SKIP LOCKED) \
             RETURNING {JOB_COLS}"
        ))
        .bind(kinds)
        .bind(c.now)
        .bind(&c.owner)
        .bind(c.lease_until)
        .bind(i64::from(c.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut out: Vec<ClaimedJob> = rows
            .iter()
            .map(|r| {
                let job = job_from_row(r)?;
                Ok(ClaimedJob { lease: Lease { job_id: job.id, generation: job.lease_generation }, job })
            })
            .collect::<Result<_>>()?;
        out.sort_by_key(|c| (c.job.available_at, c.job.created_at, c.job.id));
        Ok(out)
    }

    pub(crate) async fn m3_heartbeat(&self, lease: Lease, until: OffsetDateTime) -> Result<()> {
        let r = sqlx::query(
            "UPDATE processing_jobs SET lease_until = $3 \
             WHERE id = $1 AND status = 'leased' AND lease_generation = $2",
        )
        .bind(lease.job_id.0)
        .bind(i64::try_from(lease.generation).unwrap_or(i64::MAX))
        .bind(until)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        if r.rows_affected() != 1 {
            return Err(Error::conflict("lease is no longer held"));
        }
        Ok(())
    }

    pub(crate) async fn m3_finish_job(
        &self,
        lease: Lease,
        now: OffsetDateTime,
        outcome: JobOutcome,
        effects: JobEffects,
    ) -> Result<Job> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        lock_fenced(&mut tx, lease).await?;
        if let Some(reason) = precondition_failure(&mut tx, &effects).await? {
            let job = write_outcome(
                &mut tx,
                lease.job_id,
                now,
                &JobOutcome::Succeeded { result: serde_json::json!({ "discarded": reason }) },
            )
            .await?;
            tx.commit().await.map_err(db_err)?;
            return Ok(job);
        }
        // Effects run under a savepoint: a constraint violation must not
        // abort the transaction that records the job's fate.
        sqlx::query("SAVEPOINT apply_effects").execute(&mut *tx).await.map_err(db_err)?;
        let attempt = match apply_effects(&mut tx, effects).await {
            Ok(()) => outcome,
            Err(e) => {
                sqlx::query("ROLLBACK TO SAVEPOINT apply_effects")
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                JobOutcome::DeadLetter {
                    error: JobError { permanent: true, message: format!("effects rejected: {e}"), attempt: 0 },
                }
            }
        };
        let job = write_outcome(&mut tx, lease.job_id, now, &attempt).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(job)
    }

    pub(crate) async fn m3_release(&self, lease: Lease, now: OffsetDateTime) -> Result<()> {
        let r = sqlx::query(
            "UPDATE processing_jobs SET status = 'queued', attempts = GREATEST(attempts - 1, 0), \
                 lease_owner = NULL, lease_until = NULL, available_at = $3, updated_at = $3 \
             WHERE id = $1 AND status = 'leased' AND lease_generation = $2",
        )
        .bind(lease.job_id.0)
        .bind(i64::try_from(lease.generation).unwrap_or(i64::MAX))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        if r.rows_affected() != 1 {
            return Err(Error::conflict("lease is no longer held"));
        }
        Ok(())
    }

    pub(crate) async fn m3_get_job(&self, id: JobId) -> Result<Option<Job>> {
        let row = sqlx::query(&format!("SELECT {JOB_COLS} FROM processing_jobs WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        row.as_ref().map(job_from_row).transpose()
    }

    pub(crate) async fn m3_list_jobs(
        &self,
        tenant: TenantId,
        status: Option<JobStatus>,
        limit: u32,
    ) -> Result<Vec<Job>> {
        sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM processing_jobs \
             WHERE tenant_id = $1 AND ($2::text IS NULL OR status::text = $2) \
             ORDER BY created_at, id LIMIT $3"
        ))
        .bind(tenant.0)
        .bind(status.map(|s| s.as_str()))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(job_from_row)
        .collect()
    }

    pub(crate) async fn m3_insert_observation(
        &self,
        o: &Observation,
        job: &Job,
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        register_key(&mut tx, idempotency, o.id.0).await?;
        sqlx::query(
            "INSERT INTO observations (id, tenant_id, owner_user_id, agent_id, project_id, \
             conversation_id, visibility, content, content_hash, media_type, source, actor, \
             observed_at, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::visibility, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(o.id.0)
        .bind(o.scope.tenant_id.0)
        .bind(o.scope.owner_user_id.map(|u| u.0))
        .bind(o.scope.agent_id.map(|u| u.0))
        .bind(o.scope.project_id.map(|u| u.0))
        .bind(o.scope.conversation_id.map(|u| u.0))
        .bind(o.scope.visibility.as_str())
        .bind(&o.content)
        .bind(&o.content_hash)
        .bind(&o.media_type)
        .bind(Json(&o.source))
        .bind(Json(&o.actor))
        .bind(o.observed_at)
        .bind(o.recorded_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        insert_job(&mut tx, job).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    pub(crate) async fn m3_get_observation(&self, id: ObservationId) -> Result<Option<Observation>> {
        let row = sqlx::query(
            "SELECT id, tenant_id, owner_user_id, agent_id, project_id, conversation_id, \
             visibility::text AS visibility, content, content_hash, media_type, source, actor, \
             observed_at, recorded_at, purged_at FROM observations WHERE id = $1",
        )
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.as_ref().map(observation_from_row).transpose()
    }

    pub(crate) async fn m3_extraction_runs(&self, id: ObservationId) -> Result<Vec<ExtractionRun>> {
        sqlx::query(
            "SELECT * FROM extraction_runs WHERE observation_id = $1 \
             ORDER BY completed_at, extractor, extractor_version",
        )
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| {
            let ids: Vec<Uuid> = r.try_get("candidate_ids").map_err(db_err)?;
            Ok(ExtractionRun {
                observation_id: ObservationId(r.try_get("observation_id").map_err(db_err)?),
                extractor: r.try_get("extractor").map_err(db_err)?,
                extractor_version: r.try_get("extractor_version").map_err(db_err)?,
                candidate_ids: ids.into_iter().map(MemoryId).collect(),
                completed_at: r.try_get("completed_at").map_err(db_err)?,
            })
        })
        .collect()
    }

    pub(crate) async fn m3_purge_observation(
        &self,
        id: ObservationId,
        now: OffsetDateTime,
        delete: &[MemoryId],
        rehome: Vec<MemorySupersede>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let r = sqlx::query(
            "UPDATE observations SET content = NULL, purged_at = COALESCE(purged_at, $2) WHERE id = $1",
        )
        .bind(id.0)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if r.rows_affected() != 1 {
            return Err(Error::not_found("observation", id));
        }
        let mut rehome = rehome;
        rehome.sort_by_key(|m| m.current);
        for m in &rehome {
            supersede_memory_in(&mut tx, m).await?;
        }
        let ids: Vec<Uuid> = delete.iter().map(|m| m.0).collect();
        sqlx::query("DELETE FROM memory_records WHERE memory_id = ANY($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        for m in &ids {
            ledger(&mut tx, PurgeKind::Memory, *m, now).await?;
        }
        ledger(&mut tx, PurgeKind::Observation, id.0, now).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    pub(crate) async fn m3_get_proposal(&self, id: ProposalId) -> Result<Option<ResolutionProposal>> {
        let row = sqlx::query(&format!("SELECT {PROPOSAL_COLS} FROM resolution_proposals WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        row.as_ref().map(proposal_from_row).transpose()
    }

    pub(crate) async fn m3_list_proposals(
        &self,
        access: &AccessContext,
        status: Option<ProposalStatus>,
        limit: u32,
    ) -> Result<Vec<ResolutionProposal>> {
        let clause = access_sql(TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        sqlx::query(&format!(
            "SELECT {PROPOSAL_COLS} FROM resolution_proposals WHERE {clause} \
               AND ($5::text IS NULL OR status::text = $5) \
             ORDER BY created_at, id LIMIT $6"
        ))
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .bind(status.map(|s| s.as_str()))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(proposal_from_row)
        .collect()
    }

    pub(crate) async fn m3_decide_proposal(
        &self,
        id: ProposalId,
        d: &ProposalDecision,
        mutations: ProposalMutations,
        events: Vec<EventDraft>,
    ) -> Result<ResolutionProposal> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status::text FROM resolution_proposals WHERE id = $1 FOR UPDATE",
        )
        .bind(id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        match status.as_deref() {
            None => return Err(Error::not_found("proposal", id)),
            Some("pending") => {}
            Some(_) => return Err(Error::conflict("proposal is no longer pending")),
        }
        let mut memories = mutations.memories;
        memories.sort_by_key(|m| m.current);
        for m in &memories {
            supersede_memory_in(&mut tx, m).await?;
        }
        let mut beliefs = mutations.beliefs;
        beliefs.sort_by_key(|b| b.current);
        for b in &beliefs {
            supersede_belief_in(&mut tx, b).await?;
        }
        let row = sqlx::query(&format!(
            "UPDATE resolution_proposals SET status = $2::proposal_status, decided_at = $3, \
                 decided_by = $4, decision_reason = $5 \
             WHERE id = $1 RETURNING {PROPOSAL_COLS}"
        ))
        .bind(id.0)
        .bind(d.status.as_str())
        .bind(d.decided_at)
        .bind(Json(&d.decided_by))
        .bind(&d.reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        let proposal = proposal_from_row(&row)?;
        for e in events {
            append_event(&mut tx, e).await?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(proposal)
    }

    pub(crate) async fn m3_purged_at(&self, kind: PurgeKind, id: Uuid) -> Result<Option<OffsetDateTime>> {
        sqlx::query_scalar("SELECT purged_at FROM purge_ledger WHERE kind = $1 AND record_id = $2")
            .bind(kind.as_str())
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)
    }

    pub(crate) async fn m3_purge_evidence(
        &self,
        id: EvidenceId,
        now: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Vec<BeliefId>> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let affected: Vec<Uuid> = sqlx::query_scalar(
            "DELETE FROM belief_evidence WHERE evidence_id = $1 RETURNING belief_id",
        )
        .bind(id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        let r = sqlx::query("DELETE FROM evidence WHERE id = $1")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if r.rows_affected() != 1 {
            return Err(Error::not_found("evidence", id));
        }
        ledger(&mut tx, PurgeKind::Evidence, id.0, now).await?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        let mut out: Vec<BeliefId> = affected.into_iter().map(BeliefId).collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
}
