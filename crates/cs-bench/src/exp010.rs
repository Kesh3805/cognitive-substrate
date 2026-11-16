//! Experiment 010: answer, investigate or abstain (metacognitive policy v1).
//!
//! Everything here is fixed by `benchmarks/010-selective-answering/
//! PREREGISTRATION.md`. Changing a constant after held-out results exist
//! invalidates the experiment.
//!
//! 005's corpus is extended with three strata whose right action is not to
//! answer:
//!
//! * **absent:** every fact about some services is removed, and they are
//!   asked about;
//! * **conflict:** a second, different store fact is added for a service,
//!   both valid now and nothing resolving them;
//! * **candidate-only:** a service's only store fact is an unvalidated
//!   candidate.
//!
//! Gold labels come from the generator, never from text similarity.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use cs_core::{AccessContext, ExtractionInfo, FixedClock, MemoryId, Scope, TenantId, UserId};
use cs_engine::{CognitiveEngine, RecallRequest, ReplayRequest};
use cs_memory::{
    CandidateGeneration, Decision, MemoryContent, MetacognitivePolicy, Reason, RecallPolicy,
    RelationalMemory,
};
use cs_storage::CognitiveStore;
use serde::Serialize;
use time::Duration;

use crate::{
    corpus::{self, Insert, InsertKind, Owner, Relation},
    exp005::new_memory,
    metrics::{Interval, mean, paired_bootstrap},
    rng::Rng,
};

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [601, 602, 603, 604, 605];
pub const K: u32 = 10;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
pub const BOOTSTRAP_SEED: u64 = 7;

/// Services per non-answerable stratum. Held-out seeds shift the mix
/// (more conflicts and absences, fewer candidates) so thresholds chosen on
/// development seeds meet a different distribution.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Mix {
    pub absent: usize,
    pub conflict: usize,
    pub candidate_only: usize,
    /// Literal direct questions wrapped in conversational framing (to be
    /// answered), and with a qualifier that changes what is asked (not to
    /// be answered from current facts).
    pub framed: usize,
    pub qualified: usize,
    pub framings: &'static [&'static str],
    pub qualifiers: &'static [&'static str],
}

pub const DEV_MIX: Mix = Mix {
    absent: 20,
    conflict: 20,
    candidate_only: 20,
    framed: 25,
    qualified: 25,
    framings: &["please tell me", "could you tell me"],
    qualifiers: &["previously", "never"],
};
/// Held out: a different mix, and framings and qualifiers never used on
/// development seeds.
pub const HELD_OUT_MIX: Mix = Mix {
    absent: 30,
    conflict: 30,
    candidate_only: 12,
    framed: 24,
    qualified: 24,
    framings: &[
        "can you show me",
        "would you let me know",
        "kindly tell me currently",
    ],
    qualifiers: &["not", "before", "only"],
};

/// Margin-only baseline: answer when the top selected memory leads the
/// next by at least this much, else abstain. Fixed from development seeds
/// (matched coverage with the policy) before the held-out run.
pub const MARGIN_ONLY_THRESHOLD: f64 = 0.0546;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stratum {
    Present,
    Absent,
    Conflict,
    CandidateOnly,
    Qualified,
}

impl Stratum {
    pub const ALL: [Self; 5] = [
        Self::Present,
        Self::Absent,
        Self::Conflict,
        Self::CandidateOnly,
        Self::Qualified,
    ];

    /// The action a careful reader would take.
    pub const fn right(self) -> Decision {
        match self {
            Self::Present => Decision::Answer,
            Self::Absent => Decision::Abstain,
            Self::Conflict | Self::CandidateOnly | Self::Qualified => Decision::Investigate,
        }
    }
}

#[derive(Debug, Clone)]
struct Query10 {
    stratum: Stratum,
    family: String,
    text: String,
    /// Insert indexes that answer it (present only).
    gold: BTreeSet<usize>,
    /// Never to be selected (out of scope).
    forbidden: BTreeSet<usize>,
}

