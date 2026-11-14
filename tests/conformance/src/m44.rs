//! M4.4 conformance: procedural learning.

use cs_core::{Error, MemoryId};
use cs_engine::{
    StatusChange,
    capabilities::{ChangeStage, FinishExecution, NewCase, StartExecution, VerifyExecution},
};
use cs_memory::{
    CapabilityStage, MemoryContent, ProceduralMemory,
    capability::{
        AbortReason, CaseKind, CaseOrigin, EvaluationCase, ExecutionStatus, OracleTier, Verdict,
    },
};
use time::Duration;

use crate::Harness;

async fn capability(h: &Harness) -> MemoryId {
    let mut req = h.fact(h.alice_scope(), "x");
    req.content = MemoryContent::Procedural(ProceduralMemory {
        capability_id: None,
        name: "rotate credentials".into(),
        preconditions: vec!["vault is reachable".into()],
        steps: vec![
            "issue new key".into(),
            "deploy".into(),
            "revoke old key".into(),
        ],
        expected_outcome: Some("services authenticate with the new key".into()),
        stage: CapabilityStage::Observed,
    });
    let m = h.engine.create_memory(&h.alice, req).await.unwrap();
    stage(h, m.id, CapabilityStage::Candidate).await.unwrap();
    m.id
}

async fn stage(h: &Harness, id: MemoryId, s: CapabilityStage) -> cs_core::Result<CapabilityStage> {
    h.engine
        .change_capability_stage(
            &h.alice,
            id,
            ChangeStage {
                stage: s,
                reason: "test".into(),
            },
        )
        .await
        .map(|m| match m.content {
            MemoryContent::Procedural(p) => p.stage,
            _ => unreachable!(),
        })
}

async fn current_stage(h: &Harness, id: MemoryId) -> CapabilityStage {
    h.engine
        .get_capability(&h.alice, id)
        .await
        .unwrap()
        .evaluation
        .stage
}

/// Run once: `Some(true)` completes and passes, `Some(false)` completes
/// and fails, `None` aborts with an error. Verified at `tier`.
async fn run(
    h: &Harness,
    id: MemoryId,
    class: &str,
    case: Option<&EvaluationCase>,
    ok: Option<bool>,
    tier: OracleTier,
) -> cs_core::ExecutionId {
    let e = h
        .engine
        .start_execution(
            &h.alice,
            id,
            StartExecution {
                input_class: class.into(),
                case_id: case.map(|c| c.id),
            },
        )
        .await
        .unwrap();
    let (status, abort_reason) = match ok {
        Some(_) => (ExecutionStatus::Completed, None),
        None => (ExecutionStatus::Aborted, Some(AbortReason::Error)),
    };
    h.engine
        .finish_execution(
            &h.alice,
            e.id,
            FinishExecution {
                status,
                abort_reason,
            },
        )
        .await
        .unwrap();
    if let Some(ok) = ok {
        h.engine
            .verify_execution(
                &h.alice,
                e.id,
                VerifyExecution {
                    tier,
                    verdict: if ok { Verdict::Passed } else { Verdict::Failed },
                    detail: None,
                },
            )
            .await
            .unwrap();
    }
    e.id
}

async fn case(h: &Harness, id: MemoryId, kind: CaseKind) -> EvaluationCase {
    h.engine
        .add_case(
            &h.alice,
            id,
            NewCase {
                kind,
                input_class: format!("case:{}", kind.as_str()),
                description: None,
            },
        )
        .await
        .unwrap()
}

