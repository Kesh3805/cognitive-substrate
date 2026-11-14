//! PostgreSQL implementation of [`CognitiveStore`].
//!
//! * Every mutation and its cognitive event commit in one transaction.
//! * Event sequences are assigned under a per-stream advisory lock.
//! * Authorization is part of every read query (see [`access_sql`]); it is
//!   the SQL form of `AccessContext::can_read` and is covered by the
//!   conformance suite.

use std::ops::Bound;

use async_trait::async_trait;
use cs_core::{
    AccessContext, ActorRef, BeliefId, Bitemporal, CognitiveEvent, CognitiveEventType, Error,
    EventDraft, EventId, EventQuery, EventStreamId, EvidenceId, MemoryId, MemoryRevisionId,
    Provenance, RecallTraceId, Result, Scope, SourceRef, TenantId, TimeInterval, Visibility,
    event::EVENT_SCHEMA_VERSION,
};
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefPolarity, BeliefStatus, Confidence,
    ConfidenceInterpretation, DependencyRelation, EmbeddingRecord, Evidence, EvidenceDirection,
    MemoryContent, MemoryRecord, MemoryStatus, RecallCandidate, RecallPolicy, RecallTrace,
};
use cs_storage::{CognitiveStore, IdempotencyKey, MemoryQuery, StoredMemory};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgPoolOptions, PgRow, types::PgRange},
    types::Json,
};
use time::OffsetDateTime;
use uuid::Uuid;

mod m3;
mod m4;

/// Embedded migrations (`/migrations`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(Error::storage)?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR.run(&self.pool).await.map_err(Error::storage)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

fn db_err(e: sqlx::Error) -> Error {
    if let sqlx::Error::Database(db) = &e {
        match db.code().as_deref() {
            Some("23505") | Some("23P01") => return Error::conflict(db.message().to_owned()),
            Some("23503") => return Error::not_found("referenced record", db.message()),
            Some("23514") => return Error::validation(db.message().to_owned()),
            _ => {}
        }
    }
    Error::storage(e)
}

fn to_range(i: &TimeInterval) -> PgRange<OffsetDateTime> {
    PgRange {
        start: Bound::Included(i.start),
        end: i.end.map_or(Bound::Unbounded, Bound::Excluded),
    }
}

fn from_range(r: PgRange<OffsetDateTime>) -> Result<TimeInterval> {
    let start = match r.start {
        Bound::Included(t) | Bound::Excluded(t) => t,
        Bound::Unbounded => return Err(Error::storage("unbounded interval start")),
    };
    let end = match r.end {
        Bound::Included(t) | Bound::Excluded(t) => Some(t),
        Bound::Unbounded => None,
    };
    Ok(TimeInterval { start, end })
}

fn parse<T>(v: &str, f: impl Fn(&str) -> Option<T>, what: &str) -> Result<T> {
    f(v).ok_or_else(|| Error::storage(format!("unknown {what}: {v}")))
}

fn scope_from_row(row: &PgRow) -> Result<Scope> {
    let vis: String = row.try_get("visibility").map_err(db_err)?;
    Ok(Scope {
        tenant_id: TenantId(row.try_get("tenant_id").map_err(db_err)?),
        owner_user_id: row
            .try_get::<Option<Uuid>, _>("owner_user_id")
            .map_err(db_err)?
            .map(Into::into),
        agent_id: row
            .try_get::<Option<Uuid>, _>("agent_id")
            .map_err(db_err)?
            .map(Into::into),
        project_id: row
            .try_get::<Option<Uuid>, _>("project_id")
            .map_err(db_err)?
            .map(Into::into),
        conversation_id: row
            .try_get::<Option<Uuid>, _>("conversation_id")
            .map_err(db_err)?
            .map(Into::into),
        visibility: parse(&vis, Visibility::parse, "visibility")?,
    })
}

/// SQL predicate equivalent to `AccessContext::can_read`.
///
/// `cols` gives the SQL expressions for tenant, owner, agent, project and
/// visibility; `p` gives the parameter numbers for tenant, user, agent and
/// the project-id array.
fn access_sql(cols: [&str; 5], p: [usize; 4]) -> String {
    let [tenant, owner, agent, project, vis] = cols;
    let [pt, pu, pa, pp] = p;
    format!(
        "({tenant} = ${pt} AND (\
            ({owner} IS NOT NULL AND {owner} = ${pu}) \
            OR {vis} = 'tenant' \
            OR ({vis} = 'private' AND {owner} IS NULL AND {agent} IS NOT NULL AND {agent} = ${pa}) \
            OR ({vis} = 'agent' AND {agent} = ${pa}) \
            OR ({vis} = 'project' AND {project} = ANY(${pp}))))"
    )
}

const TABLE_ACCESS_COLS: [&str; 5] = [
    "tenant_id",
    "owner_user_id",
    "agent_id",
    "project_id",
    "visibility::text",
];

const EVENT_ACCESS_COLS: [&str; 5] = [
    "tenant_id",
    "(scope->>'owner_user_id')::uuid",
    "(scope->>'agent_id')::uuid",
    "(scope->>'project_id')::uuid",
    "(scope->>'visibility')",
];

fn project_ids(access: &AccessContext) -> Vec<Uuid> {
    access.project_ids.iter().map(|p| p.0).collect()
}

const MEMORY_COLS: &str = "m.revision_id, m.memory_id, m.revision_number, m.tenant_id, \
    m.owner_user_id, m.agent_id, m.project_id, m.conversation_id, m.visibility::text AS visibility, \
    m.content, m.provenance, m.valid_time, m.transaction_time, m.status::text AS status, \
    m.status_reason, m.supersedes_revision_id, m.superseded_by_revision_id, m.created_at, m.expires_at";

