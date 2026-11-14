//! Capability evaluation: is a procedure good enough to be trusted, and is
//! it still working? Pure and deterministic.
//!
//! * **Field evidence** is every execution not bound to an evaluation
//!   case, for the current procedure (same fingerprint). Its success rate
//!   is computed over **input classes**: each class contributes its own
//!   success rate once, so repeating one input many times cannot inflate
//!   the sample size. The Wilson lower bound (stated `z`) must clear a
//!   threshold with a minimum number of classes.
//! * **Suites** gate promotion: each required kind needs at least one
//!   case, and every required-kind case and every locked case must pass on
//!   the current procedure.
//! * **Drift** is watched separately from the lifetime record: a window of
//!   recent runs whose Wilson *upper* bound is below the floor (we are
//!   confident it is failing), or a run of consecutive failures, calls for
//!   quarantine.

use cs_memory::{
    CapabilityStage,
    capability::{AbortReason, CaseKind, EvaluationCase, Execution, ExecutionStatus, RunOutcome},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityPolicy {
    /// Normal quantile for the Wilson interval (1.96 ≈ 95% two-sided).
    pub z: f64,
    pub min_classes_evaluated: u32,
    pub evaluated_lower_bound: f64,
    pub min_classes_trusted: u32,
    pub trusted_lower_bound: f64,
    /// Case kinds that must each have at least one passing case.
    pub required_kinds: Vec<CaseKind>,
    /// Recent field runs watched for drift.
    pub window: u32,
    pub window_min: u32,
    /// Quarantine when the window's Wilson upper bound is below this.
    pub quarantine_upper_bound: f64,
    /// Quarantine after this many consecutive failed field runs.
    pub consecutive_failures: u32,
    /// A started run with no result after this long is a failure.
    pub run_timeout_seconds: i64,
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self {
            z: 1.96,
            min_classes_evaluated: 10,
            evaluated_lower_bound: 0.6,
            min_classes_trusted: 30,
            trusted_lower_bound: 0.8,
            required_kinds: vec![CaseKind::Positive, CaseKind::Negative, CaseKind::Boundary],
            window: 25,
            window_min: 10,
            quarantine_upper_bound: 0.6,
            consecutive_failures: 3,
            run_timeout_seconds: 3_600,
        }
    }
}

