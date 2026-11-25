//! Bounded predictions (M6.4, version 1): rules, a sequential monitor that
//! demotes them, and a gate that fails closed.
//!
//! * **A rule needs more than causal support.** A supported hypothesis is a
//!   claim about an average effect over its scope; a rule predicts how often
//!   the favourable outcome follows treatment in **one** context. It is
//!   minted only where that context's own effect estimate points the
//!   hypothesis's way and its treated arm has at least [`MIN_RULE_N`]
//!   outcomes, so a rule is never a two-outcome guess.
//! * **Monitoring is anytime-valid and one-sided.** Every resolved
//!   prediction multiplies a likelihood ratio into a mixture of CUSUM
//!   e-detectors testing "the live favourable rate is at least the rule's
//!   frozen null". Checking after every outcome is allowed: under the null
//!   the expected time to a false alarm is at least `1/α` resolved
//!   predictions. Only over-optimism demotes; a rule that does better than
//!   predicted is not penalised.
//! * **Silence is failure.** A prediction with no strong-tier outcome by its
//!   deadline resolves as unfavourable, so withholding bad outcomes cannot
//!   protect a rule.
//! * **Demotion is attached to the lineage** (hypothesis × context), not to
//!   a hypothesis version: re-evaluating a hypothesis does not clear it. A
//!   demoted lineage serves no rule until it is re-earned from randomized
//!   evidence recorded **after** the demotion.
//! * **The gate fails closed.** No rule, a demoted rule, or a lower bound
//!   below the threshold all refuse.
//! * **An effect gate acts on what acting changes.** With `min_effect`, the
//!   gate also needs the Newcombe lower bound of the treated-minus-control
//!   favourable rate in the context to reach it: an action whose outcome
//!   happens anyway is refused however often it "succeeds". Control
//!   holdouts are predicted and monitored too, so a control rate that rises
//!   (the action becoming redundant) demotes the lineage like a treated rate
//!   that falls.

use cs_memory::{
    causal::{Arm, Assignment, Direction, Hypothesis, HypothesisStatus, Intervention},
    prediction::{Prediction, Resolution},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::capability::wilson;

pub use cs_memory::prediction::PREDICTION_VERSION;

/// Two-sided 95% for the rule's interval.
pub const Z: f64 = 1.96;
/// Treated outcomes a context needs before it can mint a rule.
pub const MIN_RULE_N: u32 = 20;
/// Floor for the monitored null.
pub const NULL_FLOOR: f64 = 0.02;
/// Alternatives (drops below the null) mixed in the e-detector.
pub const ALTERNATIVES: [f64; 3] = [0.1, 0.2, 0.3];
/// False-alarm level: under the null, the expected number of resolved
/// predictions before a false demotion is at least `1/ALPHA`.
pub const ALPHA: f64 = 0.001;
/// Seconds after a prediction is recorded within which a strong-tier
/// outcome must arrive.
pub const OUTCOME_DEADLINE_SECONDS: i64 = 3_600;

/// Why no rule is available for a hypothesis in a context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleRefusal {
    /// The hypothesis is not `supported`.
    NotSupported,
    /// The intervention concerns another environment or treatment, or
    /// another scope.
    Unrelated,
    /// The context is not among the evaluation's `scope_contexts`.
    OutOfScope,
    /// The context's own effect estimate does not point the hypothesis's
    /// way, although the pooled effect does.
    LocallyUnsupported,
    /// Fewer than [`MIN_RULE_N`] outcomes of the arm in the context (after
    /// a demotion: recorded since the demotion).
    Imprecise,
    /// The lineage was demoted and has not been re-earned.
    Demoted,
}

/// One arm's favourable rate in a context.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArmEstimate {
    pub n: u32,
    pub favourable: u32,
    /// Jeffreys estimate `(k + ½) / (n + 1)`.
    pub estimate: f64,
    pub lower: f64,
    pub upper: f64,
}

impl ArmEstimate {
    fn new(n: u32, k: u32) -> Self {
        let (lower, upper) = wilson(f64::from(k), f64::from(n), Z).unwrap_or((0.0, 1.0));
        Self {
            n,
            favourable: k,
            estimate: (f64::from(k) + 0.5) / (f64::from(n) + 1.0),
            lower,
            upper,
        }
    }

    /// The control arm's monitored null: its upper bound, capped.
    pub fn control_null(&self) -> f64 {
        self.upper.min(1.0 - NULL_FLOOR)
    }
}

/// The treated-minus-control favourable rate in a context, with Newcombe's
/// 95% interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EffectBound {
    pub estimate: f64,
    pub lower: f64,
    pub upper: f64,
}

/// A rule, computed on read.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// The outcome value that counts as favourable.
    pub favourable: bool,
    /// Treated outcomes and how many were favourable.
    pub treated_n: u32,
    pub treated_favourable: u32,
    /// Jeffreys estimate `(k + ½) / (n + 1)`.
    pub estimate: f64,
    pub lower: f64,
    pub upper: f64,
    /// `max(lower, NULL_FLOOR)`: the live rate is monitored against the
    /// rule's own lower bound, with no further slack.
    pub null: f64,
    /// Computed from evidence recorded after the lineage's last demotion.
    pub requalified: bool,
    /// The control arm, when it has at least [`MIN_RULE_N`] outcomes
    /// (accumulated, or re-earned, by the same rules as the treated arm).
    #[serde(default)]
    pub control: Option<ArmEstimate>,
    /// Treated minus control, when the control arm is present.
    #[serde(default)]
    pub effect: Option<EffectBound>,
}

pub const fn favourable_value(direction: Direction) -> bool {
    matches!(direction, Direction::Raises)
}

fn rule_from_counts(
    favourable: bool,
    (n, k): (u32, u32),
    control: Option<(u32, u32)>,
    requalified: bool,
) -> Rule {
    let t = ArmEstimate::new(n, k);
    let control = control
        .filter(|&(n0, _)| n0 >= MIN_RULE_N)
        .map(|(n0, k0)| ArmEstimate::new(n0, k0));
    let effect = control.and_then(|c| {
        crate::causal::difference_interval(k, n, c.favourable, c.n).map(|(d, l, u)| EffectBound {
            estimate: d,
            lower: l,
            upper: u,
        })
    });
    Rule {
        favourable,
        treated_n: n,
        treated_favourable: k,
        estimate: t.estimate,
        lower: t.lower,
        upper: t.upper,
        null: t.lower.max(NULL_FLOOR),
        requalified,
        control,
        effect,
    }
}

