//! Experiment 012: read-time counterfactuals.
//!
//! Everything here is fixed by `benchmarks/012-counterfactual-bounds/
//! PREREGISTRATION.md`. Changing a constant after held-out results exist
//! invalidates the experiment.
//!
//! Every unit has both potential outcomes, drawn from a per-context mix of
//! four response types: always (1, 1), helped (1, 0), harmed (0, 1) and
//! never (0, 0), as (outcome if treated, outcome if not). The true
//! probability of necessity in a context is helped / (always + helped), and
//! of sufficiency helped / (helped + never). Evidence goes through M6.2's
//! evaluation; counterfactuals are asked only of what it supported, so a
//! hypothesis that got lucky (winner's curse) is part of the test.

use std::{collections::BTreeMap, sync::Arc};

use cs_core::{AccessContext, CognitiveEventType, EventQuery, FixedClock, Scope, TenantId, UserId};
use cs_engine::{
    CognitiveEngine,
    causal::{CounterfactualResult, NewHypothesis, NewIntervention, ObserveOutcome},
};
use cs_memory::{
    capability::OracleTier,
    causal::{Arm, Assignment, Direction, HypothesisStatus},
};
use cs_policy::counterfactual::{Query, Refusal};
use cs_storage::CognitiveStore;
use serde::Serialize;

use crate::rng::Rng;

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [801, 802, 803, 804, 805];
pub const PER_FAMILY: usize = 20;
pub const CONTEXTS: usize = 8;
pub const PER_ARM: [usize; 4] = [6, 12, 25, 50];
pub const MIN_EFFECT: f64 = 0.05;
/// Well powered: at least this many outcomes per arm in the context, and a
/// true (population) lower bound of at least this much.
pub const POWERED_PER_ARM: u32 = 25;
pub const POWERED_TRUE_LOWER: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// No harmed units.
    Monotone,
    /// Some units are harmed: the true PN sits above the monotone value.
    Defiers,
    /// The outcome is rare under both arms.
    Rare,
    /// Near-certain under treatment, near-impossible without.
    Boundary,
    /// As many harmed as helped: no average effect.
    Null,
}

impl Family {
    pub const ALL: [Self; 5] = [
        Self::Monotone,
        Self::Defiers,
        Self::Rare,
        Self::Boundary,
        Self::Null,
    ];
}

/// Shares of (always, helped, harmed, never) in one context.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Mix {
    pub always: f64,
    pub helped: f64,
    pub harmed: f64,
    pub never: f64,
}

impl Mix {
    fn p1(self) -> f64 {
        self.always + self.helped
    }
    fn p0(self) -> f64 {
        self.always + self.harmed
    }
    pub fn necessity(self) -> f64 {
        self.helped / (self.always + self.helped)
    }
    pub fn sufficiency(self) -> f64 {
        self.helped / (self.helped + self.never)
    }
}

fn uniform(rng: &mut Rng, lo: f64, hi: f64) -> f64 {
    lo + (hi - lo) * (rng.below(1_000_000) as f64 / 1_000_000.0)
}

fn mix(rng: &mut Rng, family: Family) -> Mix {
    let (always, helped, harmed) = match family {
        Family::Monotone => (uniform(rng, 0.1, 0.3), uniform(rng, 0.25, 0.5), 0.0),
        Family::Defiers => (
            uniform(rng, 0.05, 0.2),
            uniform(rng, 0.35, 0.55),
            uniform(rng, 0.05, 0.15),
        ),
        Family::Rare => (0.02, uniform(rng, 0.15, 0.25), 0.02),
        Family::Boundary => (0.01, uniform(rng, 0.85, 0.95), 0.0),
        Family::Null => {
            let both = uniform(rng, 0.1, 0.2);
            (uniform(rng, 0.1, 0.3), both, both)
        }
    };
    Mix {
        always,
        helped,
        harmed,
        never: (1.0 - always - helped - harmed).max(0.0),
    }
}

