//! M4.3 persistence: derivations (knowledge compilation).

use cs_core::{AccessContext, DerivationId, EntityId, Error, MemoryId, Result};
use cs_memory::derivation::{Derivation, DerivationStatus};
use cs_storage::{DerivationWrite, JobEffects};
use sqlx::{Row, postgres::PgRow, types::Json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{PgStore, Tx, access_sql, db_err, parse, project_ids, scope_from_row};

const DERIVATION_COLS: &str = "id, tenant_id, owner_user_id, agent_id, project_id, conversation_id, \
    visibility::text AS visibility, partition_key, operator, signature, subject_entity_id, relation, \
    object_entity_id, derived_memory_id, thresholds, support, witnesses, status, status_reason, \
    version, created_at, updated_at, \
    ARRAY(SELECT root_memory_id FROM derivation_roots r WHERE r.derivation_id = derivations.id \
          ORDER BY root_memory_id) AS roots";

fn derivation_from_row(r: &PgRow) -> Result<Derivation> {
    let status: String = r.try_get("status").map_err(db_err)?;
    let Json(thresholds) = r.try_get("thresholds").map_err(db_err)?;
    let Json(support) = r.try_get("support").map_err(db_err)?;
    let Json(witnesses) = r.try_get("witnesses").map_err(db_err)?;
    let roots: Vec<Uuid> = r.try_get("roots").map_err(db_err)?;
    Ok(Derivation {
        id: DerivationId(r.try_get("id").map_err(db_err)?),
        scope: scope_from_row(r)?,
        partition: r.try_get("partition_key").map_err(db_err)?,
        operator: r.try_get("operator").map_err(db_err)?,
        signature: r.try_get("signature").map_err(db_err)?,
        subject: EntityId(r.try_get("subject_entity_id").map_err(db_err)?),
        relation: r.try_get("relation").map_err(db_err)?,
        object: EntityId(r.try_get("object_entity_id").map_err(db_err)?),
        derived_memory_id: MemoryId(r.try_get("derived_memory_id").map_err(db_err)?),
        thresholds,
        support,
        witnesses,
        roots: roots.into_iter().map(MemoryId).collect(),
        status: parse(&status, DerivationStatus::parse, "derivation status")?,
        status_reason: r.try_get("status_reason").map_err(db_err)?,
        version: u32::try_from(r.try_get::<i32, _>("version").map_err(db_err)?)
            .map_err(Error::storage)?,
        created_at: r.try_get("created_at").map_err(db_err)?,
        updated_at: r.try_get("updated_at").map_err(db_err)?,
    })
}

fn version(v: u32) -> Result<i32> {
    i32::try_from(v).map_err(Error::storage)
}

/// Another job got there first: a derivation version moved, the signature
/// was claimed, or a revision to supersede is no longer current. The job
/// is retried rather than failed. Takes the locks the writes will need.
pub(crate) async fn contention(tx: &mut Tx<'_>, e: &JobEffects) -> Result<Option<&'static str>> {
    let mut writes: Vec<&DerivationWrite> = e.derivations.iter().collect();
    writes.sort_by_key(|w| w.derivation.id);
    for w in writes {
        let d = &w.derivation;
        let current: Option<i32> =
            sqlx::query_scalar("SELECT version FROM derivations WHERE id = $1 FOR UPDATE")
                .bind(d.id.0)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_err)?;
        let expected = w.expected_version.map(version).transpose()?;
        if current != expected {
            return Ok(Some("derivation changed concurrently"));
        }
        if expected.is_none() {
            let taken: Option<i32> = sqlx::query_scalar(
                "SELECT 1 FROM derivations WHERE partition_key = $1 AND operator = $2 \
                 AND signature = $3",
            )
            .bind(&d.partition)
            .bind(&d.operator)
            .bind(&d.signature)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_err)?;
            if taken.is_some() {
                return Ok(Some("derivation changed concurrently"));
            }
        }
    }
    let mut supersedes: Vec<_> = e.supersedes.iter().collect();
    supersedes.sort_by_key(|m| m.current);
    for m in supersedes {
        let current: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM memory_records WHERE revision_id = $1 AND upper_inf(transaction_time) \
             AND lower(transaction_time) < $2 FOR UPDATE",
        )
        .bind(m.current.0)
        .bind(m.closed_at)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;
        if current.is_none() {
            return Ok(Some("revision changed concurrently"));
        }
    }
    Ok(None)
}

