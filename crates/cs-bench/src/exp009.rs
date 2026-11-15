//! Experiment 009: adaptive candidate budgets.
//!
//! Everything here is fixed by `benchmarks/009-adaptive-budgets/
//! PREREGISTRATION.md`. Changing a constant after held-out results exist
//! invalidates the experiment.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Instant,
};

use cs_core::{AccessContext, FixedClock, MemoryId, Scope, TenantId, UserId};
use cs_engine::{CognitiveEngine, RecallRequest, ReplayRequest};
use cs_memory::{CandidateBudget, CandidateGeneration, RecallPolicy};
use cs_storage::CognitiveStore;
use serde::Serialize;

use crate::{
    corpus::Owner,
    crowded::{Corpus9, Family9, generate},
    exp005::new_memory,
    metrics::{Interval, mean, mrr_at, ndcg_at, paired_bootstrap, percentile, recall},
};

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [501, 502, 503, 504, 505];
pub const K: usize = 10;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
pub const BOOTSTRAP_SEED: u64 = 7;

/// The control: the adopted default (exp 006) with a fixed budget.
pub const CONTROL: &str = "F200";
/// The candidate: the same budget, used adaptively.
pub const PRIMARY: &str = "A";
/// Ablation: the adaptive first pass alone, never expanded.
pub const ABLATION: &str = "F50";

pub fn strategies() -> Vec<(&'static str, RecallPolicy)> {
    let base = RecallPolicy {
        max_candidates: 200,
        max_results: K as u32,
        min_score: 0.0,
        max_context_tokens: None,
        include_beliefs: false,
        candidates: CandidateGeneration::Channels {
            lexical: 100,
            dense: 0,
        },
        ..RecallPolicy::default()
    };
    vec![
        (CONTROL, base.clone()),
        (
            PRIMARY,
            RecallPolicy {
                budget: CandidateBudget::Adaptive {
                    initial: 50,
                    initial_lexical: 25,
                },
                ..base.clone()
            },
        ),
        (
            ABLATION,
            RecallPolicy {
                max_candidates: 50,
                candidates: CandidateGeneration::Channels {
                    lexical: 25,
                    dense: 0,
                },
                ..base
            },
        ),
    ]
}

/// One query under one strategy.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub seed: u64,
    pub query: usize,
    pub family: Family9,
    pub strategy: &'static str,
    pub ndcg: f64,
    pub mrr: f64,
    pub recall: f64,
    pub candidate_recall: f64,
    /// Candidates scored.
    pub candidates: usize,
    pub latency_ms: f64,
    pub scope_violations: usize,
    pub temporal_violations: usize,
    /// Store round trips for candidate generation: 2 when expanded.
    pub round_trips: u32,
    /// Adaptive only.
    pub expanded: Option<bool>,
    pub lexical_found: Option<u32>,
    pub best_overlap: Option<u32>,
    pub cut_overlap: Option<u32>,
    /// Adaptive only: exact replay selected the same revisions and
    /// recorded the same budget decision.
    pub replay_identical: Option<bool>,
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<Row>, String> {
    let corpus: Corpus9 = generate(seed);
    let clock = Arc::new(FixedClock::new(crate::corpus::start()));
    let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
    let tenant = TenantId::new_v7();
    let (alice_id, bob_id) = (UserId::new_v7(), UserId::new_v7());
    let alice = AccessContext::user(tenant, alice_id);
    let bob = AccessContext::user(tenant, bob_id);

    let mut index_of: HashMap<MemoryId, usize> = HashMap::new();
    for (i, ins) in corpus.inserts.iter().enumerate() {
        clock.set(ins.at);
        let (ctx, scope) = match ins.owner {
            Owner::Alice => (&alice, Scope::private(tenant, alice_id)),
            Owner::Bob => (&bob, Scope::private(tenant, bob_id)),
        };
        let m = engine
            .create_memory(ctx, new_memory(scope, ins))
            .await
            .map_err(|e| format!("insert {i}: {e}"))?;
        index_of.insert(m.id, i);
    }
    clock.set(corpus.query_at);

    let strategies = strategies();
    let mut rows = Vec::new();
    for (qi, q) in corpus.queries.iter().enumerate() {
        let gold: BTreeMap<usize, u8> = q.gold.iter().copied().collect();
        let relevant = gold.values().filter(|g| **g >= 1).count();
        let mut order = strategies.clone();
        let n = order.len();
        order.rotate_left(qi % n);
        for (name, policy) in order {
            let started = Instant::now();
            let resp = engine
                .recall(
                    &alice,
                    RecallRequest {
                        query: q.text.clone(),
                        policy,
                        session: None,
                    },
                )
                .await
                .map_err(|e| format!("recall: {e}"))?;
            let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
            let trace = engine
                .get_trace(&alice, resp.trace_id)
                .await
                .map_err(|e| format!("trace: {e}"))?;
            let replay_identical = match trace.budget {
                Some(decision) => {
                    let r = engine
                        .replay_recall(&alice, resp.trace_id, ReplayRequest::default())
                        .await
                        .map_err(|e| format!("replay: {e}"))?;
                    Some(r.identical && r.replay.budget == Some(decision))
                }
                None => None,
            };
            let candidates: BTreeSet<usize> = trace
                .candidates
                .iter()
                .filter_map(|c| index_of.get(&c.memory_id).copied())
                .collect();
            let selected: Vec<usize> = resp
                .results
                .iter()
                .filter_map(|r| index_of.get(&r.memory.id).copied())
                .collect();
            let grades: Vec<u8> = selected
                .iter()
                .map(|i| gold.get(i).copied().unwrap_or(0))
                .collect();
            let gold_grades: Vec<u8> = gold.values().copied().collect();
            let found = |set: &dyn Fn(usize) -> bool| {
                gold.iter().filter(|(i, g)| **g >= 1 && set(**i)).count()
            };
            let d = trace.budget;
            rows.push(Row {
                seed,
                query: qi,
                family: q.family,
                strategy: name,
                ndcg: ndcg_at(&grades, &gold_grades, K),
                mrr: mrr_at(&grades, K),
                recall: recall(found(&|i| selected.contains(&i)), relevant),
                candidate_recall: recall(found(&|i| candidates.contains(&i)), relevant),
                candidates: trace.candidates.len(),
                latency_ms,
                scope_violations: q
                    .forbidden_candidates
                    .iter()
                    .filter(|i| candidates.contains(i))
                    .count(),
                temporal_violations: q
                    .forbidden_selected
                    .iter()
                    .filter(|i| selected.contains(i))
                    .count(),
                round_trips: 1 + u32::from(d.is_some_and(|d| d.lexical_saturated)),
                expanded: d.map(|d| d.lexical_saturated),
                lexical_found: d.map(|d| d.lexical_found),
                best_overlap: d.map(|d| d.best_overlap),
                cut_overlap: d.map(|d| d.cut_overlap),
                replay_identical,
            });
        }
    }
    Ok(rows)
}

