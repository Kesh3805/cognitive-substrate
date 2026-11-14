//! Procedural learning: executions, independent verification and
//! evaluation suites for capabilities (procedural memories).
//!
//! * **Execution status is not outcome.** An execution records that a
//!   procedure ran (or aborted); whether it achieved its goal is a separate
//!   [`Verification`], made by a verifier of a stated [`OracleTier`].
//! * **Everything counts.** Executions are recorded at intent. Aborts and
//!   runs that never finish count as failures, so a procedure cannot look
//!   reliable by abandoning the runs it would fail.
//! * **Self-judgment never promotes.** Self-assessed verifications can
//!   report failure, but never success.

use cs_core::{
    ActorRef, Error, EvaluationCaseId, ExecutionId, MemoryId, MemoryRevisionId, Result, Scope,
    VerificationId,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const MAX_INPUT_CLASS: usize = 128;
pub const MAX_DETAIL: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Started,
    Completed,
    Aborted,
}

impl ExecutionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Started, Self::Completed, Self::Aborted]
            .into_iter()
            .find(|s| s.as_str() == v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    PreconditionFailed,
    Timeout,
    Cancelled,
    Error,
}

impl AbortReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreconditionFailed => "precondition_failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [
            Self::PreconditionFailed,
            Self::Timeout,
            Self::Cancelled,
            Self::Error,
        ]
        .into_iter()
        .find(|s| s.as_str() == v)
    }
}

/// How independent the verifier is from the actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleTier {
    /// Deterministic external checks: tests, schema or invariant checks,
    /// external state receipts, explicit human sign-off.
    Deterministic,
    /// Independent assertions over observed state by a different checker.
    Independent,
    /// The actor (or a model sharing its assumptions) judging itself.
    SelfAssessed,
}

impl OracleTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Independent => "independent",
            Self::SelfAssessed => "self_assessed",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Deterministic, Self::Independent, Self::SelfAssessed]
            .into_iter()
            .find(|s| s.as_str() == v)
    }

    /// Can this tier establish success?
    pub const fn is_strong(self) -> bool {
        !matches!(self, Self::SelfAssessed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Passed,
    Failed,
}

impl Verdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Passed, Self::Failed]
            .into_iter()
            .find(|s| s.as_str() == v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    pub id: VerificationId,
    pub execution_id: ExecutionId,
    pub tier: OracleTier,
    pub verdict: Verdict,
    pub verifier: ActorRef,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Execution {
    pub id: ExecutionId,
    /// The capability (a procedural memory).
    pub capability_id: MemoryId,
    /// The revision current when the execution started.
    pub capability_revision: MemoryRevisionId,
    /// [`crate::ProceduralMemory::fingerprint`] of that revision.
    pub procedure_fingerprint: uuid::Uuid,
    pub scope: Scope,
    /// Set when the execution runs an evaluation case.
    #[serde(default)]
    pub case_id: Option<EvaluationCaseId>,
    /// Opaque class of the input (e.g. a structural signature or argument
    /// hash). Never raw input.
    pub input_class: String,
    pub actor: ActorRef,
    pub status: ExecutionStatus,
    #[serde(default)]
    pub abort_reason: Option<AbortReason>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub verifications: Vec<Verification>,
}

/// What one execution says about the procedure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Failure,
    /// Completed, but no verifier able to establish success has spoken.
    Unverified,
    /// Still running, within the timeout.
    Pending,
}