/// A candidate with a passing suite (the negative case refuses on its
/// precondition) and `n` passing field runs over distinct input classes.
async fn proven(h: &Harness, n: usize, tier: OracleTier) -> MemoryId {
    let id = capability(h).await;
    for kind in [CaseKind::Positive, CaseKind::Boundary] {
        let c = case(h, id, kind).await;
        run(
            h,
            id,
            &c.input_class,
            Some(&c),
            Some(true),
            OracleTier::Deterministic,
        )
        .await;
    }
    let neg = case(h, id, CaseKind::Negative).await;
    let e = h
        .engine
        .start_execution(
            &h.alice,
            id,
            StartExecution {
                input_class: neg.input_class.clone(),
                case_id: Some(neg.id),
            },
        )
        .await
        .unwrap();
    h.engine
        .finish_execution(
            &h.alice,
            e.id,
            FinishExecution {
                status: ExecutionStatus::Aborted,
                abort_reason: Some(AbortReason::PreconditionFailed),
            },
        )
        .await
        .unwrap();
    for i in 0..n {
        run(h, id, &format!("input-{i}"), None, Some(true), tier).await;
    }
    id
}

pub async fn capabilities_earn_evaluation_with_independent_evidence(h: &Harness) {
    let bare = capability(h).await;
    assert!(matches!(
        stage(h, bare, CapabilityStage::Evaluated).await,
        Err(Error::Validation(_))
    ));

    let id = proven(h, 12, OracleTier::Independent).await;
    let view = h.engine.get_capability(&h.alice, id).await.unwrap();
    let e = &view.evaluation;
    assert_eq!(e.field.input_classes, 12);
    assert!(e.suite.iter().all(|c| c.passing), "{:?}", e.suite);
    assert_eq!(
        e.eligible,
        Some(CapabilityStage::Evaluated),
        "{:?}",
        e.blockers
    );
    assert_eq!(
        stage(h, id, CapabilityStage::Evaluated).await.unwrap(),
        CapabilityStage::Evaluated
    );
    // Thirty classes are needed for trust.
    assert!(stage(h, id, CapabilityStage::Trusted).await.is_err());

    // Evidence survives a stage-only revision (same procedure).
    let after = h.engine.get_capability(&h.alice, id).await.unwrap();
    assert_eq!(after.evaluation.field.input_classes, 12);

    // Repeating one input forty times is still one class.
    let rep = capability(h).await;
    for _ in 0..40 {
        run(h, rep, "same", None, Some(true), OracleTier::Deterministic).await;
    }
    let e = h
        .engine
        .get_capability(&h.alice, rep)
        .await
        .unwrap()
        .evaluation;
    assert_eq!((e.field.runs, e.field.input_classes), (40, 1));
}

pub async fn self_assessment_never_promotes(h: &Harness) {
    let id = proven(h, 12, OracleTier::SelfAssessed).await;
    let e = h
        .engine
        .get_capability(&h.alice, id)
        .await
        .unwrap()
        .evaluation;
    assert_eq!(e.field.unverified, 12);
    assert_eq!(e.field.input_classes, 0);
    assert_eq!(e.eligible, None);
    assert!(stage(h, id, CapabilityStage::Evaluated).await.is_err());
}

pub async fn failures_ratchet_into_locked_regressions(h: &Harness) {
    let id = proven(h, 12, OracleTier::Deterministic).await;
    stage(h, id, CapabilityStage::Evaluated).await.unwrap();

    // Completed, but the outcome check failed: a locked regression case.
    let failed = run(
        h,
        id,
        "edge-input",
        None,
        Some(false),
        OracleTier::Deterministic,
    )
    .await;
    run(
        h,
        id,
        "edge-input",
        None,
        Some(false),
        OracleTier::Deterministic,
    )
    .await;
    let view = h.engine.get_capability(&h.alice, id).await.unwrap();
    let ratchets: Vec<&EvaluationCase> = view
        .cases
        .iter()
        .filter(|c| c.kind == CaseKind::Regression)
        .collect();
    assert_eq!(ratchets.len(), 1, "one case per failing input class");
    let r = ratchets[0];
    assert!(r.locked);
    assert_eq!(
        r.origin,
        CaseOrigin::Ratchet {
            execution_id: failed
        }
    );
    assert_eq!(r.input_class, "edge-input");
    assert!(
        view.evaluation
            .blockers
            .iter()
            .any(|b| b.contains("locked")),
        "{:?}",
        view.evaluation.blockers
    );
    assert_eq!(current_stage(h, id).await, CapabilityStage::Evaluated);

    // A self-assessed failure counts as a failure but ratchets nothing.
    // It is also the third failure in a row: automatic quarantine.
    run(
        h,
        id,
        "other-input",
        None,
        Some(false),
        OracleTier::SelfAssessed,
    )
    .await;
    assert_eq!(current_stage(h, id).await, CapabilityStage::Quarantined);
    let view = h.engine.get_capability(&h.alice, id).await.unwrap();
    assert_eq!(
        view.cases
            .iter()
            .filter(|c| c.kind == CaseKind::Regression)
            .count(),
        1
    );

    // Passing the regression case clears the block.
    let r = view
        .cases
        .iter()
        .find(|c| c.kind == CaseKind::Regression)
        .unwrap()
        .clone();
    run(
        h,
        id,
        &r.input_class,
        Some(&r),
        Some(true),
        OracleTier::Deterministic,
    )
    .await;
    let e = h
        .engine
        .get_capability(&h.alice, id)
        .await
        .unwrap()
        .evaluation;
    assert!(e.suite.iter().all(|c| c.passing), "{:?}", e.suite);
}