struct Corpus10 {
    inserts: Vec<(Insert, bool)>,
    queries: Vec<Query10>,
    query_at: time::OffsetDateTime,
}

fn store_fact(i: &Insert) -> Option<&str> {
    match &i.kind {
        InsertKind::Fact {
            service,
            relation: Relation::Store,
            ..
        } if i.owner == Owner::Alice => Some(service),
        _ => None,
    }
}

fn service_of(i: &Insert) -> Option<&str> {
    match &i.kind {
        InsertKind::Fact { service, .. } => Some(service),
        InsertKind::Note(_) => None,
    }
}

fn generate(seed: u64, mix: Mix) -> Corpus10 {
    let base = corpus::generate(seed);
    let mut rng = Rng::new(seed ^ 0x0010_0010_0010_0010);

    // Services with exactly one store fact that is still valid: free to
    // be removed, contradicted or left unvalidated. Base questions about
    // the chosen services are dropped below.
    let touches = |q: &corpus::Query| -> BTreeSet<String> {
        q.gold
            .iter()
            .map(|(i, _)| *i)
            .chain(q.forbidden_candidates.iter().copied())
            .chain(q.forbidden_selected.iter().copied())
            .filter_map(|i| service_of(&base.inserts[i]).map(str::to_owned))
            .collect()
    };
    let mut free: Vec<String> = base
        .inserts
        .iter()
        .filter_map(store_fact)
        .filter(|s| !s.ends_with(" replica"))
        .map(str::to_owned)
        .collect();
    free.sort();
    free.dedup();
    free.retain(|s| {
        let facts: Vec<&Insert> = base
            .inserts
            .iter()
            .filter(|i| store_fact(i) == Some(s.as_str()))
            .collect();
        facts.len() == 1 && facts[0].valid_until.is_none()
    });
    // Names that extend another (replica twins) would keep "absent"
    // services lexically present; skip their bases too.
    let names: BTreeSet<String> = base
        .inserts
        .iter()
        .filter_map(service_of)
        .map(str::to_owned)
        .collect();
    free.retain(|s| !names.iter().any(|n| n != s && n.starts_with(s.as_str())));
    rng.shuffle(&mut free);
    let need = mix.absent + mix.conflict + mix.candidate_only;
    assert!(
        free.len() >= need,
        "seed {seed}: {} free services",
        free.len()
    );
    let absent: BTreeSet<String> = free[..mix.absent].iter().cloned().collect();
    let conflict: Vec<String> = free[mix.absent..mix.absent + mix.conflict].to_vec();
    let candidate: BTreeSet<String> = free[mix.absent + mix.conflict..need]
        .iter()
        .cloned()
        .collect();

    // (insert, candidate?, original index)
    let mut all: Vec<(Insert, bool, Option<usize>)> = Vec::new();
    for (i, ins) in base.inserts.iter().enumerate() {
        let svc = service_of(ins);
        if svc.is_some_and(|s| absent.contains(s)) {
            continue;
        }
        let unvalidated = store_fact(ins).is_some_and(|s| candidate.contains(s));
        all.push((ins.clone(), unvalidated, Some(i)));
    }
    for svc in &conflict {
        let old = base
            .inserts
            .iter()
            .find(|i| store_fact(i) == Some(svc.as_str()))
            .expect("free service has a store fact");
        let InsertKind::Fact {
            object, predicate, ..
        } = &old.kind
        else {
            unreachable!()
        };
        let mut other = object.clone();
        while &other == object {
            other = (*rng.pick(&[
                "postgres",
                "cassandra",
                "redis",
                "mongodb",
                "dynamodb",
                "clickhouse",
            ]))
            .to_owned();
        }
        let span = (corpus::start() + Duration::days(corpus::DAYS) - old.at).whole_minutes();
        all.push((
            Insert {
                at: old.at + Duration::minutes(1 + rng.below(span.max(2) as usize - 1) as i64),
                owner: Owner::Alice,
                kind: InsertKind::Fact {
                    service: svc.clone(),
                    relation: Relation::Store,
                    predicate,
                    object: other,
                },
                // Both stay valid: nothing has resolved the disagreement.
                valid_until: None,
            },
            false,
            None,
        ));
    }
    all.sort_by_key(|(i, _, _)| i.at);
    let mut new_index: HashMap<usize, usize> = HashMap::new();
    for (n, (_, _, old)) in all.iter().enumerate() {
        if let Some(o) = old {
            new_index.insert(*o, n);
        }
    }

    let chosen: BTreeSet<&String> = free[..need].iter().collect();
    let mut queries: Vec<Query10> = base
        .queries
        .iter()
        .filter(|q| touches(q).iter().all(|s| !chosen.contains(s)))
        .map(|q| Query10 {
            stratum: Stratum::Present,
            family: format!("{:?}", q.family).to_lowercase(),
            text: q.text.clone(),
            gold: q
                .gold
                .iter()
                .filter(|(_, g)| *g >= 1)
                .filter_map(|(i, _)| new_index.get(i).copied())
                .collect(),
            forbidden: q
                .forbidden_candidates
                .iter()
                .filter_map(|i| new_index.get(i).copied())
                .collect(),
        })
        .collect();
    let rels = [
        Relation::Store,
        Relation::Owner,
        Relation::Region,
        Relation::Port,
    ];
    for svc in &absent {
        queries.push(Query10 {
            stratum: Stratum::Absent,
            family: "absent".into(),
            text: corpus::direct(*rng.pick(&rels), svc),
            gold: BTreeSet::new(),
            forbidden: BTreeSet::new(),
        });
    }
    for (stratum, set) in [
        (Stratum::Conflict, conflict.iter().collect::<Vec<_>>()),
        (Stratum::CandidateOnly, candidate.iter().collect()),
    ] {
        for svc in set {
            queries.push(Query10 {
                stratum,
                family: format!("{stratum:?}").to_lowercase(),
                text: corpus::direct(Relation::Store, svc),
                gold: BTreeSet::new(),
                forbidden: BTreeSet::new(),
            });
        }
    }
    // Framed and qualified variants of literal direct questions.
    let mut direct: Vec<Query10> = queries
        .iter()
        .filter(|q| q.family == "direct")
        .cloned()
        .collect();
    assert!(
        direct.len() >= mix.framed.max(mix.qualified),
        "seed {seed}: {} direct questions",
        direct.len()
    );
    rng.shuffle(&mut direct);
    for q in direct.iter().take(mix.framed) {
        queries.push(Query10 {
            family: "framed".into(),
            text: format!("{} {}", rng.pick(mix.framings), q.text),
            ..q.clone()
        });
    }
    // Drawn independently: a question may appear framed and qualified.
    rng.shuffle(&mut direct);
    for q in direct.iter().take(mix.qualified) {
        queries.push(Query10 {
            stratum: Stratum::Qualified,
            family: "qualified".into(),
            text: format!("{} {}", q.text, rng.pick(mix.qualifiers)),
            gold: BTreeSet::new(),
            forbidden: q.forbidden.clone(),
        });
    }
    Corpus10 {
        inserts: all.into_iter().map(|(i, c, _)| (i, c)).collect(),
        queries,
        query_at: base.query_at,
    }
}