#[derive(Debug, Clone, Serialize)]
pub struct Aggregate {
    pub queries: usize,
    pub ndcg: f64,
    pub mrr: f64,
    pub recall: f64,
    pub candidate_recall: f64,
    pub candidates: f64,
    pub round_trips: f64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_p99_ms: f64,
    pub scope_violations: usize,
    pub temporal_violations: usize,
    /// Adaptive only: fraction of queries that expanded.
    pub expansion_rate: Option<f64>,
}

pub fn aggregate(rows: &[&Row]) -> Aggregate {
    let col = |f: fn(&Row) -> f64| rows.iter().map(|r| f(r)).collect::<Vec<_>>();
    let latency = col(|r| r.latency_ms);
    let expanded: Vec<f64> = rows
        .iter()
        .filter_map(|r| r.expanded.map(|e| f64::from(u8::from(e))))
        .collect();
    Aggregate {
        queries: rows.len(),
        ndcg: mean(&col(|r| r.ndcg)),
        mrr: mean(&col(|r| r.mrr)),
        recall: mean(&col(|r| r.recall)),
        candidate_recall: mean(&col(|r| r.candidate_recall)),
        candidates: mean(&col(|r| r.candidates as f64)),
        round_trips: mean(&col(|r| f64::from(r.round_trips))),
        latency_p50_ms: percentile(&latency, 50.0),
        latency_p95_ms: percentile(&latency, 95.0),
        latency_p99_ms: percentile(&latency, 99.0),
        scope_violations: rows.iter().map(|r| r.scope_violations).sum(),
        temporal_violations: rows.iter().map(|r| r.temporal_violations).sum(),
        expansion_rate: (!expanded.is_empty()).then(|| mean(&expanded)),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub criterion: &'static str,
    pub value: String,
    pub passed: bool,
}

/// What the expansions did (reported, not gated).
#[derive(Debug, Clone, Serialize)]
pub struct Expansions {
    pub expanded: usize,
    /// Expanded queries where A's candidates hold more relevant memories
    /// than the never-expanding first pass (F50).
    pub recovered_relevant: usize,
    /// Non-expanded queries where F200's candidates hold more relevant
    /// memories than A's: misses the saturation signal did not see.
    pub missed_by_signal: usize,
    pub replays: usize,
    pub replays_identical: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub overall: BTreeMap<&'static str, Aggregate>,
    pub by_family: BTreeMap<String, BTreeMap<&'static str, Aggregate>>,
    /// Paired deltas of A against F200 and against F50.
    pub deltas: BTreeMap<String, Interval>,
    pub expansions: Expansions,
    pub gates: Vec<Gate>,
    pub adopt: bool,
}

type Key = (u64, usize);

fn by_key<'a>(rows: &'a [Row], strategy: &str) -> BTreeMap<Key, &'a Row> {
    rows.iter()
        .filter(|r| r.strategy == strategy)
        .map(|r| ((r.seed, r.query), r))
        .collect()
}