/// Draw one unit's (outcome if treated, outcome if not).
fn unit(rng: &mut Rng, m: Mix) -> (bool, bool) {
    let r = uniform(rng, 0.0, 1.0);
    if r < m.always {
        (true, true)
    } else if r < m.always + m.helped {
        (true, false)
    } else if r < m.always + m.helped + m.harmed {
        (false, true)
    } else {
        (false, false)
    }
}

/// Tian–Pearl bounds at the true proportions.
fn true_bounds(m: Mix, q: Query) -> (f64, f64) {
    let (p1, p0) = (m.p1(), m.p0());
    match q {
        Query::Necessity => (((p1 - p0) / p1).max(0.0), ((1.0 - p0) / p1).min(1.0)),
        Query::Sufficiency => (
            ((p1 - p0) / (1.0 - p0)).max(0.0),
            (p1 / (1.0 - p0)).min(1.0),
        ),
    }
}

/// One counterfactual answered: a supported hypothesis, a context in its
/// scope, a question.
#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub seed: u64,
    pub hypothesis: usize,
    pub family: Family,
    pub context: usize,
    pub per_arm: u32,
    pub query: Query,
    pub truth: f64,
    pub lower: f64,
    pub upper: f64,
    pub informative: bool,
    pub true_lower: f64,
}