fn memory_from_row(row: &PgRow) -> Result<MemoryRecord> {
    let status: String = row.try_get("status").map_err(db_err)?;
    let Json(content): Json<MemoryContent> = row.try_get("content").map_err(db_err)?;
    let Json(provenance): Json<Provenance> = row.try_get("provenance").map_err(db_err)?;
    Ok(MemoryRecord {
        id: MemoryId(row.try_get("memory_id").map_err(db_err)?),
        revision_id: MemoryRevisionId(row.try_get("revision_id").map_err(db_err)?),
        revision_number: u32::try_from(row.try_get::<i32, _>("revision_number").map_err(db_err)?)
            .map_err(Error::storage)?,
        scope: scope_from_row(row)?,
        content,
        provenance,
        temporal: Bitemporal {
            valid_time: from_range(row.try_get("valid_time").map_err(db_err)?)?,
            transaction_time: from_range(row.try_get("transaction_time").map_err(db_err)?)?,
        },
        status: parse(&status, MemoryStatus::parse, "memory status")?,
        status_reason: row.try_get("status_reason").map_err(db_err)?,
        supersedes: row
            .try_get::<Option<Uuid>, _>("supersedes_revision_id")
            .map_err(db_err)?
            .map(Into::into),
        superseded_by: row
            .try_get::<Option<Uuid>, _>("superseded_by_revision_id")
            .map_err(db_err)?
            .map(Into::into),
        created_at: row.try_get("created_at").map_err(db_err)?,
        expires_at: row.try_get("expires_at").map_err(db_err)?,
    })
}

const BELIEF_COLS: &str = "id, lineage_id, tenant_id, owner_user_id, agent_id, project_id, \
    conversation_id, visibility::text AS visibility, proposition, polarity, confidence, \
    confidence_interpretation, confidence_method, revision_number, supersedes_belief_id, \
    superseded_by_belief_id, revision_reason, status::text AS status, revalidation_interval_days, \
    valid_time, transaction_time, created_at";