/// The lineage a rule belongs to: what is claimed, where, not which
/// hypothesis record claims it. Two hypotheses stating the same thing in
/// the same scope share demotions, so a new hypothesis id cannot launder a
/// bad record.
pub fn lineage(h: &Hypothesis, context: &str) -> String {
    let key = serde_json::json!([
        h.scope,
        h.environment,
        h.treatment,
        h.outcome,
        h.direction,
        context
    ]);
    let digest = Sha256::digest(key.to_string().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The outcome of a randomized treated intervention under the monitor's own
/// semantics: the strong-tier outcome if it arrived within the deadline of
/// the intervention's intent, unfavourable once the deadline has passed
/// without one, `None` while pending.
fn deadline_outcome(
    i: &Intervention,
    outcome_key: &str,
    favourable: bool,
    now: OffsetDateTime,
) -> Option<bool> {
    let deadline = i.recorded_at + Duration::seconds(OUTCOME_DEADLINE_SECONDS);
    let observed = i
        .outcomes
        .get(outcome_key)
        .filter(|o| o.tier.is_strong() && o.observed_at <= deadline && o.observed_at <= now);
    match observed {
        Some(o) => Some(o.value == favourable),
        None => (now >= deadline).then_some(false),
    }
}

/// The rule for `context` under `h`, or why there is none.
///
/// `demoted_at` is the lineage's last demotion, if any. Then the rule is
/// re-earned from **fixed blocks** of [`MIN_RULE_N`] randomized treated
/// interventions recorded after it, in intent order: the latest complete
/// block decides, so no rule appears by stopping when the evidence happens
/// to look good. Each block outcome follows the monitor's semantics (a miss
/// past the deadline is unfavourable). Otherwise the counts come from the
/// hypothesis's evaluation.
pub fn rule(
    h: &Hypothesis,
    context: &str,
    interventions: &[Intervention],
    demoted_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<Rule, RuleRefusal> {
    if h.status != HypothesisStatus::Supported {
        return Err(RuleRefusal::NotSupported);
    }
    let Some(e) = h.evaluation.as_ref() else {
        return Err(RuleRefusal::NotSupported);
    };
    if !e.scope_contexts.iter().any(|c| c == context) {
        return Err(RuleRefusal::OutOfScope);
    }
    let Some(local) = e.contexts.iter().find(|c| c.context == context) else {
        return Err(RuleRefusal::OutOfScope);
    };
    if !local.effect.is_some_and(|d| d > 0.0) {
        return Err(RuleRefusal::LocallyUnsupported);
    }
    let favourable = favourable_value(h.direction);
    // Complete blocks of fresh randomized outcomes of one arm recorded after
    // `since`, in intent order; each block's favourable count.
    let blocks = |since: OffsetDateTime, arm: Arm| -> Vec<u32> {
        let mut fresh: Vec<&Intervention> = interventions
            .iter()
            .filter(|i| {
                i.recorded_at > since
                    && i.assignment == Assignment::Randomized
                    && i.arm == arm
                    && i.context == context
                    && i.scope == h.scope
                    && h.concerns(i)
            })
            .collect();
        fresh.sort_by_key(|i| (i.recorded_at, i.id));
        let mut out = Vec::new();
        for chunk in fresh.chunks_exact(MIN_RULE_N as usize) {
            let outcomes: Option<Vec<bool>> = chunk
                .iter()
                .map(|i| deadline_outcome(i, &h.outcome, favourable, now))
                .collect();
            match outcomes {
                Some(o) => out.push(o.iter().filter(|v| **v).count() as u32),
                None => break,
            }
        }
        out
    };
    let counts = |arm: Arm| -> (u32, u32) {
        match demoted_at {
            // Never demoted: the evaluation's counts, plus complete blocks of
            // randomized evidence recorded since that evaluation (no regime
            // change has been detected, so the evidence accumulates).
            None => {
                let c = if arm == Arm::Treated {
                    local.treated
                } else {
                    local.control
                };
                let k0 = if favourable {
                    c.positive
                } else {
                    c.n - c.positive
                };
                let more = blocks(e.as_of, arm);
                (
                    c.n + MIN_RULE_N * more.len() as u32,
                    k0 + more.iter().sum::<u32>(),
                )
            }
            // Demoted: only the latest complete block after the demotion,
            // so evidence from the degraded regime cannot dilute a recovery.
            Some(at) => blocks(at, arm).last().map_or((0, 0), |&k| (MIN_RULE_N, k)),
        }
    };
    let (n, k) = counts(Arm::Treated);
    let requalified = demoted_at.is_some();
    if n < MIN_RULE_N {
        return Err(if demoted_at.is_some() {
            RuleRefusal::Demoted
        } else {
            RuleRefusal::Imprecise
        });
    }
    Ok(rule_from_counts(
        favourable,
        (n, k),
        Some(counts(Arm::Control)),
        requalified,
    ))
}

/// Can `h`'s rules be applied to intervention `i` at all? Checked before
/// [`rule`] when a prediction is requested.
pub fn applicable(h: &Hypothesis, i: &Intervention) -> Result<(), RuleRefusal> {
    if h.status != HypothesisStatus::Supported {
        return Err(RuleRefusal::NotSupported);
    }
    if i.scope != h.scope || !h.concerns(i) {
        return Err(RuleRefusal::Unrelated);
    }
    Ok(())
}

/// How `p` resolved as of `now`, and when: the strong-tier outcome if it
/// arrived by the deadline, otherwise a miss at the deadline once it has
/// passed. `None` while pending.
pub fn resolve(
    p: &Prediction,
    outcome_key: &str,
    intervention: Option<&Intervention>,
    now: OffsetDateTime,
) -> Option<(Resolution, OffsetDateTime)> {
    let observed = intervention
        .and_then(|i| i.outcomes.get(outcome_key))
        .filter(|o| o.tier.is_strong() && o.observed_at <= p.deadline && o.observed_at <= now);
    if let Some(o) = observed {
        return Some((
            Resolution::Observed {
                favourable: o.value == p.favourable,
            },
            o.observed_at,
        ));
    }
    (now >= p.deadline).then_some((Resolution::Missed, p.deadline))
}

/// One demotion of a lineage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Demotion {
    /// When the alarm became knowable: the latest resolution time in the
    /// resolved prefix that crossed the threshold.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub prediction_id: cs_core::PredictionId,
    /// The arm whose detector crossed.
    pub arm: Arm,
    pub e_value: f64,
    /// Resolved predictions in the epoch that ended.
    pub resolved: u32,
}

/// The current epoch of a lineage's monitor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Epoch {
    pub resolved: u32,
    pub favourable: u32,
    /// Resolved as misses (no strong outcome by the deadline).
    pub missed: u32,
    /// Not yet folded: pending, or resolved but behind a pending one.
    pub pending: u32,
    /// Mixture e-detector value over treated predictions; demotion at
    /// `1/ALPHA`.
    pub e_value: f64,
    /// The same over control (holdout) predictions, testing that the
    /// control's favourable rate is at most each prediction's null.
    #[serde(default)]
    pub control_e_value: f64,
    /// Mean Brier score of the resolved predictions.
    pub brier: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub threshold: f64,
    pub demotions: Vec<Demotion>,
    pub current: Epoch,
}

impl Monitor {
    pub fn demoted_at(&self) -> Option<OffsetDateTime> {
        self.demotions.last().map(|d| d.at)
    }
}

/// One CUSUM e-detector per alternative; their average is the mixture.
#[derive(Debug, Clone, Copy)]
pub struct Detector {
    m: [f64; ALTERNATIVES.len()],
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub const fn new() -> Self {
        Self {
            m: [0.0; ALTERNATIVES.len()],
        }
    }

    /// Fold one resolved prediction with null `p0`.
    pub fn update(&mut self, p0: f64, favourable: bool) -> f64 {
        for (m, delta) in self.m.iter_mut().zip(ALTERNATIVES) {
            let p1 = (p0 - delta).max(p0 / 4.0);
            let lr = if favourable {
                p1 / p0
            } else {
                (1.0 - p1) / (1.0 - p0)
            };
            *m = m.max(1.0) * lr;
        }
        self.value()
    }

    pub fn value(&self) -> f64 {
        self.m.iter().sum::<f64>() / ALTERNATIVES.len() as f64
    }
}

/// Run the monitor over one lineage's predictions.
///
/// `resolved` pairs each prediction with its resolution as of the read
/// (see [`resolve`]); order does not matter. Predictions are folded in
/// **intent order** (`recorded_at`, then id), up to the first one still
/// pending: folding in resolution order would let outcome-dependent latency
/// (failures reported faster than successes) bias the detector. A demotion
/// is dated when it became knowable, the latest resolution time in the
/// folded prefix. An epoch only counts predictions recorded after the
/// previous demotion; older ones belonged to the rule that was demoted.
pub fn monitor(resolved: &[(&Prediction, Option<(Resolution, OffsetDateTime)>)]) -> Monitor {
    let threshold = 1.0 / ALPHA;
    let mut order: Vec<_> = resolved.to_vec();
    order.sort_by_key(|(p, _)| (p.recorded_at, p.id));

    let mut demotions = Vec::new();
    let mut epoch_start: Option<OffsetDateTime> = None;
    let mut known: Option<OffsetDateTime> = None;
    let mut detector = Detector::new();
    let mut control = Detector::new();
    let mut epoch = Epoch::default();
    let mut brier_sum = 0.0;
    let mut blocked = false;
    for (p, r) in order {
        if epoch_start.is_some_and(|s| p.recorded_at <= s) {
            continue;
        }
        let Some((res, at)) = r.filter(|_| !blocked) else {
            blocked = true;
            epoch.pending += 1;
            continue;
        };
        let at = known.map_or(at, |k| k.max(at));
        known = Some(at);
        let fav = res.favourable();
        epoch.resolved += 1;
        epoch.favourable += u32::from(fav);
        epoch.missed += u32::from(res == Resolution::Missed);
        brier_sum += (p.estimate - f64::from(u8::from(fav))).powi(2);
        // A control prediction tests "rate ≤ null": the same detector on
        // the complement.
        let e = match p.arm {
            Arm::Treated => detector.update(p.null, fav),
            Arm::Control => control.update(1.0 - p.null, !fav),
        };
        if e >= threshold {
            demotions.push(Demotion {
                at,
                prediction_id: p.id,
                arm: p.arm,
                e_value: e,
                resolved: epoch.resolved,
            });
            epoch_start = Some(at);
            detector = Detector::new();
            control = Detector::new();
            epoch = Epoch::default();
            brier_sum = 0.0;
        }
    }
    epoch.e_value = detector.value();
    epoch.control_e_value = control.value();
    epoch.brier = (epoch.resolved > 0).then(|| brier_sum / f64::from(epoch.resolved));
    Monitor {
        threshold,
        demotions,
        current: epoch,
    }
}

/// Why the gate decided as it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "refusal", rename_all = "snake_case")]
pub enum GateReason {
    /// The rule's lower bound clears the threshold.
    Clear,
    /// A rule exists, but its lower bound is below the threshold.
    BelowThreshold,
    /// An effect threshold was asked for, and the context has no control
    /// estimate to compute the effect from.
    NoEffectEstimate,
    /// The effect's lower bound is below the effect threshold.
    EffectBelowThreshold,
    /// No rule: the gate fails closed.
    NoRule(RuleRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateDecision {
    pub execute: bool,
    #[serde(flatten)]
    pub reason: GateReason,
    pub threshold: f64,
    #[serde(default)]
    pub min_effect: Option<f64>,
}

/// Execute only when a rule exists, its lower bound is at least `tau` and,
/// with `min_effect`, its effect's lower bound is at least that.
pub fn gate(rule: &Result<Rule, RuleRefusal>, tau: f64, min_effect: Option<f64>) -> GateDecision {
    let reason = match rule {
        Err(r) => GateReason::NoRule(*r),
        Ok(r) if r.lower < tau => GateReason::BelowThreshold,
        Ok(r) => match (min_effect, r.effect) {
            (None, _) => GateReason::Clear,
            (Some(_), None) => GateReason::NoEffectEstimate,
            (Some(m), Some(e)) if e.lower < m => GateReason::EffectBelowThreshold,
            (Some(_), Some(_)) => GateReason::Clear,
        },
    };
    GateDecision {
        execute: reason == GateReason::Clear,
        reason,
        threshold: tau,
        min_effect,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use cs_core::{HypothesisId, InterventionId, PredictionId, Scope, TenantId};
    use cs_memory::{
        capability::OracleTier,
        causal::{ArmCounts, ContextEffect, HypothesisEvaluation, OutcomeObservation},
    };
    use time::{Duration, macros::datetime};

    use super::*;

    const T0: OffsetDateTime = datetime!(2026-01-01 0:00 UTC);
    const LATER: OffsetDateTime = datetime!(2026-02-01 0:00 UTC);

    fn scope() -> Scope {
        Scope::tenant(TenantId(uuid::Uuid::nil()))
    }

    fn hypothesis(treated: ArmCounts, effect: f64, direction: Direction) -> Hypothesis {
        Hypothesis {
            id: HypothesisId(uuid::Uuid::nil()),
            version: 2,
            scope: scope(),
            environment: "env".into(),
            treatment: "t".into(),
            outcome: "y".into(),
            direction,
            min_effect: 0.05,
            contexts: vec![],
            statement: None,
            status: HypothesisStatus::Supported,
            evaluation: Some(HypothesisEvaluation {
                version: 1,
                as_of: T0,
                interventions: vec![],
                contexts: vec![ContextEffect {
                    context: "c".into(),
                    treated,
                    control: ArmCounts { n: 20, positive: 4 },
                    effect: Some(effect),
                    lower: None,
                    upper: None,
                    eligible: true,
                }],
                eligible_contexts: 5,
                effect: None,
                opposite_context: false,
                scope_contexts: vec!["c".into()],
                observational: None,
                status: HypothesisStatus::Supported,
            }),
            created_at: T0,
            updated_at: T0,
        }
    }

    fn prediction(i: u128, null: f64, estimate: f64, at: OffsetDateTime) -> Prediction {
        Prediction {
            id: PredictionId(uuid::Uuid::from_u128(i)),
            scope: scope(),
            intervention_id: InterventionId(uuid::Uuid::from_u128(i)),
            hypothesis_id: HypothesisId(uuid::Uuid::nil()),
            hypothesis_version: 2,
            context: "c".into(),
            lineage: "l".into(),
            arm: Arm::Treated,
            favourable: true,
            estimate,
            lower: null,
            upper: 1.0,
            null,
            treated_n: 40,
            requalified: false,
            recorded_at: at,
            deadline: at + Duration::hours(1),
        }
    }

    #[test]
    fn rules_need_local_support_and_precision() {
        let ok = hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            0.6,
            Direction::Raises,
        );
        let r = rule(&ok, "c", &[], None, T0).unwrap();
        assert!(r.favourable && r.treated_favourable == 32 && !r.requalified);
        assert!((r.estimate - 32.5 / 41.0).abs() < 1e-12);
        assert!(r.lower < 0.8 && r.upper > 0.8);
        assert!((r.null - r.lower).abs() < 1e-12);
        assert_eq!(
            rule(&ok, "other", &[], None, T0),
            Err(RuleRefusal::OutOfScope)
        );

        let small = hypothesis(ArmCounts { n: 2, positive: 2 }, 0.6, Direction::Raises);
        assert_eq!(
            rule(&small, "c", &[], None, T0),
            Err(RuleRefusal::Imprecise)
        );

        let opposite = hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            -0.1,
            Direction::Raises,
        );
        assert_eq!(
            rule(&opposite, "c", &[], None, T0),
            Err(RuleRefusal::LocallyUnsupported)
        );

        let mut unsupported = ok.clone();
        unsupported.status = HypothesisStatus::Inconclusive;
        assert_eq!(
            rule(&unsupported, "c", &[], None, T0),
            Err(RuleRefusal::NotSupported)
        );
    }

    #[test]
    fn lowers_predicts_the_absent_outcome() {
        let h = hypothesis(ArmCounts { n: 40, positive: 8 }, 0.5, Direction::Lowers);
        let r = rule(&h, "c", &[], None, T0).unwrap();
        assert!(!r.favourable);
        assert_eq!(r.treated_favourable, 32);
    }

    fn treated(at: OffsetDateTime, value: bool, assignment: Assignment) -> Intervention {
        let mut outcomes = BTreeMap::new();
        outcomes.insert(
            "y".to_owned(),
            OutcomeObservation {
                value,
                tier: OracleTier::Deterministic,
                observed_at: at,
            },
        );
        Intervention {
            id: InterventionId::new_v7(),
            scope: scope(),
            environment: "env".into(),
            treatment: "t".into(),
            arm: Arm::Treated,
            assignment,
            assignment_detail: Some("d".into()),
            propensity: (assignment == Assignment::Randomized).then_some(0.5),
            context: "c".into(),
            state: BTreeMap::new(),
            execution_id: None,
            recorded_at: at,
            outcomes,
        }
    }

    #[test]
    fn demoted_lineages_requalify_only_from_fresh_randomized_evidence() {
        let h = hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            0.6,
            Direction::Raises,
        );
        let demoted = T0 + Duration::days(1);
        let old: Vec<_> = (0..30)
            .map(|_| treated(T0, true, Assignment::Randomized))
            .collect();
        assert_eq!(
            rule(&h, "c", &old, Some(demoted), LATER),
            Err(RuleRefusal::Demoted),
            "evidence from before the demotion does not requalify"
        );
        let passive: Vec<_> = (0..30)
            .map(|_| treated(demoted + Duration::hours(1), true, Assignment::Passive))
            .collect();
        assert_eq!(
            rule(&h, "c", &passive, Some(demoted), LATER),
            Err(RuleRefusal::Demoted),
            "passive evidence does not requalify"
        );
        let fresh: Vec<_> = (0..25)
            .map(|i| {
                treated(
                    demoted + Duration::hours(1),
                    i % 5 != 0,
                    Assignment::Randomized,
                )
            })
            .collect();
        let r = rule(&h, "c", &fresh, Some(demoted), LATER).unwrap();
        assert!(r.requalified);
        assert_eq!(
            (r.treated_n, r.treated_favourable),
            (20, 16),
            "one fixed block of 20; the incomplete second block is ignored"
        );
    }

    #[test]
    fn requalification_uses_the_latest_complete_block_and_counts_misses() {
        let h = hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            0.6,
            Direction::Raises,
        );
        let demoted = T0 + Duration::days(1);
        let at = |k: i64| demoted + Duration::minutes(k + 1);
        // Block 1: all failures. Block 2: all successes.
        let mut v: Vec<_> = (0..20)
            .map(|k| treated(at(k), false, Assignment::Randomized))
            .collect();
        v.extend((20..40).map(|k| treated(at(k), true, Assignment::Randomized)));
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert_eq!(
            r.treated_favourable, 20,
            "the latest complete block decides"
        );

        // A block whose outcomes never arrived: misses, once past the
        // deadline; pending before it.
        let mut silent: Vec<_> = (0..20)
            .map(|k| treated(at(k), true, Assignment::Randomized))
            .collect();
        for i in &mut silent {
            i.outcomes.clear();
        }
        assert_eq!(
            rule(&h, "c", &silent, Some(demoted), at(30)),
            Err(RuleRefusal::Demoted),
            "pending: not yet re-earned"
        );
        let r = rule(&h, "c", &silent, Some(demoted), LATER).unwrap();
        assert_eq!(r.treated_favourable, 0, "silence is failure here too");
    }

    #[test]
    fn identical_claims_share_a_lineage() {
        let a = hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            0.6,
            Direction::Raises,
        );
        let mut b = a.clone();
        b.id = HypothesisId(uuid::Uuid::from_u128(9));
        b.version = 1;
        assert_eq!(lineage(&a, "c"), lineage(&b, "c"));
        assert_ne!(lineage(&a, "c"), lineage(&a, "d"));
        let mut lowers = a.clone();
        lowers.direction = Direction::Lowers;
        assert_ne!(lineage(&a, "c"), lineage(&lowers, "c"));
    }

    #[test]
    fn outcome_latency_cannot_reorder_the_evidence() {
        // Stationary at the null: 3 in 4 favourable. Failures resolve at
        // once, successes an hour later. Read halfway, resolution order
        // would see only failures; intent order stops at the first pending
        // success.
        let preds: Vec<Prediction> = (0..40)
            .map(|i| prediction(i + 1, 0.75, 0.85, T0 + Duration::minutes(i as i64)))
            .collect();
        let pairs: Vec<_> = preds
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let fav = i % 4 != 0;
                let r =
                    (!fav).then_some((Resolution::Observed { favourable: false }, p.recorded_at));
                (p, r)
            })
            .collect();
        let m = monitor(&pairs);
        assert!(m.demotions.is_empty());
        assert_eq!(m.current.resolved, 1, "only the leading failure is folded");
        assert_eq!(m.current.pending, 39);
    }

    #[test]
    fn silence_resolves_as_a_miss_and_late_outcomes_do_not_rescue() {
        let p = prediction(1, 0.6, 0.8, T0);
        assert_eq!(resolve(&p, "y", None, T0 + Duration::minutes(5)), None);
        assert_eq!(
            resolve(&p, "y", None, T0 + Duration::hours(2)),
            Some((Resolution::Missed, p.deadline))
        );
        let late = treated(T0 + Duration::hours(3), true, Assignment::Passive);
        assert_eq!(
            resolve(&p, "y", Some(&late), T0 + Duration::hours(4)),
            Some((Resolution::Missed, p.deadline))
        );
        let on_time = treated(T0 + Duration::minutes(10), true, Assignment::Passive);
        assert_eq!(
            resolve(&p, "y", Some(&on_time), T0 + Duration::hours(4)),
            Some((
                Resolution::Observed { favourable: true },
                on_time.recorded_at
            ))
        );
    }

    fn run(outcomes: &[bool], null: f64) -> Monitor {
        let preds: Vec<Prediction> = outcomes
            .iter()
            .enumerate()
            .map(|(i, _)| {
                prediction(
                    i as u128 + 1,
                    null,
                    null + 0.1,
                    T0 + Duration::minutes(i as i64),
                )
            })
            .collect();
        let pairs: Vec<_> = preds
            .iter()
            .zip(outcomes)
            .map(|(p, &fav)| {
                (
                    p,
                    Some((Resolution::Observed { favourable: fav }, p.recorded_at)),
                )
            })
            .collect();
        monitor(&pairs)
    }

    #[test]
    fn the_monitor_demotes_a_collapse_quickly_and_only_downwards() {
        // A rule promising at least 0.75 that now never works.
        let m = run(&[false; 40], 0.75);
        let d = m.demotions.first().expect("demoted");
        assert!(d.resolved <= 12, "detected after {}", d.resolved);

        // Doing better than predicted never demotes.
        let m = run(&[true; 500], 0.3);
        assert!(m.demotions.is_empty());
        assert!(m.current.e_value < 1.0);
    }

    #[test]
    fn the_monitor_tolerates_a_healthy_rule_for_a_long_time() {
        // Live rate exactly at the null: deterministic sequence, 3 in 4.
        let outcomes: Vec<bool> = (0..2_000).map(|i| i % 4 != 0).collect();
        let m = run(&outcomes, 0.75);
        assert!(m.demotions.is_empty(), "{:?}", m.demotions.first());
    }

    #[test]
    fn a_new_epoch_ignores_predictions_from_before_the_demotion() {
        let mut preds: Vec<Prediction> = (0..20)
            .map(|i| prediction(i + 1, 0.75, 0.85, T0 + Duration::minutes(i as i64)))
            .collect();
        // Recorded early, resolved late: belongs to the demoted epoch.
        let straggler = prediction(100, 0.75, 0.85, T0);
        preds.push(straggler);
        let mut pairs: Vec<_> = preds
            .iter()
            .take(20)
            .map(|p| {
                (
                    p,
                    Some((Resolution::Observed { favourable: false }, p.recorded_at)),
                )
            })
            .collect();
        pairs.push((
            &preds[20],
            Some((Resolution::Missed, T0 + Duration::days(1))),
        ));
        let m = monitor(&pairs);
        let last = m.demotions.last().expect("demoted").at;
        let after = preds[..20].iter().filter(|p| p.recorded_at > last).count();
        assert_eq!(
            m.current.resolved as usize, after,
            "the straggler recorded before the demotion is not counted"
        );
    }

    #[test]
    fn a_rule_below_the_threshold_accumulates_fixed_blocks_of_new_evidence() {
        let h = hypothesis(
            ArmCounts {
                n: 25,
                positive: 18,
            },
            0.5,
            Direction::Raises,
        );
        let before = rule(&h, "c", &[], None, LATER).unwrap();
        assert!(before.lower < 0.6);
        let at = |k: i64| T0 + Duration::minutes(k + 1);
        // One complete block (19 of 20 favourable) and an incomplete one.
        let mut v: Vec<_> = (0..20)
            .map(|k| treated(at(k), k != 0, Assignment::Randomized))
            .collect();
        v.extend((20..35).map(|k| treated(at(k), true, Assignment::Randomized)));
        // Passive evidence and evidence older than the evaluation never count.
        v.push(treated(at(40), true, Assignment::Passive));
        v.push(treated(T0, true, Assignment::Randomized));
        let after = rule(&h, "c", &v, None, LATER).unwrap();
        assert_eq!((after.treated_n, after.treated_favourable), (45, 37));
        assert!(!after.requalified);
        assert!(after.lower > before.lower);
    }

    /// Under the null boundary (live rate exactly the null), the average run
    /// length to a false alarm is at least `1/ALPHA`. Seeded Bernoulli
    /// sequences, capped at 20,000 steps (the cap only lowers the estimate).
    #[test]
    fn average_run_length_at_the_null_is_at_least_one_over_alpha() {
        let mut state: u64 = 0x5eed;
        let mut uniform = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        };
        for p0 in [0.6, 0.75, 0.9] {
            let runs = 60;
            let mut total = 0u64;
            for _ in 0..runs {
                let mut d = Detector::new();
                let mut t = 0u64;
                while t < 20_000 {
                    t += 1;
                    if d.update(p0, uniform() < p0) >= 1.0 / ALPHA {
                        break;
                    }
                }
                total += t;
            }
            let arl = total as f64 / f64::from(runs);
            assert!(arl >= 1.0 / ALPHA, "p0 {p0}: average run length {arl}");
        }
    }

    #[test]
    fn the_gate_fails_closed() {
        let r = rule_from_counts(true, (40, 36), None, false);
        assert!(gate(&Ok(r), 0.7, None).execute);
        assert_eq!(gate(&Ok(r), 0.9, None).reason, GateReason::BelowThreshold);
        let refused = gate(&Err(RuleRefusal::Demoted), 0.0, None);
        assert!(!refused.execute);
        assert_eq!(refused.reason, GateReason::NoRule(RuleRefusal::Demoted));
    }

    #[test]
    fn the_effect_gate_refuses_what_happens_anyway() {
        // Succeeds 85% when treated, but 85% without treatment too.
        let anyway = rule_from_counts(true, (40, 34), Some((40, 34)), false);
        assert!(
            gate(&Ok(anyway), 0.6, None).execute,
            "the absolute gate acts"
        );
        assert_eq!(
            gate(&Ok(anyway), 0.0, Some(0.1)).reason,
            GateReason::EffectBelowThreshold
        );
        // Succeeds only 55% when treated, against 15% without: worth it.
        let useful = rule_from_counts(true, (40, 22), Some((40, 6)), false);
        assert!(
            !gate(&Ok(useful), 0.6, None).execute,
            "the absolute gate refuses"
        );
        let d = gate(&Ok(useful), 0.0, Some(0.1));
        assert!(d.execute, "{d:?}");
        assert!(useful.effect.unwrap().lower > 0.1);
        // No control estimate: an effect threshold cannot be met.
        let alone = rule_from_counts(true, (40, 34), Some((5, 1)), false);
        assert!(alone.control.is_none() && alone.effect.is_none());
        assert_eq!(
            gate(&Ok(alone), 0.0, Some(0.1)).reason,
            GateReason::NoEffectEstimate
        );
    }

    #[test]
    fn the_rule_carries_both_arms_and_requalifies_each_from_its_own_blocks() {
        let h = hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            0.6,
            Direction::Raises,
        );
        let r = rule(&h, "c", &[], None, T0).unwrap();
        let c = r.control.expect("the evaluation's control arm");
        assert_eq!((c.n, c.favourable), (20, 4));
        let e = r.effect.unwrap();
        assert!((e.estimate - (32.0 / 40.0 - 4.0 / 20.0)).abs() < 1e-12);
        assert!(e.lower > 0.3 && e.upper < 1.0);

        // After a demotion, each arm needs its own fresh block.
        let demoted = T0 + Duration::days(1);
        let at = |k: i64| demoted + Duration::minutes(k + 1);
        let mut v: Vec<_> = (0..20)
            .map(|k| treated(at(k), true, Assignment::Randomized))
            .collect();
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert!(r.requalified && r.control.is_none() && r.effect.is_none());
        v.extend((20..40).map(|k| {
            let mut i = treated(at(k), k % 2 == 0, Assignment::Randomized);
            i.arm = Arm::Control;
            i
        }));
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert_eq!(r.control.map(|c| (c.n, c.favourable)), Some((20, 10)));
    }

    #[test]
    fn a_rising_control_rate_demotes_the_lineage() {
        // Control holdouts were predicted at most 0.4 favourable; the
        // outcome now happens anyway.
        let preds: Vec<Prediction> = (0..40)
            .map(|i| {
                let mut p = prediction(i + 1, 0.4, 0.3, T0 + Duration::minutes(i as i64));
                p.arm = Arm::Control;
                p
            })
            .collect();
        let pairs: Vec<_> = preds
            .iter()
            .map(|p| {
                (
                    p,
                    Some((Resolution::Observed { favourable: true }, p.recorded_at)),
                )
            })
            .collect();
        let m = monitor(&pairs);
        let d = m.demotions.first().expect("demoted on the control arm");
        assert_eq!(d.arm, Arm::Control);
        // Complement null 0.6, alternative 0.3: LR 1.75 per holdout, so
        // 1.75^k / 3 crosses 1000 at k = 15.
        assert!(d.resolved <= 15, "after {}", d.resolved);

        // A control arm doing as predicted (rarely favourable) never does.
        let pairs: Vec<_> = preds
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    p,
                    Some((
                        Resolution::Observed {
                            favourable: i % 4 == 0,
                        },
                        p.recorded_at,
                    )),
                )
            })
            .collect();
        assert!(monitor(&pairs).demotions.is_empty());
    }

    // ---- Monitor: mixed arms, the folding watermark, epoch boundaries ----

    /// Treated null 0.75, unfavourable: per-alternative LRs
    /// `(1 − p1) / (1 − p0)` with `p1 = 0.65, 0.55, 0.45`.
    const TREATED_MISS_LR: [f64; 3] = [1.4, 1.8, 2.2];
    /// Control null 0.4 (complement 0.6), favourable (the outcome happened
    /// anyway): `p1 = 0.5, 0.4, 0.3` on the complement, LRs `0.5/0.4`, ...
    const CONTROL_HIT_LR: [f64; 3] = [1.25, 1.5, 1.75];

    /// The mixture after `k` consecutive steps with these LRs, all above 1:
    /// each CUSUM is then the plain product.
    fn mixture(lrs: [f64; 3], k: i32) -> f64 {
        lrs.iter().map(|l| l.powi(k)).sum::<f64>() / 3.0
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    fn control_prediction(i: u128, null: f64, at: OffsetDateTime) -> Prediction {
        let mut p = prediction(i, null, null - 0.1, at);
        p.arm = Arm::Control;
        p
    }

    fn seen(fav: bool, at: OffsetDateTime) -> Option<(Resolution, OffsetDateTime)> {
        Some((Resolution::Observed { favourable: fav }, at))
    }

    #[test]
    fn hand_checked_crossing_points() {
        // The constants the tests below rely on.
        assert!(mixture(TREATED_MISS_LR, 9) < 1000.0);
        assert!(close(mixture(TREATED_MISS_LR, 10), 1_013.988_155_767_467));
        assert!(mixture(CONTROL_HIT_LR, 14) < 1000.0);
        assert!(close(mixture(CONTROL_HIT_LR, 15), 1_629.275_602_798_909));
        let mut d = Detector::new();
        assert!(close(d.update(0.75, false), 1.8));
        let mut c = Detector::new();
        assert!(close(c.update(1.0 - 0.4, false), 1.5));
    }

    #[test]
    fn a_treated_collapse_in_a_mixed_lineage_demotes_and_resets_both_detectors() {
        // Treated at even minutes (always unfavourable), control holdouts at
        // odd minutes (always favourable: rising, but not yet alarming).
        let mut preds: Vec<Prediction> = (0..19u128)
            .map(|m| {
                let at = T0 + Duration::minutes(m as i64);
                if m % 2 == 0 {
                    prediction(m + 1, 0.75, 0.85, at)
                } else {
                    control_prediction(m + 1, 0.4, at)
                }
            })
            .collect();
        // After the demotion: one control holdout that happens anyway.
        preds.push(control_prediction(100, 0.4, T0 + Duration::minutes(30)));
        let pairs: Vec<_> = preds
            .iter()
            .map(|p| (p, seen(p.arm == Arm::Control, p.recorded_at)))
            .collect();
        let m = monitor(&pairs);
        assert_eq!(m.demotions.len(), 1, "{:?}", m.demotions);
        let d = m.demotions[0];
        // The 10th treated prediction is the 19th folded (minute 18).
        assert_eq!(d.arm, Arm::Treated);
        assert_eq!(d.prediction_id, PredictionId(uuid::Uuid::from_u128(19)));
        assert_eq!(d.resolved, 19);
        assert_eq!(d.at, T0 + Duration::minutes(18));
        assert!(close(d.e_value, mixture(TREATED_MISS_LR, 10)), "{d:?}");
        // Both detectors restarted: the treated one is empty and the control
        // one holds a single step (not the nine before the demotion).
        assert_eq!(m.current.e_value, 0.0);
        assert!(close(m.current.control_e_value, 1.5), "{:?}", m.current);
        assert_eq!((m.current.resolved, m.current.favourable), (1, 1));
        assert_eq!((m.current.missed, m.current.pending), (0, 0));
    }

    #[test]
    fn a_control_collapse_in_a_mixed_lineage_records_the_control_arm() {
        // Three treated misses (treated mixture 6.408), then fifteen control
        // holdouts that all happen anyway: the 15th crosses.
        let mut preds: Vec<Prediction> = (0..3u128)
            .map(|m| prediction(m + 1, 0.75, 0.85, T0 + Duration::minutes(m as i64)))
            .collect();
        preds.extend(
            (3..18u128).map(|m| control_prediction(m + 1, 0.4, T0 + Duration::minutes(m as i64))),
        );
        // After the demotion: one treated miss.
        preds.push(prediction(100, 0.75, 0.85, T0 + Duration::minutes(30)));
        let pairs: Vec<_> = preds
            .iter()
            .map(|p| (p, seen(p.arm == Arm::Control, p.recorded_at)))
            .collect();
        let m = monitor(&pairs);
        assert_eq!(m.demotions.len(), 1, "{:?}", m.demotions);
        let d = m.demotions[0];
        assert_eq!(d.arm, Arm::Control);
        assert_eq!(d.prediction_id, PredictionId(uuid::Uuid::from_u128(18)));
        assert_eq!(d.resolved, 18);
        assert_eq!(d.at, T0 + Duration::minutes(17));
        assert!(close(d.e_value, mixture(CONTROL_HIT_LR, 15)), "{d:?}");
        // The treated detector restarted too: one miss is 1.8, not 6.4 · 1.8.
        assert!(close(m.current.e_value, 1.8), "{:?}", m.current);
        assert_eq!(m.current.control_e_value, 0.0);
        assert_eq!((m.current.resolved, m.current.favourable), (1, 0));
    }

    /// Twelve treated predictions at null 0.75, recorded a minute apart; all
    /// but the first resolve unfavourably a minute after their intent.
    fn watermark_lineage() -> Vec<Prediction> {
        (0..12u128)
            .map(|m| prediction(m + 1, 0.75, 0.85, T0 + Duration::minutes(m as i64)))
            .collect()
    }

    fn watermark_pairs(
        preds: &[Prediction],
        first: Option<(Resolution, OffsetDateTime)>,
    ) -> Vec<(&Prediction, Option<(Resolution, OffsetDateTime)>)> {
        preds
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let r = if i == 0 {
                    first
                } else {
                    seen(false, p.recorded_at + Duration::minutes(1))
                };
                (p, r)
            })
            .collect()
    }

    #[test]
    fn a_pending_prediction_blocks_folding_the_resolved_ones_behind_it() {
        let preds = watermark_lineage();
        // The first is pending: eleven misses wait behind it.
        let m = monitor(&watermark_pairs(&preds, None));
        assert!(m.demotions.is_empty());
        assert_eq!((m.current.resolved, m.current.pending), (0, 12));
        assert_eq!(m.current.e_value, 0.0);
        assert_eq!(m.current.brier, None);

        // A pending prediction in the middle: the five before it fold.
        let mut pairs = watermark_pairs(&preds, seen(false, T0 + Duration::minutes(1)));
        pairs[5].1 = None;
        let m = monitor(&pairs);
        assert!(m.demotions.is_empty());
        assert_eq!((m.current.resolved, m.current.pending), (5, 7));
        assert!(close(m.current.e_value, mixture(TREATED_MISS_LR, 5)));
        // Order of the input does not matter.
        pairs.reverse();
        assert_eq!(monitor(&pairs), m);
    }

    #[test]
    fn once_the_watermark_moves_the_demotion_is_dated_at_the_latest_resolution() {
        let preds = watermark_lineage();
        // The first resolves late (minute 50): ten fold and cross at the
        // 10th (id 10), whose own resolution was at minute 10.
        let m = monitor(&watermark_pairs(
            &preds,
            seen(false, T0 + Duration::minutes(50)),
        ));
        assert_eq!(m.demotions.len(), 1);
        let d = m.demotions[0];
        assert_eq!(d.prediction_id, PredictionId(uuid::Uuid::from_u128(10)));
        assert_eq!(d.resolved, 10);
        assert_eq!(d.at, T0 + Duration::minutes(50));
        // The two left were recorded before the demotion: not counted, not
        // waiting.
        assert_eq!((m.current.resolved, m.current.pending), (0, 0));

        // The first misses: it resolves at its deadline.
        let m = monitor(&watermark_pairs(
            &preds,
            Some((Resolution::Missed, preds[0].deadline)),
        ));
        assert_eq!(m.demotions[0].at, T0 + Duration::hours(1));
        assert_eq!(m.demotions[0].resolved, 10);

        // The first resolves early (minute 1): the prefix's latest
        // resolution is the 10th's own (minute 10). The 11th, recorded at
        // minute 10, is excluded; the 12th, at minute 11, opens the epoch.
        let m = monitor(&watermark_pairs(
            &preds,
            seen(false, T0 + Duration::minutes(1)),
        ));
        assert_eq!(m.demotions.len(), 1);
        assert_eq!(m.demotions[0].at, T0 + Duration::minutes(10));
        assert_eq!((m.current.resolved, m.current.pending), (1, 0));
        assert!(close(m.current.e_value, 1.8));
    }

    #[test]
    fn a_new_epoch_excludes_the_demotion_instant_and_includes_the_next_microsecond() {
        // Ten misses resolved at intent: demoted at minute 9.
        let mut preds: Vec<Prediction> = (0..10u128)
            .map(|m| prediction(m + 1, 0.75, 0.85, T0 + Duration::minutes(m as i64)))
            .collect();
        let demoted = T0 + Duration::minutes(9);
        // Recorded exactly at the demotion instant, and still pending.
        preds.push(prediction(11, 0.75, 0.85, demoted));
        // One microsecond later, resolved unfavourably.
        preds.push(prediction(
            12,
            0.75,
            0.85,
            demoted + Duration::microseconds(1),
        ));
        let pairs: Vec<_> = preds
            .iter()
            .map(|p| {
                let r = match p.id.0.as_u128() {
                    11 => None,
                    12 => seen(false, T0 + Duration::minutes(20)),
                    _ => seen(false, p.recorded_at),
                };
                (p, r)
            })
            .collect();
        let m = monitor(&pairs);
        assert_eq!(m.demotions.len(), 1);
        assert_eq!(m.demotions[0].at, demoted);
        // Had the boundary prediction been included, its pending status would
        // have blocked the next one: (0 resolved, 2 pending).
        assert_eq!((m.current.resolved, m.current.pending), (1, 0));
        assert_eq!((m.current.favourable, m.current.missed), (0, 0));
        assert!(close(m.current.e_value, 1.8));
        // Brier of an estimate of 0.85 against an unfavourable outcome.
        assert!(close(m.current.brier.unwrap(), 0.85 * 0.85));
    }

    #[test]
    fn misses_count_as_unfavourable_and_are_tallied() {
        let preds: Vec<Prediction> = (0..4u128)
            .map(|m| prediction(m + 1, 0.75, 0.85, T0 + Duration::hours(2 * m as i64)))
            .collect();
        let pairs: Vec<_> = preds
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let r = if i % 2 == 0 {
                    Some((Resolution::Missed, p.deadline))
                } else {
                    seen(true, p.recorded_at + Duration::minutes(5))
                };
                (p, r)
            })
            .collect();
        let e = monitor(&pairs).current;
        assert_eq!(
            (e.resolved, e.favourable, e.missed, e.pending),
            (4, 2, 2, 0)
        );
        // Brier: two of (0.85)² and two of (0.15)².
        let brier = (2.0 * 0.85f64.powi(2) + 2.0 * 0.15f64.powi(2)) / 4.0;
        assert!(close(e.brier.unwrap(), brier));
    }

    // ---- Rules: block boundaries and `lowers` ----

    fn randomized(at: OffsetDateTime, value: bool, arm: Arm) -> Intervention {
        let mut i = treated(at, value, Assignment::Randomized);
        i.arm = arm;
        i
    }

    fn raises_40_32() -> Hypothesis {
        hypothesis(
            ArmCounts {
                n: 40,
                positive: 32,
            },
            0.6,
            Direction::Raises,
        )
    }

    #[test]
    fn requalification_happens_only_at_block_boundaries() {
        let h = raises_40_32();
        let demoted = T0 + Duration::days(1);
        let at = |k: i64| demoted + Duration::minutes(k + 1);
        // First block all favourable, second all unfavourable.
        let v: Vec<_> = (0..40)
            .map(|k| randomized(at(k), k < 20, Arm::Treated))
            .collect();
        let counts = |n: usize| {
            rule(&h, "c", &v[..n], Some(demoted), LATER)
                .map(|r| (r.treated_n, r.treated_favourable))
        };
        assert_eq!(counts(19), Err(RuleRefusal::Demoted));
        assert_eq!(counts(20), Ok((20, 20)));
        assert_eq!(counts(39), Ok((20, 20)), "still the first block");
        assert_eq!(counts(40), Ok((20, 0)), "the second block replaces it");
        let r = rule(&h, "c", &v[..20], Some(demoted), LATER).unwrap();
        assert!(r.requalified);
        assert!(close(r.estimate, 20.5 / 21.0));
    }

    #[test]
    fn a_pending_outcome_leaves_its_block_incomplete() {
        let h = raises_40_32();
        let demoted = T0 + Duration::days(1);
        let at = |k: i64| demoted + Duration::minutes(k + 1);
        let mut v: Vec<_> = (0..40)
            .map(|k| randomized(at(k), true, Arm::Treated))
            .collect();
        v[39].outcomes.clear();
        // Half an hour after the last intent: still pending.
        let now = at(39) + Duration::minutes(30);
        let r = rule(&h, "c", &v, Some(demoted), now).unwrap();
        assert_eq!((r.treated_n, r.treated_favourable), (20, 20));
        // Past its deadline it is a miss and completes the second block.
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert_eq!((r.treated_n, r.treated_favourable), (20, 19));
        // An outcome observed after `now` is not yet known.
        let mut w: Vec<_> = (0..20)
            .map(|k| randomized(at(k), true, Arm::Treated))
            .collect();
        w[19].outcomes.get_mut("y").unwrap().observed_at = at(19) + Duration::minutes(10);
        assert_eq!(
            rule(&h, "c", &w, Some(demoted), at(19) + Duration::minutes(5)),
            Err(RuleRefusal::Demoted)
        );
        assert!(rule(&h, "c", &w, Some(demoted), at(19) + Duration::minutes(10)).is_ok());
    }

    #[test]
    fn interventions_at_the_demotion_instant_do_not_requalify() {
        let h = raises_40_32();
        let demoted = T0 + Duration::days(1);
        let exact: Vec<_> = (0..20)
            .map(|_| randomized(demoted, true, Arm::Treated))
            .collect();
        assert_eq!(
            rule(&h, "c", &exact, Some(demoted), LATER),
            Err(RuleRefusal::Demoted)
        );
        let next: Vec<_> = (0..20)
            .map(|_| randomized(demoted + Duration::microseconds(1), true, Arm::Treated))
            .collect();
        let r = rule(&h, "c", &next, Some(demoted), LATER).unwrap();
        assert_eq!((r.treated_n, r.treated_favourable), (20, 20));
        // Before any demotion, the evaluation's `as_of` is the same strict
        // boundary for accumulated blocks.
        let at_eval: Vec<_> = (0..20)
            .map(|_| randomized(T0, true, Arm::Treated))
            .collect();
        assert_eq!(rule(&h, "c", &at_eval, None, LATER).unwrap().treated_n, 40);
    }

    #[test]
    fn control_blocks_are_independent_of_treated_blocks() {
        let h = raises_40_32();
        let demoted = T0 + Duration::days(1);
        let at = |k: i64| demoted + Duration::minutes(k + 1);
        // Ten of each arm, interleaved: twenty in all, but no complete block.
        let mixed: Vec<_> = (0..20)
            .map(|k| {
                let arm = if k % 2 == 0 {
                    Arm::Treated
                } else {
                    Arm::Control
                };
                randomized(at(k), true, arm)
            })
            .collect();
        assert_eq!(
            rule(&h, "c", &mixed, Some(demoted), LATER),
            Err(RuleRefusal::Demoted)
        );
        // Twenty treated and 19 control: a rule without a control estimate.
        let mut v: Vec<_> = (0..20)
            .map(|k| randomized(at(k), true, Arm::Treated))
            .collect();
        v.extend((20..39).map(|k| randomized(at(k), k % 4 == 0, Arm::Control)));
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert_eq!((r.treated_n, r.treated_favourable), (20, 20));
        assert!(r.control.is_none() && r.effect.is_none());
        // The 20th control completes its block; the treated block is unchanged.
        v.push(randomized(at(39), false, Arm::Control));
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert_eq!((r.treated_n, r.treated_favourable), (20, 20));
        // Control favourable at k = 20, 24, ..., 36: five of twenty.
        assert_eq!(r.control.map(|c| (c.n, c.favourable)), Some((20, 5)));
        let e = r.effect.unwrap();
        assert!(close(e.estimate, 1.0 - 0.25));
        // Control blocks alone never make a rule.
        let control_only: Vec<_> = (0..40)
            .map(|k| randomized(at(k), true, Arm::Control))
            .collect();
        assert_eq!(
            rule(&h, "c", &control_only, Some(demoted), LATER),
            Err(RuleRefusal::Demoted)
        );
    }

    #[test]
    fn never_demoted_lineages_accumulate_each_arm_by_whole_blocks() {
        let h = raises_40_32();
        let at = |k: i64| T0 + Duration::minutes(k + 1);
        let fresh = |n: i64, arm: Arm| -> Vec<Intervention> {
            (0..n).map(|k| randomized(at(k), true, arm)).collect()
        };
        let treated_n = |v: &[Intervention]| rule(&h, "c", v, None, LATER).unwrap().treated_n;
        assert_eq!(treated_n(&fresh(19, Arm::Treated)), 40);
        assert_eq!(treated_n(&fresh(20, Arm::Treated)), 60);
        assert_eq!(treated_n(&fresh(39, Arm::Treated)), 60);
        assert_eq!(treated_n(&fresh(40, Arm::Treated)), 80);
        // Evaluation control (20, 4) plus one complete block of 20 favourable.
        let r = rule(&h, "c", &fresh(39, Arm::Control), None, LATER).unwrap();
        assert_eq!(r.control.map(|c| (c.n, c.favourable)), Some((40, 24)));
        assert_eq!(r.treated_n, 40, "control evidence does not touch treated");
    }

    fn lowers_40_8() -> Hypothesis {
        hypothesis(ArmCounts { n: 40, positive: 8 }, 0.5, Direction::Lowers)
    }

    #[test]
    fn lowers_counts_the_absent_outcome_as_favourable_on_both_arms() {
        let h = lowers_40_8();
        // Evaluation counts: treated 32 of 40 absent, control 16 of 20 absent.
        let r = rule(&h, "c", &[], None, T0).unwrap();
        assert!(!r.favourable);
        assert_eq!((r.treated_n, r.treated_favourable), (40, 32));
        assert_eq!(r.control.map(|c| (c.n, c.favourable)), Some((20, 16)));
        assert!(r.effect.unwrap().estimate.abs() < 1e-12, "0.8 - 0.8");

        // Re-earned blocks: the outcome being absent is favourable.
        let demoted = T0 + Duration::days(1);
        let at = |k: i64| demoted + Duration::minutes(k + 1);
        let mut v: Vec<_> = (0..20)
            .map(|k| randomized(at(k), k >= 3, Arm::Treated))
            .collect();
        v.extend((20..40).map(|k| randomized(at(k), k < 25, Arm::Control)));
        let r = rule(&h, "c", &v, Some(demoted), LATER).unwrap();
        assert!(!r.favourable && r.requalified);
        assert_eq!((r.treated_n, r.treated_favourable), (20, 3));
        assert_eq!(r.control.map(|c| (c.n, c.favourable)), Some((20, 15)));
        assert!(close(r.effect.unwrap().estimate, 0.15 - 0.75));

        // Silence is failure under `lowers` too: a missing outcome is not an
        // absent one.
        let mut silent: Vec<_> = (0..20)
            .map(|k| randomized(at(k), false, Arm::Treated))
            .collect();
        for i in &mut silent {
            i.outcomes.clear();
        }
        let r = rule(&h, "c", &silent, Some(demoted), LATER).unwrap();
        assert_eq!(r.treated_favourable, 0);
    }

    #[test]
    fn lowers_predictions_resolve_favourable_when_the_outcome_is_absent() {
        let mut p = prediction(1, 0.6, 0.8, T0);
        p.favourable = false;
        let absent = treated(T0 + Duration::minutes(5), false, Assignment::Randomized);
        let present = treated(T0 + Duration::minutes(5), true, Assignment::Randomized);
        let now = T0 + Duration::minutes(10);
        assert_eq!(
            resolve(&p, "y", Some(&absent), now),
            Some((
                Resolution::Observed { favourable: true },
                absent.recorded_at
            ))
        );
        assert_eq!(
            resolve(&p, "y", Some(&present), now),
            Some((
                Resolution::Observed { favourable: false },
                present.recorded_at
            ))
        );
        let missed = resolve(&p, "y", None, p.deadline).unwrap();
        assert_eq!(missed, (Resolution::Missed, p.deadline));
        assert!(!missed.0.favourable());
        // Exactly at the deadline an observation still counts; a self-assessed
        // one never does.
        let at_deadline = treated(p.deadline, false, Assignment::Randomized);
        assert_eq!(
            resolve(&p, "y", Some(&at_deadline), p.deadline),
            Some((Resolution::Observed { favourable: true }, p.deadline))
        );
        let mut weak = absent.clone();
        weak.outcomes.get_mut("y").unwrap().tier = OracleTier::SelfAssessed;
        assert_eq!(resolve(&p, "y", Some(&weak), now), None);
    }

    // ---- Gate, effect interval and detector numerics ----

    #[test]
    fn rule_numbers_are_hand_checkable() {
        // Wilson 95% for 36 of 40.
        let r = rule_from_counts(true, (40, 36), None, false);
        assert!((r.lower - 0.769_479_256_195_261).abs() < 1e-12, "{r:?}");
        assert!((r.upper - 0.960_421_112_404_425).abs() < 1e-12, "{r:?}");
        assert!(close(r.estimate, 36.5 / 41.0));
        assert!(close(r.null, r.lower));
        // A rule whose lower bound is 0 is monitored at the floor.
        let zero = rule_from_counts(true, (20, 0), None, false);
        assert_eq!((zero.lower, zero.null), (0.0, NULL_FLOOR));
        // A control arm at 20 of 20 has an upper bound just below 1; its
        // monitored null is capped at 0.98.
        let c = ArmEstimate::new(20, 20);
        assert!(c.upper > 0.999_999 && c.upper <= 1.0);
        assert_eq!(c.control_null(), 1.0 - NULL_FLOOR);
        // A control arm with a low upper bound is monitored at that bound.
        let low = ArmEstimate::new(20, 0);
        assert!(close(low.control_null(), low.upper));
    }

    #[test]
    fn the_gate_checks_in_order_and_ties_execute() {
        let r = rule_from_counts(true, (40, 36), Some((40, 10)), false);
        let e = r.effect.expect("control present");
        let bare = rule_from_counts(true, (40, 36), None, false);
        let above = |x: f64| x + 1e-9;

        // No rule beats everything, and the inputs are echoed.
        let d = gate(&Err(RuleRefusal::Imprecise), 2.0, Some(5.0));
        assert_eq!(d.reason, GateReason::NoRule(RuleRefusal::Imprecise));
        assert!(!d.execute);
        assert_eq!((d.threshold, d.min_effect), (2.0, Some(5.0)));
        // Below threshold beats a missing effect estimate and a small effect.
        assert_eq!(
            gate(&Ok(bare), above(bare.lower), Some(-1.0)).reason,
            GateReason::BelowThreshold
        );
        assert_eq!(
            gate(&Ok(r), above(r.lower), Some(above(e.lower))).reason,
            GateReason::BelowThreshold
        );
        // A missing estimate refuses even the most permissive effect gate.
        let d = gate(&Ok(bare), 0.0, Some(-1.0));
        assert_eq!(d.reason, GateReason::NoEffectEstimate);
        assert!(!d.execute);
        assert!(gate(&Ok(bare), 0.0, None).execute);
        // A small effect is refused once the threshold is met.
        assert_eq!(
            gate(&Ok(r), r.lower, Some(above(e.lower))).reason,
            GateReason::EffectBelowThreshold
        );
        // Ties execute: a threshold equal to the lower bound, and a minimum
        // effect equal to the effect's lower bound.
        let d = gate(&Ok(r), r.lower, None);
        assert_eq!((d.execute, d.reason), (true, GateReason::Clear));
        let d = gate(&Ok(r), r.lower, Some(e.lower));
        assert_eq!((d.execute, d.reason), (true, GateReason::Clear));
        // The gate never executes for any refusal.
        for refusal in [
            RuleRefusal::NotSupported,
            RuleRefusal::Unrelated,
            RuleRefusal::OutOfScope,
            RuleRefusal::LocallyUnsupported,
            RuleRefusal::Imprecise,
            RuleRefusal::Demoted,
        ] {
            assert!(!gate(&Err(refusal), 0.0, None).execute);
        }
    }

    #[test]
    fn difference_interval_is_sane() {
        use crate::causal::difference_interval;
        assert_eq!(difference_interval(0, 0, 3, 10), None);
        assert_eq!(difference_interval(3, 10, 0, 0), None);
        assert_eq!(difference_interval(0, 0, 0, 0), None);

        // 0 of 10 against 0 of 10: Wilson upper 0.27754 on each arm.
        let (d, l, u) = difference_interval(0, 10, 0, 10).unwrap();
        assert_eq!(d, 0.0);
        assert!((u - 0.277_540_168_766_616_5).abs() < 1e-12, "{u}");
        assert!((l + u).abs() < 1e-15);
        // 10 of 10 against 0 of 10: lower 1 − √2 · 0.27754, upper 1.
        let (d, l, u) = difference_interval(10, 10, 0, 10).unwrap();
        assert_eq!(d, 1.0);
        assert!((l - (1.0 - 2f64.sqrt() * 0.277_540_168_766_616_5)).abs() < 1e-12);
        assert!((u - 1.0).abs() < 1e-12);

        for (k1, n1, k0, n0) in [
            (0, 1, 1, 1),
            (3, 7, 5, 9),
            (36, 40, 10, 40),
            (1, 50, 49, 50),
            (20, 20, 20, 20),
            (7, 300, 2, 3),
        ] {
            let (d, l, u) = difference_interval(k1, n1, k0, n0).unwrap();
            assert!(
                (-1.0..=1.0).contains(&l) && (-1.0..=1.0).contains(&u),
                "{k1}/{n1} vs {k0}/{n0}: ({l}, {u})"
            );
            assert!(
                l <= d && d <= u,
                "{k1}/{n1} vs {k0}/{n0}: {d} not in ({l}, {u})"
            );
            assert!(close(
                d,
                f64::from(k1) / f64::from(n1) - f64::from(k0) / f64::from(n0)
            ));
            // Swapping the arms negates the interval.
            let (sd, sl, su) = difference_interval(k0, n0, k1, n1).unwrap();
            assert_eq!((sd, sl, su), (-d, -u, -l));
        }
    }

    #[test]
    fn the_detector_is_finite_at_the_floor() {
        // p0 = 0.02: every alternative is p0 / 4 = 0.005.
        let mut d = Detector::new();
        assert!(close(d.update(NULL_FLOOR, true), 0.25));
        let mut d = Detector::new();
        assert!(close(d.update(NULL_FLOOR, false), 0.995 / 0.98));
        // All failures: (0.995 / 0.98)^k crosses 1000 at k = 455.
        let mut d = Detector::new();
        let mut crossed = None;
        for k in 1..=10_000u32 {
            let v = d.update(NULL_FLOOR, false);
            assert!(v.is_finite() && v > 0.0, "step {k}: {v}");
            if crossed.is_none() && v >= 1.0 / ALPHA {
                crossed = Some(k);
            }
        }
        assert_eq!(crossed, Some(455));
        // Repeated successes at the floor hold every CUSUM at 0.25.
        let mut d = Detector::new();
        for _ in 0..10 {
            d.update(NULL_FLOOR, true);
        }
        assert!(close(d.value(), 0.25));
        // The same through the monitor.
        let m = run(&[false; 500], NULL_FLOOR);
        assert_eq!(m.demotions.len(), 1);
        assert_eq!(m.demotions[0].resolved, 455);
        assert!(m.demotions[0].e_value.is_finite());
    }

    #[test]
    fn the_detector_is_finite_at_a_high_null() {
        // p0 = 0.98: alternatives 0.88, 0.78, 0.68.
        let mut d = Detector::new();
        assert!(close(
            d.update(0.98, true),
            (0.88 + 0.78 + 0.68) / 0.98 / 3.0
        ));
        // One miss: LRs 6, 11, 16; three misses: 1881.
        let mut d = Detector::new();
        assert!(close(d.update(0.98, false), 11.0));
        assert!(close(d.update(0.98, false), (36.0 + 121.0 + 256.0) / 3.0));
        assert!(close(d.update(0.98, false), 1881.0));
        // Thirty misses through the monitor: a demotion every three, each
        // epoch starting afresh, never NaN.
        let m = run(&[false; 30], 0.98);
        assert_eq!(m.demotions.len(), 10);
        for d in &m.demotions {
            assert_eq!(d.resolved, 3);
            assert!(close(d.e_value, 1881.0), "{d:?}");
        }
        assert_eq!(m.current.resolved, 0);
        assert_eq!((m.current.e_value, m.current.control_e_value), (0.0, 0.0));
        // A control holdout at the capped null tests the complement, 0.02.
        let preds: Vec<Prediction> = (0..1_000u128)
            .map(|i| control_prediction(i + 1, 0.98, T0 + Duration::minutes(i as i64)))
            .collect();
        let pairs: Vec<_> = preds
            .iter()
            .enumerate()
            .map(|(i, p)| (p, seen(i % 3 != 0, p.recorded_at)))
            .collect();
        let m = monitor(&pairs);
        assert!(
            m.demotions.is_empty(),
            "a control rate of 2/3 is within 0.98"
        );
        assert!(m.current.control_e_value.is_finite());
        assert_eq!(m.current.e_value, 0.0);
        // Long sequences at a rate above each extreme null stay finite and
        // positive (1 in 7 favourable at the floor, 99 in 100 at 0.98).
        type Case = (f64, fn(u32) -> bool);
        let cases: [Case; 2] = [(NULL_FLOOR, |k| k % 7 == 0), (0.98, |k| k % 100 != 0)];
        for (p0, favourable) in cases {
            let mut d = Detector::new();
            for k in 0..10_000u32 {
                let v = d.update(p0, favourable(k));
                assert!(v.is_finite() && v > 0.0, "p0 {p0}, step {k}: {v}");
            }
        }
    }
}
