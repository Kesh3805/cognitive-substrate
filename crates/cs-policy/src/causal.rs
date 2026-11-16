//! Evaluation of causal hypotheses (M6.2, evaluation version 1).
//!
//! Only randomized interventions with an outcome established by a strong
//! oracle tier count. Each context is a stratum. The pooled effect is a
//! stratified Newcombe–Wilson interval: Mantel–Haenszel weights pool each
//! arm's proportion, each arm gets a Wilson interval at its effective
//! sample size, and Newcombe's square-and-add rule combines them. Unlike a
//! Wald variance, it does not collapse when a sparse stratum is all 0 or 1.

use std::collections::BTreeMap;

use cs_memory::causal::{
    Arm, ArmCounts, Assignment, CAUSAL_EVALUATION_VERSION, ContextEffect, Hypothesis,
    HypothesisEvaluation, HypothesisStatus, Intervention, PooledEffect,
};
use time::OffsetDateTime;

use crate::capability::wilson;

/// Two-sided 95%.
pub const Z: f64 = 1.96;
/// Outcomes per arm for a context to count.
pub const MIN_PER_ARM: u32 = 2;
/// Eligible contexts needed before any decision.
pub const MIN_CONTEXTS: u32 = 5;
/// Total Mantel–Haenszel weight needed before any decision, so that many
/// contexts with a single outcome per arm cannot decide.
pub const MIN_WEIGHT: f64 = 5.0;

fn proportion(c: ArmCounts) -> f64 {
    f64::from(c.positive) / f64::from(c.n)
}

/// Newcombe's hybrid score interval for `p1 − p0`, from two Wilson
/// intervals.
fn newcombe(p1: f64, (l1, u1): (f64, f64), p0: f64, (l0, u0): (f64, f64)) -> (f64, f64, f64) {
    let d = p1 - p0;
    (
        d,
        d - ((p1 - l1).powi(2) + (u0 - p0).powi(2)).sqrt(),
        d + ((u1 - p1).powi(2) + (p0 - l0).powi(2)).sqrt(),
    )
}

/// `(estimate, lower, upper)` in the hypothesis's direction.
fn signed(h: &Hypothesis, (d, l, u): (f64, f64, f64)) -> (f64, f64, f64) {
    match h.direction {
        cs_memory::causal::Direction::Raises => (d, l, u),
        cs_memory::causal::Direction::Lowers => (-d, -u, -l),
    }
}

/// Per-context counts over interventions with the given assignment.
fn counts<'a>(
    h: &Hypothesis,
    interventions: &'a [Intervention],
    assignment: Assignment,
) -> (
    BTreeMap<String, (ArmCounts, ArmCounts)>,
    Vec<&'a Intervention>,
) {
    let mut by_context: BTreeMap<String, (ArmCounts, ArmCounts)> = BTreeMap::new();
    let mut used = Vec::new();
    for i in interventions {
        if i.assignment != assignment || !h.concerns(i) {
            continue;
        }
        let Some(value) = i.eligible_outcome(&h.outcome) else {
            continue;
        };
        let entry = by_context.entry(i.context.clone()).or_default();
        let arm = match i.arm {
            Arm::Treated => &mut entry.0,
            Arm::Control => &mut entry.1,
        };
        arm.n += 1;
        arm.positive += u32::from(value);
        used.push(i);
    }
    (by_context, used)
}

fn context_effect(h: &Hypothesis, context: &str, t: ArmCounts, c: ArmCounts) -> ContextEffect {
    let interval = (t.n > 0 && c.n > 0).then(|| {
        let (pt, pc) = (proportion(t), proportion(c));
        let wt = wilson(f64::from(t.positive), f64::from(t.n), Z).unwrap_or((0.0, 1.0));
        let wc = wilson(f64::from(c.positive), f64::from(c.n), Z).unwrap_or((0.0, 1.0));
        signed(h, newcombe(pt, wt, pc, wc))
    });
    ContextEffect {
        context: context.to_owned(),
        treated: t,
        control: c,
        effect: interval.map(|i| i.0),
        lower: interval.map(|i| i.1),
        upper: interval.map(|i| i.2),
        eligible: t.n >= MIN_PER_ARM && c.n >= MIN_PER_ARM,
    }
}