/// One probe whose refusal is known in advance.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub seed: u64,
    pub hypothesis: usize,
    pub supported: bool,
    pub expected: Refusal,
    pub got: Option<Refusal>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SeedRun {
    pub answers: Vec<Answer>,
    pub probes: Vec<Probe>,
    pub supported: BTreeMap<String, (usize, usize)>,
    /// Nothing observable changed while counterfactuals were computed.
    pub nothing_written: bool,
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<SeedRun, String> {
    let mut rng = Rng::new(seed ^ 0x0012_0012_0012_0012);
    let clock = Arc::new(FixedClock::new(crate::corpus::start()));
    let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
    let tenant = TenantId::new_v7();
    let user = UserId::new_v7();
    let ctx = AccessContext::user(tenant, user);
    let scope = Scope::private(tenant, user);
    let err = |e: cs_core::Error| e.to_string();

    struct Built {
        family: Family,
        mixes: Vec<Mix>,
        per_arm: usize,
        hypothesis: cs_memory::causal::Hypothesis,
        /// Per context: a treated unit with the outcome and a control unit
        /// without it (randomized, deterministic tier), if any.
        askable: Vec<(
            Option<cs_core::InterventionId>,
            Option<cs_core::InterventionId>,
        )>,
        /// (intervention, expected refusal when supported).
        probes: Vec<(cs_core::InterventionId, Refusal)>,
    }

    let mut run = SeedRun::default();
    let mut built = Vec::new();
    let mut j = 0usize;
    for family in Family::ALL {
        for _ in 0..PER_FAMILY {
            let environment = format!("env-{j}@1");
            let hypothesis = engine
                .create_hypothesis(
                    &ctx,
                    NewHypothesis {
                        scope: scope.clone(),
                        environment: environment.clone(),
                        treatment: "x".into(),
                        outcome: "y".into(),
                        direction: Direction::Raises,
                        min_effect: MIN_EFFECT,
                        contexts: Vec::new(),
                        statement: None,
                    },
                )
                .await
                .map_err(err)?;
            let mixes: Vec<Mix> = (0..CONTEXTS).map(|_| mix(&mut rng, family)).collect();
            let per_arm = *rng.pick(&PER_ARM);
            let record = async |context: String,
                                arm: Arm,
                                assignment: Assignment,
                                value: bool,
                                tier: OracleTier|
                   -> Result<cs_core::InterventionId, String> {
                clock.advance(time::Duration::seconds(1));
                let randomized = assignment == Assignment::Randomized;
                let i = engine
                    .record_intervention(
                        &ctx,
                        NewIntervention {
                            scope: scope.clone(),
                            environment: environment.clone(),
                            treatment: "x".into(),
                            arm,
                            assignment,
                            assignment_detail: randomized.then(|| format!("seed {seed}")),
                            propensity: randomized.then_some(0.5),
                            context,
                            state: BTreeMap::new(),
                            execution_id: None,
                        },
                    )
                    .await
                    .map_err(err)?;
                engine
                    .observe_outcome(
                        &ctx,
                        i.id,
                        ObserveOutcome {
                            outcome: "y".into(),
                            value,
                            tier,
                        },
                    )
                    .await
                    .map_err(err)?;
                Ok(i.id)
            };
            let mut askable = Vec::new();
            for (k, m) in mixes.iter().enumerate() {
                let (mut treated_present, mut control_absent) = (None, None);
                for _ in 0..per_arm {
                    for arm in [Arm::Treated, Arm::Control] {
                        let (y1, y0) = unit(&mut rng, *m);
                        let value = if arm == Arm::Treated { y1 } else { y0 };
                        let id = record(
                            format!("class-{k}"),
                            arm,
                            Assignment::Randomized,
                            value,
                            OracleTier::Deterministic,
                        )
                        .await?;
                        match (arm, value) {
                            (Arm::Treated, true) if treated_present.is_none() => {
                                treated_present = Some(id);
                            }
                            (Arm::Control, false) if control_absent.is_none() => {
                                control_absent = Some(id);
                            }
                            _ => {}
                        }
                    }
                }
                askable.push((treated_present, control_absent));
            }
            // Probes, recorded before evaluation: none of them counts as
            // evidence (passive, one unit in an otherwise empty context,
            // self-assessed), and a treated unit without the outcome.
            let probes = vec![
                (
                    record(
                        "class-0".into(),
                        Arm::Treated,
                        Assignment::Passive,
                        true,
                        OracleTier::Deterministic,
                    )
                    .await?,
                    Refusal::NotRandomized,
                ),
                (
                    record(
                        "unsampled".into(),
                        Arm::Treated,
                        Assignment::Randomized,
                        true,
                        OracleTier::Deterministic,
                    )
                    .await?,
                    Refusal::OutOfScope,
                ),
                (
                    record(
                        "class-0".into(),
                        Arm::Treated,
                        Assignment::Randomized,
                        true,
                        OracleTier::SelfAssessed,
                    )
                    .await?,
                    Refusal::NoEligibleOutcome,
                ),
                (
                    record(
                        "class-0".into(),
                        Arm::Treated,
                        Assignment::Randomized,
                        false,
                        OracleTier::SelfAssessed,
                    )
                    .await?,
                    Refusal::NoEligibleOutcome,
                ),
            ];
            clock.advance(time::Duration::seconds(1));
            let hypothesis = engine
                .evaluate_hypothesis(&ctx, hypothesis.id, 1)
                .await
                .map_err(err)?;
            built.push(Built {
                family,
                mixes,
                per_arm,
                hypothesis,
                askable,
                probes,
            });
            j += 1;
        }
    }

    // Snapshot what could change.
    let counts = async || -> Result<BTreeMap<&'static str, usize>, String> {
        let mut m = BTreeMap::new();
        for t in CognitiveEventType::ALL {
            let n = engine
                .events(
                    &ctx,
                    EventQuery {
                        event_type: Some(t),
                        limit: Some(1000),
                        ..Default::default()
                    },
                )
                .await
                .map_err(err)?
                .len();
            m.insert(t.as_str(), n);
        }
        Ok(m)
    };
    let before = counts().await?;

    for (j, b) in built.iter().enumerate() {
        let supported = b.hypothesis.status == HypothesisStatus::Supported;
        let entry = run.supported.entry(format!("{:?}", b.family)).or_default();
        entry.0 += usize::from(supported);
        entry.1 += 1;
        let ask = async |id| -> Result<CounterfactualResult, String> {
            Ok(engine
                .counterfactual(&ctx, id, b.hypothesis.id)
                .await
                .map_err(err)?
                .result)
        };
        for (id, expected) in &b.probes {
            let got = match ask(*id).await? {
                CounterfactualResult::Refused { reason } => Some(reason),
                CounterfactualResult::Bounds { .. } => None,
            };
            run.probes.push(Probe {
                seed,
                hypothesis: j,
                supported,
                expected: if supported {
                    *expected
                } else {
                    Refusal::NotSupported
                },
                got,
            });
        }
        if !supported {
            continue;
        }
        let scope = &b
            .hypothesis
            .evaluation
            .as_ref()
            .ok_or("no evaluation")?
            .scope_contexts;
        for (k, (treated_present, control_absent)) in b.askable.iter().enumerate() {
            if !scope.contains(&format!("class-{k}")) {
                continue;
            }
            for (id, query) in [
                (treated_present, Query::Necessity),
                (control_absent, Query::Sufficiency),
            ] {
                let Some(id) = id else { continue };
                match ask(*id).await? {
                    CounterfactualResult::Bounds { bounds } => {
                        let m = b.mixes[k];
                        run.answers.push(Answer {
                            seed,
                            hypothesis: j,
                            family: b.family,
                            context: k,
                            per_arm: bounds.context.treated.n.min(bounds.context.control.n),
                            query,
                            truth: match query {
                                Query::Necessity => m.necessity(),
                                Query::Sufficiency => m.sufficiency(),
                            },
                            lower: bounds.lower,
                            upper: bounds.upper,
                            informative: bounds.informative,
                            true_lower: true_bounds(m, query).0,
                        });
                    }
                    CounterfactualResult::Refused { reason } => {
                        // An in-scope randomized unit with a matching
                        // outcome may still be refused for a degenerate
                        // region; recorded as a probe expecting bounds.
                        run.probes.push(Probe {
                            seed,
                            hypothesis: j,
                            supported,
                            expected: Refusal::InsufficientEvidence,
                            got: Some(reason),
                        });
                    }
                }
            }
        }
        let _ = b.per_arm;
    }

    let after = counts().await?;
    let mut unchanged = before
        .iter()
        .filter(|(_, n)| **n < 1000)
        .all(|(t, n)| after.get(t) == Some(n));
    for b in &built {
        let now = engine
            .get_hypothesis(&ctx, b.hypothesis.id)
            .await
            .map_err(err)?;
        unchanged &= now.version == b.hypothesis.version;
    }
    run.nothing_written = unchanged;
    Ok(run)
}

