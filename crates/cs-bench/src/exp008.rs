//! Experiment 008: how far can gap validation be trusted?
//!
//! Everything here is fixed by `benchmarks/008-gap-validation/
//! PREREGISTRATION.md`. Questions about facts that are present (in
//! several forms) and absent (in several forms) are asked and validated;
//! the outcome is compared with what the generator knows.

use std::{collections::BTreeSet, sync::Arc};

use cs_core::{AccessContext, ExtractionInfo, MemoryId, Scope, TenantId, UserId};
use cs_engine::{CognitiveEngine, needs::AskQuestion};
use cs_memory::need::{GapValidationOutcome, NeedStatus};
use cs_storage::CognitiveStore;
use serde::Serialize;

use crate::{
    corpus::{InsertKind, Owner, Relation, direct, generate, semantic},
    exp005::new_memory,
    rng::Rng,
};

pub const DEV_SEEDS: [u64; 1] = [1];
pub const HELD_OUT_SEEDS: [u64; 5] = [401, 402, 403, 404, 405];
pub const PER_STRATUM: usize = 80;
/// Framing notes added to every corpus for the boilerplate stratum.
pub const FRAMING_NOTES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stratum {
    PresentDirect,
    PresentParaphrased,
    PresentUnvalidated,
    AbsentStandard,
    AbsentBoilerplate,
    AbsentOtherScope,
}

impl Stratum {
    pub const ALL: [Self; 6] = [
        Self::PresentDirect,
        Self::PresentParaphrased,
        Self::PresentUnvalidated,
        Self::AbsentStandard,
        Self::AbsentBoilerplate,
        Self::AbsentOtherScope,
    ];

