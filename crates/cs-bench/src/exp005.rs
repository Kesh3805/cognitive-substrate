//! Experiment 005: candidate generation (channels vs. recency v1).
//!
//! Everything here is fixed by `benchmarks/005-candidate-generation/
//! PREREGISTRATION.md`. Changing a constant after held-out results exist
//! invalidates the experiment.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Instant,
};

use cs_core::{
    AccessContext, FixedClock, MemoryId, Scope, SourceRef, TenantId, TimeInterval, UserId,
};
use cs_engine::{CognitiveEngine, NewMemory, ProvenanceInput, RecallRequest};
use cs_memory::{CandidateGeneration, EpisodicMemory, MemoryContent, RecallPolicy, SemanticMemory};
use cs_storage::CognitiveStore;
use serde::Serialize;

use crate::{
    corpus::{Age, Corpus, Family, InsertKind, Owner, generate},
    metrics::{Interval, mean, mrr_at, ndcg_at, paired_bootstrap, percentile, recall},
};

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [101, 102, 103, 104, 105];
pub const K: usize = 10;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
pub const BOOTSTRAP_SEED: u64 = 7;

pub fn strategies() -> Vec<(&'static str, CandidateGeneration)> {
    vec![
        ("v1", CandidateGeneration::RecencyV1),
        (
            "L",
            CandidateGeneration::Channels {
                lexical: 100,
                dense: 0,
            },
        ),
        (
            "D",
            CandidateGeneration::Channels {
                lexical: 0,
                dense: 100,
            },
        ),
        (
            "LD",
            CandidateGeneration::Channels {
                lexical: 50,
                dense: 50,
            },
        ),
    ]
}

fn policy(candidates: CandidateGeneration) -> RecallPolicy {
    RecallPolicy {
        max_candidates: 200,
        max_results: K as u32,
        min_score: 0.0,
        max_context_tokens: None,
        include_beliefs: false,
        candidates,
        ..RecallPolicy::default()
    }
}

/// One query under one strategy.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub seed: u64,
    pub query: usize,
    pub family: Family,
    pub age: Age,
    pub strategy: &'static str,
    pub ndcg: f64,
    pub mrr: f64,
    pub recall: f64,
    pub candidate_recall: f64,
    pub candidates: usize,
    pub tokens: u32,
    pub latency_ms: f64,
    pub scope_violations: usize,
    pub temporal_violations: usize,
}

fn new_memory(scope: Scope, ins: &crate::corpus::Insert) -> NewMemory {
    let content = match &ins.kind {
        InsertKind::Fact {
            service,
            predicate,
            object,
            ..
        } => MemoryContent::Semantic(SemanticMemory {
            proposition: ins.text(),
            subject: Some(service.clone()),
            predicate: Some((*predicate).to_owned()),
            object: Some(object.clone()),
        }),
        InsertKind::Note(t) => MemoryContent::Episodic(EpisodicMemory {
            description: t.clone(),
            event_id: None,
            participants: Vec::new(),
            subject: None,
            predicate: None,
            object: None,
        }),
    };
    NewMemory {
        scope,
        content,
        provenance: ProvenanceInput {
            source: SourceRef::UserInput { message_id: None },
            observed_at: Some(ins.at),
            extraction: None,
            parent_evidence: Vec::new(),
        },
        valid_time: Some(TimeInterval {
            start: ins.at,
            end: ins.valid_until,
        }),
        expires_at: None,
        idempotency_key: None,
    }
}

