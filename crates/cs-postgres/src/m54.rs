//! M5.4 persistence: information needs.

use cs_core::{
    AccessContext, Error, EventDraft, EventStreamId, InformationNeedId, RecallTraceId, Result,
};
use cs_memory::need::{InformationNeed, NeedStatus, TermStats};
use cs_storage::MemoryQuery;
use sqlx::{Row, types::Json};
use time::OffsetDateTime;

use super::{
    PgStore, Tx, access_sql, append_event, bind_memory_query, db_err, memory_filters, project_ids,
};

fn version(v: u32) -> Result<i32> {
    i32::try_from(v).map_err(Error::storage)
}

async fn lock_need(
    tx: &mut Tx<'_>,
    partition: &str,
    signature: &str,
) -> Result<Option<InformationNeed>> {
    let row: Option<Json<InformationNeed>> = sqlx::query_scalar(
        "SELECT body FROM information_needs WHERE partition_key = $1 AND signature = $2 FOR UPDATE",
    )
    .bind(partition)
    .bind(signature)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(row.map(|Json(n)| n))
}

async fn upsert(tx: &mut Tx<'_>, n: &InformationNeed) -> Result<()> {
    let s = &n.scope;
    sqlx::query(
        "INSERT INTO information_needs (id, tenant_id, owner_user_id, agent_id, project_id, \
         conversation_id, visibility, partition_key, signature, status, last_seen, version, body) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6::visibility, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, \
             last_seen = EXCLUDED.last_seen, version = EXCLUDED.version, body = EXCLUDED.body",
    )
    .bind(n.id.0)
    .bind(s.tenant_id.0)
    .bind(s.owner_user_id.map(|u| u.0))
    .bind(s.agent_id.map(|a| a.0))
    .bind(s.project_id.map(|p| p.0))
    .bind(s.visibility.as_str())
    .bind(&n.partition)
    .bind(n.signature.key())
    .bind(n.status.as_str())
    .bind(n.last_seen)
    .bind(version(n.version)?)
    .bind(Json(n))
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

impl PgStore {
    pub(crate) async fn m54_term_stats(
        &self,
        q: &MemoryQuery,
        terms: &[String],
    ) -> Result<TermStats> {
        let filters = memory_filters();
        // $17: the terms. One statement counts the eligible population and
        // each term's document frequency within it.
        let sql = format!(
            "WITH eligible AS (SELECT m.index_terms FROM memory_records m WHERE {filters}) \
             SELECT (SELECT count(*) FROM eligible) AS documents, \
                    ARRAY(SELECT (SELECT count(*) FROM eligible e WHERE e.index_terms @> ARRAY[t]) \
                          FROM unnest($17::text[]) WITH ORDINALITY AS u(t, i) ORDER BY i) AS frequencies"
        );
        let row = bind_memory_query(sqlx::query(&sql), q)
            .bind(terms.to_vec())
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?;
        let documents: i64 = row.try_get("documents").map_err(db_err)?;
        let frequencies: Vec<i64> = row.try_get("frequencies").map_err(db_err)?;
        Ok(TermStats {
            documents: u64::try_from(documents).map_err(Error::storage)?,
            frequencies: frequencies
                .into_iter()
                .map(|f| u64::try_from(f).map_err(Error::storage))
                .collect::<Result<_>>()?,
        })
    }

    pub(crate) async fn m54_get_need(&self, id: InformationNeedId) -> Result<Option<InformationNeed>> {
        let row: Option<Json<InformationNeed>> =
            sqlx::query_scalar("SELECT body FROM information_needs WHERE id = $1")
                .bind(id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.map(|Json(n)| n))
    }

    pub(crate) async fn m54_find_need(
        &self,
        partition: &str,
        signature: &str,
    ) -> Result<Option<InformationNeed>> {
        let row: Option<Json<InformationNeed>> = sqlx::query_scalar(
            "SELECT body FROM information_needs WHERE partition_key = $1 AND signature = $2",
        )
        .bind(partition)
        .bind(signature)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|Json(n)| n))
    }

    pub(crate) async fn m54_list_needs(
        &self,
        access: &AccessContext,
        status: Option<NeedStatus>,
        limit: u32,
    ) -> Result<Vec<InformationNeed>> {
        let clause = access_sql(super::TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        let rows: Vec<Json<InformationNeed>> = sqlx::query_scalar(&format!(
            "SELECT body FROM information_needs WHERE {clause} \
               AND ($5::text IS NULL OR status = $5) \
             ORDER BY last_seen DESC, id LIMIT $6"
        ))
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .bind(status.map(NeedStatus::as_str))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|Json(n)| n).collect())
    }

    pub(crate) async fn m54_observe_need(
        &self,
        template: &InformationNeed,
        session_key: &str,
        trace: RecallTraceId,
        at: OffsetDateTime,
        mut event: EventDraft,
    ) -> Result<InformationNeed> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let key = template.signature.key();
        // Insert the template unless the need exists; a concurrent first
        // miss loses the race harmlessly and observes instead.
        let inserted = sqlx::query(
            "INSERT INTO information_needs (id, tenant_id, owner_user_id, agent_id, project_id, \
             conversation_id, visibility, partition_key, signature, status, last_seen, version, \
             body) VALUES ($1, $2, $3, $4, $5, NULL, $6::visibility, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (partition_key, signature) DO NOTHING",
        )
        .bind(template.id.0)
        .bind(template.scope.tenant_id.0)
        .bind(template.scope.owner_user_id.map(|u| u.0))
        .bind(template.scope.agent_id.map(|a| a.0))
        .bind(template.scope.project_id.map(|p| p.0))
        .bind(template.scope.visibility.as_str())
        .bind(&template.partition)
        .bind(&key)
        .bind(template.status.as_str())
        .bind(template.last_seen)
        .bind(version(template.version)?)
        .bind(Json(template))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?
        .rows_affected()
            == 1;
        let mut need = lock_need(&mut tx, &template.partition, &key)
            .await?
            .ok_or_else(|| Error::storage("information need vanished"))?;
        let new_session = sqlx::query(
            "INSERT INTO information_need_sessions (need_id, session_key) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(need.id.0)
        .bind(session_key)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?
        .rows_affected()
            == 1;
        if !inserted {
            need.observe(new_session, trace, at);
            upsert(&mut tx, &need).await?;
        }
        event.stream_id = EventStreamId(need.id.0);
        event.payload = cs_storage::need_observed_payload(&need, trace);
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(need)
    }

    pub(crate) async fn m54_write_need(
        &self,
        need: &InformationNeed,
        expected_version: Option<u32>,
        event: EventDraft,
    ) -> Result<InformationNeed> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let current: Option<i32> =
            sqlx::query_scalar("SELECT version FROM information_needs WHERE id = $1 FOR UPDATE")
                .bind(need.id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if current != expected_version.map(version).transpose()? {
            return Err(Error::conflict("information need changed; reload it"));
        }
        if expected_version.is_none()
            && lock_need(&mut tx, &need.partition, &need.signature.key())
                .await?
                .is_some()
        {
            return Err(Error::conflict("this need is already recorded"));
        }
        upsert(&mut tx, need).await?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(need.clone())
    }

    pub(crate) async fn m54_purge_need(&self, id: InformationNeedId, event: EventDraft) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let r = sqlx::query("DELETE FROM information_needs WHERE id = $1")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if r.rows_affected() == 0 {
            return Err(Error::not_found("information need", id));
        }
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}
