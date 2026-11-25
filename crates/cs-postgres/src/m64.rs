//! M6.4 persistence: predictions (immutable, one per intervention and
//! lineage).

use cs_core::{Error, EventDraft, Result};
use cs_memory::{causal::Intervention, prediction::Prediction};
use sqlx::types::Json;

use super::{PgStore, append_event, db_err};

impl PgStore {
    pub(crate) async fn m64_insert_prediction(
        &self,
        p: &Prediction,
        outcome_key: &str,
        event: EventDraft,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Serialized with outcome observation, which locks the same row.
        let row: Option<Json<Intervention>> =
            sqlx::query_scalar("SELECT body FROM interventions WHERE id = $1 FOR UPDATE")
                .bind(p.intervention_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some(Json(i)) = row else {
            return Err(Error::not_found("intervention", p.intervention_id));
        };
        if i.outcomes.contains_key(outcome_key) {
            return Err(Error::conflict(
                "the outcome is already recorded; a prediction is made at intent",
            ));
        }
        let r = sqlx::query(
            "INSERT INTO predictions (id, intervention_id, hypothesis_id, lineage, \
             recorded_at, body) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT DO NOTHING",
        )
        .bind(p.id.0)
        .bind(p.intervention_id.0)
        .bind(p.hypothesis_id.0)
        .bind(&p.lineage)
        .bind(p.recorded_at)
        .bind(Json(p))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if r.rows_affected() == 0 {
            return Err(Error::conflict(
                "this intervention already has a prediction in this lineage",
            ));
        }
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub(crate) async fn m64_predictions_for(&self, lineage: &str) -> Result<Vec<Prediction>> {
        let rows: Vec<Json<Prediction>> = sqlx::query_scalar(
            "SELECT body FROM predictions WHERE lineage = $1 ORDER BY recorded_at, id",
        )
        .bind(lineage)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|Json(p)| p).collect())
    }
}