/// Insert or update one derivation and replace its root set.
pub(crate) async fn write_derivation(tx: &mut Tx<'_>, w: &DerivationWrite) -> Result<()> {
    let d = &w.derivation;
    let s = &d.scope;
    sqlx::query(
        "INSERT INTO derivations (id, tenant_id, owner_user_id, agent_id, project_id, \
         conversation_id, visibility, partition_key, operator, signature, subject_entity_id, \
         relation, object_entity_id, derived_memory_id, thresholds, support, witnesses, status, \
         status_reason, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6::visibility, $7, $8, $9, $10, $11, $12, $13, $14, \
                 $15, $16, $17, $18, $19, $20, $21) \
         ON CONFLICT (id) DO UPDATE SET signature = EXCLUDED.signature, \
             subject_entity_id = EXCLUDED.subject_entity_id, \
             object_entity_id = EXCLUDED.object_entity_id, thresholds = EXCLUDED.thresholds, \
             support = EXCLUDED.support, witnesses = EXCLUDED.witnesses, \
             status = EXCLUDED.status, status_reason = EXCLUDED.status_reason, \
             version = EXCLUDED.version, updated_at = EXCLUDED.updated_at",
    )
    .bind(d.id.0)
    .bind(s.tenant_id.0)
    .bind(s.owner_user_id.map(|u| u.0))
    .bind(s.agent_id.map(|a| a.0))
    .bind(s.project_id.map(|p| p.0))
    .bind(s.visibility.as_str())
    .bind(&d.partition)
    .bind(&d.operator)
    .bind(&d.signature)
    .bind(d.subject.0)
    .bind(&d.relation)
    .bind(d.object.0)
    .bind(d.derived_memory_id.0)
    .bind(Json(&d.thresholds))
    .bind(Json(&d.support))
    .bind(Json(&d.witnesses))
    .bind(d.status.as_str())
    .bind(&d.status_reason)
    .bind(version(d.version)?)
    .bind(d.created_at)
    .bind(d.updated_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    sqlx::query("DELETE FROM derivation_roots WHERE derivation_id = $1")
        .bind(d.id.0)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    sqlx::query(
        "INSERT INTO derivation_roots (derivation_id, root_memory_id) \
         SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(d.id.0)
    .bind(d.roots.iter().map(|m| m.0).collect::<Vec<_>>())
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// A purge reaches derivations in the same transaction: a derivation of a
/// purged memory goes with it, and one citing a purged root is flagged for
/// revalidation.
pub(crate) async fn flag_derivations(
    tx: &mut Tx<'_>,
    memories: &[Uuid],
    at: OffsetDateTime,
) -> Result<()> {
    sqlx::query("DELETE FROM derivations WHERE derived_memory_id = ANY($1)")
        .bind(memories)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    sqlx::query(
        "UPDATE derivations SET status = 'revalidation_required', \
             status_reason = 'a supporting root was purged', version = version + 1, updated_at = $2 \
         WHERE id IN (SELECT derivation_id FROM derivation_roots WHERE root_memory_id = ANY($1))",
    )
    .bind(memories)
    .bind(at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

impl PgStore {
    pub(crate) async fn m43_get_derivation(&self, id: DerivationId) -> Result<Option<Derivation>> {
        sqlx::query(&format!(
            "SELECT {DERIVATION_COLS} FROM derivations WHERE id = $1"
        ))
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?
        .as_ref()
        .map(derivation_from_row)
        .transpose()
    }

    pub(crate) async fn m43_derivation_for(
        &self,
        partition: &str,
        operator: &str,
        signature: &str,
    ) -> Result<Option<Derivation>> {
        sqlx::query(&format!(
            "SELECT {DERIVATION_COLS} FROM derivations \
             WHERE partition_key = $1 AND operator = $2 AND signature = $3"
        ))
        .bind(partition)
        .bind(operator)
        .bind(signature)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?
        .as_ref()
        .map(derivation_from_row)
        .transpose()
    }

    pub(crate) async fn m43_derivations_citing(&self, memory: MemoryId) -> Result<Vec<Derivation>> {
        sqlx::query(&format!(
            "SELECT {DERIVATION_COLS} FROM derivations WHERE id IN \
             (SELECT derivation_id FROM derivation_roots WHERE root_memory_id = $1) ORDER BY id"
        ))
        .bind(memory.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(derivation_from_row)
        .collect()
    }

    pub(crate) async fn m43_list_derivations(
        &self,
        access: &AccessContext,
        status: Option<DerivationStatus>,
        limit: u32,
    ) -> Result<Vec<Derivation>> {
        let clause = access_sql(super::TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        sqlx::query(&format!(
            "SELECT {DERIVATION_COLS} FROM derivations WHERE {clause} \
               AND ($5::text IS NULL OR status = $5) \
             ORDER BY created_at DESC, id DESC LIMIT $6"
        ))
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .bind(status.map(DerivationStatus::as_str))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(derivation_from_row)
        .collect()
    }
}
