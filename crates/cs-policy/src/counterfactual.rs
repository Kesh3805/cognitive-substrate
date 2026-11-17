//! Read-time counterfactuals (M6.3, version 1).
//!
//! A counterfactual is computed when asked for, from one randomized
//! intervention and one supported hypothesis, and never stored. It answers
//! one of two questions about that unit:
//!
//! * treated, outcome present: would the outcome have been absent without
//!   the treatment? (probability of necessity, PN);
//! * control, outcome absent: would the treatment have produced it?
//!   (probability of sufficiency, PS).
//!
//! These are probabilities over the context's experimental population, not
//! the probability that this particular episode would have gone otherwise:
//! the episode only selects which question applies. They are not
//! identifiable from data; with a randomized (exogenous) assignment, Tian
//! and Pearl's bounds follow from the experimental proportions in the
//! unit's context. Each of the two bounds is a ratio of two independent
//! proportions, or one minus such a ratio, and gets its own one-sided 97.5%
//! limit (Katz's log interval for a ratio of proportions), so the lower
//! bound is below the true lower bound and the upper bound above the true
//! upper bound with at least 95% joint confidence (Bonferroni). The width
//! therefore reflects both incomplete identification and finite data. No monotonicity is assumed: a stated direction is a claim about an
//! average, not about every unit.

use cs_memory::causal::{
    Arm, Assignment, ContextEffect, Direction, Hypothesis, HypothesisStatus, Intervention,
};
use serde::{Deserialize, Serialize};

/// One-sided 97.5% per bound: at least 95% joint coverage for the pair.
pub const Z_ONE_SIDED: f64 = 1.959_963_984_540_054;

pub const COUNTERFACTUAL_VERSION: u32 = 1;

/// Which counterfactual question was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    /// P(outcome absent without treatment | treated, outcome present).
    Necessity,
    /// P(outcome present with treatment | control, outcome absent).
    Sufficiency,
}

/// Why no counterfactual can be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The hypothesis is not `supported`.
    NotSupported,
    /// The intervention's assignment was passive: not exogenous.
    NotRandomized,
    /// The intervention is not about the hypothesis's environment and
    /// treatment.
    Unrelated,
    /// The intervention's context is not among the hypothesis's
    /// `scope_contexts`.
    OutOfScope,
    /// The intervention has no outcome for the hypothesis's key from a
    /// tier that counts.
    NoEligibleOutcome,
    /// Treated with the outcome absent, or control with it present: there
    /// is no counterfactual question this version answers.
    OutcomeMismatch,
    /// The joint region reaches a proportion where the bound is undefined
    /// (P(outcome | treated) = 0 for necessity, P(outcome | control) = 1
    /// for sufficiency): nothing informative can be said.
    InsufficientEvidence,
}

/// What the bounds assume, stated with every result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assumption {
    /// The arm was assigned at random (declared by the caller, not
    /// verified).
    Exogeneity,
    /// The observed outcome is the potential outcome under the arm received.
    Consistency,
    /// One unit's treatment does not change another's outcome.
    NoInterference,
    /// The context's experimental units represent this unit's population.
    ContextPopulation,
}

pub const ASSUMPTIONS: [Assumption; 4] = [
    Assumption::Exogeneity,
    Assumption::Consistency,
    Assumption::NoInterference,
    Assumption::ContextPopulation,
];

/// Bounds on one unit's counterfactual probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub query: Query,
    pub lower: f64,
    pub upper: f64,
    /// The context's experimental counts the bounds came from.
    pub context: ContextEffect,
    pub assumptions: Vec<Assumption>,
    /// The bounds say something (not the vacuous `[0, 1]`). Vacuous bounds
    /// mean the context has too little evidence for this question.
    pub informative: bool,
}

/// One-sided 97.5% upper limit of `(a / na) / (b / nb)` for independent
/// binomial counts (Katz's log method), or `None` when `b = 0`. When any
/// cell is empty, 0.5 is added to each count and 1 to each total
/// (Haldane), so the limit stays finite and conservative.
fn ratio_upper(a: u32, na: u32, b: u32, nb: u32) -> Option<f64> {
    if b == 0 || na == 0 || nb == 0 {
        return None;
    }
    let (mut a, mut na, mut b, mut nb) = (f64::from(a), f64::from(na), f64::from(b), f64::from(nb));
    if a == 0.0 || a == na || b == nb {
        a += 0.5;
        na += 1.0;
        b += 0.5;
        nb += 1.0;
    }
    let ratio = (a / na) / (b / nb);
    let se = (1.0 / a - 1.0 / na + 1.0 / b - 1.0 / nb).max(0.0).sqrt();
    Some(ratio * (Z_ONE_SIDED * se).exp())
}