/// Load one seed's corpus into `store` and run every query under every
/// strategy.
pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<Row>, String> {
    let corpus: Corpus = generate(seed);
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

    let mut rows = Vec::new();
    for (qi, q) in corpus.queries.iter().enumerate() {
        let gold: BTreeMap<usize, u8> = q.gold.iter().copied().collect();
        let relevant = gold.values().filter(|g| **g >= 1).count();
        for (name, strategy) in strategies() {
            let started = Instant::now();
            let resp = engine
                .recall(
                    &alice,
                    RecallRequest {
                        query: q.text.clone(),
                        policy: policy(strategy),
                    },
                )
                .await
                .map_err(|e| format!("recall: {e}"))?;
            let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
            let trace = engine
                .get_trace(&alice, resp.trace_id)
                .await
                .map_err(|e| format!("trace: {e}"))?;
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
            rows.push(Row {
                seed,
                query: qi,
                family: q.family,
                age: q.age,
                strategy: name,
                ndcg: ndcg_at(&grades, &gold_grades, K),
                mrr: mrr_at(&grades, K),
                recall: recall(found(&|i| selected.contains(&i)), relevant),
                candidate_recall: recall(found(&|i| candidates.contains(&i)), relevant),
                candidates: trace.candidates.len(),
                tokens: resp
                    .results
                    .iter()
                    .map(|r| cs_policy::scoring::estimate_tokens(&r.memory.content.index_text()))
                    .sum(),
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
    pub tokens: f64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub scope_violations: usize,
    pub temporal_violations: usize,
}

pub fn aggregate(rows: &[&Row]) -> Aggregate {
    let col = |f: fn(&Row) -> f64| rows.iter().map(|r| f(r)).collect::<Vec<_>>();
    let latency = col(|r| r.latency_ms);
    Aggregate {
        queries: rows.len(),
        ndcg: mean(&col(|r| r.ndcg)),
        mrr: mean(&col(|r| r.mrr)),
        recall: mean(&col(|r| r.recall)),
        candidate_recall: mean(&col(|r| r.candidate_recall)),
        tokens: mean(&col(|r| f64::from(r.tokens))),
        latency_p50_ms: percentile(&latency, 50.0),
        latency_p95_ms: percentile(&latency, 95.0),
        scope_violations: rows.iter().map(|r| r.scope_violations).sum(),
        temporal_violations: rows.iter().map(|r| r.temporal_violations).sum(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub criterion: &'static str,
    pub value: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub overall: BTreeMap<&'static str, Aggregate>,
    pub by_family: BTreeMap<String, BTreeMap<&'static str, Aggregate>>,
    pub by_age: BTreeMap<String, BTreeMap<&'static str, Aggregate>>,
    /// Paired deltas against v1: nDCG@10, MRR@10, candidate recall.
    pub deltas: BTreeMap<&'static str, BTreeMap<&'static str, Interval>>,
    pub gates: Vec<Gate>,
    pub adopt_ld: bool,
}

fn paired(rows: &[Row], strategy: &str, f: fn(&Row) -> f64) -> (Vec<f64>, Vec<f64>) {
    let key = |r: &Row| (r.seed, r.query);
    let base: BTreeMap<(u64, usize), f64> = rows
        .iter()
        .filter(|r| r.strategy == "v1")
        .map(|r| (key(r), f(r)))
        .collect();
    let mut a = Vec::new();
    let mut b = Vec::new();
    for r in rows.iter().filter(|r| r.strategy == strategy) {
        if let Some(x) = base.get(&key(r)) {
            a.push(*x);
            b.push(f(r));
        }
    }
    (a, b)
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
    let by_family = Family::ALL
        .iter()
        .map(|f| (format!("{f:?}").to_lowercase(), group(&|r| r.family == *f)))
        .collect();
    let by_age = [Age::Recent, Age::Old]
        .iter()
        .map(|a| (format!("{a:?}").to_lowercase(), group(&|r| r.age == *a)))
        .collect();
    let mut deltas = BTreeMap::new();
    for n in names.iter().filter(|n| **n != "v1") {
        let mut m = BTreeMap::new();
        for (metric, f) in [
            ("ndcg", (|r: &Row| r.ndcg) as fn(&Row) -> f64),
            ("mrr", |r: &Row| r.mrr),
            ("candidate_recall", |r: &Row| r.candidate_recall),
        ] {
            let (a, b) = paired(rows, n, f);
            m.insert(
                metric,
                paired_bootstrap(&a, &b, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
            );
        }
        deltas.insert(*n, m);
    }

    // ---- preregistered gates for LD -------------------------------------
    let v1 = &overall["v1"];
    let ld = &overall["LD"];
    let d = &deltas["LD"];
    let mut gates = Vec::new();
    let mut gate = |criterion, value: String, passed| {
        gates.push(Gate {
            criterion,
            value,
            passed,
        });
    };
    let rel = if v1.ndcg > 0.0 {
        d["ndcg"].mean / v1.ndcg
    } else {
        f64::INFINITY
    };
    gate(
        "nDCG@10 delta >= +0.03 absolute and >= 5% relative, CI lower bound > 0",
        format!(
            "{:+.4} ({:+.1}%), CI [{:+.4}, {:+.4}]",
            d["ndcg"].mean,
            rel * 100.0,
            d["ndcg"].lower,
            d["ndcg"].upper
        ),
        d["ndcg"].mean >= 0.03 && rel >= 0.05 && d["ndcg"].lower > 0.0,
    );
    gate(
        "candidate recall delta >= +5 percentage points",
        format!("{:+.2} pp", d["candidate_recall"].mean * 100.0),
        d["candidate_recall"].mean >= 0.05,
    );
    let worst = Family::ALL
        .iter()
        .map(|f| {
            let key = format!("{f:?}").to_lowercase();
            let fam: &BTreeMap<&str, Aggregate> = &summary_family(rows, *f);
            (key, fam["LD"].ndcg - fam["v1"].ndcg)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or_default();
    gate(
        "no family regresses nDCG@10 by more than 0.02",
        format!("worst family {} {:+.4}", worst.0, worst.1),
        worst.1 >= -0.02,
    );
    let recent: Vec<&Row> = rows.iter().filter(|r| r.age == Age::Recent).collect();
    let recent_delta = mean(
        &recent
            .iter()
            .filter(|r| r.strategy == "LD")
            .map(|r| r.ndcg)
            .collect::<Vec<_>>(),
    ) - mean(
        &recent
            .iter()
            .filter(|r| r.strategy == "v1")
            .map(|r| r.ndcg)
            .collect::<Vec<_>>(),
    );
    gate(
        "recent stratum nDCG@10 does not regress by more than 0.02 (amendment 1)",
        format!("{recent_delta:+.4}"),
        recent_delta >= -0.02,
    );
    gate(
        "MRR delta CI not entirely below 0",
        format!("CI [{:+.4}, {:+.4}]", d["mrr"].lower, d["mrr"].upper),
        d["mrr"].upper >= 0.0,
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
    gate(
        "mean tokens <= 1.05x v1",
        format!("{:.3}x", ld.tokens / v1.tokens.max(f64::MIN_POSITIVE)),
        ld.tokens <= 1.05 * v1.tokens,
    );
    gate(
        "p95 latency <= 1.25x v1",
        format!(
            "{:.2}x ({:.2} ms vs {:.2} ms)",
            ld.latency_p95_ms / v1.latency_p95_ms.max(f64::MIN_POSITIVE),
            ld.latency_p95_ms,
            v1.latency_p95_ms
        ),
        ld.latency_p95_ms <= 1.25 * v1.latency_p95_ms,
    );
    let adopt_ld = gates.iter().all(|g| g.passed);
    Summary {
        overall,
        by_family,
        by_age,
        deltas,
        gates,
        adopt_ld,
    }
}

fn summary_family(rows: &[Row], f: Family) -> BTreeMap<&'static str, Aggregate> {
    strategies()
        .into_iter()
        .map(|(n, _)| {
            let rs: Vec<&Row> = rows
                .iter()
                .filter(|r| r.strategy == n && r.family == f)
                .collect();
            (n, aggregate(&rs))
        })
        .collect()
}

/// Two policies differ only in candidate generation.
pub fn policy_for(strategy: CandidateGeneration) -> RecallPolicy {
    policy(strategy)
}