/// Stratified Newcombe–Wilson over the eligible contexts, or `None` when
/// there are fewer than [`MIN_CONTEXTS`] of them or their total weight is
/// below [`MIN_WEIGHT`].
fn pooled(h: &Hypothesis, contexts: &[ContextEffect]) -> Option<PooledEffect> {
    let eligible: Vec<&ContextEffect> = contexts.iter().filter(|c| c.eligible).collect();
    if (eligible.len() as u32) < MIN_CONTEXTS {
        return None;
    }
    let weights: Vec<f64> = eligible
        .iter()
        .map(|c| {
            let (t, k) = (f64::from(c.treated.n), f64::from(c.control.n));
            t * k / (t + k)
        })
        .collect();
    let total: f64 = weights.iter().sum();
    if total < MIN_WEIGHT {
        return None;
    }
    // One arm: weighted proportion and Wilson interval at the effective
    // sample size n_eff = 1 / Σ (ŵ² / n).
    let arm = |pick: fn(&ContextEffect) -> ArmCounts| -> (f64, (f64, f64)) {
        let mut p = 0.0;
        let mut inv = 0.0;
        for (c, w) in eligible.iter().zip(&weights) {
            let a = pick(c);
            let share = w / total;
            p += share * proportion(a);
            inv += share * share / f64::from(a.n);
        }
        let n_eff = 1.0 / inv;
        (p, wilson(p * n_eff, n_eff, Z).unwrap_or((0.0, 1.0)))
    };
    let (pt, wt) = arm(|c| c.treated);
    let (pc, wc) = arm(|c| c.control);
    let (estimate, lower, upper) = signed(h, newcombe(pt, wt, pc, wc));
    Some(PooledEffect {
        estimate,
        lower,
        upper,
        weight: total,
    })
}

/// Evaluate a hypothesis against interventions (any order; those that do
/// not concern it are ignored).
///
/// | Condition (randomized evidence, eligible contexts) | Status |
/// |---|---|
/// | fewer than 5 eligible contexts, or total weight < 5 | `proposed` |
/// | lower bound > `min_effect`, and no eligible context's interval lies entirely below 0 | `supported` |
/// | upper bound < `min_effect` | `refuted` |
/// | otherwise | `inconclusive` |
pub fn evaluate(
    h: &Hypothesis,
    interventions: &[Intervention],
    as_of: OffsetDateTime,
) -> HypothesisEvaluation {
    let (randomized, used) = counts(h, interventions, Assignment::Randomized);
    let contexts: Vec<ContextEffect> = randomized
        .iter()
        .map(|(ctx, (t, c))| context_effect(h, ctx, *t, *c))
        .collect();
    let effect = pooled(h, &contexts);
    let opposite_context = contexts
        .iter()
        .any(|c| c.eligible && c.upper.is_some_and(|u| u < 0.0));
    let status = match effect {
        None => HypothesisStatus::Proposed,
        Some(e) if e.lower > h.min_effect && !opposite_context => HypothesisStatus::Supported,
        Some(e) if e.upper < h.min_effect => HypothesisStatus::Refuted,
        Some(_) => HypothesisStatus::Inconclusive,
    };
    let scope_contexts = if matches!(
        status,
        HypothesisStatus::Supported | HypothesisStatus::Refuted
    ) {
        contexts
            .iter()
            .filter(|c| c.eligible)
            .map(|c| c.context.clone())
            .collect()
    } else {
        Vec::new()
    };
    let (passive, _) = counts(h, interventions, Assignment::Passive);
    let passive_contexts: Vec<ContextEffect> = passive
        .iter()
        .map(|(ctx, (t, c))| context_effect(h, ctx, *t, *c))
        .collect();
    let mut ids: Vec<_> = used.iter().map(|i| i.id).collect();
    ids.sort();
    HypothesisEvaluation {
        version: CAUSAL_EVALUATION_VERSION,
        as_of,
        interventions: ids,
        eligible_contexts: contexts.iter().filter(|c| c.eligible).count() as u32,
        contexts,
        effect,
        opposite_context,
        scope_contexts,
        observational: pooled(h, &passive_contexts),
        status,
    }
}

#[cfg(test)]
mod tests {
    use cs_core::{HypothesisId, InterventionId, Scope, TenantId};
    use cs_memory::{
        capability::OracleTier,
        causal::{Direction, OutcomeObservation},
    };
    use time::macros::datetime;

    use super::*;

    const T: OffsetDateTime = datetime!(2026-01-01 0:00 UTC);

    fn hypothesis(direction: Direction, min_effect: f64) -> Hypothesis {
        Hypothesis {
            id: HypothesisId::new_v7(),
            version: 1,
            scope: Scope::tenant(TenantId::new_v7()),
            environment: "env@1".into(),
            treatment: "x".into(),
            outcome: "y".into(),
            direction,
            min_effect,
            contexts: Vec::new(),
            statement: None,
            status: HypothesisStatus::Proposed,
            evaluation: None,
            created_at: T,
            updated_at: T,
        }
    }

    fn run(
        context: &str,
        arm: Arm,
        assignment: Assignment,
        value: bool,
        tier: OracleTier,
    ) -> Intervention {
        let mut outcomes = BTreeMap::new();
        outcomes.insert(
            "y".to_owned(),
            OutcomeObservation {
                value,
                tier,
                observed_at: T,
            },
        );
        Intervention {
            id: InterventionId::new_v7(),
            scope: Scope::tenant(TenantId::new_v7()),
            environment: "env@1".into(),
            treatment: "x".into(),
            arm,
            assignment,
            assignment_detail: Some("seed 1".into()),
            propensity: (assignment == Assignment::Randomized).then_some(0.5),
            context: context.into(),
            state: BTreeMap::new(),
            execution_id: None,
            recorded_at: T,
            outcomes,
        }
    }

