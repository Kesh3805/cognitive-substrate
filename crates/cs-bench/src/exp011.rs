//! Experiment 011: causal hypotheses under confounding.
//!
//! Everything here is fixed by `benchmarks/011-causal-evidence/
//! PREREGISTRATION.md`. Changing a constant after held-out results exist
//! invalidates the experiment.
//!
//! Each hypothesis gets its own environment. Units arrive in contexts; each
//! unit has a hidden confounder `u` that varies **within** contexts and
//! raises the outcome. A passive agent chooses the treatment more often
//! when `u` is set (confounding) or when the outcome is already present
//! (reverse causation). Randomized interventions draw the arm at
//! probability 0.5. The true effect is known to the generator.

use std::{collections::BTreeMap, sync::Arc};

use cs_core::{AccessContext, FixedClock, Scope, TenantId, UserId};
use cs_engine::{
    CognitiveEngine,
    causal::{NewHypothesis, NewIntervention, ObserveOutcome},
};
use cs_memory::{
    capability::OracleTier,
    causal::{Arm, Assignment, Direction, HypothesisStatus, Intervention},
};
use cs_storage::CognitiveStore;
use serde::Serialize;

use crate::rng::Rng;

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [701, 702, 703, 704, 705];
/// Hypotheses per type per seed.
pub const PER_TYPE: usize = 40;
/// Contexts in each environment.
pub const CONTEXTS: usize = 12;
/// Passive units per context.
pub const PASSIVE_PER_CONTEXT: usize = 10;
/// The minimum effect every hypothesis states.
pub const MIN_EFFECT: f64 = 0.05;
/// A cause the detection gate expects to be found: true effect at least
/// this large, with at least this many randomized outcomes per arm in
/// total. Below this, the evaluation is underpowered by design and the
/// rate is reported, not gated.
pub const DETECTABLE_EFFECT: f64 = 0.25;
pub const ADEQUATE_PER_ARM_TOTAL: usize = 60;

const BASE: f64 = 0.4;
const CONFOUNDER_LIFT: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// +0.3 in every context.
    StrongPositive,
    /// +0.12 in every context: near the minimum effect.
    NearBoundary,
    /// −0.3 in every context; stated as "lowers" (true) or "raises" (false).
    Negative,
    /// No effect, strong confounding of the passive choice.
    ConfoundedNull,
    /// No effect; the passive agent acts when the outcome is already there.
    ReverseCausation,
    /// +0.4 in a third of the contexts, 0 elsewhere.
    ContextDependent,
    /// +0.3 in every context but one, where it is −0.4.
    HarmfulInOne,
}

impl Kind {
    pub const ALL: [Self; 7] = [
        Self::StrongPositive,
        Self::NearBoundary,
        Self::Negative,
        Self::ConfoundedNull,
        Self::ReverseCausation,
        Self::ContextDependent,
        Self::HarmfulInOne,
    ];
}

#[derive(Debug, Clone)]
struct Spec {
    kind: Kind,
    direction: Direction,
    /// True effect of the treatment on P(outcome) in each context.
    effects: Vec<f64>,
    /// Confounder prevalence per context (it also varies within one).
    prevalence: Vec<f64>,
    randomized_contexts: Vec<usize>,
    per_arm: usize,
}

fn spec(rng: &mut Rng, kind: Kind) -> Spec {
    let effects: Vec<f64> = (0..CONTEXTS)
        .map(|k| match kind {
            Kind::StrongPositive => 0.3,
            Kind::NearBoundary => 0.12,
            Kind::Negative => -0.3,
            Kind::ConfoundedNull | Kind::ReverseCausation => 0.0,
            Kind::ContextDependent => {
                if k % 3 == 0 {
                    0.4
                } else {
                    0.0
                }
            }
            Kind::HarmfulInOne => {
                if k == 0 {
                    -0.4
                } else {
                    0.3
                }
            }
        })
        .collect();
    let direction = match kind {
        Kind::Negative if rng.chance(0.5) => Direction::Lowers,
        _ => Direction::Raises,
    };
    let prevalence = (0..CONTEXTS)
        .map(|_| 0.2 + 0.6 * (rng.below(1000) as f64 / 1000.0))
        .collect();
    let mut contexts: Vec<usize> = (0..CONTEXTS).collect();
    rng.shuffle(&mut contexts);
    // Uneven budgets: 3..=12 contexts, 2..=8 per arm.
    contexts.truncate(3 + rng.below(10));
    contexts.sort_unstable();
    Spec {
        kind,
        direction,
        effects,
        prevalence,
        randomized_contexts: contexts,
        per_arm: 2 + rng.below(7),
    }
}