impl Execution {
    /// The outcome as of `now`. Aborts and runs past `timeout` are
    /// failures. Among verifications, the strongest tier present decides,
    /// and a failure at that tier wins; self-assessment can only fail.
    pub fn outcome(&self, now: OffsetDateTime, timeout: time::Duration) -> RunOutcome {
        match self.status {
            ExecutionStatus::Aborted => RunOutcome::Failure,
            ExecutionStatus::Started if now - self.started_at > timeout => RunOutcome::Failure,
            ExecutionStatus::Started => RunOutcome::Pending,
            ExecutionStatus::Completed => {
                let strongest = self
                    .verifications
                    .iter()
                    .filter(|v| v.tier.is_strong())
                    .map(|v| v.tier)
                    .min();
                match strongest {
                    Some(t) => {
                        let failed = self
                            .verifications
                            .iter()
                            .any(|v| v.tier == t && v.verdict == Verdict::Failed);
                        if failed {
                            RunOutcome::Failure
                        } else {
                            RunOutcome::Success
                        }
                    }
                    None if self
                        .verifications
                        .iter()
                        .any(|v| v.verdict == Verdict::Failed) =>
                    {
                        RunOutcome::Failure
                    }
                    None => RunOutcome::Unverified,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// The procedure should achieve its goal.
    Positive,
    /// The procedure should refuse or fail safe.
    Negative,
    /// Edge of the input domain.
    Boundary,
    /// A past production failure. Locked: every later version must pass.
    Regression,
}

impl CaseKind {
    pub const ALL: [Self; 4] = [
        Self::Positive,
        Self::Negative,
        Self::Boundary,
        Self::Regression,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Boundary => "boundary",
            Self::Regression => "regression",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaseOrigin {
    Authored,
    /// Materialized from an execution that completed but failed a strong
    /// verification (the failure ratchet).
    Ratchet {
        execution_id: ExecutionId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationCase {
    pub id: EvaluationCaseId,
    pub capability_id: MemoryId,
    pub scope: Scope,
    pub kind: CaseKind,
    pub input_class: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Locked cases cannot be removed, and must pass for any promotion.
    pub locked: bool,
    pub origin: CaseOrigin,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub fn validate_input_class(v: &str) -> Result<()> {
    if v.trim().is_empty() || v.chars().count() > MAX_INPUT_CLASS {
        return Err(Error::validation(format!(
            "input_class must be 1..={MAX_INPUT_CLASS} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_core::{TenantId, UserId};
    use time::macros::datetime;

    fn exec(status: ExecutionStatus, verifications: &[(OracleTier, Verdict)]) -> Execution {
        let id = ExecutionId::new_v7();
        let actor = ActorRef::User {
            user_id: UserId::new_v7(),
        };
        Execution {
            id,
            capability_id: MemoryId::new_v7(),
            capability_revision: MemoryRevisionId::new_v7(),
            procedure_fingerprint: uuid::Uuid::nil(),
            scope: Scope::tenant(TenantId::new_v7()),
            case_id: None,
            input_class: "x".into(),
            actor: actor.clone(),
            status,
            abort_reason: None,
            started_at: datetime!(2026-01-01 0:00 UTC),
            finished_at: None,
            verifications: verifications
                .iter()
                .map(|&(tier, verdict)| Verification {
                    id: VerificationId::new_v7(),
                    execution_id: id,
                    tier,
                    verdict,
                    verifier: actor.clone(),
                    detail: None,
                    recorded_at: datetime!(2026-01-01 0:00 UTC),
                })
                .collect(),
        }
    }

    #[test]
    fn outcome_rules() {
        use ExecutionStatus::*;
        use OracleTier::*;
        use Verdict::*;
        let now = datetime!(2026-01-01 0:30 UTC);
        let t = time::Duration::hours(1);
        let o = |s, v: &[_]| exec(s, v).outcome(now, t);
        assert_eq!(o(Aborted, &[]), RunOutcome::Failure);
        assert_eq!(o(Started, &[]), RunOutcome::Pending);
        assert_eq!(
            exec(Started, &[]).outcome(now + time::Duration::hours(2), t),
            RunOutcome::Failure,
            "a run that never finishes is a failure"
        );
        assert_eq!(o(Completed, &[]), RunOutcome::Unverified);
        assert_eq!(
            o(Completed, &[(SelfAssessed, Passed)]),
            RunOutcome::Unverified,
            "self-judgment never establishes success"
        );
        assert_eq!(o(Completed, &[(SelfAssessed, Failed)]), RunOutcome::Failure);
        assert_eq!(o(Completed, &[(Independent, Passed)]), RunOutcome::Success);
        assert_eq!(
            o(Completed, &[(Independent, Failed), (Deterministic, Passed)]),
            RunOutcome::Success,
            "the strongest tier decides"
        );
        assert_eq!(
            o(
                Completed,
                &[(Deterministic, Passed), (Deterministic, Failed)]
            ),
            RunOutcome::Failure,
            "a failure at the deciding tier wins"
        );
    }
}