/// Bounds from counts: `x1` of `n1` treated and `x0` of `n0` control units
/// with the outcome. `None` when the bound's denominator has no units
/// (necessity needs `x1 > 0`; sufficiency needs `x0 < n0`).
fn bounds(query: Query, x1: u32, n1: u32, x0: u32, n0: u32) -> Option<(f64, f64)> {
    let (lower_ratio, upper_ratio) = match query {
        // PN in [1 − p0/p1, (1 − p0)/p1].
        Query::Necessity => (
            ratio_upper(x0, n0, x1, n1)?,
            ratio_upper(n0 - x0, n0, x1, n1)?,
        ),
        // PS in [1 − (1 − p1)/(1 − p0), p1/(1 − p0)].
        Query::Sufficiency => (
            ratio_upper(n1 - x1, n1, n0 - x0, n0)?,
            ratio_upper(x1, n1, n0 - x0, n0)?,
        ),
    };
    Some(((1.0 - lower_ratio).max(0.0), upper_ratio.min(1.0)))
}

/// Tian and Pearl's bounds at known proportions, for reference.
pub fn point_bounds(query: Query, p1: f64, p0: f64) -> (f64, f64) {
    match query {
        Query::Necessity if p1 > 0.0 => (((p1 - p0) / p1).max(0.0), ((1.0 - p0) / p1).min(1.0)),
        Query::Sufficiency if p0 < 1.0 => (
            ((p1 - p0) / (1.0 - p0)).max(0.0),
            (p1 / (1.0 - p0)).min(1.0),
        ),
        _ => (0.0, 1.0),
    }
}