fn belief_from_row(row: &PgRow) -> Result<Belief> {
    let polarity: String = row.try_get("polarity").map_err(db_err)?;
    let status: String = row.try_get("status").map_err(db_err)?;
    let interp: String = row.try_get("confidence_interpretation").map_err(db_err)?;
    Ok(Belief {
        id: BeliefId(row.try_get("id").map_err(db_err)?),
        lineage_id: BeliefId(row.try_get("lineage_id").map_err(db_err)?),
        scope: scope_from_row(row)?,
        proposition: row.try_get("proposition").map_err(db_err)?,
        polarity: parse(&polarity, BeliefPolarity::parse, "polarity")?,
        confidence: Confidence {
            value: row.try_get("confidence").map_err(db_err)?,
            interpretation: parse(&interp, ConfidenceInterpretation::parse, "interpretation")?,
            method: row.try_get("confidence_method").map_err(db_err)?,
        },
        revision_number: u32::try_from(row.try_get::<i32, _>("revision_number").map_err(db_err)?)
            .map_err(Error::storage)?,
        supersedes: row
            .try_get::<Option<Uuid>, _>("supersedes_belief_id")
            .map_err(db_err)?
            .map(Into::into),
        superseded_by: row
            .try_get::<Option<Uuid>, _>("superseded_by_belief_id")
            .map_err(db_err)?
            .map(Into::into),
        revision_reason: row.try_get("revision_reason").map_err(db_err)?,
        status: parse(&status, BeliefStatus::parse, "belief status")?,
        revalidation_interval_days: row
            .try_get::<Option<i32>, _>("revalidation_interval_days")
            .map_err(db_err)?
            .map(|d| u32::try_from(d).unwrap_or(0)),
        temporal: Bitemporal {
            valid_time: from_range(row.try_get("valid_time").map_err(db_err)?)?,
            transaction_time: from_range(row.try_get("transaction_time").map_err(db_err)?)?,
        },
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}

fn dependency_from_row(row: &PgRow) -> Result<BeliefDependency> {
    let relation: String = row.try_get("relation").map_err(db_err)?;
    Ok(BeliefDependency {
        from: BeliefId(row.try_get("from_belief_id").map_err(db_err)?),
        to: BeliefId(row.try_get("to_belief_id").map_err(db_err)?),
        relation: parse(&relation, DependencyRelation::parse, "relation")?,
        strength: row.try_get("strength").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}

fn link_from_row(row: &PgRow) -> Result<BeliefEvidence> {
    let direction: String = row.try_get("direction").map_err(db_err)?;
    Ok(BeliefEvidence {
        evidence_id: EvidenceId(row.try_get("evidence_id").map_err(db_err)?),
        direction: parse(&direction, EvidenceDirection::parse, "direction")?,
        strength: row.try_get("strength").map_err(db_err)?,
    })
}

fn event_from_row(row: &PgRow) -> Result<CognitiveEvent> {
    let event_type: String = row.try_get("event_type").map_err(db_err)?;
    let Json(scope): Json<Scope> = row.try_get("scope").map_err(db_err)?;
    let Json(actor): Json<ActorRef> = row.try_get("actor").map_err(db_err)?;
    Ok(CognitiveEvent {
        id: EventId(row.try_get("id").map_err(db_err)?),
        stream_id: EventStreamId(row.try_get("stream_id").map_err(db_err)?),
        sequence: u64::try_from(row.try_get::<i64, _>("sequence").map_err(db_err)?)
            .map_err(Error::storage)?,
        scope,
        actor,
        event_type: parse(&event_type, CognitiveEventType::parse, "event type")?,
        payload: row.try_get("payload").map_err(db_err)?,
        occurred_at: row.try_get("occurred_at").map_err(db_err)?,
        recorded_at: row.try_get("recorded_at").map_err(db_err)?,
        causation_id: row
            .try_get::<Option<Uuid>, _>("causation_id")
            .map_err(db_err)?
            .map(Into::into),
        correlation_id: row.try_get("correlation_id").map_err(db_err)?,
        schema_version: u32::try_from(row.try_get::<i32, _>("schema_version").map_err(db_err)?)
            .map_err(Error::storage)?,
    })
}

// ---------------------------------------------------------------------------
// Transactional helpers
// ---------------------------------------------------------------------------

type Tx<'a> = Transaction<'a, Postgres>;

async fn append_event(tx: &mut Tx<'_>, draft: EventDraft) -> Result<CognitiveEvent> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(draft.stream_id.0)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    let next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM cognitive_events WHERE stream_id = $1",
    )
    .bind(draft.stream_id.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_err)?;
    let recorded_at: OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO cognitive_events (id, stream_id, sequence, tenant_id, scope, actor, \
         event_type, payload, occurred_at, causation_id, correlation_id, schema_version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING recorded_at",
    )
    .bind(draft.id.0)
    .bind(draft.stream_id.0)
    .bind(next)
    .bind(draft.scope.tenant_id.0)
    .bind(Json(&draft.scope))
    .bind(Json(&draft.actor))
    .bind(draft.event_type.as_str())
    .bind(&draft.payload)
    .bind(draft.occurred_at)
    .bind(draft.causation_id.map(|c| c.0))
    .bind(draft.correlation_id)
    .bind(i32::try_from(EVENT_SCHEMA_VERSION).unwrap_or(1))
    .fetch_one(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(draft.into_event(u64::try_from(next).unwrap_or(0), recorded_at))
}

async fn register_key(tx: &mut Tx<'_>, key: Option<IdempotencyKey<'_>>, id: Uuid) -> Result<()> {
    if let Some(k) = key {
        sqlx::query(
            "INSERT INTO idempotency_keys (tenant_id, kind, key, resource_id) VALUES ($1, $2, $3, $4)",
        )
        .bind(k.tenant_id.0)
        .bind(k.kind)
        .bind(k.key)
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    Ok(())
}

async fn insert_memory_row(
    tx: &mut Tx<'_>,
    r: &MemoryRecord,
    embedding: Option<&EmbeddingRecord>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO memory_records (revision_id, memory_id, revision_number, tenant_id, \
         owner_user_id, agent_id, project_id, conversation_id, visibility, domain, content, \
         provenance, valid_time, transaction_time, status, status_reason, supersedes_revision_id, \
         superseded_by_revision_id, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::visibility, $10::memory_domain, $11, $12, \
         $13, $14, $15::memory_status, $16, $17, $18, $19, $20)",
    )
    .bind(r.revision_id.0)
    .bind(r.id.0)
    .bind(i32::try_from(r.revision_number).map_err(Error::storage)?)
    .bind(r.scope.tenant_id.0)
    .bind(r.scope.owner_user_id.map(|u| u.0))
    .bind(r.scope.agent_id.map(|u| u.0))
    .bind(r.scope.project_id.map(|u| u.0))
    .bind(r.scope.conversation_id.map(|u| u.0))
    .bind(r.scope.visibility.as_str())
    .bind(r.domain().as_str())
    .bind(Json(&r.content))
    .bind(Json(&r.provenance))
    .bind(to_range(&r.temporal.valid_time))
    .bind(to_range(&r.temporal.transaction_time))
    .bind(r.status.as_str())
    .bind(&r.status_reason)
    .bind(r.supersedes.map(|s| s.0))
    .bind(r.superseded_by.map(|s| s.0))
    .bind(r.created_at)
    .bind(r.expires_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    if let Some(e) = embedding {
        upsert_embedding(tx, EmbeddingTarget::Memory, r.revision_id.0, e).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum EmbeddingTarget {
    Memory,
    Belief,
}

/// Idempotently attach a vector to an exact revision. A revision that no
/// longer exists (purged) is skipped, never resurrected.
pub(crate) async fn upsert_embedding(
    tx: &mut Tx<'_>,
    target: EmbeddingTarget,
    id: Uuid,
    e: &EmbeddingRecord,
) -> Result<()> {
    let sql = match target {
        EmbeddingTarget::Memory => {
            "INSERT INTO memory_embeddings (revision_id, model_name, model_version, dimensions, \
             embedding, content_hash, embedded_at) \
             SELECT $1, $2, $3, $4, $5, $6, $7 WHERE EXISTS \
                 (SELECT 1 FROM memory_records WHERE revision_id = $1) \
             ON CONFLICT (revision_id, model_name, model_version) DO NOTHING"
        }
        EmbeddingTarget::Belief => {
            "INSERT INTO belief_embeddings (belief_id, model_name, model_version, dimensions, \
             embedding, content_hash, embedded_at) \
             SELECT $1, $2, $3, $4, $5, $6, $7 WHERE EXISTS (SELECT 1 FROM beliefs WHERE id = $1) \
             ON CONFLICT (belief_id, model_name, model_version) DO NOTHING"
        }
    };
    sqlx::query(sql)
        .bind(id)
        .bind(&e.model_name)
        .bind(&e.model_version)
        .bind(i32::try_from(e.dimensions()).map_err(Error::storage)?)
        .bind(pgvector::Vector::from(e.vector.clone()))
        .bind(&e.content_hash)
        .bind(e.embedded_at)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub(crate) async fn enqueue_all(tx: &mut Tx<'_>, jobs: &[cs_memory::Job]) -> Result<()> {
    for j in jobs {
        m3::insert_job(tx, j).await?;
    }
    Ok(())
}

async fn check_evidence_tenant(
    tx: &mut Tx<'_>,
    tenant: TenantId,
    links: &[BeliefEvidence],
) -> Result<()> {
    if links.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = links.iter().map(|l| l.evidence_id.0).collect();
    let found: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM evidence WHERE id = ANY($1) AND tenant_id = $2")
            .bind(&ids)
            .bind(tenant.0)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_err)?;
    if usize::try_from(found).unwrap_or(0) != ids.len() {
        return Err(Error::not_found("evidence", "one or more evidence ids"));
    }
    Ok(())
}

async fn insert_belief_rows(
    tx: &mut Tx<'_>,
    b: &Belief,
    links: &[BeliefEvidence],
    deps: &[BeliefDependency],
) -> Result<()> {
    check_evidence_tenant(tx, b.scope.tenant_id, links).await?;
    sqlx::query(
        "INSERT INTO beliefs (id, lineage_id, tenant_id, owner_user_id, agent_id, project_id, \
         conversation_id, visibility, proposition, polarity, confidence, confidence_interpretation, \
         confidence_method, revision_number, supersedes_belief_id, superseded_by_belief_id, \
         revision_reason, status, revalidation_interval_days, valid_time, transaction_time, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::visibility, $9, $10, $11, $12, $13, $14, $15, $16, \
         $17, $18::belief_status, $19, $20, $21, $22)",
    )
    .bind(b.id.0)
    .bind(b.lineage_id.0)
    .bind(b.scope.tenant_id.0)
    .bind(b.scope.owner_user_id.map(|u| u.0))
    .bind(b.scope.agent_id.map(|u| u.0))
    .bind(b.scope.project_id.map(|u| u.0))
    .bind(b.scope.conversation_id.map(|u| u.0))
    .bind(b.scope.visibility.as_str())
    .bind(&b.proposition)
    .bind(b.polarity.as_str())
    .bind(b.confidence.value)
    .bind(b.confidence.interpretation.as_str())
    .bind(&b.confidence.method)
    .bind(i32::try_from(b.revision_number).map_err(Error::storage)?)
    .bind(b.supersedes.map(|s| s.0))
    .bind(b.superseded_by.map(|s| s.0))
    .bind(&b.revision_reason)
    .bind(b.status.as_str())
    .bind(b.revalidation_interval_days.map(|d| i32::try_from(d).unwrap_or(i32::MAX)))
    .bind(to_range(&b.temporal.valid_time))
    .bind(to_range(&b.temporal.transaction_time))
    .bind(b.created_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    for l in links {
        sqlx::query(
            "INSERT INTO belief_evidence (belief_id, evidence_id, direction, strength) \
             VALUES ($1, $2, $3::evidence_direction, $4)",
        )
        .bind(b.id.0)
        .bind(l.evidence_id.0)
        .bind(l.direction.as_str())
        .bind(l.strength)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    for d in deps {
        insert_dependency_row(tx, d).await?;
    }
    Ok(())
}

async fn insert_dependency_row(tx: &mut Tx<'_>, d: &BeliefDependency) -> Result<()> {
    sqlx::query(
        "INSERT INTO belief_dependencies (from_belief_id, to_belief_id, relation, strength, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(d.from.0)
    .bind(d.to.0)
    .bind(d.relation.as_str())
    .bind(d.strength)
    .bind(d.created_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CognitiveStore
// ---------------------------------------------------------------------------

#[async_trait]
impl CognitiveStore for PgStore {
    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn idempotent_lookup(
        &self,
        tenant_id: TenantId,
        kind: &'static str,
        key: &str,
    ) -> Result<Option<Uuid>> {
        sqlx::query_scalar(
            "SELECT resource_id FROM idempotency_keys WHERE tenant_id = $1 AND kind = $2 AND key = $3",
        )
        .bind(tenant_id.0)
        .bind(kind)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)
    }

    async fn insert_memory(
        &self,
        record: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        follow_up: &[cs_memory::Job],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        register_key(&mut tx, idempotency, record.id.0).await?;
        insert_memory_row(&mut tx, record, embedding).await?;
        enqueue_all(&mut tx, follow_up).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn supersede_memory(
        &self,
        current: MemoryRevisionId,
        closed_at: OffsetDateTime,
        next: &MemoryRecord,
        embedding: Option<&EmbeddingRecord>,
        follow_up: &[cs_memory::Job],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let closed = sqlx::query(
            "UPDATE memory_records \
             SET transaction_time = tstzrange(lower(transaction_time), $2, '[)'), \
                 superseded_by_revision_id = $3 \
             WHERE revision_id = $1 AND upper_inf(transaction_time) \
               AND lower(transaction_time) < $2",
        )
        .bind(current.0)
        .bind(closed_at)
        .bind(next.revision_id.0)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if closed.rows_affected() != 1 {
            return Err(Error::conflict("revision is no longer current"));
        }
        insert_memory_row(&mut tx, next, embedding).await?;
        enqueue_all(&mut tx, follow_up).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn current_memory(&self, id: MemoryId) -> Result<Option<MemoryRecord>> {
        let row = sqlx::query(&format!(
            "SELECT {MEMORY_COLS} FROM memory_records m \
             WHERE m.memory_id = $1 AND upper_inf(m.transaction_time)"
        ))
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.as_ref().map(memory_from_row).transpose()
    }

    async fn memory_history(&self, id: MemoryId) -> Result<Vec<MemoryRecord>> {
        sqlx::query(&format!(
            "SELECT {MEMORY_COLS} FROM memory_records m WHERE m.memory_id = $1 \
             ORDER BY lower(m.transaction_time), m.revision_number"
        ))
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(memory_from_row)
        .collect()
    }

    async fn query_memories(&self, q: &MemoryQuery) -> Result<Vec<StoredMemory>> {
        let access = access_sql(
            [
                "m.tenant_id",
                "m.owner_user_id",
                "m.agent_id",
                "m.project_id",
                "m.visibility::text",
            ],
            [1, 2, 3, 4],
        );
        let sql = format!(
            "SELECT {MEMORY_COLS}, e.embedding FROM memory_records m \
             LEFT JOIN memory_embeddings e ON e.revision_id = m.revision_id \
                 AND e.model_name = $5 AND e.model_version = $6 \
                 AND (e.embedded_at IS NULL OR e.embedded_at <= $9) \
             WHERE {access} \
               AND m.domain::text = ANY($7) \
               AND m.status::text = ANY($8) \
               AND lower(m.transaction_time) <= $9 \
               AND ($10 OR m.transaction_time @> $9) \
               AND ($16 OR m.valid_time @> $11) \
               AND (m.expires_at IS NULL OR m.expires_at > $9) \
               AND ($12::uuid IS NULL OR m.project_id = $12) \
               AND ($13::uuid IS NULL OR m.conversation_id = $13) \
               AND ($14::uuid IS NULL OR m.agent_id = $14) \
             ORDER BY lower(m.transaction_time) DESC, m.revision_id ASC \
             LIMIT $15"
        );
        let filter = q.scope_filter.clone().unwrap_or_default();
        let (model, version) = q.embedding_model.clone().unwrap_or_default();
        let domains: Vec<&str> = q.domains.iter().map(|d| d.as_str()).collect();
        let statuses: Vec<&str> = q.statuses.iter().map(|s| s.as_str()).collect();
        let rows = sqlx::query(&sql)
            .bind(q.access.tenant_id.0)
            .bind(q.access.user_id.map(|u| u.0))
            .bind(q.access.agent_id.map(|a| a.0))
            .bind(project_ids(&q.access))
            .bind(model)
            .bind(version)
            .bind(domains)
            .bind(statuses)
            .bind(q.as_of_transaction_time)
            .bind(q.include_superseded)
            .bind(q.as_of_valid_time)
            .bind(filter.project_id.map(|p| p.0))
            .bind(filter.conversation_id.map(|c| c.0))
            .bind(filter.agent_id.map(|a| a.0))
            .bind(i64::from(q.limit))
            .bind(q.ignore_valid_time)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        rows.iter()
            .map(|row| {
                let embedding: Option<pgvector::Vector> =
                    row.try_get("embedding").map_err(db_err)?;
                Ok(StoredMemory {
                    record: memory_from_row(row)?,
                    embedding: embedding.map(|v| v.to_vec()),
                })
            })
            .collect()
    }

    async fn purge_memory(&self, id: MemoryId, event: EventDraft) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let deleted = sqlx::query("DELETE FROM memory_records WHERE memory_id = $1")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if deleted.rows_affected() == 0 {
            return Err(Error::not_found("memory", id));
        }
        sqlx::query(
            "INSERT INTO purge_ledger (kind, record_id, purged_at) VALUES ('memory', $1, $2) \
             ON CONFLICT (kind, record_id) DO NOTHING",
        )
        .bind(id.0)
        .bind(event.occurred_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn insert_evidence(&self, e: &Evidence, event: EventDraft) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO evidence (id, tenant_id, source_type, source, actor, observed_at, \
             recorded_at, extraction, content_hash, summary) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(e.id.0)
        .bind(e.tenant_id.0)
        .bind(e.source.kind())
        .bind(Json(&e.source))
        .bind(Json(&e.actor))
        .bind(e.observed_at)
        .bind(e.recorded_at)
        .bind(e.extraction.as_ref().map(Json))
        .bind(&e.content_hash)
        .bind(&e.summary)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn get_evidence(&self, ids: &[EvidenceId]) -> Result<Vec<Evidence>> {
        let ids: Vec<Uuid> = ids.iter().map(|i| i.0).collect();
        let rows = sqlx::query(
            "SELECT id, tenant_id, source, actor, observed_at, recorded_at, extraction, \
             content_hash, summary FROM evidence WHERE id = ANY($1) ORDER BY id",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter()
            .map(|r| {
                let Json(source): Json<SourceRef> = r.try_get("source").map_err(db_err)?;
                let Json(actor): Json<ActorRef> = r.try_get("actor").map_err(db_err)?;
                let extraction: Option<Json<cs_core::ExtractionInfo>> =
                    r.try_get("extraction").map_err(db_err)?;
                Ok(Evidence {
                    id: EvidenceId(r.try_get("id").map_err(db_err)?),
                    tenant_id: TenantId(r.try_get("tenant_id").map_err(db_err)?),
                    source,
                    actor,
                    observed_at: r.try_get("observed_at").map_err(db_err)?,
                    recorded_at: r.try_get("recorded_at").map_err(db_err)?,
                    extraction: extraction.map(|j| j.0),
                    content_hash: r.try_get("content_hash").map_err(db_err)?,
                    summary: r.try_get("summary").map_err(db_err)?,
                })
            })
            .collect()
    }

    async fn insert_belief(
        &self,
        belief: &Belief,
        evidence: &[BeliefEvidence],
        dependencies: &[BeliefDependency],
        embedding: Option<&EmbeddingRecord>,
        idempotency: Option<IdempotencyKey<'_>>,
        follow_up: &[cs_memory::Job],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        register_key(&mut tx, idempotency, belief.id.0).await?;
        insert_belief_rows(&mut tx, belief, evidence, dependencies).await?;
        if let Some(e) = embedding {
            upsert_embedding(&mut tx, EmbeddingTarget::Belief, belief.id.0, e).await?;
        }
        enqueue_all(&mut tx, follow_up).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn supersede_belief(
        &self,
        current: BeliefId,
        closed_at: OffsetDateTime,
        next: &Belief,
        evidence: &[BeliefEvidence],
        dependencies: &[BeliefDependency],
        embedding: Option<&EmbeddingRecord>,
        follow_up: &[cs_memory::Job],
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let closed = sqlx::query(
            "UPDATE beliefs \
             SET transaction_time = tstzrange(lower(transaction_time), $2, '[)'), \
                 superseded_by_belief_id = $3 \
             WHERE id = $1 AND upper_inf(transaction_time) AND lower(transaction_time) < $2",
        )
        .bind(current.0)
        .bind(closed_at)
        .bind(next.id.0)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if closed.rows_affected() != 1 {
            return Err(Error::conflict("belief revision is no longer current"));
        }
        insert_belief_rows(&mut tx, next, evidence, dependencies).await?;
        if let Some(e) = embedding {
            upsert_embedding(&mut tx, EmbeddingTarget::Belief, next.id.0, e).await?;
        }
        enqueue_all(&mut tx, follow_up).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn get_belief(&self, id: BeliefId) -> Result<Option<Belief>> {
        let row = sqlx::query(&format!("SELECT {BELIEF_COLS} FROM beliefs WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        row.as_ref().map(belief_from_row).transpose()
    }

    async fn current_belief(&self, lineage_id: BeliefId) -> Result<Option<Belief>> {
        let row = sqlx::query(&format!(
            "SELECT {BELIEF_COLS} FROM beliefs WHERE lineage_id = $1 AND upper_inf(transaction_time)"
        ))
        .bind(lineage_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.as_ref().map(belief_from_row).transpose()
    }

    async fn belief_lineage(&self, lineage_id: BeliefId) -> Result<Vec<Belief>> {
        sqlx::query(&format!(
            "SELECT {BELIEF_COLS} FROM beliefs WHERE lineage_id = $1 ORDER BY revision_number"
        ))
        .bind(lineage_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(belief_from_row)
        .collect()
    }

    async fn belief_evidence(&self, id: BeliefId) -> Result<Vec<BeliefEvidence>> {
        sqlx::query(
            "SELECT evidence_id, direction::text AS direction, strength FROM belief_evidence \
             WHERE belief_id = $1 ORDER BY evidence_id",
        )
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(link_from_row)
        .collect()
    }

    async fn insert_dependency(
        &self,
        dependency: &BeliefDependency,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        insert_dependency_row(&mut tx, dependency).await?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn dependencies_of(&self, id: BeliefId) -> Result<Vec<BeliefDependency>> {
        sqlx::query(
            "SELECT * FROM belief_dependencies WHERE to_belief_id = $1 \
             ORDER BY from_belief_id, relation",
        )
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(dependency_from_row)
        .collect()
    }

    async fn dependents_of(&self, id: BeliefId) -> Result<Vec<BeliefDependency>> {
        sqlx::query(
            "SELECT * FROM belief_dependencies WHERE from_belief_id = $1 \
             ORDER BY to_belief_id, relation",
        )
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(dependency_from_row)
        .collect()
    }

    async fn beliefs_for(&self, access: &AccessContext) -> Result<Vec<Belief>> {
        let clause = access_sql(TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        sqlx::query(&format!(
            "SELECT {BELIEF_COLS} FROM beliefs WHERE {clause} ORDER BY id"
        ))
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(belief_from_row)
        .collect()
    }

    async fn evidence_links(&self, ids: &[BeliefId]) -> Result<Vec<(BeliefId, BeliefEvidence)>> {
        let ids: Vec<Uuid> = ids.iter().map(|i| i.0).collect();
        sqlx::query(
            "SELECT belief_id, evidence_id, direction::text AS direction, strength \
             FROM belief_evidence WHERE belief_id = ANY($1) ORDER BY belief_id, evidence_id",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| {
            Ok((
                BeliefId(r.try_get("belief_id").map_err(db_err)?),
                link_from_row(r)?,
            ))
        })
        .collect()
    }

    async fn dependencies_among(&self, ids: &[BeliefId]) -> Result<Vec<BeliefDependency>> {
        let ids: Vec<Uuid> = ids.iter().map(|i| i.0).collect();
        sqlx::query(
            "SELECT * FROM belief_dependencies \
             WHERE from_belief_id = ANY($1) AND to_belief_id = ANY($1) \
             ORDER BY from_belief_id, to_belief_id, relation",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(dependency_from_row)
        .collect()
    }

    async fn append_event(&self, event: EventDraft) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn list_events(
        &self,
        access: &AccessContext,
        q: &EventQuery,
    ) -> Result<Vec<CognitiveEvent>> {
        let clause = access_sql(EVENT_ACCESS_COLS, [1, 2, 3, 4]);
        let sql = format!(
            "SELECT * FROM cognitive_events WHERE {clause} \
               AND ($5::uuid IS NULL OR stream_id = $5) \
               AND ($6::text IS NULL OR event_type = $6) \
               AND ($7::bigint IS NULL OR sequence > $7) \
             ORDER BY id LIMIT $8"
        );
        sqlx::query(&sql)
            .bind(access.tenant_id.0)
            .bind(access.user_id.map(|u| u.0))
            .bind(access.agent_id.map(|a| a.0))
            .bind(project_ids(access))
            .bind(q.stream_id.map(|s| s.0))
            .bind(q.event_type.map(|t| t.as_str()))
            .bind(
                q.after_sequence
                    .map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
            )
            .bind(i64::from(q.limit.unwrap_or(100).min(1000)))
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?
            .iter()
            .map(event_from_row)
            .collect()
    }

    async fn insert_trace(&self, t: &RecallTrace, event: EventDraft) -> Result<CognitiveEvent> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO recall_traces (id, tenant_id, policy, scoring_version, query_hash, \
             query_text, embedding_model, embedding_version, data_snapshot, executed_at, \
             belief_candidates, dense, query_embedding) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(t.id.0)
        .bind(t.tenant_id.0)
        .bind(Json(&t.policy))
        .bind(i32::try_from(t.scoring_version).unwrap_or(1))
        .bind(&t.query_hash)
        .bind(&t.query_text)
        .bind(&t.embedding_model)
        .bind(&t.embedding_version)
        .bind(&t.data_snapshot)
        .bind(t.executed_at)
        .bind(Json(&t.belief_candidates))
        .bind(Json(&t.dense))
        .bind(t.query_embedding.clone().map(pgvector::Vector::from))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        let c = &t.candidates;
        sqlx::query(
            "INSERT INTO recall_trace_candidates (trace_id, rank, memory_id, revision_id, score, \
             dense_score, lexical_score, recency_score, selected, reason) \
             SELECT $1, * FROM UNNEST($2::int[], $3::uuid[], $4::uuid[], $5::float8[], \
                 $6::float8[], $7::float8[], $8::float8[], $9::bool[], $10::text[])",
        )
        .bind(t.id.0)
        .bind(
            c.iter()
                .map(|x| i32::try_from(x.rank).unwrap_or(i32::MAX))
                .collect::<Vec<_>>(),
        )
        .bind(c.iter().map(|x| x.memory_id.0).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.revision_id.0).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.score).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.dense_score).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.lexical_score).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.recency_score).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.selected).collect::<Vec<_>>())
        .bind(c.iter().map(|x| x.reason.clone()).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        let event = append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(event)
    }

    async fn get_trace(&self, id: RecallTraceId) -> Result<Option<RecallTrace>> {
        let Some(row) = sqlx::query("SELECT * FROM recall_traces WHERE id = $1")
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?
        else {
            return Ok(None);
        };
        let candidates =
            sqlx::query("SELECT * FROM recall_trace_candidates WHERE trace_id = $1 ORDER BY rank")
                .bind(id.0)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?
                .iter()
                .map(|r| {
                    Ok(RecallCandidate {
                        memory_id: MemoryId(r.try_get("memory_id").map_err(db_err)?),
                        revision_id: MemoryRevisionId(r.try_get("revision_id").map_err(db_err)?),
                        rank: u32::try_from(r.try_get::<i32, _>("rank").map_err(db_err)?)
                            .map_err(Error::storage)?,
                        score: r.try_get("score").map_err(db_err)?,
                        dense_score: r.try_get("dense_score").map_err(db_err)?,
                        lexical_score: r.try_get("lexical_score").map_err(db_err)?,
                        recency_score: r.try_get("recency_score").map_err(db_err)?,
                        selected: r.try_get("selected").map_err(db_err)?,
                        reason: r.try_get("reason").map_err(db_err)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
        let Json(policy): Json<RecallPolicy> = row.try_get("policy").map_err(db_err)?;
        let Json(belief_candidates): Json<Vec<cs_memory::RecallBeliefCandidate>> =
            row.try_get("belief_candidates").map_err(db_err)?;
        let Json(dense): Json<cs_memory::DenseStatus> = row.try_get("dense").map_err(db_err)?;
        let query_embedding: Option<pgvector::Vector> =
            row.try_get("query_embedding").map_err(db_err)?;
        Ok(Some(RecallTrace {
            id,
            tenant_id: TenantId(row.try_get("tenant_id").map_err(db_err)?),
            policy,
            scoring_version: u32::try_from(
                row.try_get::<i32, _>("scoring_version").map_err(db_err)?,
            )
            .map_err(Error::storage)?,
            query_hash: row.try_get("query_hash").map_err(db_err)?,
            query_text: row.try_get("query_text").map_err(db_err)?,
            embedding_model: row.try_get("embedding_model").map_err(db_err)?,
            embedding_version: row.try_get("embedding_version").map_err(db_err)?,
            candidates,
            belief_candidates,
            dense,
            query_embedding: query_embedding.map(|v| v.to_vec()),
            data_snapshot: row.try_get("data_snapshot").map_err(db_err)?,
            executed_at: row.try_get("executed_at").map_err(db_err)?,
        }))
    }
    // ---- M3 (implemented in m3.rs) -------------------------------------------

    async fn query_beliefs(
        &self,
        q: &cs_storage::BeliefQuery,
    ) -> Result<Vec<cs_storage::StoredBelief>> {
        self.m3_query_beliefs(q).await
    }

    async fn job_counts(&self, tenant_id: TenantId) -> Result<Vec<cs_storage::JobCount>> {
        self.m3_job_counts(tenant_id).await
    }

    async fn embedding_coverage(
        &self,
        tenant: Option<TenantId>,
        model_name: &str,
        model_version: &str,
    ) -> Result<cs_storage::EmbeddingCoverage> {
        self.m3_embedding_coverage(tenant, model_name, model_version)
            .await
    }

    async fn enqueue_job(&self, job: &cs_memory::Job) -> Result<cs_memory::Job> {
        self.m3_enqueue_job(job).await
    }

    async fn claim_jobs(&self, claim: &cs_storage::JobClaim) -> Result<Vec<cs_memory::ClaimedJob>> {
        self.m3_claim_jobs(claim).await
    }

    async fn heartbeat_job(&self, lease: cs_memory::Lease, until: OffsetDateTime) -> Result<()> {
        self.m3_heartbeat(lease, until).await
    }

    async fn finish_job(
        &self,
        lease: cs_memory::Lease,
        now: OffsetDateTime,
        outcome: cs_memory::JobOutcome,
        effects: cs_storage::JobEffects,
    ) -> Result<cs_memory::Job> {
        self.m3_finish_job(lease, now, outcome, effects).await
    }

    async fn release_job(&self, lease: cs_memory::Lease, now: OffsetDateTime) -> Result<()> {
        self.m3_release(lease, now).await
    }

    async fn get_job(&self, id: cs_core::JobId) -> Result<Option<cs_memory::Job>> {
        self.m3_get_job(id).await
    }

    async fn list_jobs(
        &self,
        tenant_id: TenantId,
        status: Option<cs_memory::JobStatus>,
        limit: u32,
    ) -> Result<Vec<cs_memory::Job>> {
        self.m3_list_jobs(tenant_id, status, limit).await
    }

    async fn insert_observation(
        &self,
        observation: &cs_memory::Observation,
        job: &cs_memory::Job,
        idempotency: Option<IdempotencyKey<'_>>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        self.m3_insert_observation(observation, job, idempotency, event)
            .await
    }

    async fn get_observation(
        &self,
        id: cs_core::ObservationId,
    ) -> Result<Option<cs_memory::Observation>> {
        self.m3_get_observation(id).await
    }

    async fn extraction_runs(
        &self,
        id: cs_core::ObservationId,
    ) -> Result<Vec<cs_memory::ExtractionRun>> {
        self.m3_extraction_runs(id).await
    }

    async fn purge_observation(
        &self,
        id: cs_core::ObservationId,
        now: OffsetDateTime,
        delete: &[MemoryId],
        rehome: Vec<cs_storage::MemorySupersede>,
        event: EventDraft,
    ) -> Result<CognitiveEvent> {
        self.m3_purge_observation(id, now, delete, rehome, event)
            .await
    }

    async fn get_proposal(
        &self,
        id: cs_core::ProposalId,
    ) -> Result<Option<cs_memory::ResolutionProposal>> {
        self.m3_get_proposal(id).await
    }

    async fn insert_proposal(
        &self,
        proposal: &cs_memory::ResolutionProposal,
        event: EventDraft,
    ) -> Result<cs_memory::ResolutionProposal> {
        self.m3_insert_proposal(proposal, event).await
    }

    async fn list_proposals(
        &self,
        access: &AccessContext,
        status: Option<cs_memory::ProposalStatus>,
        limit: u32,
    ) -> Result<Vec<cs_memory::ResolutionProposal>> {
        self.m3_list_proposals(access, status, limit).await
    }

    async fn decide_proposal(
        &self,
        id: cs_core::ProposalId,
        decision: &cs_memory::ProposalDecision,
        mutations: cs_storage::ProposalMutations,
        events: Vec<EventDraft>,
    ) -> Result<cs_memory::ResolutionProposal> {
        self.m3_decide_proposal(id, decision, mutations, events)
            .await
    }

    async fn get_entity(&self, id: cs_core::EntityId) -> Result<Option<cs_memory::entity::Entity>> {
        self.m4_get_entity(id).await
    }

    async fn entities_by_key(
        &self,
        access: &AccessContext,
        key: &str,
    ) -> Result<Vec<cs_memory::entity::Entity>> {
        self.m4_entities_by_key(access, key).await
    }

    async fn entity_aliases(
        &self,
        id: cs_core::EntityId,
    ) -> Result<Vec<cs_memory::entity::EntityAlias>> {
        self.m4_entity_aliases(id).await
    }

    async fn entity_mentions(
        &self,
        id: cs_core::EntityId,
        limit: u32,
    ) -> Result<Vec<cs_memory::entity::EntityMention>> {
        self.m4_entity_mentions(id, limit).await
    }

    async fn add_entity_alias(
        &self,
        id: cs_core::EntityId,
        expected_version: u32,
        alias: &cs_memory::entity::EntityAlias,
        event: EventDraft,
    ) -> Result<cs_memory::entity::Entity> {
        self.m4_add_entity_alias(id, expected_version, alias, event)
            .await
    }

    async fn set_entity_kind(
        &self,
        id: cs_core::EntityId,
        expected_version: u32,
        kind: cs_memory::entity::EntityKind,
        at: OffsetDateTime,
        event: EventDraft,
    ) -> Result<cs_memory::entity::Entity> {
        self.m4_set_entity_kind(id, expected_version, kind, at, event)
            .await
    }

    async fn partition_entities(
        &self,
        partition: &str,
        limit: u32,
    ) -> Result<Vec<cs_memory::entity::Entity>> {
        self.m4_partition_entities(partition, limit).await
    }

    async fn entity_edges(
        &self,
        query: &cs_storage::EdgeQuery,
    ) -> Result<Vec<cs_memory::entity::KnowledgeEdge>> {
        self.m4_entity_edges(query).await
    }

    async fn purged_at(
        &self,
        kind: cs_storage::PurgeKind,
        id: Uuid,
    ) -> Result<Option<OffsetDateTime>> {
        self.m3_purged_at(kind, id).await
    }

    async fn purge_evidence(
        &self,
        id: EvidenceId,
        now: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Vec<BeliefId>> {
        self.m3_purge_evidence(id, now, event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_sql_references_all_parameters() {
        let s = access_sql(TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        for p in ["$1", "$2", "$3", "$4"] {
            assert!(s.contains(p), "{s}");
        }
    }

    #[test]
    fn range_roundtrip() {
        let i = TimeInterval::from(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(from_range(to_range(&i)).unwrap(), i);
    }
}
