//! Experiment 013: prediction-gated execution under regime shifts.
//!
//! Everything here is fixed by `benchmarks/013-prediction-gating/
//! PREREGISTRATION.md`. Changing a constant after held-out results exist
//! invalidates the experiment.
//!
//! Each run is one environment with [`CONTEXTS`] contexts and one stated
//! hypothesis ("the treatment raises success"). A randomized burn-in of
//! [`BURN_IN_PER_ARM`] outcomes per arm and context is shared by every arm.
//! Then [`OPPORTUNITIES`] opportunities arrive, each in a uniformly drawn
//! context, and each arm decides whether to act. Every arm sees the same
//! opportunity stream and the same uniform draws (common random numbers),
//! so differences are paired.
//!
//! Arms:
//! * `oracle`: acts exactly when acting has positive expected utility.
//! * `refuse`: never acts.
//! * `global`: the existing capability drift filter over all runs
//!   (3 consecutive failures, or window-25 Wilson upper < 0.6); quarantine
//!   is sticky, as it is in the system.
//! * `per_context`: the same filter, per context.
//! * `per_context_probing`: per context, the window rule only, probing
//!   quarantined contexts with treated runs and re-earning from fixed blocks
//!   like the gate.
//! * `per_context_cusum`: the gate's own detector, null and re-earning on
//!   raw executions per context, without the causal layer: it isolates what
//!   the hypothesis adds beyond a better detector.
//! * `gate`: the M6.4 prediction gate at [`TAU`] through the real engine.

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
use cs_policy::{
    capability::wilson,
    prediction::{ALPHA, Detector, MIN_RULE_N, NULL_FLOOR, RuleRefusal, Z},
};
use cs_storage::CognitiveStore;
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::{
    metrics::{Interval, mean, paired_bootstrap_at, percentile},
    rng::Rng,
};

pub const DEV_SEEDS: [u64; 3] = [1, 2, 3];
pub const HELD_OUT_SEEDS: [u64; 5] = [901, 902, 903, 904, 905];
/// Runs per family per seed.
pub const PER_FAMILY: usize = 8;
pub const CONTEXTS: usize = 6;
pub const BURN_IN_PER_ARM: usize = 25;
pub const OPPORTUNITIES: usize = 1_500;
/// The regime shift.
pub const SHIFT_AT: usize = 400;
/// The `recovery` family returns to its base rates here.
pub const RETURN_AT: usize = 700;
/// The `gradual` family drifts over this many opportunities.
pub const DRIFT_SPAN: usize = 300;
/// Contexts that shift (all, in the `global` family).
pub const SHIFTED: usize = 2;
/// Utility of one opportunity.
pub const SUCCESS: f64 = 1.0;
pub const FAILURE: f64 = -2.0;
pub const REFUSAL: f64 = -0.25;
/// The gate's threshold on the rule's lower bound: the break-even success
/// rate for the utilities above is 7/12 ≈ 0.583.
pub const TAU: f64 = 0.6;
/// Recovery (every probing arm): while acting is withheld in a context
/// where evidence could restore it, a **canary** probe runs at this rate.
/// Canaries are treated runs that never count towards re-earning.
pub const CANARY_RATE: f64 = 0.125;
/// Consecutive canary successes that start a burst.
pub const CANARY_STREAK: u32 = 3;
/// During a burst, refused opportunities are probed at this rate until one
/// fresh block of `MIN_RULE_N` treated outcomes is complete.
pub const BURST_RATE: f64 = 0.5;
/// The gate's burst probes are randomized with this propensity of
/// treatment; control probes feed the hypothesis, treated ones re-earn.
pub const PROBE_PROPENSITY: f64 = 0.9;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
pub const BOOTSTRAP_SEED: u64 = 13;

/// Existing capability drift rules (`docs/specifications/capabilities.md`).
const WINDOW: usize = 25;
const WINDOW_MIN: usize = 10;
const QUARANTINE_UPPER: f64 = 0.6;
const CONSECUTIVE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// No change.
    Stationary,
    /// Two contexts drop to 0.25 at the shift.
    Cliff,
    /// Two contexts fall linearly by 0.4 over [`DRIFT_SPAN`].
    Gradual,
    /// Two contexts drop to 0.25, then return to base at [`RETURN_AT`].
    Recovery,
    /// Two contexts start at 0.55 and rise to 0.9 at the shift.
    Upward,
    /// Every context drops to 0.4.
    Global,
}

