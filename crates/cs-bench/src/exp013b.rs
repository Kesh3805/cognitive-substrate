//! Experiment 013B: does acting on the effect, not the success rate, pay?
//!
//! Everything here is fixed by `benchmarks/013-prediction-gating/
//! PREREGISTRATION.md` (part B). Changing a constant after held-out results
//! exist invalidates the experiment.
//!
//! Unlike 013A, refusing is not a flat penalty: it yields the **control**
//! outcome, and acting costs [`COST`]. So acting is worth it exactly when
//! `3·(p_treated − p_control) > COST` (an outcome is worth +1, a failure −2),
//! whatever the success rate. Each run has one context of each kind below
//! (two of the first), with a little jitter:
//!
//! | Kind | Control | Treated | Worth acting |
//! |---|---|---|---|
//! | benefit | 0.35 | 0.85 | yes |
//! | low success, useful | 0.15 | 0.55 | yes |
//! | happens anyway | 0.85 | 0.87 | no |
//! | mild backfire | 0.80 | 0.72 | no |
//! | insufficient | 0.60 | 0.65 | no |
//!
//! A success-rate filter acts in "happens anyway" and "mild backfire" and
//! refuses "low success, useful"; an effect gate should do the opposite.
//!
//! Arms: `oracle`, `refuse`, `always_act`, `success_filter` (per-context
//! window filter on treated success, sticky), `absolute_gate` (M6.4 at
//! τ = 0.6, no effect threshold), `effect_baseline` (acts where the
//! burn-in's Newcombe lower bound of the effect reaches [`MIN_EFFECT`];
//! never updates) and `effect_gate` (M6.4 with `min_effect`, control
//! holdouts and randomized probes).

use std::{collections::BTreeMap, sync::Arc};

use cs_core::{AccessContext, FixedClock, HypothesisId, Scope, TenantId, UserId};
use cs_engine::{
    CognitiveEngine,
    causal::{NewHypothesis, NewIntervention, ObserveOutcome},
    predictions::{PredictionResult, RuleResult},
};
use cs_memory::{
    capability::OracleTier,
    causal::{Arm, Assignment, Direction, HypothesisStatus},
};
use cs_policy::{capability::wilson, causal::difference_interval, prediction::RuleRefusal};
use cs_storage::CognitiveStore;
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::{
    metrics::{Interval, mean, paired_bootstrap_at},
    rng::Rng,
};

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [911, 912, 913, 914, 915];
pub const PER_FAMILY: usize = 8;
pub const BURN_IN_PER_ARM: usize = 40;
pub const OPPORTUNITIES: usize = 1_200;
pub const SHIFT_AT: usize = 400;
pub const SUCCESS: f64 = 1.0;
pub const FAILURE: f64 = -2.0;
/// The cost of acting.
pub const COST: f64 = 0.3;
/// The effect that pays for the cost: `COST / (SUCCESS − FAILURE)`.
pub const MIN_EFFECT: f64 = 0.1;
/// The absolute gate's threshold (as in 013A).
pub const TAU: f64 = 0.6;
/// Share of the effect gate's cleared opportunities run as a randomized
/// holdout (treated or control at 0.5), so both arms stay monitored.
pub const HOLDOUT: f64 = 0.1;
/// Randomized probes (treated or control at 0.5) on refused opportunities:
/// in a demoted lineage, and where a rule exists but its effect is short.
pub const DEMOTED_PROBE_RATE: f64 = 0.25;
pub const SHORT_PROBE_RATE: f64 = 0.05;
/// Contexts that shift.
pub const SHIFTED: usize = 2;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
pub const BOOTSTRAP_SEED: u64 = 13;

const WINDOW: usize = 25;
const WINDOW_MIN: usize = 10;
const QUARANTINE_UPPER: f64 = 0.6;