/// P(outcome) for one unit.
fn p_outcome(base: f64, effect: f64, treated: bool, u: bool) -> f64 {
    (base + if treated { effect } else { 0.0 } + if u { CONFOUNDER_LIFT } else { 0.0 })
        .clamp(0.0, 1.0)
}

/// The true effect over the randomized contexts, weighted as the estimator
/// weighs them (equal arms per context, so equal weights), in the stated
/// direction. On the probability scale the effect is exactly the added
/// lift, as nothing is clamped (BASE − 0.4 ≥ 0 and BASE + 0.4 + lift ≤ 1).
fn true_effect(s: &Spec) -> f64 {
    let mean = s
        .randomized_contexts
        .iter()
        .map(|k| s.effects[*k])
        .sum::<f64>()
        / s.randomized_contexts.len() as f64;
    s.direction.signed(mean)
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub seed: u64,
    pub hypothesis: usize,
    pub kind: Kind,
    pub direction: Direction,
    pub contexts: usize,
    pub per_arm: usize,
    pub true_effect: f64,
    pub status: HypothesisStatus,
    pub estimate: Option<f64>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub opposite_context: bool,
    pub observational_lower: Option<f64>,
    /// The same rule applied to passive evidence as if it were randomized.
    pub naive_status: HypothesisStatus,
    /// Re-evaluating the same evidence gives the same evaluation.
    pub deterministic: bool,
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<Row>, String> {
    let mut rng = Rng::new(seed ^ 0x0011_0011_0011_0011);
    let clock = Arc::new(FixedClock::new(crate::corpus::start()));
    let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
    let tenant = TenantId::new_v7();
    let user = UserId::new_v7();
    let ctx = AccessContext::user(tenant, user);
    let scope = Scope::private(tenant, user);

    let mut specs = Vec::new();
    for kind in Kind::ALL {
        for _ in 0..PER_TYPE {
            specs.push(spec(&mut rng, kind));
        }
    }
    let mut rows = Vec::new();
    for (j, s) in specs.iter().enumerate() {
        let environment = format!("env-{j}@1");
        let hypothesis = engine
            .create_hypothesis(
                &ctx,
                NewHypothesis {
                    scope: scope.clone(),
                    environment: environment.clone(),
                    treatment: "x".into(),
                    outcome: "y".into(),
                    direction: s.direction,
                    min_effect: MIN_EFFECT,
                    contexts: Vec::new(),
                    statement: None,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        let record = async |context: usize,
                            arm: Arm,
                            assignment: Assignment,
                            value: bool|
               -> Result<(), String> {
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
                        context: format!("class-{context}"),
                        state: BTreeMap::new(),
                        execution_id: None,
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
            engine
                .observe_outcome(
                    &ctx,
                    i.id,
                    ObserveOutcome {
                        outcome: "y".into(),
                        value,
                        tier: OracleTier::Deterministic,
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        };
        // Passive evidence over every context: the agent's own choices.
        for k in 0..CONTEXTS {
            for _ in 0..PASSIVE_PER_CONTEXT {
                let u = rng.chance(s.prevalence[k]);
                let (treated, value) = match s.kind {
                    Kind::ReverseCausation => {
                        // The outcome is already present before acting, and
                        // the agent acts on it; the treatment changes nothing.
                        let already = rng.chance(p_outcome(BASE, 0.0, false, u));
                        (rng.chance(if already { 0.85 } else { 0.15 }), already)
                    }
                    _ => {
                        let treated = rng.chance(if u { 0.85 } else { 0.15 });
                        (
                            treated,
                            rng.chance(p_outcome(BASE, s.effects[k], treated, u)),
                        )
                    }
                };
                let arm = if treated { Arm::Treated } else { Arm::Control };
                record(k, arm, Assignment::Passive, value).await?;
            }
        }
        // Randomized evidence over the sampled contexts.
        for &k in &s.randomized_contexts {
            for _ in 0..s.per_arm {
                for arm in [Arm::Treated, Arm::Control] {
                    let u = rng.chance(s.prevalence[k]);
                    let value = rng.chance(p_outcome(BASE, s.effects[k], arm == Arm::Treated, u));
                    record(k, arm, Assignment::Randomized, value).await?;
                }
            }
        }
        clock.advance(time::Duration::seconds(1));
        let evaluated = engine
            .evaluate_hypothesis(&ctx, hypothesis.id, 1)
            .await
            .map_err(|e| e.to_string())?;
        let e = evaluated.evaluation.clone().ok_or("no evaluation")?;
        // Determinism: the pure evaluation over the same evidence, again.
        let evidence: Vec<Intervention> = {
            let mut v = Vec::new();
            for id in &e.interventions {
                v.push(
                    engine
                        .get_intervention(&ctx, *id)
                        .await
                        .map_err(|e| e.to_string())?,
                );
            }
            v
        };
        let again = cs_policy::causal::evaluate(&hypothesis, &evidence, e.as_of);
        // Naive baseline: passive evidence read as if randomized.
        let all = engine_interventions(&engine, &ctx, &environment).await?;
        let relabelled: Vec<Intervention> = all
            .into_iter()
            .filter(|i| i.assignment == Assignment::Passive)
            .map(|mut i| {
                i.assignment = Assignment::Randomized;
                i.propensity = Some(0.5);
                i
            })
            .collect();
        let naive = cs_policy::causal::evaluate(&hypothesis, &relabelled, e.as_of);
        rows.push(Row {
            seed,
            hypothesis: j,
            kind: s.kind,
            direction: s.direction,
            contexts: s.randomized_contexts.len(),
            per_arm: s.per_arm,
            true_effect: true_effect(s),
            status: evaluated.status,
            estimate: e.effect.map(|p| p.estimate),
            lower: e.effect.map(|p| p.lower),
            upper: e.effect.map(|p| p.upper),
            opposite_context: e.opposite_context,
            observational_lower: e.observational.map(|p| p.lower),
            naive_status: naive.status,
            deterministic: again.status == e.status
                && again.effect == e.effect
                && again.interventions == e.interventions,
        });
    }
    Ok(rows)
}

async fn engine_interventions(
    engine: &CognitiveEngine,
    ctx: &AccessContext,
    environment: &str,
) -> Result<Vec<Intervention>, String> {
    engine
        .store()
        .interventions_for(ctx, environment, "x", 100_000)
        .await
        .map_err(|e| e.to_string())
}

/// Hypotheses whose true effect is at most the minimum effect: supporting
/// one is a false support.
fn non_cause(r: &Row) -> bool {
    r.true_effect <= MIN_EFFECT
}

/// Detectable causes: effect comfortably above the minimum, adequate
/// budget.
fn detectable(r: &Row) -> bool {
    r.true_effect >= DETECTABLE_EFFECT && r.contexts * r.per_arm >= ADEQUATE_PER_ARM_TOTAL
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub criterion: &'static str,
    pub value: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rate {
    pub n: usize,
    pub k: usize,
    pub rate: f64,
    /// Wilson 95% upper bound.
    pub upper: f64,
}

fn rate(k: usize, n: usize) -> Rate {
    let upper = cs_policy::capability::wilson(k as f64, n as f64, 1.96).map_or(1.0, |(_, u)| u);
    Rate {
        n,
        k,
        rate: if n == 0 { 0.0 } else { k as f64 / n as f64 },
        upper,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// Kind → status → count.
    pub statuses: BTreeMap<String, BTreeMap<String, usize>>,
    pub naive_statuses: BTreeMap<String, BTreeMap<String, usize>>,
    pub false_supported: Rate,
    pub naive_false_supported: Rate,
    pub detection: Rate,
    /// Every cause above the minimum effect, whatever its budget
    /// (reported).
    pub detection_all: Rate,
    pub false_refuted: Rate,
    /// Hypotheses with an effect interval: share whose interval holds the
    /// true effect.
    pub coverage: Rate,
    /// Harmful-in-one hypotheses whose harmed context was sampled, and how
    /// many of them were nevertheless supported (reported).
    pub harmed_supported: Rate,
    pub gates: Vec<Gate>,
    pub adopt: bool,
}

pub fn summarize(rows: &[Row]) -> Summary {
    let tally = |f: fn(&Row) -> HypothesisStatus| {
        let mut m: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for r in rows {
            *m.entry(format!("{:?}", r.kind))
                .or_default()
                .entry(f(r).as_str().to_owned())
                .or_default() += 1;
        }
        m
    };
    let count = |pred: &dyn Fn(&Row) -> bool, hit: &dyn Fn(&Row) -> bool| {
        let n: Vec<&Row> = rows.iter().filter(|r| pred(r)).collect();
        rate(n.iter().filter(|r| hit(r)).count(), n.len())
    };
    let false_supported = count(&non_cause, &|r| r.status == HypothesisStatus::Supported);
    let naive_false_supported = count(&non_cause, &|r| {
        r.naive_status == HypothesisStatus::Supported
    });
    let detection = count(&detectable, &|r| r.status == HypothesisStatus::Supported);
    let detection_all = count(&|r| r.true_effect > MIN_EFFECT, &|r| {
        r.status == HypothesisStatus::Supported
    });
    let false_refuted = count(&|r| r.true_effect > MIN_EFFECT, &|r| {
        r.status == HypothesisStatus::Refuted
    });
    let coverage = count(&|r| r.lower.is_some(), &|r| {
        r.lower.is_some_and(|l| l <= r.true_effect) && r.upper.is_some_and(|u| u >= r.true_effect)
    });
    let harmed_supported = count(&|r| r.kind == Kind::HarmfulInOne, &|r| {
        r.status == HypothesisStatus::Supported
    });
    let deterministic = rows.iter().all(|r| r.deterministic);
    let mut gates = Vec::new();
    let mut gate = |criterion, value: String, passed| {
        gates.push(Gate {
            criterion,
            value,
            passed,
        });
    };
    gate(
        "false-supported rate <= 0.01 with Wilson 95% upper bound <= 0.02",
        format!(
            "{}/{} = {:.4} (upper {:.4})",
            false_supported.k, false_supported.n, false_supported.rate, false_supported.upper
        ),
        false_supported.rate <= 0.01 && false_supported.upper <= 0.02,
    );
    gate(
        "detectable causes (effect >= 0.25, >= 60 randomized outcomes per arm) supported >= 0.70",
        format!("{}/{} = {:.4}", detection.k, detection.n, detection.rate),
        detection.rate >= 0.70,
    );
    gate(
        "false-refuted rate <= 0.01",
        format!(
            "{}/{} = {:.4}",
            false_refuted.k, false_refuted.n, false_refuted.rate
        ),
        false_refuted.rate <= 0.01,
    );
    gate(
        "95% intervals cover the true effect >= 0.90",
        format!("{}/{} = {:.4}", coverage.k, coverage.n, coverage.rate),
        coverage.rate >= 0.90,
    );
    gate(
        "the naive baseline is fooled: false-supported >= 0.20",
        format!(
            "{}/{} = {:.4}",
            naive_false_supported.k, naive_false_supported.n, naive_false_supported.rate
        ),
        naive_false_supported.rate >= 0.20,
    );
    gate(
        "every evaluation is reproduced exactly",
        format!("{}", deterministic),
        deterministic,
    );
    let adopt = gates.iter().all(|g| g.passed);
    Summary {
        statuses: tally(|r| r.status),
        naive_statuses: tally(|r| r.naive_status),
        false_supported,
        naive_false_supported,
        detection,
        detection_all,
        false_refuted,
        coverage,
        harmed_supported,
        gates,
        adopt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_deterministic_and_unclamped() {
        let mut a = Rng::new(5);
        let mut b = Rng::new(5);
        for kind in Kind::ALL {
            let (x, y) = (spec(&mut a, kind), spec(&mut b, kind));
            assert_eq!(x.randomized_contexts, y.randomized_contexts);
            assert!((3..=CONTEXTS).contains(&x.randomized_contexts.len()));
            assert!((2..=8).contains(&x.per_arm));
            for e in &x.effects {
                assert!(BASE + e >= 0.0 && BASE + e + CONFOUNDER_LIFT <= 1.0);
            }
        }
    }
}