#[derive(Debug, Clone, Serialize)]
pub struct Rate {
    pub n: usize,
    pub k: usize,
    pub rate: f64,
}

fn rate(k: usize, n: usize) -> Rate {
    Rate {
        n,
        k,
        rate: if n == 0 { 0.0 } else { k as f64 / n as f64 },
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
    pub supported: BTreeMap<String, (usize, usize)>,
    pub coverage_necessity: Rate,
    pub coverage_sufficiency: Rate,
    /// Family → query → coverage.
    pub coverage_by_family: BTreeMap<String, BTreeMap<String, Rate>>,
    pub sharpness: Rate,
    pub refusals_correct: Rate,
    /// Refusals of in-scope, matching, randomized units (reported).
    pub insufficient: usize,
    pub nothing_written: bool,
    pub gates: Vec<Gate>,
    pub adopt: bool,
}

pub fn summarize(runs: &[SeedRun]) -> Summary {
    let answers: Vec<&Answer> = runs.iter().flat_map(|r| &r.answers).collect();
    let covered = |a: &&Answer| a.lower <= a.truth + 1e-12 && a.truth <= a.upper + 1e-12;
    let cov = |q: Query, fam: Option<Family>| {
        let s: Vec<&&Answer> = answers
            .iter()
            .filter(|a| a.query == q && fam.is_none_or(|f| a.family == f))
            .collect();
        rate(s.iter().filter(|a| covered(a)).count(), s.len())
    };
    let coverage_necessity = cov(Query::Necessity, None);
    let coverage_sufficiency = cov(Query::Sufficiency, None);
    let coverage_by_family = Family::ALL
        .iter()
        .map(|f| {
            (
                format!("{f:?}"),
                [Query::Necessity, Query::Sufficiency]
                    .into_iter()
                    .map(|q| (format!("{q:?}"), cov(q, Some(*f))))
                    .collect(),
            )
        })
        .collect();
    let powered: Vec<&&Answer> = answers
        .iter()
        .filter(|a| a.per_arm >= POWERED_PER_ARM && a.true_lower >= POWERED_TRUE_LOWER)
        .collect();
    let sharpness = rate(
        powered.iter().filter(|a| a.lower > 0.0).count(),
        powered.len(),
    );
    let probes: Vec<&Probe> = runs
        .iter()
        .flat_map(|r| &r.probes)
        .filter(|p| p.expected != Refusal::InsufficientEvidence)
        .collect();
    let refusals_correct = rate(
        probes.iter().filter(|p| p.got == Some(p.expected)).count(),
        probes.len(),
    );
    let insufficient = runs
        .iter()
        .flat_map(|r| &r.probes)
        .filter(|p| p.expected == Refusal::InsufficientEvidence)
        .count();
    let nothing_written = runs.iter().all(|r| r.nothing_written);
    let mut supported: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in runs {
        for (k, (s, n)) in &r.supported {
            let e = supported.entry(k.clone()).or_default();
            e.0 += s;
            e.1 += n;
        }
    }
    let mut gates = Vec::new();
    let mut gate = |criterion, value: String, passed| {
        gates.push(Gate {
            criterion,
            value,
            passed,
        });
    };
    for (criterion, r) in [
        (
            "necessity bounds contain the true PN >= 0.95",
            &coverage_necessity,
        ),
        (
            "sufficiency bounds contain the true PS >= 0.95",
            &coverage_sufficiency,
        ),
    ] {
        gate(
            criterion,
            format!("{}/{} = {:.4}", r.k, r.n, r.rate),
            r.n > 0 && r.rate >= 0.95,
        );
    }
    gate(
        "well-powered contexts give an informative lower bound (> 0) >= 0.80",
        format!("{}/{} = {:.4}", sharpness.k, sharpness.n, sharpness.rate),
        sharpness.n > 0 && sharpness.rate >= 0.80,
    );
    gate(
        "every probe is refused with the expected reason",
        format!("{}/{}", refusals_correct.k, refusals_correct.n),
        refusals_correct.n > 0 && refusals_correct.k == refusals_correct.n,
    );
    gate(
        "computing counterfactuals writes nothing",
        format!("{nothing_written}"),
        nothing_written,
    );
    let adopt = gates.iter().all(|g| g.passed);
    Summary {
        supported,
        coverage_necessity,
        coverage_sufficiency,
        coverage_by_family,
        sharpness,
        refusals_correct,
        insufficient,
        nothing_written,
        gates,
        adopt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_are_distributions_and_truth_lies_in_the_true_bounds() {
        let mut rng = Rng::new(9);
        for f in Family::ALL {
            for _ in 0..50 {
                let m = mix(&mut rng, f);
                let total = m.always + m.helped + m.harmed + m.never;
                assert!((total - 1.0).abs() < 1e-9 && m.never >= 0.0, "{f:?} {m:?}");
                for q in [Query::Necessity, Query::Sufficiency] {
                    let (l, u) = true_bounds(m, q);
                    let t = match q {
                        Query::Necessity => m.necessity(),
                        Query::Sufficiency => m.sufficiency(),
                    };
                    assert!(l - 1e-12 <= t && t <= u + 1e-12, "{f:?} {q:?} {l} {t} {u}");
                }
            }
        }
    }
}