const KINDS: [(f64, f64); 6] = [
    (0.35, 0.85),
    (0.35, 0.85),
    (0.15, 0.55),
    (0.85, 0.87),
    (0.80, 0.72),
    (0.60, 0.65),
];
const CONTEXTS: usize = KINDS.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Stationary,
    /// The two benefit contexts' control rate rises to 0.85: acting becomes
    /// useless while still "succeeding".
    ControlRise,
    /// The two benefit contexts' treated rate falls to 0.35.
    EffectReversal,
}

impl Family {
    pub const ALL: [Self; 3] = [Self::Stationary, Self::ControlRise, Self::EffectReversal];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmName {
    Oracle,
    Refuse,
    AlwaysAct,
    SuccessFilter,
    AbsoluteGate,
    EffectBaseline,
    EffectGate,
}

impl ArmName {
    pub const ALL: [Self; 7] = [
        Self::Oracle,
        Self::Refuse,
        Self::AlwaysAct,
        Self::SuccessFilter,
        Self::AbsoluteGate,
        Self::EffectBaseline,
        Self::EffectGate,
    ];
}

struct World {
    family: Family,
    control: Vec<f64>,
    treated: Vec<f64>,
}

impl World {
    fn rates(&self, c: usize, t: usize) -> (f64, f64) {
        let (mut pc, mut pt) = (self.control[c], self.treated[c]);
        if c < SHIFTED && t >= SHIFT_AT {
            match self.family {
                Family::Stationary => {}
                Family::ControlRise => pc = 0.85,
                Family::EffectReversal => pt = 0.35,
            }
        }
        (pc, pt)
    }