/// The counterfactual for `intervention` under `hypothesis`, or why there
/// is none.
pub fn counterfactual(
    hypothesis: &Hypothesis,
    intervention: &Intervention,
) -> Result<Bounds, Refusal> {
    if hypothesis.status != HypothesisStatus::Supported {
        return Err(Refusal::NotSupported);
    }
    let Some(evaluation) = &hypothesis.evaluation else {
        return Err(Refusal::NotSupported);
    };
    if intervention.environment != hypothesis.environment
        || intervention.treatment != hypothesis.treatment
    {
        return Err(Refusal::Unrelated);
    }
    if intervention.assignment != Assignment::Randomized {
        return Err(Refusal::NotRandomized);
    }
    if !evaluation.scope_contexts.contains(&intervention.context) {
        return Err(Refusal::OutOfScope);
    }
    let Some(value) = intervention.eligible_outcome(&hypothesis.outcome) else {
        return Err(Refusal::NoEligibleOutcome);
    };
    // The hypothesis's outcome direction decides which outcome value is
    // "present": for `lowers`, the treatment is meant to prevent it, so the
    // question is about the outcome's absence.
    let present = match hypothesis.direction {
        Direction::Raises => value,
        Direction::Lowers => !value,
    };
    let query = match (intervention.arm, present) {
        (Arm::Treated, true) => Query::Necessity,
        (Arm::Control, false) => Query::Sufficiency,
        _ => return Err(Refusal::OutcomeMismatch),
    };
    let context = evaluation
        .contexts
        .iter()
        .find(|c| c.context == intervention.context && c.eligible)
        .ok_or(Refusal::OutOfScope)?
        .clone();
    // Counts of the hypothesis's "present" outcome.
    let present_count = |positive: u32, n: u32| match hypothesis.direction {
        Direction::Raises => positive,
        Direction::Lowers => n - positive,
    };
    let (x1, n1) = (
        present_count(context.treated.positive, context.treated.n),
        context.treated.n,
    );
    let (x0, n0) = (
        present_count(context.control.positive, context.control.n),
        context.control.n,
    );
    // Undefined when the bound's denominator has no units: refuse rather
    // than divide by nothing.
    let (lower, upper) = bounds(query, x1, n1, x0, n0).ok_or(Refusal::InsufficientEvidence)?;
    Ok(Bounds {
        query,
        lower,
        upper,
        context,
        assumptions: ASSUMPTIONS.to_vec(),
        informative: lower > 0.0 || upper < 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_bounds_match_tian_and_pearl() {
        // p1 = 0.8, p0 = 0.2.
        let (l, u) = point_bounds(Query::Necessity, 0.8, 0.2);
        assert!((l - 0.75).abs() < 1e-12 && (u - 1.0).abs() < 1e-12);
        let (l, u) = point_bounds(Query::Sufficiency, 0.8, 0.2);
        assert!((l - 0.75).abs() < 1e-12 && (u - 1.0).abs() < 1e-12);
        // p1 = 0.5, p0 = 0.4: PN in [0.2, 1], PS in [1/6, 5/6].
        let (l, u) = point_bounds(Query::Necessity, 0.5, 0.4);
        assert!((l - 0.2).abs() < 1e-12 && (u - 1.0).abs() < 1e-12);
        let (l, u) = point_bounds(Query::Sufficiency, 0.5, 0.4);
        assert!((l - 1.0 / 6.0).abs() < 1e-12 && (u - 5.0 / 6.0).abs() < 1e-12);
    }

    use std::collections::BTreeMap;

    use cs_core::{HypothesisId, InterventionId, Scope, TenantId};
    use cs_memory::{
        capability::OracleTier,
        causal::{ArmCounts, OutcomeObservation},
    };
    use time::macros::datetime;

    fn unit(
        arm: Arm,
        assignment: Assignment,
        value: bool,
        tier: OracleTier,
        ctx: &str,
    ) -> Intervention {
        let t = datetime!(2026-01-01 0:00 UTC);
        let mut outcomes = BTreeMap::new();
        outcomes.insert(
            "y".to_owned(),
            OutcomeObservation {
                value,
                tier,
                observed_at: t,
            },
        );
        Intervention {
            id: InterventionId::new_v7(),
            scope: Scope::tenant(TenantId::new_v7()),
            environment: "e@1".into(),
            treatment: "x".into(),
            arm,
            assignment,
            assignment_detail: Some("seed".into()),
            propensity: (assignment == Assignment::Randomized).then_some(0.5),
            context: ctx.into(),
            state: BTreeMap::new(),
            execution_id: None,
            recorded_at: t,
            outcomes,
        }
    }

    /// A hypothesis supported over contexts c0..c5, each with 40 per arm,
    /// 32 treated and 8 control outcomes present.
    fn supported(direction: Direction) -> Hypothesis {
        let t = datetime!(2026-01-01 0:00 UTC);
        let mut data = Vec::new();
        for k in 0..6 {
            let c = format!("c{k}");
            for i in 0..40 {
                let (yt, yc) = (i < 32, i < 8);
                let (yt, yc) = match direction {
                    Direction::Raises => (yt, yc),
                    Direction::Lowers => (!yt, !yc),
                };
                data.push(unit(
                    Arm::Treated,
                    Assignment::Randomized,
                    yt,
                    OracleTier::Deterministic,
                    &c,
                ));
                data.push(unit(
                    Arm::Control,
                    Assignment::Randomized,
                    yc,
                    OracleTier::Deterministic,
                    &c,
                ));
            }
        }
        let mut h = Hypothesis {
            id: HypothesisId::new_v7(),
            version: 2,
            scope: Scope::tenant(TenantId::new_v7()),
            environment: "e@1".into(),
            treatment: "x".into(),
            outcome: "y".into(),
            direction,
            min_effect: 0.05,
            contexts: Vec::new(),
            statement: None,
            status: HypothesisStatus::Proposed,
            evaluation: None,
            created_at: t,
            updated_at: t,
        };
        let e = crate::causal::evaluate(&h, &data, t);
        assert_eq!(e.status, HypothesisStatus::Supported);
        h.status = e.status;
        h.evaluation = Some(e);
        h
    }

    #[test]
    fn answers_only_the_questions_it_can() {
        let h = supported(Direction::Raises);
        let ok = counterfactual(
            &h,
            &unit(
                Arm::Treated,
                Assignment::Randomized,
                true,
                OracleTier::Independent,
                "c0",
            ),
        )
        .unwrap();
        assert_eq!(ok.query, Query::Necessity);
        assert!(
            ok.informative && ok.lower > 0.0 && ok.upper <= 1.0,
            "{ok:?}"
        );
        // Point bounds at p1 = 0.8, p0 = 0.2 are [0.75, 1]: the joint
        // region contains them.
        assert!(ok.lower <= 0.75 && ok.upper >= 1.0 - 1e-12);
        assert_eq!(
            ok.context.treated,
            ArmCounts {
                n: 40,
                positive: 32
            }
        );
        let ps = counterfactual(
            &h,
            &unit(
                Arm::Control,
                Assignment::Randomized,
                false,
                OracleTier::Independent,
                "c1",
            ),
        )
        .unwrap();
        assert_eq!(ps.query, Query::Sufficiency);

        let refuse = |i: Intervention| counterfactual(&h, &i).unwrap_err();
        assert_eq!(
            refuse(unit(
                Arm::Treated,
                Assignment::Passive,
                true,
                OracleTier::Independent,
                "c0"
            )),
            Refusal::NotRandomized
        );
        assert_eq!(
            refuse(unit(
                Arm::Treated,
                Assignment::Randomized,
                true,
                OracleTier::Independent,
                "elsewhere"
            )),
            Refusal::OutOfScope
        );
        assert_eq!(
            refuse(unit(
                Arm::Treated,
                Assignment::Randomized,
                true,
                OracleTier::SelfAssessed,
                "c0"
            )),
            Refusal::NoEligibleOutcome
        );
        assert_eq!(
            refuse(unit(
                Arm::Treated,
                Assignment::Randomized,
                false,
                OracleTier::Independent,
                "c0"
            )),
            Refusal::OutcomeMismatch
        );
        assert_eq!(
            refuse(unit(
                Arm::Control,
                Assignment::Randomized,
                true,
                OracleTier::Independent,
                "c0"
            )),
            Refusal::OutcomeMismatch
        );
        let mut other = unit(
            Arm::Treated,
            Assignment::Randomized,
            true,
            OracleTier::Independent,
            "c0",
        );
        other.treatment = "z".into();
        assert_eq!(refuse(other), Refusal::Unrelated);

        let mut refuted = h.clone();
        refuted.status = HypothesisStatus::Refuted;
        assert_eq!(
            counterfactual(
                &refuted,
                &unit(
                    Arm::Treated,
                    Assignment::Randomized,
                    true,
                    OracleTier::Independent,
                    "c0"
                )
            )
            .unwrap_err(),
            Refusal::NotSupported
        );
    }

    #[test]
    fn lowers_asks_about_the_outcome_it_prevents() {
        let h = supported(Direction::Lowers);
        // Treated with the outcome absent: was the treatment necessary for
        // its absence?
        let ok = counterfactual(
            &h,
            &unit(
                Arm::Treated,
                Assignment::Randomized,
                false,
                OracleTier::Deterministic,
                "c2",
            ),
        )
        .unwrap();
        assert_eq!(ok.query, Query::Necessity);
        assert!(ok.informative);
    }

    #[test]
    fn large_samples_converge_on_the_analytic_interval() {
        let n = 1_000_000;
        for q in [Query::Necessity, Query::Sufficiency] {
            let (l, u) = bounds(q, 600_000, n, 300_000, n).unwrap();
            let (pl, pu) = point_bounds(q, 0.6, 0.3);
            assert!(
                (l - pl).abs() < 0.01 && (u - pu).abs() < 0.01,
                "{q:?} {l} {u}"
            );
            assert!(l <= pl && u >= pu, "limits stay on the safe side");
        }
    }

    #[test]
    fn finite_samples_widen_and_stay_in_range() {
        for q in [Query::Necessity, Query::Sufficiency] {
            let (l, u) = bounds(q, 24, 40, 12, 40).unwrap();
            let (pl, pu) = point_bounds(q, 0.6, 0.3);
            assert!(l <= pl && u >= pu, "{q:?}");
            assert!((0.0..=1.0).contains(&l) && (0.0..=1.0).contains(&u));
        }
        // Empty and full cells stay finite.
        let (l, u) = bounds(Query::Necessity, 10, 10, 0, 10).unwrap();
        assert!(l > 0.0 && u <= 1.0, "{l} {u}");
        // Undefined denominators.
        assert_eq!(bounds(Query::Necessity, 0, 10, 3, 10), None);
        assert_eq!(bounds(Query::Sufficiency, 5, 10, 10, 10), None);
    }
}
