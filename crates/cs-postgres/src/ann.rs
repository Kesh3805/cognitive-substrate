//! Experimental: HNSW nearest-neighbour search, for experiment 007.
//!
//! Nothing in the engine uses this. It exists so the benchmark measures
//! approximate search against exact search over **exactly** the same
//! eligible population: both queries use the store's own eligibility
//! filters ([`memory_filters`]) and differ only in whether the distance
//! expression can use a per-space HNSW index.
//!
//! The index is a partial expression index per embedding space, created
//! only by an explicit call ([`PgStore::create_ann_index`]). Settings are
//! transaction-local (`SET LOCAL`), so they cannot leak through the pool.

use cs_core::{Error, MemoryRevisionId, Result};
use cs_storage::MemoryQuery;
use sqlx::Row;

use super::{PgStore, bind_memory_query, db_err, memory_filters};

/// How a nearest-neighbour query runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NearestMode {
    /// Exact: the distance expression cannot use the HNSW index.
    Exact,
    /// HNSW with `iterative_scan = strict_order` and these settings.
    Hnsw { ef_search: u32 },
}

/// The `k` nearest eligible revisions, re-sorted by (distance, revision id).
#[derive(Debug, Clone)]
pub struct NearestResult {
    pub rows: Vec<(MemoryRevisionId, f64)>,
    /// Whether the plan used the HNSW index (from `EXPLAIN`).
    pub used_index: bool,
    /// Wall-clock time of the search query alone (not the `EXPLAIN` or
    /// the transaction-local settings).
    pub elapsed: std::time::Duration,
}

/// Quote a string literal for SQL (the embedding space names come from
/// configuration, but are still escaped).
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn index_name(model: &str, version: &str, dims: usize) -> String {
    let h = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("{model}\u{1f}{version}\u{1f}{dims}").as_bytes(),
    );
    format!("idx_ann_{}", &h.simple().to_string()[..16])
}

impl PgStore {
    /// Create the per-space HNSW index if it does not exist. Explicit and
    /// idempotent; never called implicitly.
    pub async fn create_ann_index(
        &self,
        model: &str,
        version: &str,
        dims: usize,
    ) -> Result<String> {
        let name = index_name(model, version, dims);
        let sql = format!(
            "CREATE INDEX IF NOT EXISTS {name} ON memory_embeddings \
             USING hnsw ((embedding::vector({dims})) vector_cosine_ops) \
             WHERE model_name = {} AND model_version = {}",
            literal(model),
            literal(version)
        );
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Enough memory that the graph is built in memory, not spilled. A
        // serial build keeps that memory local: a parallel build needs it
        // in shared memory, which containers often cap (64 MB by default).
        for set in [
            "SET LOCAL maintenance_work_mem = '1GB'",
            "SET LOCAL max_parallel_maintenance_workers = 0",
        ] {
            sqlx::query(set).execute(&mut *tx).await.map_err(db_err)?;
        }
        sqlx::query(&sql).execute(&mut *tx).await.map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(name)
    }

    /// Refresh planner statistics after a bulk load.
    pub async fn analyze(&self) -> Result<()> {
        sqlx::query("ANALYZE memory_records, memory_embeddings")
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// The `k = q.limit` nearest eligible revisions to `vector` in the
    /// space `q.embedding_model`, under `mode`.
    pub async fn nearest(
        &self,
        q: &MemoryQuery,
        vector: &[f32],
        mode: NearestMode,
    ) -> Result<NearestResult> {
        let (model, version) = q
            .embedding_model
            .clone()
            .ok_or_else(|| Error::validation("nearest needs an embedding space"))?;
        let dims = vector.len();
        let filters = memory_filters();
        // Only the HNSW form uses the indexed expression and the literal
        // partial-index predicate the planner needs to match the index.
        let distance = match mode {
            NearestMode::Exact => "(e.embedding <=> $17::vector)".to_owned(),
            NearestMode::Hnsw { .. } => {
                format!("(e.embedding::vector({dims}) <=> $17::vector({dims}))")
            }
        };
        let space = match mode {
            NearestMode::Exact => String::new(),
            NearestMode::Hnsw { .. } => format!(
                "AND e.model_name = {} AND e.model_version = {}",
                literal(&model),
                literal(&version)
            ),
        };
        // Exact search breaks ties inside the scan, so the reference set is
        // deterministic. HNSW cannot (a tie-breaker stops Postgres using
        // the index); its ties are resolved by the outer re-sort.
        let tie = match mode {
            NearestMode::Exact => ", m.revision_id",
            NearestMode::Hnsw { .. } => "",
        };
        let inner = format!(
            "SELECT m.revision_id, {distance} AS distance \
             FROM memory_embeddings e JOIN memory_records m ON m.revision_id = e.revision_id \
             WHERE e.model_name = $5 AND e.model_version = $6 {space} \
               AND (e.embedded_at IS NULL OR e.embedded_at <= $9) \
               AND vector_dims(e.embedding) = {dims} \
               AND {filters} \
             ORDER BY {distance}{tie} LIMIT $15"
        );
        // Tie-breaking happens outside the index scan: adding it to the
        // inner ORDER BY would stop Postgres using the index.
        let sql = format!(
            "WITH ann AS MATERIALIZED ({inner}) \
             SELECT revision_id, distance FROM ann ORDER BY distance + 0, revision_id"
        );
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if let NearestMode::Hnsw { ef_search } = mode {
            for set in [
                format!("SET LOCAL hnsw.ef_search = {ef_search}"),
                "SET LOCAL hnsw.iterative_scan = strict_order".to_owned(),
                // Left to itself, the planner prefers exact search through
                // the tenant index for every scope in this workload; this
                // forces the ordered HNSW scan so that HNSW is what runs.
                "SET LOCAL enable_sort = off".to_owned(),
            ] {
                sqlx::query(&set).execute(&mut *tx).await.map_err(db_err)?;
            }
        }
        let explain: serde_json::Value =
            bind_memory_query(sqlx::query(&format!("EXPLAIN (FORMAT JSON) {sql}")), q)
                .bind(pgvector::Vector::from(vector.to_vec()))
                .fetch_one(&mut *tx)
                .await
                .map_err(db_err)?
                .try_get(0)
                .map_err(db_err)?;
        let used_index = explain
            .to_string()
            .contains(&index_name(&model, &version, dims));
        let started = std::time::Instant::now();
        let rows = bind_memory_query(sqlx::query(&sql), q)
            .bind(pgvector::Vector::from(vector.to_vec()))
            .fetch_all(&mut *tx)
            .await
            .map_err(db_err)?;
        let elapsed = started.elapsed();
        tx.commit().await.map_err(db_err)?;
        let rows = rows
            .iter()
            .map(|r| {
                Ok((
                    MemoryRevisionId(r.try_get("revision_id").map_err(db_err)?),
                    r.try_get::<f64, _>("distance").map_err(db_err)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(NearestResult {
            rows,
            used_index,
            elapsed,
        })
    }
}