    fn worth_acting(&self, c: usize, t: usize) -> bool {
        let (pc, pt) = self.rates(c, t);
        (SUCCESS - FAILURE) * (pt - pc) > COST
    }
}

fn uniform(rng: &mut Rng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

fn outcome(y: bool) -> f64 {
    if y { SUCCESS } else { FAILURE }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tally {
    pub opportunities: u32,
    pub acted: u32,
    pub probes: u32,
    pub utility: f64,
}

impl Tally {
    /// Acting: the treated outcome, minus the cost. Not acting: the
    /// control outcome.
    fn add(&mut self, acted: bool, y_treated: bool, y_control: bool) {
        self.opportunities += 1;
        self.acted += u32::from(acted);
        self.utility += if acted {
            outcome(y_treated) - COST
        } else {
            outcome(y_control)
        };
    }

    pub fn mean_utility(&self) -> f64 {
        self.utility / f64::from(self.opportunities.max(1))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReport {
    pub context: usize,
    pub shifted: bool,
    /// The effect gate's last decision before the shift was to act.
    pub exposed: bool,
    pub detection_delay: Option<u32>,
    pub demotions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRow {
    pub seed: u64,
    pub family: Family,
    pub run: usize,
    /// Each gate's hypothesis was supported after the burn-in.
    pub supported: bool,
    pub arms: BTreeMap<ArmName, Tally>,
    pub contexts: Vec<ContextReport>,
}

/// The generator seed of one run. Seed, run and family occupy disjoint bits,
/// so no two (seed, family, run) triples share a generator. (An earlier
/// version XORed seed and run into the same low bits, so seeds 1–3 replayed
/// the same eight runs per family.)
fn run_seed_value(seed: u64, family: Family, run: usize) -> u64 {
    0x013b_013b_013b_013b ^ (seed & 0xFFFF) ^ ((run as u64 & 0xFF) << 16) ^ ((family as u64) << 24)
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<RunRow>, String> {
    run_seed_with(store, seed, HOLDOUT).await
}

/// As [`run_seed`], with another holdout rate: for development sweeps only.
/// Held-out runs use [`HOLDOUT`].
pub async fn run_seed_with(
    store: Arc<dyn CognitiveStore>,
    seed: u64,
    holdout: f64,
) -> Result<Vec<RunRow>, String> {
    let mut rows = Vec::new();
    for family in Family::ALL {
        for run in 0..PER_FAMILY {
            rows.push(run_one(store.clone(), seed, family, run, holdout).await?);
        }
    }
    Ok(rows)
}

/// One gate's engine state: its own environment, so its probes and holdouts
/// never feed the other gate's evidence.
struct GateEnv {
    environment: String,
    hypothesis: HypothesisId,
}

#[allow(clippy::too_many_lines)]
async fn run_one(
    store: Arc<dyn CognitiveStore>,
    seed: u64,
    family: Family,
    run: usize,
    holdout: f64,
) -> Result<RunRow, String> {
    let err = |e: cs_core::Error| e.to_string();
    let mut rng = Rng::new(run_seed_value(seed, family, run));
    let mut control = Vec::with_capacity(CONTEXTS);
    let mut treated = Vec::with_capacity(CONTEXTS);
    for (pc, pt) in KINDS {
        let j = |rng: &mut Rng| 0.06 * uniform(rng) - 0.03;
        control.push((pc + j(&mut rng)).clamp(0.01, 0.99));
        treated.push((pt + j(&mut rng)).clamp(0.01, 0.99));
    }
    let w = World {
        family,
        control,
        treated,
    };
    let clock = Arc::new(FixedClock::new(crate::corpus::start()));
    let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
    let tenant = TenantId::new_v7();
    let user = UserId::new_v7();
    let ctx = AccessContext::user(tenant, user);
    let scope = Scope::private(tenant, user);
    let new = |env: &str, context: usize, arm: Arm, assignment: Assignment| {
        let randomized = assignment == Assignment::Randomized;
        NewIntervention {
            scope: scope.clone(),
            environment: env.to_owned(),
            treatment: "act".into(),
            arm,
            assignment,
            assignment_detail: randomized.then(|| "draw".into()),
            propensity: randomized.then_some(0.5),
            context: format!("c{context}"),
            state: BTreeMap::new(),
            execution_id: None,
        }
    };
    let observe = |value: bool| ObserveOutcome {
        outcome: "success".into(),
        value,
        tier: OracleTier::Deterministic,
    };

    // Shared randomized burn-in, recorded identically in both gates'
    // environments.
    let envs = [
        format!("exp013b-{seed}-{family:?}-{run}-absolute"),
        format!("exp013b-{seed}-{family:?}-{run}-effect"),
    ];
    let mut counts = [[(0u32, 0u32); 2]; CONTEXTS]; // [context][arm] = (n, k)
    let mut filter_runs: Vec<Vec<bool>> = vec![Vec::new(); CONTEXTS];
    for _ in 0..BURN_IN_PER_ARM {
        for c in 0..CONTEXTS {
            for (a, arm) in [Arm::Treated, Arm::Control].into_iter().enumerate() {
                let p = if arm == Arm::Treated {
                    w.treated[c]
                } else {
                    w.control[c]
                };
                let value = uniform(&mut rng) < p;
                counts[c][a].0 += 1;
                counts[c][a].1 += u32::from(value);
                if arm == Arm::Treated {
                    filter_runs[c].push(value);
                }
                clock.advance(Duration::minutes(1));
                for env in &envs {
                    let i = engine
                        .record_intervention(&ctx, new(env, c, arm, Assignment::Randomized))
                        .await
                        .map_err(err)?;
                    engine
                        .observe_outcome(&ctx, i.id, observe(value))
                        .await
                        .map_err(err)?;
                }
            }
        }
    }
    let mut gates = Vec::new();
    let mut supported = true;
    for env in &envs {
        let h = engine
            .create_hypothesis(
                &ctx,
                NewHypothesis {
                    scope: scope.clone(),
                    environment: env.clone(),
                    treatment: "act".into(),
                    outcome: "success".into(),
                    direction: Direction::Raises,
                    min_effect: 0.05,
                    contexts: Vec::new(),
                    statement: None,
                },
            )
            .await
            .map_err(err)?;
        let h = engine
            .evaluate_hypothesis(&ctx, h.id, 1)
            .await
            .map_err(err)?;
        supported &= h.status == HypothesisStatus::Supported;
        gates.push(GateEnv {
            environment: env.clone(),
            hypothesis: h.id,
        });
    }
    let (absolute, effect) = (&gates[0], &gates[1]);

    // Static baselines from the burn-in.
    let baseline_acts: Vec<bool> = (0..CONTEXTS)
        .map(|c| {
            let [(nt, kt), (nc, kc)] = counts[c];
            difference_interval(kt, nt, kc, nc).is_some_and(|(_, l, _)| l >= MIN_EFFECT)
        })
        .collect();
    let quarantined = |runs: &[bool]| {
        let window = &runs[runs.len().saturating_sub(WINDOW)..];
        window.len() >= WINDOW_MIN
            && wilson(
                window.iter().filter(|s| **s).count() as f64,
                window.len() as f64,
                1.96,
            )
            .is_some_and(|w| w.1 < QUARANTINE_UPPER)
    };
    let mut filter_off: Vec<bool> = filter_runs.iter().map(|r| quarantined(r)).collect();

    let mut arms: BTreeMap<ArmName, Tally> = ArmName::ALL
        .into_iter()
        .map(|a| (a, Tally::default()))
        .collect();
    let mut reports: Vec<ContextReport> = (0..CONTEXTS)
        .map(|c| ContextReport {
            context: c,
            shifted: c < SHIFTED && family != Family::Stationary,
            exposed: false,
            detection_delay: None,
            demotions: 0,
        })
        .collect();
    let mut seen = [0usize; CONTEXTS];
    let mut since_shift = [0u32; CONTEXTS];

    for t in 0..OPPORTUNITIES {
        let c = rng.below(CONTEXTS);
        let (pc, pt) = w.rates(c, t);
        let yt = uniform(&mut rng) < pt;
        let yc = uniform(&mut rng) < pc;
        let draw = uniform(&mut rng);
        let arm_draw = uniform(&mut rng);
        if t >= SHIFT_AT {
            since_shift[c] += 1;
        }
        let mut add = |a: ArmName, acted: bool| {
            arms.get_mut(&a)
                .expect("every arm has a tally")
                .add(acted, yt, yc);
        };
        add(ArmName::Oracle, w.worth_acting(c, t));
        add(ArmName::Refuse, false);
        add(ArmName::AlwaysAct, true);
        add(ArmName::EffectBaseline, baseline_acts[c]);
        let acted = !filter_off[c];
        if acted {
            filter_runs[c].push(yt);
            filter_off[c] = quarantined(&filter_runs[c]);
        }
        add(ArmName::SuccessFilter, acted);

        // The absolute gate: outcome-only, as in 013A, without probing.
        let view = engine
            .prediction_rule(&ctx, absolute.hypothesis, &format!("c{c}"), Some(TAU), None)
            .await
            .map_err(err)?;
        let acted = view.gate.is_some_and(|g| g.execute);
        clock.advance(Duration::minutes(1));
        if acted {
            let i = engine
                .record_intervention(
                    &ctx,
                    new(&absolute.environment, c, Arm::Treated, Assignment::Passive),
                )
                .await
                .map_err(err)?;
            engine
                .predict(&ctx, i.id, absolute.hypothesis)
                .await
                .map_err(err)?;
            engine
                .observe_outcome(&ctx, i.id, observe(yt))
                .await
                .map_err(err)?;
        }
        arms.get_mut(&ArmName::AbsoluteGate)
            .expect("every arm has a tally")
            .add(acted, yt, yc);

        // The effect gate.
        let view = engine
            .prediction_rule(
                &ctx,
                effect.hypothesis,
                &format!("c{c}"),
                Some(0.0),
                Some(MIN_EFFECT),
            )
            .await
            .map_err(err)?;
        let clear = view.gate.is_some_and(|g| g.execute);
        if t < SHIFT_AT {
            reports[c].exposed = clear;
        }
        let demoted = matches!(
            view.result,
            RuleResult::Refused {
                reason: RuleRefusal::Demoted
            }
        );
        let short = matches!(view.result, RuleResult::Rule { .. }) && !clear;
        let randomize = if clear {
            draw < holdout
        } else if demoted {
            draw < DEMOTED_PROBE_RATE
        } else {
            short && draw < SHORT_PROBE_RATE
        };
        let tally = arms
            .get_mut(&ArmName::EffectGate)
            .expect("every arm has a tally");
        if clear || randomize {
            let (arm, assignment) = if randomize {
                let arm = if arm_draw < 0.5 {
                    Arm::Treated
                } else {
                    Arm::Control
                };
                (arm, Assignment::Randomized)
            } else {
                (Arm::Treated, Assignment::Passive)
            };
            let i = engine
                .record_intervention(&ctx, new(&effect.environment, c, arm, assignment))
                .await
                .map_err(err)?;
            if clear
                && let PredictionResult::Refused { reason } = engine
                    .predict(&ctx, i.id, effect.hypothesis)
                    .await
                    .map_err(err)?
            {
                return Err(format!(
                    "effect gate cleared but {arm:?} refused: {reason:?}"
                ));
            }
            clock.advance(Duration::minutes(1));
            let value = if arm == Arm::Treated { yt } else { yc };
            engine
                .observe_outcome(&ctx, i.id, observe(value))
                .await
                .map_err(err)?;
            if !clear {
                tally.probes += 1;
            }
            tally.add(arm == Arm::Treated, yt, yc);
        } else {
            tally.add(false, yt, yc);
        }

        let n = view.monitor.demotions.len();
        if n > seen[c] {
            let r = &mut reports[c];
            r.demotions += (n - seen[c]) as u32;
            if r.shifted && t >= SHIFT_AT && r.detection_delay.is_none() {
                r.detection_delay = Some(since_shift[c]);
            }
            seen[c] = n;
        }
    }
    Ok(RunRow {
        seed,
        family,
        run,
        supported,
        arms,
        contexts: reports,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub criterion: &'static str,
    pub value: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FamilySummary {
    pub family: Family,
    pub runs: usize,
    pub supported: usize,
    pub utility: BTreeMap<ArmName, f64>,
    /// Effect gate minus each other arm, paired by run, 95% CI (unadjusted;
    /// the gates apply their own Bonferroni levels).
    pub effect_gate_minus: BTreeMap<ArmName, Interval>,
    pub exposed_shifted: usize,
    pub detected: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub families: Vec<FamilySummary>,
    pub gates: Vec<Gate>,
    pub adopt: bool,
}

pub fn summarize(rows: &[RunRow]) -> Summary {
    let per_run = |rs: &[&RunRow], a: ArmName| -> Vec<f64> {
        rs.iter().map(|r| r.arms[&a].mean_utility()).collect()
    };
    let diff = |f: Family, a: ArmName, alpha: f64| -> Interval {
        let rs: Vec<&RunRow> = rows.iter().filter(|r| r.family == f).collect();
        paired_bootstrap_at(
            &per_run(&rs, a),
            &per_run(&rs, ArmName::EffectGate),
            BOOTSTRAP_RESAMPLES,
            BOOTSTRAP_SEED,
            alpha,
        )
    };
    let families: Vec<FamilySummary> = Family::ALL
        .into_iter()
        .map(|f| {
            let rs: Vec<&RunRow> = rows.iter().filter(|r| r.family == f).collect();
            let shifted: Vec<&ContextReport> = rs
                .iter()
                .flat_map(|r| r.contexts.iter())
                .filter(|c| c.shifted && c.exposed)
                .collect();
            FamilySummary {
                family: f,
                runs: rs.len(),
                supported: rs.iter().filter(|r| r.supported).count(),
                utility: ArmName::ALL
                    .into_iter()
                    .map(|a| (a, mean(&per_run(&rs, a))))
                    .collect(),
                effect_gate_minus: ArmName::ALL
                    .into_iter()
                    .filter(|a| *a != ArmName::EffectGate)
                    .map(|a| (a, diff(f, a, 0.05)))
                    .collect(),
                exposed_shifted: shifted.len(),
                detected: shifted
                    .iter()
                    .filter(|c| c.detection_delay.is_some())
                    .count(),
            }
        })
        .collect();

    let check = |f: Family, arms: &[ArmName], margin: f64, alpha: f64| -> Vec<String> {
        arms.iter()
            .filter_map(|&a| {
                let i = diff(f, a, alpha);
                (i.lower <= margin).then(|| format!("{f:?} vs {a:?}: {i:?}"))
            })
            .collect()
    };
    let b1_arms = [
        ArmName::AlwaysAct,
        ArmName::Refuse,
        ArmName::SuccessFilter,
        ArmName::AbsoluteGate,
    ];
    let b1 = check(Family::Stationary, &b1_arms, 0.0, 0.05 / 4.0);
    let b2: Vec<String> = Family::ALL
        .into_iter()
        .flat_map(|f| check(f, &[ArmName::EffectBaseline], -0.02, 0.05 / 3.0))
        .collect();
    let b3 = check(
        Family::ControlRise,
        &[ArmName::EffectBaseline, ArmName::AbsoluteGate],
        0.0,
        0.05 / 2.0,
    );
    let b4 = check(
        Family::EffectReversal,
        &[ArmName::EffectBaseline],
        0.0,
        0.05,
    );
    let rise = families
        .iter()
        .find(|s| s.family == Family::ControlRise)
        .expect("every family is summarized");
    let summary = |v: &[String], all: &str| {
        if v.is_empty() {
            all.to_owned()
        } else {
            v.join("; ")
        }
    };
    let gates = vec![
        Gate {
            criterion: "B1 stationary: effect gate beats always-act, refuse, the success filter and the absolute gate (Bonferroni CI lower > 0)",
            value: summary(&b1, "all 4 comparisons"),
            passed: b1.is_empty(),
        },
        Gate {
            criterion: "B2 every family: effect gate non-inferior to the static effect baseline (CI lower > −0.02)",
            value: summary(&b2, "all 3 families"),
            passed: b2.is_empty(),
        },
        Gate {
            criterion: "B3 control rise: effect gate beats the static effect baseline and the absolute gate",
            value: summary(&b3, "both comparisons"),
            passed: b3.is_empty(),
        },
        Gate {
            criterion: "B4 effect reversal: effect gate beats the static effect baseline",
            value: summary(&b4, "the comparison"),
            passed: b4.is_empty(),
        },
        Gate {
            criterion: "B5 control rise: ≥ 90% of exposed shifted contexts demoted",
            value: format!("{}/{}", rise.detected, rise.exposed_shifted),
            passed: rise.exposed_shifted > 0
                && rise.detected as f64 >= 0.9 * rise.exposed_shifted as f64,
        },
    ];
    let adopt = gates.iter().all(|g| g.passed);
    Summary {
        families,
        gates,
        adopt,
    }
}

#[cfg(test)]
mod seeding_tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_run_has_its_own_generator() {
        for seeds in [&DEV_SEEDS[..], &HELD_OUT_SEEDS[..]] {
            let mut seen = HashSet::new();
            for &seed in seeds {
                for family in Family::ALL {
                    for run in 0..PER_FAMILY {
                        assert!(seen.insert(run_seed_value(seed, family, run)));
                    }
                }
            }
        }
    }
}