impl Family {
    pub const ALL: [Self; 6] = [
        Self::Stationary,
        Self::Cliff,
        Self::Gradual,
        Self::Recovery,
        Self::Upward,
        Self::Global,
    ];

    /// Families whose shift makes acting worse somewhere.
    pub const fn harmful(self) -> bool {
        matches!(
            self,
            Self::Cliff | Self::Gradual | Self::Recovery | Self::Global
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmName {
    Oracle,
    Refuse,
    Global,
    PerContext,
    PerContextProbing,
    PerContextCusum,
    Gate,
}

impl ArmName {
    pub const ALL: [Self; 7] = [
        Self::Oracle,
        Self::Refuse,
        Self::Global,
        Self::PerContext,
        Self::PerContextProbing,
        Self::PerContextCusum,
        Self::Gate,
    ];
    /// What the gate must beat (G1) and not lose to (G2).
    pub const EXISTING: [Self; 4] = [
        Self::Refuse,
        Self::Global,
        Self::PerContext,
        Self::PerContextProbing,
    ];
}

struct World {
    family: Family,
    base: Vec<f64>,
    control: Vec<f64>,
}

impl World {
    fn shifted(&self, c: usize) -> bool {
        self.family == Family::Global || c < SHIFTED
    }

    /// P(success | treated, context, opportunity index).
    fn treated(&self, c: usize, t: usize) -> f64 {
        let b = self.base[c];
        if !self.shifted(c) {
            return b;
        }
        match self.family {
            Family::Stationary => b,
            Family::Cliff if t >= SHIFT_AT => 0.25,
            Family::Gradual => {
                let k = t.saturating_sub(SHIFT_AT).min(DRIFT_SPAN) as f64 / DRIFT_SPAN as f64;
                b - 0.4 * k
            }
            Family::Recovery if (SHIFT_AT..RETURN_AT).contains(&t) => 0.25,
            Family::Upward if t >= SHIFT_AT => 0.9,
            Family::Global if t >= SHIFT_AT => 0.4,
            _ => b,
        }
    }

    /// Acting has positive expected utility.
    fn worth_acting(&self, c: usize, t: usize) -> bool {
        let p = self.treated(c, t);
        p * SUCCESS + (1.0 - p) * FAILURE > REFUSAL
    }
}

fn world(family: Family, rng: &mut Rng) -> World {
    let mut base = Vec::with_capacity(CONTEXTS);
    let mut control = Vec::with_capacity(CONTEXTS);
    for c in 0..CONTEXTS {
        let mut b = 0.8 + 0.15 * uniform(rng);
        if family == Family::Upward && c < SHIFTED {
            b = 0.55;
        }
        base.push(b);
        control.push((b - (0.3 + 0.2 * uniform(rng))).max(0.0));
    }
    World {
        family,
        base,
        control,
    }
}

fn uniform(rng: &mut Rng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// What an arm did with one opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Success,
    Failure,
    Refusal,
}

impl Act {
    fn of(success: bool) -> Self {
        if success {
            Self::Success
        } else {
            Self::Failure
        }
    }

    fn utility(self) -> f64 {
        match self {
            Self::Success => SUCCESS,
            Self::Failure => FAILURE,
            Self::Refusal => REFUSAL,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tally {
    pub opportunities: u32,
    pub successes: u32,
    pub failures: u32,
    pub refusals: u32,
    /// Probes run (treated probes count as successes or failures; control
    /// probes as refusals).
    pub probes: u32,
    pub utility: f64,
    pub post_shift_utility: f64,
}

impl Tally {
    fn add(&mut self, act: Act, t: usize) {
        self.opportunities += 1;
        match act {
            Act::Success => self.successes += 1,
            Act::Failure => self.failures += 1,
            Act::Refusal => self.refusals += 1,
        }
        self.utility += act.utility();
        if t >= SHIFT_AT {
            self.post_shift_utility += act.utility();
        }
    }

    pub fn mean_utility(&self) -> f64 {
        self.utility / f64::from(self.opportunities.max(1))
    }
}

/// Fixed blocks of `MIN_RULE_N` treated outcomes; the latest complete one.
#[derive(Debug, Clone, Default)]
struct Blocks {
    current: Vec<bool>,
    latest: Option<u32>,
}

impl Blocks {
    fn push(&mut self, success: bool) {
        self.current.push(success);
        if self.current.len() == MIN_RULE_N as usize {
            self.latest = Some(self.current.iter().filter(|s| **s).count() as u32);
            self.current.clear();
        }
    }

    /// The latest complete block's Wilson lower bound.
    fn lower(&self) -> Option<f64> {
        self.latest
            .and_then(|k| wilson(f64::from(k), f64::from(MIN_RULE_N), Z).map(|w| w.0))
    }
}

/// What to do with one refused opportunity under [`Recovery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Nothing,
    Canary,
    Burst,
}

/// Canary-triggered recovery, identical in every probing arm. Canaries
/// watch cheaply for a sign of recovery; a burst then collects exactly one
/// fresh block, so a block never straddles canary evidence and is never cut
/// short by peeking.
#[derive(Debug, Clone, Copy, Default)]
struct Recovery {
    streak: u32,
    burst: bool,
    burst_treated: u32,
}

impl Recovery {
    fn next(&self, draw: f64) -> Probe {
        match (self.burst, draw) {
            (true, d) if d < BURST_RATE => Probe::Burst,
            (false, d) if d < CANARY_RATE => Probe::Canary,
            _ => Probe::Nothing,
        }
    }

    fn canary(&mut self, success: bool) {
        self.streak = if success { self.streak + 1 } else { 0 };
        if self.streak >= CANARY_STREAK {
            *self = Self {
                burst: true,
                ..Self::default()
            };
        }
    }

    /// One treated burst outcome; the burst ends with its block.
    fn burst_treated(&mut self) {
        self.burst_treated += 1;
        if self.burst_treated == MIN_RULE_N {
            *self = Self::default();
        }
    }
}

/// A context's state under the capability drift filter.
#[derive(Debug, Clone, Default)]
struct Drift {
    /// Decisive outcomes, oldest first.
    runs: Vec<bool>,
    quarantined: bool,
    probes: Blocks,
}

impl Drift {
    fn record(&mut self, success: bool, consecutive_rule: bool) {
        self.runs.push(success);
        let n = self.runs.len();
        let lead = self.runs.iter().rev().take_while(|s| !**s).count();
        let window = &self.runs[n.saturating_sub(WINDOW)..];
        let upper = wilson(
            window.iter().filter(|s| **s).count() as f64,
            window.len() as f64,
            1.96,
        )
        .map_or(1.0, |w| w.1);
        if (consecutive_rule && lead >= CONSECUTIVE)
            || (window.len() >= WINDOW_MIN && upper < QUARANTINE_UPPER)
        {
            self.quarantined = true;
            self.probes = Blocks::default();
        }
    }

    /// Re-earn from fixed blocks of treated probes, as the gate does.
    fn probe(&mut self, success: bool) {
        self.probes.push(success);
        if !self.probes.current.is_empty() {
            return;
        }
        if self.probes.lower().is_some_and(|l| l >= TAU) {
            self.quarantined = false;
            self.runs.clear();
        }
    }
}

/// The gate's detector, null and re-earning on raw executions, per context.
/// Like a rule: evidence accumulates in fixed blocks until a demotion;
/// after one, only the latest block counts.
#[derive(Debug, Clone)]
struct Cusum {
    n: u32,
    k: u32,
    detector: Detector,
    demoted: bool,
    ever_demoted: bool,
    probes: Blocks,
}

impl Cusum {
    fn new(k: u32, n: u32) -> Self {
        Self {
            n,
            k,
            detector: Detector::new(),
            demoted: false,
            ever_demoted: false,
            probes: Blocks::default(),
        }
    }

    fn lower(&self) -> f64 {
        wilson(f64::from(self.k), f64::from(self.n), Z).map_or(0.0, |w| w.0)
    }

    fn executes(&self) -> bool {
        !self.demoted && self.lower() >= TAU
    }

    fn record(&mut self, success: bool) {
        if self.detector.update(self.lower().max(NULL_FLOOR), success) >= 1.0 / ALPHA {
            self.demoted = true;
            self.ever_demoted = true;
            self.probes = Blocks::default();
            self.detector = Detector::new();
        }
    }

    fn probing(&self) -> bool {
        !self.executes()
    }

    fn probe(&mut self, success: bool) {
        self.probes.push(success);
        if !self.probes.current.is_empty() {
            return;
        }
        let kb = self.probes.latest.unwrap_or(0);
        if self.ever_demoted {
            (self.n, self.k) = (MIN_RULE_N, kb);
        } else {
            self.n += MIN_RULE_N;
            self.k += kb;
        }
        self.demoted = false;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReport {
    pub context: usize,
    pub shifted: bool,
    /// Opportunities in this context from the shift to the gate's first
    /// demotion after it (`None`: not demoted by the end).
    pub detection_delay: Option<u32>,
    /// Demotions while the context's rate was unchanged (false alarms).
    pub false_demotions: u32,
    pub demotions: u32,
    /// The gate acted on a requalified rule after the return (`recovery`).
    pub requalified_after_return: bool,
    /// The gate's last decision in this context before the shift was to
    /// act: only then is there anything for it to detect.
    #[serde(default)]
    pub exposed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRow {
    pub seed: u64,
    pub family: Family,
    pub run: usize,
    pub supported: bool,
    pub arms: BTreeMap<ArmName, Tally>,
    pub contexts: Vec<ContextReport>,
}

/// The generator seed of one run. Seed, run and family occupy disjoint bits,
/// so no two (seed, family, run) triples share a generator. (An earlier
/// version XORed seed and run into the same low bits, so seeds 1–3 replayed
/// the same eight runs per family.)
fn run_seed_value(seed: u64, family: Family, run: usize) -> u64 {
    0x0013_0013_0013_0013 ^ (seed & 0xFFFF) ^ ((run as u64 & 0xFF) << 16) ^ ((family as u64) << 24)
}

pub async fn run_seed(store: Arc<dyn CognitiveStore>, seed: u64) -> Result<Vec<RunRow>, String> {
    let mut rows = Vec::new();
    for family in Family::ALL {
        for run in 0..PER_FAMILY {
            rows.push(run_one(store.clone(), seed, family, run).await?);
        }
    }
    Ok(rows)
}

fn tally(arms: &mut BTreeMap<ArmName, Tally>, arm: ArmName) -> &mut Tally {
    arms.get_mut(&arm).expect("every arm has a tally")
}

#[allow(clippy::too_many_lines)]
async fn run_one(
    store: Arc<dyn CognitiveStore>,
    seed: u64,
    family: Family,
    run: usize,
) -> Result<RunRow, String> {
    let err = |e: cs_core::Error| e.to_string();
    let mut rng = Rng::new(run_seed_value(seed, family, run));
    let w = world(family, &mut rng);
    let clock = Arc::new(FixedClock::new(crate::corpus::start()));
    let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
    let tenant = TenantId::new_v7();
    let user = UserId::new_v7();
    let ctx = AccessContext::user(tenant, user);
    let scope = Scope::private(tenant, user);
    let environment = format!("exp013-{seed}-{family:?}-{run}");
    let new = |context: usize, arm: Arm, assignment: Assignment, propensity: f64| {
        let randomized = assignment == Assignment::Randomized;
        NewIntervention {
            scope: scope.clone(),
            environment: environment.clone(),
            treatment: "act".into(),
            arm,
            assignment,
            assignment_detail: randomized.then(|| "draw".into()),
            propensity: randomized.then_some(propensity),
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

    // Shared randomized burn-in.
    let mut global = Drift::default();
    let mut per_context = vec![Drift::default(); CONTEXTS];
    let mut probing = vec![Drift::default(); CONTEXTS];
    let mut burn_in = [0u32; CONTEXTS];
    for _ in 0..BURN_IN_PER_ARM {
        for c in 0..CONTEXTS {
            for arm in [Arm::Treated, Arm::Control] {
                let p = if arm == Arm::Treated {
                    w.base[c]
                } else {
                    w.control[c]
                };
                let value = uniform(&mut rng) < p;
                clock.advance(Duration::minutes(1));
                let i = engine
                    .record_intervention(&ctx, new(c, arm, Assignment::Randomized, 0.5))
                    .await
                    .map_err(err)?;
                engine
                    .observe_outcome(&ctx, i.id, observe(value))
                    .await
                    .map_err(err)?;
                if arm == Arm::Treated {
                    // The baselines' field history. Burn-in quarantines are
                    // part of the existing rules.
                    global.record(value, true);
                    per_context[c].record(value, true);
                    probing[c].record(value, false);
                    burn_in[c] += u32::from(value);
                }
            }
        }
    }
    let n_burn = BURN_IN_PER_ARM as u32;
    let mut cusum: Vec<Cusum> = burn_in.iter().map(|&k| Cusum::new(k, n_burn)).collect();
    let hyp = engine
        .create_hypothesis(
            &ctx,
            NewHypothesis {
                scope: scope.clone(),
                environment: environment.clone(),
                treatment: "act".into(),
                outcome: "success".into(),
                direction: Direction::Raises,
                min_effect: 0.1,
                contexts: Vec::new(),
                statement: None,
            },
        )
        .await
        .map_err(err)?;
    let hyp = engine
        .evaluate_hypothesis(&ctx, hyp.id, 1)
        .await
        .map_err(err)?;
    let supported = hyp.status == HypothesisStatus::Supported;
    let hyp_id: HypothesisId = hyp.id;

    let mut arms: BTreeMap<ArmName, Tally> = ArmName::ALL
        .into_iter()
        .map(|a| (a, Tally::default()))
        .collect();
    let mut demotions_seen = [0usize; CONTEXTS];
    let mut since_shift = [0u32; CONTEXTS];
    let mut reports: Vec<ContextReport> = (0..CONTEXTS)
        .map(|c| ContextReport {
            context: c,
            shifted: w.shifted(c) && family != Family::Stationary,
            detection_delay: None,
            false_demotions: 0,
            demotions: 0,
            requalified_after_return: false,
            exposed: false,
        })
        .collect();
    // Recovery schedulers per context, one per probing arm.
    let mut probing_recovery = [Recovery::default(); CONTEXTS];
    let mut cusum_recovery = [Recovery::default(); CONTEXTS];
    let mut gate_recovery = [Recovery::default(); CONTEXTS];

    for t in 0..OPPORTUNITIES {
        let c = rng.below(CONTEXTS);
        let u = uniform(&mut rng);
        let probe_draw = uniform(&mut rng);
        let arm_draw = uniform(&mut rng);
        let success = u < w.treated(c, t);
        if t >= SHIFT_AT {
            since_shift[c] += 1;
        }

        let oracle = if w.worth_acting(c, t) {
            Act::of(success)
        } else {
            Act::Refusal
        };
        tally(&mut arms, ArmName::Oracle).add(oracle, t);
        tally(&mut arms, ArmName::Refuse).add(Act::Refusal, t);

        let a = if global.quarantined {
            Act::Refusal
        } else {
            global.record(success, true);
            Act::of(success)
        };
        tally(&mut arms, ArmName::Global).add(a, t);

        let a = if per_context[c].quarantined {
            Act::Refusal
        } else {
            per_context[c].record(success, true);
            Act::of(success)
        };
        tally(&mut arms, ArmName::PerContext).add(a, t);

        let tl = tally(&mut arms, ArmName::PerContextProbing);
        let a = if !probing[c].quarantined {
            probing_recovery[c] = Recovery::default();
            probing[c].record(success, false);
            Act::of(success)
        } else {
            match probing_recovery[c].next(probe_draw) {
                Probe::Nothing => Act::Refusal,
                Probe::Canary => {
                    tl.probes += 1;
                    probing_recovery[c].canary(success);
                    Act::of(success)
                }
                Probe::Burst => {
                    tl.probes += 1;
                    probing_recovery[c].burst_treated();
                    probing[c].probe(success);
                    Act::of(success)
                }
            }
        };
        tl.add(a, t);

        let tl = tally(&mut arms, ArmName::PerContextCusum);
        let a = if cusum[c].executes() {
            cusum_recovery[c] = Recovery::default();
            cusum[c].record(success);
            Act::of(success)
        } else if cusum[c].probing() {
            match cusum_recovery[c].next(probe_draw) {
                Probe::Nothing => Act::Refusal,
                Probe::Canary => {
                    tl.probes += 1;
                    cusum_recovery[c].canary(success);
                    Act::of(success)
                }
                Probe::Burst => {
                    tl.probes += 1;
                    cusum_recovery[c].burst_treated();
                    cusum[c].probe(success);
                    Act::of(success)
                }
            }
        } else {
            Act::Refusal
        };
        tl.add(a, t);

        // The prediction gate, through the engine.
        let view = engine
            .prediction_rule(&ctx, hyp_id, &format!("c{c}"), Some(TAU), None)
            .await
            .map_err(err)?;
        let decision = view.gate.expect("threshold given");
        // Probe where evidence could restore acting: a demoted lineage, or
        // a rule below the threshold (which accumulates new blocks).
        let re_earning = match &view.result {
            RuleResult::Refused {
                reason: RuleRefusal::Demoted,
            } => true,
            RuleResult::Rule { .. } => !decision.execute,
            RuleResult::Refused { .. } => false,
        };
        if t < SHIFT_AT {
            reports[c].exposed = decision.execute;
        }
        if decision.execute {
            gate_recovery[c] = Recovery::default();
        }
        let probe = if re_earning {
            gate_recovery[c].next(probe_draw)
        } else {
            Probe::Nothing
        };
        clock.advance(Duration::minutes(1));
        let a = if decision.execute {
            let i = engine
                .record_intervention(&ctx, new(c, Arm::Treated, Assignment::Passive, 0.0))
                .await
                .map_err(err)?;
            if let PredictionResult::Refused { reason } =
                engine.predict(&ctx, i.id, hyp_id).await.map_err(err)?
            {
                return Err(format!("gate cleared but prediction refused: {reason:?}"));
            }
            clock.advance(Duration::minutes(1));
            engine
                .observe_outcome(&ctx, i.id, observe(success))
                .await
                .map_err(err)?;
            Act::of(success)
        } else if probe == Probe::Canary {
            // A passive treated run: recorded, never predicted, never
            // counted towards re-earning (only randomized evidence is).
            tally(&mut arms, ArmName::Gate).probes += 1;
            let i = engine
                .record_intervention(&ctx, new(c, Arm::Treated, Assignment::Passive, 0.0))
                .await
                .map_err(err)?;
            clock.advance(Duration::minutes(1));
            engine
                .observe_outcome(&ctx, i.id, observe(success))
                .await
                .map_err(err)?;
            gate_recovery[c].canary(success);
            Act::of(success)
        } else if probe == Probe::Burst {
            tally(&mut arms, ArmName::Gate).probes += 1;
            let arm = if arm_draw < PROBE_PROPENSITY {
                Arm::Treated
            } else {
                Arm::Control
            };
            if arm == Arm::Treated {
                gate_recovery[c].burst_treated();
            }
            let i = engine
                .record_intervention(&ctx, new(c, arm, Assignment::Randomized, PROBE_PROPENSITY))
                .await
                .map_err(err)?;
            let value = if arm == Arm::Treated {
                success
            } else {
                u < w.control[c]
            };
            clock.advance(Duration::minutes(1));
            engine
                .observe_outcome(&ctx, i.id, observe(value))
                .await
                .map_err(err)?;
            if arm == Arm::Treated {
                Act::of(success)
            } else {
                Act::Refusal
            }
        } else {
            Act::Refusal
        };
        tally(&mut arms, ArmName::Gate).add(a, t);
        if let RuleResult::Rule { rule } = &view.result
            && rule.requalified
            && decision.execute
            && family == Family::Recovery
            && t >= RETURN_AT
        {
            reports[c].requalified_after_return = true;
        }

        // Demotions are computed on read; count the new ones.
        let n = view.monitor.demotions.len();
        if n > demotions_seen[c] {
            let r = &mut reports[c];
            let new_ones = (n - demotions_seen[c]) as u32;
            r.demotions += new_ones;
            let harmful_now = r.shifted
                && family != Family::Upward
                && t >= SHIFT_AT
                && !(family == Family::Recovery && t >= RETURN_AT);
            if harmful_now {
                if r.detection_delay.is_none() {
                    r.detection_delay = Some(since_shift[c]);
                }
            } else {
                r.false_demotions += new_ones;
            }
            demotions_seen[c] = n;
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
    /// Counts towards adoption. The superseded G1 is reported, not binding.
    pub binding: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FamilySummary {
    pub family: Family,
    pub runs: usize,
    pub supported: usize,
    /// Mean utility per opportunity, by arm.
    pub utility: BTreeMap<ArmName, f64>,
    /// Gate minus each other arm, paired by run, Bonferroni-adjusted CI.
    pub gate_minus: BTreeMap<ArmName, Interval>,
    pub runs_with_false_demotion: usize,
    /// Wilson 95% upper bound of the same rate.
    pub false_demotion_upper: f64,
    pub shifted_contexts: usize,
    pub detected: usize,
    /// Over all shifted contexts, undetected ones at +∞.
    pub median_delay: Option<f64>,
    pub p90_delay: Option<f64>,
    pub requalified: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub families: Vec<FamilySummary>,
    pub gates: Vec<Gate>,
    pub adopt: bool,
}

/// An existing arm is admissible in G1 when its stationary utility is
/// within this margin of the oracle's: it does not buy harm containment by
/// giving up healthy work. The same margin as G2 and G6.
pub const ADMISSIBLE_MARGIN: f64 = 0.02;
/// G4's median delay targets. Cliff (to 0.25): 20. Global (to 0.40):
/// Wald's approximation ln(1/α) / KL(0.40 ‖ 0.70) ≈ 36 opportunities for a
/// typical null of 0.70, rounded up to 40.
pub const G4_CLIFF_MEDIAN: f64 = 20.0;
pub const G4_GLOBAL_MEDIAN: f64 = 40.0;

/// Simultaneous comparisons: G1 (4 harmful families × 4 existing arms),
/// G2 (4 existing arms) and G6 (6 families × the CUSUM baseline).
const G1_COMPARISONS: f64 = 16.0;
const G2_COMPARISONS: f64 = 4.0;
const G6_COMPARISONS: f64 = 6.0;

fn delay_quantile(delays: &[f64], shifted: usize, q: f64) -> Option<f64> {
    if shifted == 0 {
        return None;
    }
    let mut all = delays.to_vec();
    all.extend(std::iter::repeat_n(f64::INFINITY, shifted - delays.len()));
    let v = percentile(&all, q);
    v.is_finite().then_some(v)
}

pub fn summarize(rows: &[RunRow]) -> Summary {
    let mut families = Vec::new();
    for family in Family::ALL {
        let rs: Vec<&RunRow> = rows.iter().filter(|r| r.family == family).collect();
        let per_run =
            |a: ArmName| -> Vec<f64> { rs.iter().map(|r| r.arms[&a].mean_utility()).collect() };
        let utility: BTreeMap<ArmName, f64> = ArmName::ALL
            .into_iter()
            .map(|a| (a, mean(&per_run(a))))
            .collect();
        let gate = per_run(ArmName::Gate);
        let alpha = |a: ArmName| -> f64 {
            if a == ArmName::PerContextCusum {
                0.05 / G6_COMPARISONS
            } else if family.harmful() {
                0.05 / G1_COMPARISONS
            } else {
                0.05 / G2_COMPARISONS
            }
        };
        let gate_minus: BTreeMap<ArmName, Interval> = ArmName::ALL
            .into_iter()
            .filter(|a| *a != ArmName::Gate)
            .map(|a| {
                (
                    a,
                    paired_bootstrap_at(
                        &per_run(a),
                        &gate,
                        BOOTSTRAP_RESAMPLES,
                        BOOTSTRAP_SEED,
                        alpha(a),
                    ),
                )
            })
            .collect();
        let shifted: Vec<&ContextReport> = rs
            .iter()
            .flat_map(|r| r.contexts.iter())
            .filter(|c| c.shifted && c.exposed)
            .collect();
        let delays: Vec<f64> = shifted
            .iter()
            .filter_map(|c| c.detection_delay.map(f64::from))
            .collect();
        let false_runs = rs
            .iter()
            .filter(|r| r.contexts.iter().any(|c| c.false_demotions > 0))
            .count();
        families.push(FamilySummary {
            family,
            runs: rs.len(),
            supported: rs.iter().filter(|r| r.supported).count(),
            utility,
            gate_minus,
            runs_with_false_demotion: false_runs,
            false_demotion_upper: wilson(false_runs as f64, rs.len() as f64, 1.96)
                .map_or(1.0, |w| w.1),
            shifted_contexts: shifted.len(),
            detected: delays.len(),
            median_delay: delay_quantile(&delays, shifted.len(), 50.0),
            p90_delay: delay_quantile(&delays, shifted.len(), 90.0),
            requalified: shifted
                .iter()
                .filter(|c| c.requalified_after_return)
                .count(),
        });
    }
    let fam = |f: Family| {
        families
            .iter()
            .find(|s| s.family == f)
            .expect("every family is summarized")
    };
    let stationary = fam(Family::Stationary);
    let g1: Vec<String> = Family::ALL
        .into_iter()
        .filter(|f| f.harmful())
        .flat_map(|f| {
            ArmName::EXISTING.into_iter().filter_map(move |a| {
                let i = fam(f).gate_minus[&a];
                (i.lower <= 0.0).then(|| format!("{f:?} vs {a:?}: {i:?}"))
            })
        })
        .collect();
    // Amended G1: non-inferiority to every admissible existing arm.
    let oracle_stationary = stationary.utility[&ArmName::Oracle];
    let admissible: Vec<ArmName> = ArmName::EXISTING
        .into_iter()
        .filter(|a| stationary.utility[a] >= oracle_stationary - ADMISSIBLE_MARGIN)
        .collect();
    let g1_adoption: Vec<String> = Family::ALL
        .into_iter()
        .filter(|f| f.harmful())
        .flat_map(|f| {
            admissible.iter().filter_map(move |a| {
                let i = fam(f).gate_minus[a];
                (i.lower <= -ADMISSIBLE_MARGIN).then(|| format!("{f:?} vs {a:?}: {i:?}"))
            })
        })
        .collect();
    let g2: Vec<String> = ArmName::EXISTING
        .into_iter()
        .filter_map(|a| {
            let i = stationary.gate_minus[&a];
            (i.lower <= -0.02).then(|| format!("vs {a:?}: {i:?}"))
        })
        .collect();
    let g6: Vec<String> = Family::ALL
        .into_iter()
        .filter_map(|f| {
            let i = fam(f).gate_minus[&ArmName::PerContextCusum];
            (i.lower <= -0.02).then(|| format!("{f:?}: {i:?}"))
        })
        .collect();
    let (cliff, global) = (fam(Family::Cliff), fam(Family::Global));
    let recovery = fam(Family::Recovery);
    let gates = vec![
        Gate {
            criterion: "G1 (superseded, reported) every harmful family: gate beats every existing arm",
            value: if g1.is_empty() {
                "all 16 comparisons".into()
            } else {
                g1.join("; ")
            },
            passed: g1.is_empty(),
            binding: false,
        },
        Gate {
            criterion: "G1 every harmful family: gate non-inferior (CI lower > −0.02) to every admissible existing arm",
            value: format!(
                "admissible {admissible:?}; {}",
                if g1_adoption.is_empty() {
                    "no failing comparison".into()
                } else {
                    g1_adoption.join("; ")
                }
            ),
            passed: g1_adoption.is_empty(),
            binding: true,
        },
        Gate {
            criterion: "G2 stationary: gate non-inferior to every existing arm (CI lower > −0.02)",
            value: if g2.is_empty() {
                "all 4 comparisons".into()
            } else {
                g2.join("; ")
            },
            passed: g2.is_empty(),
            binding: true,
        },
        Gate {
            criterion: "G3 stationary: runs with any false demotion ≤ 5%",
            value: format!(
                "{}/{} (Wilson upper {:.3})",
                stationary.runs_with_false_demotion,
                stationary.runs,
                stationary.false_demotion_upper
            ),
            passed: (stationary.runs_with_false_demotion as f64) <= 0.05 * stationary.runs as f64,
            binding: true,
        },
        Gate {
            criterion: "G4 cliff and global: ≥ 95% of exposed shifted contexts demoted; median delay ≤ 20 (cliff), ≤ 40 (global)",
            value: format!(
                "cliff {}/{} median {:?} p90 {:?}; global {}/{} median {:?} p90 {:?}",
                cliff.detected,
                cliff.shifted_contexts,
                cliff.median_delay,
                cliff.p90_delay,
                global.detected,
                global.shifted_contexts,
                global.median_delay,
                global.p90_delay
            ),
            passed: (cliff.detected + global.detected) as f64
                >= 0.95 * (cliff.shifted_contexts + global.shifted_contexts) as f64
                && cliff.median_delay.is_some_and(|m| m <= G4_CLIFF_MEDIAN)
                && global.median_delay.is_some_and(|m| m <= G4_GLOBAL_MEDIAN),
            binding: true,
        },
        Gate {
            criterion: "G5 recovery: ≥ 80% of returned contexts acted on again from a re-earned rule",
            value: format!("{}/{}", recovery.requalified, recovery.shifted_contexts),
            passed: recovery.requalified as f64 >= 0.8 * recovery.shifted_contexts as f64,
            binding: true,
        },
        Gate {
            criterion: "G6 every family: gate non-inferior to the per-context CUSUM (CI lower > −0.02)",
            value: if g6.is_empty() {
                "all 6 families".into()
            } else {
                g6.join("; ")
            },
            passed: g6.is_empty(),
            binding: true,
        },
    ];
    let adopt = gates.iter().filter(|g| g.binding).all(|g| g.passed);
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
