//! M6.2 persistence: causal hypotheses (versioned) and interventions.

use cs_core::{AccessContext, Error, EventDraft, HypothesisId, InterventionId, Result, Scope};
use cs_memory::causal::{Hypothesis, HypothesisStatus, Intervention, OutcomeObservation};
use sqlx::types::Json;

use super::{PgStore, access_sql, append_event, db_err, project_ids};

fn version(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(Error::storage)
}

fn scope_cols(
    s: &Scope,
) -> (
    uuid::Uuid,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    &'static str,
) {
    (
        s.tenant_id.0,
        s.owner_user_id.map(|u| u.0),
        s.agent_id.map(|a| a.0),
        s.project_id.map(|p| p.0),
        s.visibility.as_str(),
    )
}

impl PgStore {
    pub(crate) async fn m62_write_hypothesis(
        &self,
        h: &Hypothesis,
        expected_version: Option<u64>,
        event: EventDraft,
    ) -> Result<Hypothesis> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Serialize writers of one hypothesis.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 62))")
            .bind(h.id.0.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT max(version) FROM hypothesis_versions WHERE id = $1")
                .bind(h.id.0)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_err)?;
        if current != expected_version.map(version).transpose()? {
            return Err(Error::conflict("hypothesis changed; reload it"));
        }
        let (tenant, owner, agent, project, vis) = scope_cols(&h.scope);
        sqlx::query(
            "INSERT INTO hypothesis_versions (id, version, tenant_id, owner_user_id, agent_id, \
             project_id, visibility, status, updated_at, body) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::visibility, $8, $9, $10)",
        )
        .bind(h.id.0)
        .bind(version(h.version)?)
        .bind(tenant)
        .bind(owner)
        .bind(agent)
        .bind(project)
        .bind(vis)
        .bind(h.status.as_str())
        .bind(h.updated_at)
        .bind(Json(h))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(h.clone())
    }

    pub(crate) async fn m62_get_hypothesis(&self, id: HypothesisId) -> Result<Option<Hypothesis>> {
        let row: Option<Json<Hypothesis>> = sqlx::query_scalar(
            "SELECT body FROM hypothesis_versions WHERE id = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|Json(h)| h))
    }

    pub(crate) async fn m62_hypothesis_history(&self, id: HypothesisId) -> Result<Vec<Hypothesis>> {
        let rows: Vec<Json<Hypothesis>> = sqlx::query_scalar(
            "SELECT body FROM hypothesis_versions WHERE id = $1 ORDER BY version",
        )
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|Json(h)| h).collect())
    }

    pub(crate) async fn m62_list_hypotheses(
        &self,
        access: &AccessContext,
        status: Option<HypothesisStatus>,
        limit: u32,
    ) -> Result<Vec<Hypothesis>> {
        let clause = access_sql(super::TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        let rows: Vec<Json<Hypothesis>> = sqlx::query_scalar(&format!(
            "SELECT body FROM (SELECT DISTINCT ON (id) * FROM hypothesis_versions \
                 WHERE {clause} ORDER BY id, version DESC) h \
             WHERE ($5::text IS NULL OR status = $5) \
             ORDER BY updated_at DESC, id LIMIT $6"
        ))
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .bind(status.map(HypothesisStatus::as_str))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|Json(h)| h).collect())
    }

    pub(crate) async fn m62_insert_intervention(
        &self,
        i: &Intervention,
        event: EventDraft,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let (tenant, owner, agent, project, vis) = scope_cols(&i.scope);
        let r = sqlx::query(
            "INSERT INTO interventions (id, tenant_id, owner_user_id, agent_id, project_id, \
             visibility, environment, treatment, recorded_at, body) \
             VALUES ($1, $2, $3, $4, $5, $6::visibility, $7, $8, $9, $10) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(i.id.0)
        .bind(tenant)
        .bind(owner)
        .bind(agent)
        .bind(project)
        .bind(vis)
        .bind(&i.environment)
        .bind(&i.treatment)
        .bind(i.recorded_at)
        .bind(Json(i))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if r.rows_affected() == 0 {
            return Err(Error::conflict("intervention already recorded"));
        }
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub(crate) async fn m62_get_intervention(
        &self,
        id: InterventionId,
    ) -> Result<Option<Intervention>> {
        let row: Option<Json<Intervention>> =
            sqlx::query_scalar("SELECT body FROM interventions WHERE id = $1")
                .bind(id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.map(|Json(i)| i))
    }

    pub(crate) async fn m62_observe_intervention(
        &self,
        id: InterventionId,
        key: &str,
        observation: &OutcomeObservation,
        event: EventDraft,
    ) -> Result<Intervention> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let row: Option<Json<Intervention>> =
            sqlx::query_scalar("SELECT body FROM interventions WHERE id = $1 FOR UPDATE")
                .bind(id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some(Json(mut i)) = row else {
            return Err(Error::not_found("intervention", id));
        };
        if i.outcomes.contains_key(key) {
            return Err(Error::conflict("this outcome is already recorded"));
        }
        i.outcomes.insert(key.to_owned(), observation.clone());
        sqlx::query("UPDATE interventions SET body = $2 WHERE id = $1")
            .bind(id.0)
            .bind(Json(&i))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(i)
    }

    pub(crate) async fn m62_interventions_for(
        &self,
        access: &AccessContext,
        environment: &str,
        treatment: &str,
        limit: u32,
    ) -> Result<Vec<Intervention>> {
        let clause = access_sql(super::TABLE_ACCESS_COLS, [1, 2, 3, 4]);
        let rows: Vec<Json<Intervention>> = sqlx::query_scalar(&format!(
            "SELECT body FROM interventions WHERE {clause} \
               AND environment = $5 AND treatment = $6 \
             ORDER BY recorded_at, id LIMIT $7"
        ))
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .bind(environment)
        .bind(treatment)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|Json(i)| i).collect())
    }
}