    pub const fn present(self) -> bool {
        matches!(
            self,
            Self::PresentDirect | Self::PresentParaphrased | Self::PresentUnvalidated
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub seed: u64,
    pub stratum: Stratum,
    pub verbose: bool,
    pub status: NeedStatus,
    pub incomplete: bool,
    /// A present fact declared a (candidate) gap.
    pub false_gap: bool,
    /// An absent fact declared to have evidence.
    pub missed_gap: bool,
    /// Unvalidated-only evidence reported as satisfied.
    pub silently_satisfied: bool,
    /// Evidence references the asker cannot read.
    pub leaked: usize,
}

/// Name vocabularies that never occur in the base corpus, one per kind of
/// synthetic service, so no stratum's names share a token with another's.
#[derive(Debug, Clone, Copy)]
enum Names {
    Unvalidated,
    BobOnly,
    AbsentStandard,
    AbsentBoilerplate,
}

impl Names {
    fn words(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Unvalidated => (
                &[
                    "kepler", "vireo", "tamarin", "osprey", "juniper", "sable", "marlin", "quill",
                ],
                &[
                    "switchyard",
                    "foundry",
                    "depot",
                    "exchange",
                    "registry",
                    "outpost",
                ],
            ),
            Self::BobOnly => (
                &[
                    "basalt", "fennel", "cirrus", "lumen", "garnet", "willow", "cobble", "heron",
                ],
                &["terminal", "dock", "aviary", "kiln", "granary", "wharf"],
            ),
            Self::AbsentStandard => (
                &[
                    "tundra", "maple", "puffin", "saffron", "argon", "bramble", "cedar", "dune",
                ],
                &[
                    "silo",
                    "quarry",
                    "orchard",
                    "lighthouse",
                    "causeway",
                    "mill",
                ],
            ),
            Self::AbsentBoilerplate => (
                &[
                    "ibis",
                    "lichen",
                    "nimbus",
                    "obsidian",
                    "plover",
                    "quartzite",
                    "russet",
                    "sorrel",
                ],
                &["tannery", "vestry", "warren", "yard", "ziggurat", "bastion"],
            ),
        }
    }
}

fn unseen_service(rng: &mut Rng, kind: Names, taken: &BTreeSet<String>) -> String {
    let (a, b) = kind.words();
    loop {
        let n = format!("{} {} {}", rng.pick(a), rng.pick(b), rng.pick(a));
        if !taken.contains(&n) {
            return n;
        }
    }
}

fn verbose(q: &str) -> String {
    format!("could you please tell me, for the platform team handbook, {q} right now")
}

fn store_fact(svc: &str, rng: &mut Rng) -> String {
    format!(
        "{svc} stores data in {}",
        rng.pick(&["postgres", "cassandra", "redis", "mongodb", "dynamodb"])
    )
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<Row>, String> {
    let corpus = generate(seed);
    let mut rng = Rng::new(seed ^ 0x0008);
    let engine = CognitiveEngine::new(store);
    let tenant = TenantId::new_v7();
    let (alice_id, bob_id) = (UserId::new_v7(), UserId::new_v7());
    let alice = AccessContext::user(tenant, alice_id);
    let bob = AccessContext::user(tenant, bob_id);
    let alice_scope = Scope::private(tenant, alice_id);
    let bob_scope = Scope::private(tenant, bob_id);

    // ---- base corpus ----------------------------------------------------------
    let mut alice_ids: BTreeSet<MemoryId> = BTreeSet::new();
    let mut services: BTreeSet<String> = BTreeSet::new();
    let mut facts: Vec<(Relation, String)> = Vec::new();
    for ins in &corpus.inserts {
        let (ctx, scope) = match ins.owner {
            Owner::Alice => (&alice, alice_scope.clone()),
            Owner::Bob => (&bob, bob_scope.clone()),
        };
        let m = engine
            .create_memory(ctx, new_memory(scope, ins))
            .await
            .map_err(|e| e.to_string())?;
        if ins.owner == Owner::Alice {
            alice_ids.insert(m.id);
            if let InsertKind::Fact {
                service, relation, ..
            } = &ins.kind
                && ins.valid_until.is_none()
                && *relation != Relation::Depends
            {
                services.insert(service.clone());
                facts.push((*relation, service.clone()));
            }
        }
    }
    // Framing notes: the boilerplate words of the boilerplate questions,
    // about services that exist.
    let named: Vec<String> = services.iter().cloned().collect();
    for _ in 0..FRAMING_NOTES {
        let svc = rng.pick(&named).clone();
        let text = format!("the current deployment status of the {svc} is green");
        let m = engine
            .create_memory(&alice, crate::exp008::fact(alice_scope.clone(), &text))
            .await
            .map_err(|e| e.to_string())?;
        alice_ids.insert(m.id);
    }
    // Facts only Bob has, and facts Alice has only as unvalidated
    // candidates, about services that appear nowhere else.
    let mut taken = services.clone();
    let mut bob_only = Vec::new();
    let mut unvalidated = Vec::new();
    for i in 0..2 * PER_STRATUM {
        let kind = if i % 2 == 0 {
            Names::BobOnly
        } else {
            Names::Unvalidated
        };
        let svc = unseen_service(&mut rng, kind, &taken);
        taken.insert(svc.clone());
        let text = store_fact(&svc, &mut rng);
        if i % 2 == 0 {
            engine
                .create_memory(&bob, fact(bob_scope.clone(), &text))
                .await
                .map_err(|e| e.to_string())?;
            bob_only.push(svc);
        } else {
            let mut req = fact(alice_scope.clone(), &text);
            req.provenance.extraction = Some(ExtractionInfo {
                model: None,
                model_version: None,
                prompt_version: None,
                extractor_version: "bench".into(),
            });
            let m = engine
                .create_memory(&alice, req)
                .await
                .map_err(|e| e.to_string())?;
            alice_ids.insert(m.id);
            unvalidated.push(svc);
        }
    }

    // ---- questions ------------------------------------------------------------
    let mut questions: Vec<(Stratum, bool, String)> = Vec::new();
    let mut pool = facts.clone();
    rng.shuffle(&mut pool);
    for (i, (rel, svc)) in pool.iter().take(PER_STRATUM).enumerate() {
        let v = i % 2 == 1;
        let q = direct(*rel, svc);
        questions.push((Stratum::PresentDirect, v, if v { verbose(&q) } else { q }));
    }
    for (rel, svc) in pool.iter().skip(PER_STRATUM).take(PER_STRATUM) {
        questions.push((Stratum::PresentParaphrased, false, semantic(*rel, svc)));
    }
    for svc in &unvalidated {
        questions.push((
            Stratum::PresentUnvalidated,
            false,
            direct(Relation::Store, svc),
        ));
    }
    for i in 0..PER_STRATUM {
        let svc = unseen_service(&mut rng, Names::AbsentStandard, &taken);
        taken.insert(svc.clone());
        let rel = *rng.pick(&[
            Relation::Store,
            Relation::Owner,
            Relation::Region,
            Relation::Port,
        ]);
        let v = i % 2 == 1;
        let q = direct(rel, &svc);
        questions.push((Stratum::AbsentStandard, v, if v { verbose(&q) } else { q }));
    }
    for _ in 0..PER_STRATUM {
        let svc = unseen_service(&mut rng, Names::AbsentBoilerplate, &taken);
        taken.insert(svc.clone());
        questions.push((
            Stratum::AbsentBoilerplate,
            false,
            format!("what is the current deployment status of the {svc}"),
        ));
    }
    for svc in &bob_only {
        questions.push((
            Stratum::AbsentOtherScope,
            false,
            direct(Relation::Store, svc),
        ));
    }

    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for (stratum, verbose, q) in questions {
        let asked = engine
            .ask(
                &alice,
                AskQuestion {
                    question: q.clone(),
                    impact: 1,
                    cost: 1,
                },
            )
            .await
            .map_err(|e| format!("ask {q:?}: {e}"))?;
        // Two questions with the same signature are one need; validate once.
        if !seen.insert(asked.need.id) {
            continue;
        }
        let v = engine
            .validate_need(&alice, asked.need.id)
            .await
            .map_err(|e| format!("validate {q:?}: {e}"))?
            .need;
        let outcome = v.validation.as_ref().map(|g| &g.outcome);
        let incomplete = matches!(outcome, Some(GapValidationOutcome::Incomplete { .. }));
        let gap = matches!(
            v.status,
            NeedStatus::CandidateGap | NeedStatus::ValidatedGap
        );
        let evidence = matches!(
            v.status,
            NeedStatus::Satisfied | NeedStatus::PendingPromotion
        );
        rows.push(Row {
            seed,
            stratum,
            verbose,
            status: v.status,
            incomplete,
            false_gap: stratum.present() && gap,
            missed_gap: !stratum.present() && evidence,
            silently_satisfied: stratum == Stratum::PresentUnvalidated
                && v.status == NeedStatus::Satisfied,
            leaked: v
                .resolved_by
                .iter()
                .filter(|r| !alice_ids.contains(&r.memory_id))
                .count(),
        });
    }
    Ok(rows)
}

pub(crate) fn fact(scope: Scope, text: &str) -> cs_engine::NewMemory {
    cs_engine::NewMemory {
        scope,
        content: cs_memory::MemoryContent::Semantic(cs_memory::SemanticMemory {
            proposition: text.into(),
            subject: None,
            predicate: None,
            object: None,
        }),
        provenance: cs_engine::ProvenanceInput {
            source: cs_core::SourceRef::UserInput { message_id: None },
            observed_at: None,
            extraction: None,
            parent_evidence: Vec::new(),
        },
        valid_time: None,
        expires_at: None,
        idempotency_key: None,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StratumSummary {
    pub questions: usize,
    pub statuses: std::collections::BTreeMap<String, usize>,
    pub incomplete: usize,
    /// False-gap rate (present strata) or missed-gap rate (absent strata).
    pub error_rate: f64,
    pub error_upper_95: f64,
    pub silently_satisfied: usize,
    pub leaked: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub criterion: String,
    pub value: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub strata: Vec<(Stratum, StratumSummary)>,
    pub auto_validate_gates: Vec<Gate>,
    pub propose_only_gates: Vec<Gate>,
    pub automatic_validation_permitted: bool,
    pub gap_proposals_permitted: bool,
}

pub fn summarize(rows: &[Row]) -> Summary {
    let strata: Vec<(Stratum, StratumSummary)> = Stratum::ALL
        .iter()
        .map(|s| {
            let rs: Vec<&Row> = rows.iter().filter(|r| r.stratum == *s).collect();
            let errors = rs
                .iter()
                .filter(|r| {
                    if s.present() {
                        r.false_gap
                    } else {
                        r.missed_gap
                    }
                })
                .count();
            let n = rs.len() as f64;
            let upper =
                cs_policy::capability::wilson(errors as f64, n, 1.96).map_or(1.0, |(_, hi)| hi);
            let mut statuses = std::collections::BTreeMap::new();
            for r in &rs {
                *statuses.entry(r.status.as_str().to_owned()).or_insert(0) += 1;
            }
            (
                *s,
                StratumSummary {
                    questions: rs.len(),
                    statuses,
                    incomplete: rs.iter().filter(|r| r.incomplete).count(),
                    error_rate: if n > 0.0 { errors as f64 / n } else { 0.0 },
                    error_upper_95: upper,
                    silently_satisfied: rs.iter().filter(|r| r.silently_satisfied).count(),
                    leaked: rs.iter().map(|r| r.leaked).sum(),
                },
            )
        })
        .collect();
    let get = |s: Stratum| &strata.iter().find(|(x, _)| *x == s).expect("stratum").1;
    let rate = |s: Stratum, point: f64, upper: Option<f64>, name: &str| {
        let a = get(s);
        Gate {
            criterion: match upper {
                Some(u) => format!("{name} ≤ {point} (95% upper ≤ {u})"),
                None => format!("{name} ≤ {point}"),
            },
            value: format!(
                "{:.4} (upper {:.4}, n = {})",
                a.error_rate, a.error_upper_95, a.questions
            ),
            passed: a.error_rate <= point && upper.is_none_or(|u| a.error_upper_95 <= u),
        }
    };
    let leaked: usize = strata.iter().map(|(_, a)| a.leaked).sum();
    let silent = get(Stratum::PresentUnvalidated).silently_satisfied;
    let structural = vec![
        Gate {
            criterion: "zero evidence leaked from other scopes".into(),
            value: leaked.to_string(),
            passed: leaked == 0,
        },
        Gate {
            criterion: "zero unvalidated-only evidence reported as satisfied".into(),
            value: silent.to_string(),
            passed: silent == 0,
        },
    ];
    let mut auto = vec![
        rate(
            Stratum::PresentDirect,
            0.01,
            Some(0.02),
            "false-gap rate, present direct",
        ),
        rate(
            Stratum::PresentParaphrased,
            0.10,
            Some(0.15),
            "false-gap rate, present paraphrased",
        ),
        rate(
            Stratum::PresentUnvalidated,
            0.01,
            Some(0.02),
            "false-gap rate, present unvalidated",
        ),
        rate(
            Stratum::AbsentStandard,
            0.03,
            Some(0.05),
            "missed-gap rate, absent standard",
        ),
        rate(
            Stratum::AbsentBoilerplate,
            0.05,
            Some(0.08),
            "missed-gap rate, absent boilerplate",
        ),
        rate(
            Stratum::AbsentOtherScope,
            0.0,
            None,
            "missed-gap rate, absent (other scope)",
        ),
    ];
    auto.extend(structural.clone());
    let mut propose = vec![
        rate(
            Stratum::PresentDirect,
            0.05,
            None,
            "false-gap rate, present direct",
        ),
        rate(
            Stratum::AbsentStandard,
            0.08,
            None,
            "missed-gap rate, absent standard",
        ),
        rate(
            Stratum::AbsentBoilerplate,
            0.10,
            None,
            "missed-gap rate, absent boilerplate",
        ),
    ];
    propose.extend(structural);
    Summary {
        automatic_validation_permitted: auto.iter().all(|g| g.passed),
        gap_proposals_permitted: propose.iter().all(|g| g.passed),
        strata,
        auto_validate_gates: auto,
        propose_only_gates: propose,
    }
}
