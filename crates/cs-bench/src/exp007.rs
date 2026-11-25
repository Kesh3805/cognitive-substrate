//! Experiment 007: HNSW vs exact nearest-neighbour search under per-scope
//! filtering.
//!
//! Everything here is fixed by `benchmarks/007-ann-under-filtering/
//! PREREGISTRATION.md`.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use cs_core::{AccessContext, MemoryRevisionId, Scope, SourceRef, TenantId, UserId};
use cs_engine::{CognitiveEngine, NewMemory, ProvenanceInput};
use cs_memory::{MemoryContent, MemoryDomain, MemoryStatus, SemanticMemory};
use cs_postgres::{
    PgStore,
    ann::{NearestMode, NearestResult},
};
use cs_storage::{CandidateOrder, MemoryQuery};
use serde::Serialize;

use crate::{
    metrics::{mean, paired_bootstrap, percentile},
    rng::Rng,
};

pub const DEV_SEEDS: [u64; 1] = [1];
pub const HELD_OUT_SEEDS: [u64; 3] = [301, 302, 303];
pub const K: u32 = 50;
pub const EF_SEARCH: u32 = 100;
pub const QUERIES_PER_STRATUM: usize = 40;
pub const LOAD_CONCURRENCY: usize = 16;

/// Tenant sizes (memories per tenant): 100,000 vectors in total, skewed.
pub const TENANT_SIZES: &[(usize, usize)] = &[
    (40_000, 1),
    (15_000, 1),
    (10_000, 1),
    (5_000, 2),
    (2_000, 5),
    (1_000, 5),
    (300, 20),
    (100, 30),
    (20, 50),
];

pub fn total() -> usize {
    TENANT_SIZES.iter().map(|(s, n)| s * n).sum()
}

/// Selectivity stratum: the tenant's share of the index population.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Stratum {
    /// ≥ 10%
    Broad,
    /// 1–10%
    Medium,
    /// 0.1–1%
    Narrow,
    /// < 0.1%
    Tiny,
}

impl Stratum {
    pub const ALL: [Self; 4] = [Self::Broad, Self::Medium, Self::Narrow, Self::Tiny];

    pub fn of(size: usize) -> Self {
        let s = size as f64 / total() as f64;
        if s >= 0.10 {
            Self::Broad
        } else if s >= 0.01 {
            Self::Medium
        } else if s >= 0.001 {
            Self::Narrow
        } else {
            Self::Tiny
        }
    }
}

const WORDS: &[&str] = &[
    "ledger",
    "gateway",
    "invoice",
    "cache",
    "deploy",
    "latency",
    "region",
    "replica",
    "token",
    "session",
    "billing",
    "search",
    "index",
    "queue",
    "stream",
    "schema",
    "backup",
    "alert",
    "budget",
    "rollout",
    "tenant",
    "audit",
    "policy",
    "metric",
    "shard",
    "vector",
    "router",
    "payment",
    "refund",
    "catalog",
    "pricing",
    "retention",
    "incident",
    "capacity",
    "failover",
    "quota",
    "webhook",
    "export",
    "import",
    "migration",
];

fn sentence(rng: &mut Rng, words: usize) -> String {
    (0..words)
        .map(|_| *rng.pick(WORDS))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub seed: u64,
    pub query: usize,
    pub stratum: Stratum,
    pub tenant_size: usize,
    pub exact: usize,
    pub hnsw: usize,
    pub recall: f64,
    pub underfill: bool,
    pub used_index: bool,
    pub exact_ms: f64,
    pub hnsw_ms: f64,
    pub violations: usize,
}

struct Tenant {
    ctx: AccessContext,
    size: usize,
    ids: BTreeSet<MemoryRevisionId>,
}