fn paired(rows: &[Row], base: &str, other: &str, f: fn(&Row) -> f64) -> Interval {
    let a = by_key(rows, base);
    let b = by_key(rows, other);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for (k, r) in &b {
        if let Some(x) = a.get(k) {
            xs.push(f(x));
            ys.push(f(r));
        }
    }
    paired_bootstrap(&xs, &ys, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED)
}

pub fn summarize(rows: &[Row]) -> Summary {
    let names: Vec<&'static str> = strategies().into_iter().map(|(n, _)| n).collect();
    let group = |pred: &dyn Fn(&Row) -> bool| -> BTreeMap<&'static str, Aggregate> {
        names
            .iter()
            .map(|n| {
                let rs: Vec<&Row> = rows
                    .iter()
                    .filter(|r| r.strategy == *n && pred(r))
                    .collect();
                (*n, aggregate(&rs))
            })
            .collect()
    };
    let overall = group(&|_| true);
    let by_family = Family9::ALL
        .iter()
        .map(|f| (format!("{f:?}").to_lowercase(), group(&|r| r.family == *f)))
        .collect();

    let mut deltas = BTreeMap::new();
    for (base, label) in [(CONTROL, "A-F200"), (ABLATION, "A-F50")] {
        for (metric, f) in [
            ("ndcg", (|r: &Row| r.ndcg) as fn(&Row) -> f64),
            ("mrr", |r: &Row| r.mrr),
            ("candidate_recall", |r: &Row| r.candidate_recall),
        ] {
            deltas.insert(format!("{label} {metric}"), paired(rows, base, PRIMARY, f));
        }
    }

    let a = by_key(rows, PRIMARY);
    let f50 = by_key(rows, ABLATION);
    let f200 = by_key(rows, CONTROL);
    let mut ex = Expansions {
        expanded: 0,
        recovered_relevant: 0,
        missed_by_signal: 0,
        replays: 0,
        replays_identical: 0,
    };
    for (k, r) in &a {
        if r.expanded == Some(true) {
            ex.expanded += 1;
            if f50
                .get(k)
                .is_some_and(|x| r.candidate_recall > x.candidate_recall)
            {
                ex.recovered_relevant += 1;
            }
        } else if f200
            .get(k)
            .is_some_and(|x| x.candidate_recall > r.candidate_recall)
        {
            ex.missed_by_signal += 1;
        }
        if let Some(same) = r.replay_identical {
            ex.replays += 1;
            ex.replays_identical += usize::from(same);
        }
    }

    // ---- preregistered gates for A against F200 -------------------------
    let c = &overall[CONTROL];
    let p = &overall[PRIMARY];
    let ndcg = &deltas["A-F200 ndcg"];
    let cand = &deltas["A-F200 candidate_recall"];
    let mut gates = Vec::new();
    let mut gate = |criterion, value: String, passed| {
        gates.push(Gate {
            criterion,
            value,
            passed,
        });
    };
    gate(
        "nDCG@10 non-inferior: 95% CI lower bound of A - F200 >= -0.01",
        format!(
            "{:+.4}, CI [{:+.4}, {:+.4}]",
            ndcg.mean, ndcg.lower, ndcg.upper
        ),
        ndcg.lower >= -0.01,
    );
    gate(
        "candidate recall non-inferior: mean A - F200 >= -0.01",
        format!("{:+.4}", cand.mean),
        cand.mean >= -0.01,
    );
    let ratio = |x: f64, y: f64| x / y.max(f64::MIN_POSITIVE);
    gate(
        "mean candidates scored <= 0.6x F200",
        format!(
            "{:.3}x ({:.1} vs {:.1})",
            ratio(p.candidates, c.candidates),
            p.candidates,
            c.candidates
        ),
        p.candidates <= 0.6 * c.candidates,
    );
    gate(
        "p95 latency <= 0.9x F200",
        format!(
            "{:.3}x ({:.2} ms vs {:.2} ms)",
            ratio(p.latency_p95_ms, c.latency_p95_ms),
            p.latency_p95_ms,
            c.latency_p95_ms
        ),
        p.latency_p95_ms <= 0.9 * c.latency_p95_ms,
    );
    gate(
        "p99 latency <= 1.0x F200",
        format!(
            "{:.3}x ({:.2} ms vs {:.2} ms)",
            ratio(p.latency_p99_ms, c.latency_p99_ms),
            p.latency_p99_ms,
            c.latency_p99_ms
        ),
        p.latency_p99_ms <= c.latency_p99_ms,
    );
    gate(
        "exact replay reproduces every adaptive selection and budget decision",
        format!("{}/{}", ex.replays_identical, ex.replays),
        ex.replays > 0 && ex.replays_identical == ex.replays,
    );
    let violations: usize = overall
        .values()
        .map(|a| a.scope_violations + a.temporal_violations)
        .sum();
    gate(
        "zero scope and temporal violations (all strategies)",
        format!("{violations}"),
        violations == 0,
    );
    let adopt = gates.iter().all(|g| g.passed);
    Summary {
        overall,
        by_family,
        deltas,
        expansions: ex,
        gates,
        adopt,
    }
}