/// Wilson score interval for `successes` out of `n` (fractional counts
/// allowed). `None` when `n` is zero.
pub fn wilson(successes: f64, n: f64, z: f64) -> Option<(f64, f64)> {
    if n <= 0.0 {
        return None;
    }
    let p = (successes / n).clamp(0.0, 1.0);
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let centre = p + z2 / (2.0 * n);
    let margin = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    Some((
        ((centre - margin) / denom).clamp(0.0, 1.0),
        ((centre + margin) / denom).clamp(0.0, 1.0),
    ))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldEvidence {
    /// Decisive runs (successes + failures).
    pub runs: u32,
    pub successes: u32,
    /// Includes aborts and timeouts.
    pub failures: u32,
    pub aborted: u32,
    pub unverified: u32,
    pub pending: u32,
    /// Distinct input classes with at least one decisive run.
    pub input_classes: u32,
    /// Class-weighted success rate.
    pub success_rate: Option<f64>,
    pub wilson_lower: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowEvidence {
    pub runs: u32,
    pub successes: u32,
    pub wilson_upper: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: cs_core::EvaluationCaseId,
    pub kind: CaseKind,
    pub locked: bool,
    /// Latest decisive outcome on the current procedure.
    pub latest: Option<RunOutcome>,
    pub passing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEvaluation {
    pub stage: CapabilityStage,
    pub policy: CapabilityPolicy,
    pub field: FieldEvidence,
    pub window: WindowEvidence,
    pub consecutive_failures: u32,
    pub suite: Vec<CaseResult>,
    pub missing_kinds: Vec<CaseKind>,
    /// The highest stage the evidence supports (`evaluated` or `trusted`).
    pub eligible: Option<CapabilityStage>,
    /// Why the next stage is not supported.
    pub blockers: Vec<String>,
    /// Set when recent evidence calls for quarantine.
    pub quarantine: Option<String>,
}

fn latest_decisive(
    runs: &[&Execution],
    kind: CaseKind,
    now: OffsetDateTime,
    timeout: time::Duration,
) -> Option<RunOutcome> {
    let mut runs: Vec<&&Execution> = runs.iter().collect();
    runs.sort_by_key(|e| std::cmp::Reverse((e.started_at, e.id)));
    runs.into_iter().find_map(|e| {
        // Refusing on a failed precondition is what a negative case asks.
        if kind == CaseKind::Negative
            && e.status == ExecutionStatus::Aborted
            && e.abort_reason == Some(AbortReason::PreconditionFailed)
        {
            return Some(RunOutcome::Success);
        }
        match e.outcome(now, timeout) {
            o @ (RunOutcome::Success | RunOutcome::Failure) => Some(o),
            _ => None,
        }
    })
}

pub fn evaluate(
    stage: CapabilityStage,
    fingerprint: uuid::Uuid,
    executions: &[Execution],
    cases: &[EvaluationCase],
    policy: &CapabilityPolicy,
    now: OffsetDateTime,
) -> CapabilityEvaluation {
    let timeout = time::Duration::seconds(policy.run_timeout_seconds);
    let current: Vec<&Execution> = executions
        .iter()
        .filter(|e| e.procedure_fingerprint == fingerprint)
        .collect();

    // ---- field evidence ---------------------------------------------------
    let mut field = FieldEvidence::default();
    let mut classes: std::collections::BTreeMap<&str, (u32, u32)> = Default::default();
    let mut decisive: Vec<(&Execution, bool)> = Vec::new();
    for e in current.iter().filter(|e| e.case_id.is_none()) {
        if e.status == ExecutionStatus::Aborted {
            field.aborted += 1;
        }
        match e.outcome(now, timeout) {
            RunOutcome::Success => {
                field.successes += 1;
                classes.entry(&e.input_class).or_default().0 += 1;
                classes.entry(&e.input_class).or_default().1 += 1;
                decisive.push((e, true));
            }
            RunOutcome::Failure => {
                field.failures += 1;
                classes.entry(&e.input_class).or_default().1 += 1;
                decisive.push((e, false));
            }
            RunOutcome::Unverified => field.unverified += 1,
            RunOutcome::Pending => field.pending += 1,
        }
    }
    field.runs = field.successes + field.failures;
    field.input_classes = u32::try_from(classes.len()).unwrap_or(u32::MAX);
    let n = classes.len() as f64;
    let s: f64 = classes
        .values()
        .map(|&(ok, all)| f64::from(ok) / f64::from(all))
        .sum();
    field.success_rate = (n > 0.0).then(|| s / n);
    field.wilson_lower = wilson(s, n, policy.z).map(|(lo, _)| lo);

    // ---- drift ------------------------------------------------------------
    decisive.sort_by_key(|(e, _)| std::cmp::Reverse((e.started_at, e.id)));
    let consecutive_failures =
        u32::try_from(decisive.iter().take_while(|(_, ok)| !ok).count()).unwrap_or(u32::MAX);
    let recent: Vec<bool> = decisive
        .iter()
        .take(policy.window as usize)
        .map(|(_, ok)| *ok)
        .collect();
    let window = WindowEvidence {
        runs: u32::try_from(recent.len()).unwrap_or(u32::MAX),
        successes: u32::try_from(recent.iter().filter(|ok| **ok).count()).unwrap_or(u32::MAX),
        wilson_upper: wilson(
            recent.iter().filter(|ok| **ok).count() as f64,
            recent.len() as f64,
            policy.z,
        )
        .map(|(_, hi)| hi),
    };
    let quarantine = if consecutive_failures >= policy.consecutive_failures {
        Some(format!(
            "{consecutive_failures} consecutive failed runs"
        ))
    } else if window.runs >= policy.window_min
        && window
            .wilson_upper
            .is_some_and(|hi| hi < policy.quarantine_upper_bound)
    {
        Some(format!(
            "recent success rate is confidently below {:.0}% ({} of {} runs)",
            policy.quarantine_upper_bound * 100.0,
            window.successes,
            window.runs
        ))
    } else {
        None
    };

    // ---- suites -------------------------------------------------------------
    let suite: Vec<CaseResult> = cases
        .iter()
        .map(|c| {
            let runs: Vec<&Execution> = current
                .iter()
                .copied()
                .filter(|e| e.case_id == Some(c.id))
                .collect();
            let latest = latest_decisive(&runs, c.kind, now, timeout);
            CaseResult {
                case_id: c.id,
                kind: c.kind,
                locked: c.locked,
                latest,
                passing: latest == Some(RunOutcome::Success),
            }
        })
        .collect();
    let missing_kinds: Vec<CaseKind> = policy
        .required_kinds
        .iter()
        .copied()
        .filter(|k| !cases.iter().any(|c| c.kind == *k))
        .collect();

    // ---- eligibility --------------------------------------------------------
    let mut blockers = Vec::new();
    if !missing_kinds.is_empty() {
        blockers.push(format!(
            "no {} case",
            missing_kinds
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", no ")
        ));
    }
    let failing = suite
        .iter()
        .filter(|r| (r.locked || policy.required_kinds.contains(&r.kind)) && !r.passing)
        .count();
    if failing > 0 {
        blockers.push(format!("{failing} required or locked case(s) not passing"));
    }
    if let Some(q) = &quarantine {
        blockers.push(format!("quarantine signal: {q}"));
    }
    let clears = |min: u32, lb: f64| {
        field.input_classes >= min && field.wilson_lower.is_some_and(|w| w >= lb)
    };
    let gates_ok = blockers.is_empty();
    let eligible = if !gates_ok {
        None
    } else if clears(policy.min_classes_trusted, policy.trusted_lower_bound) {
        Some(CapabilityStage::Trusted)
    } else if clears(policy.min_classes_evaluated, policy.evaluated_lower_bound) {
        Some(CapabilityStage::Evaluated)
    } else {
        None
    };
    let (min, lb) = if eligible == Some(CapabilityStage::Evaluated) {
        (policy.min_classes_trusted, policy.trusted_lower_bound)
    } else {
        (policy.min_classes_evaluated, policy.evaluated_lower_bound)
    };
    if eligible != Some(CapabilityStage::Trusted) {
        if field.input_classes < min {
            blockers.push(format!(
                "{} of {min} input classes with decisive field runs",
                field.input_classes
            ));
        }
        if field.wilson_lower.is_none_or(|w| w < lb) {
            blockers.push(format!(
                "Wilson lower bound {} below {lb} (z = {})",
                field
                    .wilson_lower
                    .map_or_else(|| "n/a".to_owned(), |w| format!("{w:.3}")),
                policy.z
            ));
        }
    }

    CapabilityEvaluation {
        stage,
        policy: policy.clone(),
        field,
        window,
        consecutive_failures,
        suite,
        missing_kinds,
        eligible,
        blockers,
        quarantine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_core::{
        ActorRef, EvaluationCaseId, ExecutionId, MemoryId, MemoryRevisionId, Scope, TenantId,
        UserId, VerificationId,
    };
    use cs_memory::capability::{CaseOrigin, OracleTier, Verdict, Verification};
    use time::macros::datetime;

    const T0: OffsetDateTime = datetime!(2026-01-01 0:00 UTC);

    fn run(class: &str, ok: Option<bool>, minute: i64, case: Option<EvaluationCaseId>) -> Execution {
        let id = ExecutionId::new_v7();
        let actor = ActorRef::User {
            user_id: UserId::from_uuid(uuid::Uuid::nil()),
        };
        Execution {
            id,
            capability_id: MemoryId::from_uuid(uuid::Uuid::nil()),
            capability_revision: MemoryRevisionId::from_uuid(uuid::Uuid::nil()),
            procedure_fingerprint: uuid::Uuid::nil(),
            scope: Scope::tenant(TenantId::from_uuid(uuid::Uuid::nil())),
            case_id: case,
            input_class: class.into(),
            actor: actor.clone(),
            status: if ok.is_some() {
                ExecutionStatus::Completed
            } else {
                ExecutionStatus::Aborted
            },
            abort_reason: None,
            started_at: T0 + time::Duration::minutes(minute),
            finished_at: None,
            verifications: ok
                .map(|ok| Verification {
                    id: VerificationId::new_v7(),
                    execution_id: id,
                    tier: OracleTier::Deterministic,
                    verdict: if ok { Verdict::Passed } else { Verdict::Failed },
                    verifier: actor,
                    detail: None,
                    recorded_at: T0,
                })
                .into_iter()
                .collect(),
        }
    }

    fn case(kind: CaseKind) -> EvaluationCase {
        EvaluationCase {
            id: EvaluationCaseId::new_v7(),
            capability_id: MemoryId::from_uuid(uuid::Uuid::nil()),
            scope: Scope::tenant(TenantId::from_uuid(uuid::Uuid::nil())),
            kind,
            input_class: kind.as_str().into(),
            description: None,
            locked: kind == CaseKind::Regression,
            origin: CaseOrigin::Authored,
            created_at: T0,
        }
    }

    fn eval(runs: &[Execution], cases: &[EvaluationCase]) -> CapabilityEvaluation {
        evaluate(
            CapabilityStage::Candidate,
            uuid::Uuid::nil(),
            runs,
            cases,
            &CapabilityPolicy::default(),
            T0 + time::Duration::minutes(10),
        )
    }

    #[test]
    fn wilson_matches_reference_values() {
        // 8 of 10 at z = 1.96: [0.4902, 0.9433].
        let (lo, hi) = wilson(8.0, 10.0, 1.96).unwrap();
        assert!((lo - 0.4902).abs() < 1e-3 && (hi - 0.9433).abs() < 1e-3);
        assert!(wilson(0.0, 0.0, 1.96).is_none());
    }

    #[test]
    fn repeating_one_input_does_not_inflate_evidence() {
        let runs: Vec<Execution> = (0..40).map(|i| run("same", Some(true), i, None)).collect();
        let e = eval(&runs, &[]);
        assert_eq!(e.field.runs, 40);
        assert_eq!(e.field.input_classes, 1);
        assert!(e.field.wilson_lower.unwrap() < 0.3);
    }

    #[test]
    fn suites_and_independent_classes_make_a_capability_eligible() {
        let cases: Vec<EvaluationCase> = [CaseKind::Positive, CaseKind::Negative, CaseKind::Boundary]
            .into_iter()
            .map(case)
            .collect();
        let mut runs: Vec<Execution> = (0..12)
            .map(|i| run(&format!("c{i}"), Some(true), i, None))
            .collect();
        let without_suite = eval(&runs, &cases);
        assert_eq!(without_suite.eligible, None, "cases not run yet");
        runs.extend(cases.iter().map(|c| run(&c.input_class, Some(true), 0, Some(c.id))));
        let e = eval(&runs, &cases);
        assert_eq!(e.eligible, Some(CapabilityStage::Evaluated), "{:?}", e.blockers);
        assert!(!e.blockers.is_empty(), "trusted still blocked by class count");
    }

    #[test]
    fn aborts_count_and_drift_quarantines() {
        let mut runs: Vec<Execution> = (0..12)
            .map(|i| run(&format!("c{i}"), Some(true), i, None))
            .collect();
        for i in 20..23 {
            runs.push(run(&format!("d{i}"), None, i, None));
        }
        let e = eval(&runs, &[]);
        assert_eq!(e.field.aborted, 3);
        assert_eq!(e.field.failures, 3);
        assert_eq!(e.consecutive_failures, 3);
        assert!(e.quarantine.is_some());
    }

    #[test]
    fn a_failing_locked_case_blocks_promotion() {
        let mut cases: Vec<EvaluationCase> = [CaseKind::Positive, CaseKind::Negative, CaseKind::Boundary]
            .into_iter()
            .map(case)
            .collect();
        cases.push(case(CaseKind::Regression));
        let mut runs: Vec<Execution> = (0..12)
            .map(|i| run(&format!("c{i}"), Some(true), i, None))
            .collect();
        runs.extend(cases.iter().map(|c| {
            run(
                &c.input_class,
                Some(c.kind != CaseKind::Regression),
                0,
                Some(c.id),
            )
        }));
        assert_eq!(eval(&runs, &cases).eligible, None);
    }
}