/// Load one seed into an empty database, build the index, run queries.
pub async fn run_seed(store: Arc<PgStore>, seed: u64) -> Result<Vec<Row>, String> {
    let mut rng = Rng::new(seed);
    let engine = CognitiveEngine::new(store.clone());
    let space = engine.active_space().ok_or("no embedding space")?;
    let (model, version) = space
        .split_once('@')
        .map(|(m, v)| (m.to_owned(), v.to_owned()))
        .ok_or("unexpected space id")?;

    // ---- load ---------------------------------------------------------------
    let mut tenants = Vec::new();
    let mut work = Vec::new();
    for &(size, count) in TENANT_SIZES {
        for _ in 0..count {
            let tenant = TenantId::new_v7();
            let user = UserId::new_v7();
            let idx = tenants.len();
            tenants.push(Tenant {
                ctx: AccessContext::user(tenant, user),
                size,
                ids: BTreeSet::new(),
            });
            for n in 0..size {
                let text = format!("{} item {idx}-{n}", sentence(&mut rng, 6));
                work.push((idx, Scope::private(tenant, user), text));
            }
        }
    }
    rng.shuffle(&mut work);
    let mut done = 0usize;
    for chunk in work.chunks(LOAD_CONCURRENCY * 64) {
        let mut tasks = tokio::task::JoinSet::new();
        for part in chunk.chunks(64) {
            let engine = engine.clone();
            let part: Vec<_> = part.to_vec();
            let ctxs: Vec<AccessContext> = part
                .iter()
                .map(|(i, _, _)| tenants[*i].ctx.clone())
                .collect();
            tasks.spawn(async move {
                let mut out = Vec::with_capacity(part.len());
                for ((idx, scope, text), ctx) in part.into_iter().zip(ctxs) {
                    let m = engine
                        .create_memory(
                            &ctx,
                            NewMemory {
                                scope,
                                content: MemoryContent::Semantic(SemanticMemory {
                                    proposition: text,
                                    subject: None,
                                    predicate: None,
                                    object: None,
                                }),
                                provenance: ProvenanceInput {
                                    source: SourceRef::UserInput { message_id: None },
                                    observed_at: None,
                                    extraction: None,
                                    parent_evidence: Vec::new(),
                                },
                                valid_time: None,
                                expires_at: None,
                                idempotency_key: None,
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    out.push((idx, m.revision_id));
                }
                Ok::<_, String>(out)
            });
        }
        while let Some(r) = tasks.join_next().await {
            for (idx, rev) in r.map_err(|e| e.to_string())?? {
                tenants[idx].ids.insert(rev);
            }
        }
        done += chunk.len();
        eprintln!("  loaded {done}/{}", work.len());
    }
    let dims = engine
        .embed_query("probe")
        .await
        .map_err(|e| e.to_string())?
        .len();
    eprintln!("  building HNSW index ({dims} d)");
    store
        .create_ann_index(&model, &version, dims)
        .await
        .map_err(|e| e.to_string())?;
    store.analyze().await.map_err(|e| e.to_string())?;

    // ---- queries ------------------------------------------------------------
    let mut rows = Vec::new();
    let now = time::OffsetDateTime::now_utc();
    let mut qi = 0usize;
    for stratum in Stratum::ALL {
        let pool: Vec<usize> = (0..tenants.len())
            .filter(|&i| Stratum::of(tenants[i].size) == stratum)
            .collect();
        for _ in 0..QUERIES_PER_STRATUM {
            let t = &tenants[*rng.pick(&pool)];
            let text = sentence(&mut rng, 4);
            let vector = engine.embed_query(&text).await.map_err(|e| e.to_string())?;
            let q = MemoryQuery {
                access: t.ctx.clone(),
                domains: MemoryDomain::ALL.to_vec(),
                statuses: vec![MemoryStatus::Active],
                scope_filter: None,
                as_of_valid_time: now,
                as_of_transaction_time: now,
                include_superseded: false,
                ignore_valid_time: false,
                embedding_model: Some((model.clone(), version.clone())),
                limit: K,
                order: CandidateOrder::Recent,
            };
            let run = |mode| store.nearest(&q, &vector, mode);
            // Rotate which mode runs first.
            let (exact, hnsw): (NearestResult, NearestResult) = if qi.is_multiple_of(2) {
                let e = run(NearestMode::Exact).await.map_err(|e| e.to_string())?;
                let h = run(NearestMode::Hnsw {
                    ef_search: EF_SEARCH,
                })
                .await
                .map_err(|e| e.to_string())?;
                (e, h)
            } else {
                let h = run(NearestMode::Hnsw {
                    ef_search: EF_SEARCH,
                })
                .await
                .map_err(|e| e.to_string())?;
                let e = run(NearestMode::Exact).await.map_err(|e| e.to_string())?;
                (e, h)
            };
            let e: BTreeSet<MemoryRevisionId> = exact.rows.iter().map(|(r, _)| *r).collect();
            let h: BTreeSet<MemoryRevisionId> = hnsw.rows.iter().map(|(r, _)| *r).collect();
            let expected = (K as usize).min(t.size);
            let ms = |d: Duration| d.as_secs_f64() * 1_000.0;
            rows.push(Row {
                seed,
                query: qi,
                stratum,
                tenant_size: t.size,
                exact: e.len(),
                hnsw: h.len(),
                recall: tie_aware_recall(&exact.rows, &hnsw.rows),
                underfill: h.len() < expected,
                used_index: hnsw.used_index,
                exact_ms: ms(exact.elapsed),
                hnsw_ms: ms(hnsw.elapsed),
                violations: h
                    .iter()
                    .chain(e.iter())
                    .filter(|r| !t.ids.contains(r))
                    .count(),
            });
            qi += 1;
        }
    }
    Ok(rows)
}

/// Recall@k that treats ties as equivalent: an approximate result counts
/// when its distance is no greater than the exact k-th distance. Without
/// this, equal distances (common with hashing embeddings) would be scored
/// as approximation errors.
pub fn tie_aware_recall(
    exact: &[(MemoryRevisionId, f64)],
    approx: &[(MemoryRevisionId, f64)],
) -> f64 {
    let Some(kth) = exact.iter().map(|(_, d)| *d).reduce(f64::max) else {
        return 1.0;
    };
    let hits = approx.iter().filter(|(_, d)| *d <= kth + 1e-9).count();
    hits.min(exact.len()) as f64 / exact.len() as f64
}

#[derive(Debug, Clone, Serialize)]
pub struct StratumSummary {
    pub queries: usize,
    pub recall: f64,
    pub recall_ci_lower: f64,
    pub recall_ci_upper: f64,
    pub min_recall: f64,
    pub underfill: usize,
    pub index_used: usize,
    pub exact_p50_ms: f64,
    pub exact_p95_ms: f64,
    pub hnsw_p50_ms: f64,
    pub hnsw_p95_ms: f64,
    pub speedup_p95: f64,
    pub violations: usize,
    pub passes: bool,
    pub failed_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub strata: Vec<(Stratum, StratumSummary)>,
    pub overall_recall: f64,
    pub overall_recall_ci_lower: f64,
    pub any_stratum_passes: bool,
}

pub fn summarize(rows: &[Row]) -> Summary {
    let ci = |xs: &[f64]| {
        let zero = vec![0.0; xs.len()];
        paired_bootstrap(&zero, xs, 10_000, 7)
    };
    let strata = Stratum::ALL
        .iter()
        .map(|s| {
            let rs: Vec<&Row> = rows.iter().filter(|r| r.stratum == *s).collect();
            let recall: Vec<f64> = rs.iter().map(|r| r.recall).collect();
            let interval = ci(&recall);
            let exact: Vec<f64> = rs.iter().map(|r| r.exact_ms).collect();
            let hnsw: Vec<f64> = rs.iter().map(|r| r.hnsw_ms).collect();
            let (e95, h95) = (percentile(&exact, 95.0), percentile(&hnsw, 95.0));
            let most_selective = *s == Stratum::Tiny;
            let (point, lower) = if most_selective {
                (0.95, 0.93)
            } else {
                (0.98, 0.97)
            };
            let mut failed = Vec::new();
            let mean_recall = mean(&recall);
            if mean_recall < point {
                failed.push(format!("mean recall {mean_recall:.4} < {point}"));
            }
            if interval.lower < lower {
                failed.push(format!("recall CI lower {:.4} < {lower}", interval.lower));
            }
            let underfill = rs.iter().filter(|r| r.underfill).count();
            if underfill > 0 {
                failed.push(format!("{underfill} underfilled queries"));
            }
            let index_used = rs.iter().filter(|r| r.used_index).count();
            if index_used < rs.len() {
                failed.push(format!(
                    "HNSW index used in only {index_used}/{} queries",
                    rs.len()
                ));
            }
            let speedup = if h95 > 0.0 { e95 / h95 } else { 0.0 };
            if speedup < 2.0 {
                failed.push(format!("p95 speedup {speedup:.2}x < 2.0x"));
            }
            let violations = rs.iter().map(|r| r.violations).sum();
            if violations > 0 {
                failed.push(format!("{violations} authorization violations"));
            }
            (
                *s,
                StratumSummary {
                    queries: rs.len(),
                    recall: mean_recall,
                    recall_ci_lower: interval.lower,
                    recall_ci_upper: interval.upper,
                    min_recall: recall.iter().copied().fold(1.0, f64::min),
                    underfill,
                    index_used,
                    exact_p50_ms: percentile(&exact, 50.0),
                    exact_p95_ms: e95,
                    hnsw_p50_ms: percentile(&hnsw, 50.0),
                    hnsw_p95_ms: h95,
                    speedup_p95: speedup,
                    violations,
                    passes: failed.is_empty(),
                    failed_criteria: failed,
                },
            )
        })
        .collect::<Vec<_>>();
    let all: Vec<f64> = rows.iter().map(|r| r.recall).collect();
    let overall = ci(&all);
    Summary {
        any_stratum_passes: strata.iter().any(|(_, s)| s.passes),
        strata,
        overall_recall: mean(&all),
        overall_recall_ci_lower: overall.lower,
    }
}