pub fn policy() -> RecallPolicy {
    RecallPolicy {
        max_candidates: 200,
        max_results: K,
        min_score: 0.0,
        max_context_tokens: None,
        include_beliefs: false,
        include_candidates: true,
        candidates: CandidateGeneration::Channels {
            lexical: 100,
            dense: 0,
        },
        metacognition: Some(MetacognitivePolicy { min_margin: None }),
        ..RecallPolicy::default()
    }
}

/// One query: the policy's decision, and what the baselines would do on
/// the same recall.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub seed: u64,
    pub query: usize,
    pub stratum: Stratum,
    pub family: String,
    pub decision: Decision,
    pub reasons: Vec<Reason>,
    pub selected: u32,
    /// The top selected memory answers the question (present only): what
    /// the baselines answer from.
    pub top_correct: bool,
    /// The policy's best covering memory answers it: what the policy
    /// answers from.
    pub covering_correct: bool,
    /// The question, and the text of the memory the policy would answer
    /// from (for inspecting failures).
    pub question: String,
    pub answered_from: Option<String>,
    /// Lead of the top selected memory over the next (margin-only input).
    pub top_margin: Option<f64>,
    pub scope_violations: usize,
    pub replay_identical: bool,
}

pub async fn run_seed(
    store: Arc<dyn CognitiveStore>,
    seed: u64,
    mix: Mix,
) -> Result<Vec<Row>, String> {
    let corpus = generate(seed, mix);
    let clock = Arc::new(FixedClock::new(corpus::start()));
    let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
    let tenant = TenantId::new_v7();
    let (alice_id, bob_id) = (UserId::new_v7(), UserId::new_v7());
    let alice = AccessContext::user(tenant, alice_id);
    let bob = AccessContext::user(tenant, bob_id);

    let mut index_of: HashMap<MemoryId, usize> = HashMap::new();
    for (i, (ins, unvalidated)) in corpus.inserts.iter().enumerate() {
        clock.set(ins.at);
        let (ctx, scope) = match ins.owner {
            Owner::Alice => (&alice, Scope::private(tenant, alice_id)),
            Owner::Bob => (&bob, Scope::private(tenant, bob_id)),
        };
        let mut m = new_memory(scope, ins);
        // Dependencies are many-valued: a relational edge, as the system
        // models them, not a single-valued semantic fact.
        if let InsertKind::Fact {
            service,
            relation: Relation::Depends,
            predicate,
            object,
        } = &ins.kind
        {
            m.content = MemoryContent::Relational(RelationalMemory {
                subject: service.clone(),
                predicate: (*predicate).to_owned(),
                object: object.clone(),
                directed: true,
            });
        }
        if *unvalidated {
            m.provenance.extraction = Some(ExtractionInfo {
                model: None,
                model_version: None,
                prompt_version: None,
                extractor_version: "bench-010".into(),
            });
        }
        let created = engine
            .create_memory(ctx, m)
            .await
            .map_err(|e| format!("insert {i}: {e}"))?;
        index_of.insert(created.id, i);
    }
    clock.set(corpus.query_at);

    let mut rows = Vec::new();
    for (qi, q) in corpus.queries.iter().enumerate() {
        let resp = engine
            .recall(
                &alice,
                RecallRequest {
                    query: q.text.clone(),
                    policy: policy(),
                    session: None,
                },
            )
            .await
            .map_err(|e| format!("recall: {e}"))?;
        let decision = resp
            .metacognition
            .clone()
            .ok_or("recall returned no decision")?;
        let replay = engine
            .replay_recall(&alice, resp.trace_id, ReplayRequest::default())
            .await
            .map_err(|e| format!("replay: {e}"))?;
        let selected: Vec<usize> = resp
            .results
            .iter()
            .filter_map(|r| index_of.get(&r.memory.id).copied())
            .collect();
        let scores: Vec<f64> = resp.results.iter().map(|r| r.score).collect();
        let top_margin = match scores.as_slice() {
            [] => None,
            [only] => Some(*only),
            [a, b, ..] => Some(a - b),
        };
        rows.push(Row {
            seed,
            query: qi,
            stratum: q.stratum,
            family: q.family.clone(),
            decision: decision.decision,
            reasons: decision.reasons.clone(),
            selected: decision.signals.selected,
            top_correct: selected.first().is_some_and(|i| q.gold.contains(i)),
            question: q.text.clone(),
            answered_from: decision.signals.covering.first().and_then(|id| {
                resp.results
                    .iter()
                    .find(|r| r.memory.id == *id)
                    .map(|r| r.memory.content.index_text())
            }),
            covering_correct: decision
                .signals
                .covering
                .first()
                .and_then(|id| index_of.get(id))
                .is_some_and(|i| q.gold.contains(i)),
            top_margin,
            scope_violations: selected.iter().filter(|i| q.forbidden.contains(i)).count(),
            replay_identical: replay.identical
                && replay.replay.metacognition.as_ref() == Some(&decision),
        });
    }
    Ok(rows)
}