    /// `n` per arm in each of `k` contexts, with `t` and `c` positives.
    fn trial(k: usize, n: u32, t: u32, c: u32, assignment: Assignment) -> Vec<Intervention> {
        let mut v = Vec::new();
        for ctx in 0..k {
            let name = format!("c{ctx}");
            for i in 0..n {
                v.push(run(
                    &name,
                    Arm::Treated,
                    assignment,
                    i < t,
                    OracleTier::Deterministic,
                ));
                v.push(run(
                    &name,
                    Arm::Control,
                    assignment,
                    i < c,
                    OracleTier::Deterministic,
                ));
            }
        }
        v
    }

    #[test]
    fn strong_effects_are_supported_in_their_direction() {
        let data = trial(6, 8, 7, 1, Assignment::Randomized);
        let e = evaluate(&hypothesis(Direction::Raises, 0.05), &data, T);
        assert_eq!(e.status, HypothesisStatus::Supported, "{e:?}");
        assert_eq!(e.scope_contexts.len(), 6);
        let p = e.effect.unwrap();
        assert!((p.estimate - 0.75).abs() < 1e-9 && p.lower > 0.05);
        // The same data refutes "lowers".
        let e = evaluate(&hypothesis(Direction::Lowers, 0.05), &data, T);
        assert_eq!(e.status, HypothesisStatus::Refuted);
    }

    #[test]
    fn nulls_are_refuted_only_with_a_minimum_effect_and_enough_data() {
        let null = trial(12, 10, 5, 5, Assignment::Randomized);
        assert_eq!(
            evaluate(&hypothesis(Direction::Raises, 0.0), &null, T).status,
            HypothesisStatus::Inconclusive
        );
        let e = evaluate(&hypothesis(Direction::Raises, 0.2), &null, T);
        assert_eq!(e.status, HypothesisStatus::Refuted, "{:?}", e.effect);
    }

    #[test]
    fn sparse_evidence_decides_nothing() {
        // Four contexts.
        let few = trial(4, 10, 10, 0, Assignment::Randomized);
        assert_eq!(
            evaluate(&hypothesis(Direction::Raises, 0.0), &few, T).status,
            HypothesisStatus::Proposed
        );
        // Ten contexts with one outcome per arm: not eligible.
        let thin = trial(10, 1, 1, 0, Assignment::Randomized);
        assert_eq!(
            evaluate(&hypothesis(Direction::Raises, 0.0), &thin, T).status,
            HypothesisStatus::Proposed
        );
    }

    #[test]
    fn passive_and_self_assessed_evidence_never_decide() {
        let passive = trial(10, 10, 10, 0, Assignment::Passive);
        let e = evaluate(&hypothesis(Direction::Raises, 0.0), &passive, T);
        assert_eq!(e.status, HypothesisStatus::Proposed);
        assert!(e.observational.unwrap().lower > 0.5, "lift is reported");
        assert!(e.interventions.is_empty());

        let mut weak = trial(10, 10, 10, 0, Assignment::Randomized);
        for i in &mut weak {
            for o in i.outcomes.values_mut() {
                o.tier = OracleTier::SelfAssessed;
            }
        }
        assert_eq!(
            evaluate(&hypothesis(Direction::Raises, 0.0), &weak, T).status,
            HypothesisStatus::Proposed
        );
    }

    #[test]
    fn an_opposite_context_blocks_support() {
        let mut data = trial(8, 10, 9, 1, Assignment::Randomized);
        // One context where the treatment clearly hurts.
        for i in 0..10 {
            data.push(run(
                "harmed",
                Arm::Treated,
                Assignment::Randomized,
                false,
                OracleTier::Independent,
            ));
            data.push(run(
                "harmed",
                Arm::Control,
                Assignment::Randomized,
                i < 9,
                OracleTier::Independent,
            ));
        }
        let e = evaluate(&hypothesis(Direction::Raises, 0.0), &data, T);
        assert!(
            e.effect.unwrap().lower > 0.0,
            "the pooled effect alone is positive"
        );
        assert!(e.opposite_context);
        assert_eq!(e.status, HypothesisStatus::Inconclusive);
    }

    #[test]
    fn all_zero_strata_do_not_collapse_the_interval() {
        // Every stratum is 2 vs 0 out of 2: Wald would give zero variance.
        let data = trial(5, 2, 2, 0, Assignment::Randomized);
        let e = evaluate(&hypothesis(Direction::Raises, 0.0), &data, T);
        let p = e.effect.unwrap();
        assert!(p.upper - p.lower > 0.2, "{p:?}");
        assert!(p.upper <= 1.0 && p.lower >= -1.0);
    }

    #[test]
    fn declared_contexts_and_environment_scope_the_evidence() {
        let data = trial(6, 8, 7, 1, Assignment::Randomized);
        let mut h = hypothesis(Direction::Raises, 0.0);
        h.contexts = vec!["c0".into(), "c1".into()];
        assert_eq!(evaluate(&h, &data, T).status, HypothesisStatus::Proposed);
        let mut h = hypothesis(Direction::Raises, 0.0);
        h.environment = "env@2".into();
        assert!(evaluate(&h, &data, T).contexts.is_empty());
    }
}
