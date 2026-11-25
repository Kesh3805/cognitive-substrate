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
}