pub async fn drift_quarantines_and_aborts_count(h: &Harness) {
    let id = proven(h, 12, OracleTier::Deterministic).await;
    stage(h, id, CapabilityStage::Evaluated).await.unwrap();

    // Aborted runs are failures; three in a row quarantine automatically.
    for i in 0..3 {
        run(
            h,
            id,
            &format!("late-{i}"),
            None,
            None,
            OracleTier::Deterministic,
        )
        .await;
    }
    assert_eq!(current_stage(h, id).await, CapabilityStage::Quarantined);
    let e = h
        .engine
        .get_capability(&h.alice, id)
        .await
        .unwrap()
        .evaluation;
    assert_eq!(e.field.aborted, 3);
    assert_eq!(e.consecutive_failures, 3);

    // Quarantine is not deprecation: reinstate as a candidate, never
    // straight back to evaluated.
    assert!(stage(h, id, CapabilityStage::Evaluated).await.is_err());
    assert_eq!(
        stage(h, id, CapabilityStage::Candidate).await.unwrap(),
        CapabilityStage::Candidate
    );

    // A run that never reports back is a failure once it times out.
    h.engine
        .start_execution(
            &h.alice,
            id,
            StartExecution {
                input_class: "silent".into(),
                case_id: None,
            },
        )
        .await
        .unwrap();
    let before = h
        .engine
        .get_capability(&h.alice, id)
        .await
        .unwrap()
        .evaluation;
    assert_eq!(before.field.pending, 1);
    h.clock.advance(Duration::hours(2));
    let after = h
        .engine
        .get_capability(&h.alice, id)
        .await
        .unwrap()
        .evaluation;
    assert_eq!(after.field.pending, 0);
    assert_eq!(after.field.failures, before.field.failures + 1);

    // Deprecation is terminal and stops new executions.
    stage(h, id, CapabilityStage::Deprecated).await.unwrap();
    assert!(stage(h, id, CapabilityStage::Candidate).await.is_err());
    assert!(matches!(
        h.engine
            .start_execution(
                &h.alice,
                id,
                StartExecution {
                    input_class: "x".into(),
                    case_id: None,
                },
            )
            .await,
        Err(Error::Conflict(_))
    ));

    // Bob can neither see nor run Alice's capability.
    assert!(matches!(
        h.engine.get_capability(&h.bob, id).await,
        Err(Error::NotFound { .. })
    ));

    // Executions and cases go with the capability on purge.
    let any = h
        .engine
        .list_executions(&h.alice, id, Some(1))
        .await
        .unwrap()[0]
        .id;
    h.engine
        .purge_memory(
            &h.alice,
            id,
            StatusChange {
                reason: "erasure".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        h.engine.get_execution(&h.alice, any).await,
        Err(Error::NotFound { .. })
    ));
}
