//! M4.4 persistence: capability executions, verifications and evaluation
//! cases.

use cs_core::{
    EvaluationCaseId, Error, EventDraft, ExecutionId, MemoryId, MemoryRevisionId, Result, Scope,
    VerificationId,
};
use cs_memory::capability::{
    AbortReason, CaseKind, EvaluationCase, Execution, ExecutionStatus, OracleTier, Verdict,
    Verification,
};
use sqlx::{Row, postgres::PgRow, types::Json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{PgStore, Tx, append_event, db_err, parse, scope_from_row};

const EXECUTION_COLS: &str = "id, capability_id, capability_revision, procedure_fingerprint, \
    tenant_id, owner_user_id, agent_id, project_id, conversation_id, visibility::text AS visibility, \
    case_id, input_class, actor, status, abort_reason, started_at, finished_at";

const VERIFICATION_COLS: &str =
    "id, execution_id, tier, verdict, verifier, detail, recorded_at";

const CASE_COLS: &str = "id, capability_id, tenant_id, owner_user_id, agent_id, project_id, \
    conversation_id, visibility::text AS visibility, kind, input_class, description, locked, origin, \
    created_at";

fn execution_from_row(r: &PgRow) -> Result<Execution> {
    let status: String = r.try_get("status").map_err(db_err)?;
    let reason: Option<String> = r.try_get("abort_reason").map_err(db_err)?;
    let Json(actor) = r.try_get("actor").map_err(db_err)?;
    Ok(Execution {
        id: ExecutionId(r.try_get("id").map_err(db_err)?),
        capability_id: MemoryId(r.try_get("capability_id").map_err(db_err)?),
        capability_revision: MemoryRevisionId(r.try_get("capability_revision").map_err(db_err)?),
        procedure_fingerprint: r.try_get("procedure_fingerprint").map_err(db_err)?,
        scope: scope_from_row(r)?,
        case_id: r
            .try_get::<Option<Uuid>, _>("case_id")
            .map_err(db_err)?
            .map(EvaluationCaseId),
        input_class: r.try_get("input_class").map_err(db_err)?,
        actor,
        status: parse(&status, ExecutionStatus::parse, "execution status")?,
        abort_reason: reason
            .map(|v| parse(&v, AbortReason::parse, "abort reason"))
            .transpose()?,
        started_at: r.try_get("started_at").map_err(db_err)?,
        finished_at: r.try_get("finished_at").map_err(db_err)?,
        verifications: Vec::new(),
    })
}

fn verification_from_row(r: &PgRow) -> Result<Verification> {
    let tier: String = r.try_get("tier").map_err(db_err)?;
    let verdict: String = r.try_get("verdict").map_err(db_err)?;
    let Json(verifier) = r.try_get("verifier").map_err(db_err)?;
    Ok(Verification {
        id: VerificationId(r.try_get("id").map_err(db_err)?),
        execution_id: ExecutionId(r.try_get("execution_id").map_err(db_err)?),
        tier: parse(&tier, OracleTier::parse, "oracle tier")?,
        verdict: parse(&verdict, Verdict::parse, "verdict")?,
        verifier,
        detail: r.try_get("detail").map_err(db_err)?,
        recorded_at: r.try_get("recorded_at").map_err(db_err)?,
    })
}

fn case_from_row(r: &PgRow) -> Result<EvaluationCase> {
    let kind: String = r.try_get("kind").map_err(db_err)?;
    let Json(origin) = r.try_get("origin").map_err(db_err)?;
    Ok(EvaluationCase {
        id: EvaluationCaseId(r.try_get("id").map_err(db_err)?),
        capability_id: MemoryId(r.try_get("capability_id").map_err(db_err)?),
        scope: scope_from_row(r)?,
        kind: parse(&kind, CaseKind::parse, "case kind")?,
        input_class: r.try_get("input_class").map_err(db_err)?,
        description: r.try_get("description").map_err(db_err)?,
        locked: r.try_get("locked").map_err(db_err)?,
        origin,
        created_at: r.try_get("created_at").map_err(db_err)?,
    })
}

fn scope_binds(s: &Scope) -> (Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>, &'static str) {
    (
        s.tenant_id.0,
        s.owner_user_id.map(|u| u.0),
        s.agent_id.map(|a| a.0),
        s.project_id.map(|p| p.0),
        s.conversation_id.map(|c| c.0),
        s.visibility.as_str(),
    )
}

/// Insert a case; `false` if an equal (capability, kind, input class)
/// case already exists.
async fn insert_case_row(tx: &mut Tx<'_>, c: &EvaluationCase) -> Result<bool> {
    let (t, o, a, p, cv, v) = scope_binds(&c.scope);
    let r = sqlx::query(
        "INSERT INTO evaluation_cases (id, capability_id, tenant_id, owner_user_id, agent_id, \
         project_id, conversation_id, visibility, kind, input_class, description, locked, origin, \
         created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::visibility, $9, $10, $11, $12, $13, $14) \
         ON CONFLICT (capability_id, kind, input_class) DO NOTHING",
    )
    .bind(c.id.0)
    .bind(c.capability_id.0)
    .bind(t)
    .bind(o)
    .bind(a)
    .bind(p)
    .bind(cv)
    .bind(v)
    .bind(c.kind.as_str())
    .bind(&c.input_class)
    .bind(&c.description)
    .bind(c.locked)
    .bind(Json(&c.origin))
    .bind(c.created_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(r.rows_affected() == 1)
}

/// Executions, verifications and cases go with their capability.
pub(crate) async fn drop_capability_records(tx: &mut Tx<'_>, memories: &[Uuid]) -> Result<()> {
    for sql in [
        "DELETE FROM capability_executions WHERE capability_id = ANY($1)",
        "DELETE FROM evaluation_cases WHERE capability_id = ANY($1)",
    ] {
        sqlx::query(sql)
            .bind(memories)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

impl PgStore {
    async fn attach_verifications(&self, mut execs: Vec<Execution>) -> Result<Vec<Execution>> {
        let ids: Vec<Uuid> = execs.iter().map(|e| e.id.0).collect();
        let rows = sqlx::query(&format!(
            "SELECT {VERIFICATION_COLS} FROM execution_verifications \
             WHERE execution_id = ANY($1) ORDER BY recorded_at, id"
        ))
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        for r in &rows {
            let v = verification_from_row(r)?;
            if let Some(e) = execs.iter_mut().find(|e| e.id == v.execution_id) {
                e.verifications.push(v);
            }
        }
        Ok(execs)
    }

    async fn one_execution(&self, id: ExecutionId) -> Result<Option<Execution>> {
        let row = sqlx::query(&format!(
            "SELECT {EXECUTION_COLS} FROM capability_executions WHERE id = $1"
        ))
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        match row {
            None => Ok(None),
            Some(r) => Ok(self
                .attach_verifications(vec![execution_from_row(&r)?])
                .await?
                .pop()),
        }
    }

    pub(crate) async fn m44_insert_execution(
        &self,
        e: &Execution,
        event: EventDraft,
    ) -> Result<Execution> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let (t, o, a, p, cv, v) = scope_binds(&e.scope);
        sqlx::query(
            "INSERT INTO capability_executions (id, capability_id, capability_revision, \
             procedure_fingerprint, tenant_id, owner_user_id, agent_id, project_id, conversation_id, \
             visibility, case_id, input_class, actor, status, abort_reason, started_at, finished_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::visibility, $11, $12, $13, $14, $15, \
                     $16, $17)",
        )
        .bind(e.id.0)
        .bind(e.capability_id.0)
        .bind(e.capability_revision.0)
        .bind(e.procedure_fingerprint)
        .bind(t)
        .bind(o)
        .bind(a)
        .bind(p)
        .bind(cv)
        .bind(v)
        .bind(e.case_id.map(|c| c.0))
        .bind(&e.input_class)
        .bind(Json(&e.actor))
        .bind(e.status.as_str())
        .bind(e.abort_reason.map(AbortReason::as_str))
        .bind(e.started_at)
        .bind(e.finished_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(e.clone())
    }

    pub(crate) async fn m44_finish_execution(
        &self,
        id: ExecutionId,
        status: ExecutionStatus,
        abort_reason: Option<AbortReason>,
        at: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Execution> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let current: Option<String> = sqlx::query_scalar(
            "SELECT status FROM capability_executions WHERE id = $1 FOR UPDATE",
        )
        .bind(id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        match current.as_deref() {
            None => return Err(Error::not_found("execution", id)),
            Some("started") => {}
            Some(_) => return Err(Error::conflict("execution already finished")),
        }
        sqlx::query(
            "UPDATE capability_executions SET status = $2, abort_reason = $3, finished_at = $4 \
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(status.as_str())
        .bind(abort_reason.map(AbortReason::as_str))
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        self.one_execution(id)
            .await?
            .ok_or_else(|| Error::not_found("execution", id))
    }

    pub(crate) async fn m44_add_verification(
        &self,
        v: &Verification,
        ratchet: Option<(&EvaluationCase, EventDraft)>,
        event: EventDraft,
    ) -> Result<Execution> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let exists: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM capability_executions WHERE id = $1 FOR SHARE",
        )
        .bind(v.execution_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if exists.is_none() {
            return Err(Error::not_found("execution", v.execution_id));
        }
        sqlx::query(
            "INSERT INTO execution_verifications (id, execution_id, tier, verdict, verifier, \
             detail, recorded_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(v.id.0)
        .bind(v.execution_id.0)
        .bind(v.tier.as_str())
        .bind(v.verdict.as_str())
        .bind(Json(&v.verifier))
        .bind(&v.detail)
        .bind(v.recorded_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        append_event(&mut tx, event).await?;
        if let Some((case, event)) = ratchet
            && insert_case_row(&mut tx, case).await?
        {
            append_event(&mut tx, event).await?;
        }
        tx.commit().await.map_err(db_err)?;
        self.one_execution(v.execution_id)
            .await?
            .ok_or_else(|| Error::not_found("execution", v.execution_id))
    }

    pub(crate) async fn m44_get_execution(&self, id: ExecutionId) -> Result<Option<Execution>> {
        self.one_execution(id).await
    }

    pub(crate) async fn m44_executions_for(
        &self,
        capability: MemoryId,
        limit: u32,
    ) -> Result<Vec<Execution>> {
        let execs = sqlx::query(&format!(
            "SELECT {EXECUTION_COLS} FROM capability_executions WHERE capability_id = $1 \
             ORDER BY started_at, id LIMIT $2"
        ))
        .bind(capability.0)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(execution_from_row)
        .collect::<Result<Vec<_>>>()?;
        self.attach_verifications(execs).await
    }

    pub(crate) async fn m44_insert_case(
        &self,
        c: &EvaluationCase,
        event: EventDraft,
    ) -> Result<EvaluationCase> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if !insert_case_row(&mut tx, c).await? {
            return Err(Error::conflict(
                "a case of this kind already covers that input class",
            ));
        }
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(c.clone())
    }

    pub(crate) async fn m44_cases_for(&self, capability: MemoryId) -> Result<Vec<EvaluationCase>> {
        sqlx::query(&format!(
            "SELECT {CASE_COLS} FROM evaluation_cases WHERE capability_id = $1 \
             ORDER BY created_at, id"
        ))
        .bind(capability.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(case_from_row)
        .collect()
    }
}