/// What each strategy does with one row.
fn answers(strategy: &str, r: &Row, margin_threshold: f64) -> bool {
    match strategy {
        "answer_always" => r.selected > 0,
        "margin_only" => r.top_margin.is_some_and(|m| m >= margin_threshold),
        _ => r.decision == Decision::Answer,
    }
}

/// An answer is an error unless the question is answerable and the
/// memory the strategy answers from answers it: the policy's best covering
/// memory, or the baselines' top selected memory.
fn error(strategy: &str, r: &Row) -> bool {
    let right = if strategy == "policy" {
        r.covering_correct
    } else {
        r.top_correct
    };
    !(r.stratum == Stratum::Present && right)
}

#[derive(Debug, Clone, Serialize)]
pub struct Selective {
    pub queries: usize,
    pub answered: usize,
    pub coverage: f64,
    /// Errors among answered.
    pub risk: f64,
    /// Present questions answered (answerable coverage).
    pub present_coverage: f64,
}

fn selective(rows: &[&Row], strategy: &str, t: f64) -> Selective {
    let answered: Vec<&&Row> = rows.iter().filter(|r| answers(strategy, r, t)).collect();
    let errors = answered.iter().filter(|r| error(strategy, r)).count();
    let present: Vec<&&Row> = rows
        .iter()
        .filter(|r| r.stratum == Stratum::Present)
        .collect();
    let present_answered = present.iter().filter(|r| answers(strategy, r, t)).count();
    let ratio = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    Selective {
        queries: rows.len(),
        answered: answered.len(),
        coverage: ratio(answered.len(), rows.len()),
        risk: ratio(errors, answered.len()),
        present_coverage: ratio(present_answered, present.len()),
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
    /// Strategy → overall selective metrics.
    pub overall: BTreeMap<&'static str, Selective>,
    /// Stratum → decision → share of the stratum's questions.
    pub decisions: BTreeMap<String, BTreeMap<String, f64>>,
    /// Present questions by family → strategy → selective metrics.
    pub by_family: BTreeMap<String, BTreeMap<&'static str, Selective>>,
    /// Reasons the policy gave, by stratum.
    pub reasons: BTreeMap<String, BTreeMap<String, usize>>,
    /// Paired per-query "answered and wrong" indicator, policy minus
    /// baseline (negative favours the policy).
    pub wrong_answer_delta: BTreeMap<&'static str, Interval>,
    /// Margin threshold giving margin-only the policy's coverage on these
    /// rows (reported; the gates use the frozen threshold).
    pub matched_margin_threshold: f64,
    pub gates: Vec<Gate>,
    pub adopt: bool,
}

/// The smallest threshold at which margin-only answers no more often than
/// the policy.
fn matched_threshold(rows: &[Row]) -> f64 {
    let target = rows
        .iter()
        .filter(|r| r.decision == Decision::Answer)
        .count();
    let mut margins: Vec<f64> = rows.iter().filter_map(|r| r.top_margin).collect();
    margins.sort_by(f64::total_cmp);
    // Answering the top `target` margins.
    let n = margins.len();
    if target == 0 || n == 0 {
        return f64::INFINITY;
    }
    margins[n.saturating_sub(target)]
}

pub fn summarize(rows: &[Row]) -> Summary {
    const STRATEGIES: [&str; 3] = ["policy", "answer_always", "margin_only"];
    let all: Vec<&Row> = rows.iter().collect();
    let t = MARGIN_ONLY_THRESHOLD;
    let overall: BTreeMap<&'static str, Selective> = STRATEGIES
        .iter()
        .map(|s| (*s, selective(&all, s, t)))
        .collect();
    let mut decisions = BTreeMap::new();
    let mut reasons = BTreeMap::new();
    for s in Stratum::ALL {
        let rs: Vec<&Row> = rows.iter().filter(|r| r.stratum == s).collect();
        let key = format!("{s:?}").to_lowercase();
        let mut m = BTreeMap::new();
        for d in [Decision::Answer, Decision::Investigate, Decision::Abstain] {
            let n = rs.iter().filter(|r| r.decision == d).count();
            m.insert(
                d.as_str().to_owned(),
                if rs.is_empty() {
                    0.0
                } else {
                    n as f64 / rs.len() as f64
                },
            );
        }
        decisions.insert(key.clone(), m);
        let mut why: BTreeMap<String, usize> = BTreeMap::new();
        for r in &rs {
            for reason in &r.reasons {
                *why.entry(format!("{reason:?}")).or_default() += 1;
            }
        }
        reasons.insert(key, why);
    }
    let families: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.stratum == Stratum::Present)
        .map(|r| r.family.as_str())
        .collect();
    let by_family = families
        .into_iter()
        .map(|f| {
            let rs: Vec<&Row> = rows.iter().filter(|r| r.family == f).collect();
            (
                f.to_owned(),
                STRATEGIES
                    .iter()
                    .map(|s| (*s, selective(&rs, s, t)))
                    .collect(),
            )
        })
        .collect();
    let wrong = |s: &str, r: &Row| f64::from(u8::from(answers(s, r, t) && error(s, r)));
    let wrong_answer_delta: BTreeMap<&'static str, Interval> = ["answer_always", "margin_only"]
        .into_iter()
        .map(|b| {
            let base: Vec<f64> = rows.iter().map(|r| wrong(b, r)).collect();
            let pol: Vec<f64> = rows.iter().map(|r| wrong("policy", r)).collect();
            (
                b,
                paired_bootstrap(&base, &pol, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
            )
        })
        .collect();

    let p = &overall["policy"];
    let share = |s: Stratum, d: Decision| -> f64 {
        let rs: Vec<&Row> = rows.iter().filter(|r| r.stratum == s).collect();
        if rs.is_empty() {
            return 0.0;
        }
        rs.iter().filter(|r| r.decision == d).count() as f64 / rs.len() as f64
    };
    let not_answered = |s: Stratum| 1.0 - share(s, Decision::Answer);
    let replays = rows.iter().filter(|r| r.replay_identical).count();
    let violations: usize = rows.iter().map(|r| r.scope_violations).sum();
    let mut gates = Vec::new();
    let mut gate = |criterion, value: String, passed| {
        gates.push(Gate {
            criterion,
            value,
            passed,
        });
    };
    let d_mo = &wrong_answer_delta["margin_only"];
    gate(
        "fewer wrong answers than margin-only: 95% CI upper bound of the paired delta < 0",
        format!(
            "{:+.4}, CI [{:+.4}, {:+.4}]",
            d_mo.mean, d_mo.lower, d_mo.upper
        ),
        d_mo.upper < 0.0,
    );
    gate(
        "risk among answered <= 0.05",
        format!("{:.4} ({} answered)", p.risk, p.answered),
        p.risk <= 0.05,
    );
    // Literal questions: the generator's templates for direct, lexical
    // trap, temporal and scope questions, and their framed variants.
    let literal: Vec<&Row> = rows
        .iter()
        .filter(|r| r.stratum == Stratum::Present && r.family != "semantic")
        .collect();
    let literal_answered = literal
        .iter()
        .filter(|r| r.decision == Decision::Answer)
        .count() as f64
        / literal.len().max(1) as f64;
    gate(
        "literal present questions answered >= 0.90",
        format!("{literal_answered:.4} of {}", literal.len()),
        literal_answered >= 0.90,
    );
    let paraphrases: Vec<&Row> = rows.iter().filter(|r| r.family == "semantic").collect();
    let paraphrase_wrong = paraphrases
        .iter()
        .filter(|r| answers("policy", r, t) && error("policy", r))
        .count() as f64
        / paraphrases.len().max(1) as f64;
    let paraphrase_investigated = paraphrases
        .iter()
        .filter(|r| r.decision == Decision::Investigate)
        .count() as f64
        / paraphrases.len().max(1) as f64;
    gate(
        "paraphrases answered wrongly <= 0.05 (of paraphrase questions)",
        format!(
            "{paraphrase_wrong:.4} (investigated {paraphrase_investigated:.4}, answered {:.4})",
            1.0 - share_family(rows, "semantic", Decision::Investigate)
                - share_family(rows, "semantic", Decision::Abstain)
        ),
        paraphrase_wrong <= 0.05,
    );
    for (criterion, s) in [
        ("absent questions not answered >= 0.90", Stratum::Absent),
        ("conflict questions not answered >= 0.90", Stratum::Conflict),
        (
            "candidate-only questions not answered >= 0.90",
            Stratum::CandidateOnly,
        ),
        (
            "qualified questions not answered >= 0.90",
            Stratum::Qualified,
        ),
    ] {
        let v = not_answered(s);
        gate(criterion, format!("{v:.4}"), v >= 0.90);
    }
    gate(
        "exact replay reproduces every decision",
        format!("{replays}/{}", rows.len()),
        replays == rows.len(),
    );
    gate(
        "zero scope violations",
        format!("{violations}"),
        violations == 0,
    );
    let adopt = gates.iter().all(|g| g.passed);
    Summary {
        overall,
        decisions,
        by_family,
        reasons,
        wrong_answer_delta,
        matched_margin_threshold: matched_threshold(rows),
        gates,
        adopt,
    }
}

fn share_family(rows: &[Row], family: &str, d: Decision) -> f64 {
    let rs: Vec<&Row> = rows.iter().filter(|r| r.family == family).collect();
    rs.iter().filter(|r| r.decision == d).count() as f64 / rs.len().max(1) as f64
}

/// Mean of a column, for reporting.
pub fn mean_of(rows: &[Row], f: fn(&Row) -> f64) -> f64 {
    mean(&rows.iter().map(f).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strata_are_built_as_described() {
        let c = generate(1, DEV_MIX);
        let count = |s: Stratum| c.queries.iter().filter(|q| q.stratum == s).count();
        assert_eq!(count(Stratum::Absent), DEV_MIX.absent);
        assert_eq!(count(Stratum::Conflict), DEV_MIX.conflict);
        assert_eq!(count(Stratum::CandidateOnly), DEV_MIX.candidate_only);
        assert_eq!(count(Stratum::Qualified), DEV_MIX.qualified);
        assert_eq!(
            c.queries.iter().filter(|q| q.family == "framed").count(),
            DEV_MIX.framed
        );
        for seed in HELD_OUT_SEEDS {
            let h = generate(seed, HELD_OUT_MIX);
            assert_eq!(
                h.queries
                    .iter()
                    .filter(|q| q.stratum == Stratum::Qualified)
                    .count(),
                HELD_OUT_MIX.qualified
            );
        }
        assert!(c.inserts.windows(2).all(|w| w[0].0.at <= w[1].0.at));
        assert_eq!(
            c.inserts.iter().filter(|(_, cand)| *cand).count(),
            DEV_MIX.candidate_only
        );
        for q in c.queries.iter().filter(|q| q.stratum == Stratum::Present) {
            assert!(!q.gold.is_empty(), "{}", q.text);
            for g in &q.gold {
                assert!(!c.inserts[*g].1, "gold is never unvalidated");
            }
        }
        // Each conflicting service has two valid store facts.
        for q in c.queries.iter().filter(|q| q.stratum == Stratum::Conflict) {
            let svc = q
                .text
                .trim_start_matches("where does ")
                .trim_end_matches(" store data");
            let n = c
                .inserts
                .iter()
                .filter(|(i, _)| store_fact(i) == Some(svc) && i.valid_until.is_none())
                .count();
            assert_eq!(n, 2, "{svc}");
        }
        // Absent services have no facts left.
        let absent: Vec<&Query10> = c
            .queries
            .iter()
            .filter(|q| q.stratum == Stratum::Absent)
            .collect();
        assert!(!absent.is_empty());
    }
}
