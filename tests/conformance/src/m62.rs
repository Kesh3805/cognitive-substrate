//! M6.2 conformance: causal hypotheses and interventions.

use cs_core::Error;
use cs_engine::causal::{NewHypothesis, NewIntervention, ObserveOutcome};
use cs_memory::{
    capability::OracleTier,
    causal::{Arm, Assignment, Direction, HypothesisStatus},
};

use crate::Harness;

fn hypothesis(h: &Harness) -> NewHypothesis {
    NewHypothesis {
        scope: h.alice_scope(),
        environment: "deploy@1".into(),
        treatment: "warm-cache".into(),
        outcome: "fast".into(),
        direction: Direction::Raises,
        min_effect: 0.1,
        contexts: Vec::new(),
        statement: Some("warming the cache makes deploys fast".into()),
    }
}

fn intervention(h: &Harness, context: &str, arm: Arm, assignment: Assignment) -> NewIntervention {
    let randomized = assignment == Assignment::Randomized;
    NewIntervention {
        scope: h.alice_scope(),
        environment: "deploy@1".into(),
        treatment: "warm-cache".into(),
        arm,
        assignment,
        assignment_detail: randomized.then(|| "seed 7".into()),
        propensity: randomized.then_some(0.5),
        context: context.into(),
        state: Default::default(),
        execution_id: None,
    }
}

/// Record `per_arm` runs per arm in each context: treated always `true`,
/// control always `false`.
async fn trial(h: &Harness, contexts: usize, per_arm: usize, assignment: Assignment) {
    for c in 0..contexts {
        for _ in 0..per_arm {
            for (arm, value) in [(Arm::Treated, true), (Arm::Control, false)] {
                h.tick(1);
                let i = h
                    .engine
                    .record_intervention(
                        &h.alice,
                        intervention(h, &format!("class-{c}"), arm, assignment),
                    )
                    .await
                    .unwrap();
                h.engine
                    .observe_outcome(
                        &h.alice,
                        i.id,
                        ObserveOutcome {
                            outcome: "fast".into(),
                            value,
                            tier: OracleTier::Deterministic,
                        },
                    )
                    .await
                    .unwrap();
            }
        }
    }
}

/// Passive evidence is reported but never decides; randomized evidence
/// does, within the sampled contexts; every evaluation is kept in the
/// hypothesis's history.
pub async fn hypotheses_follow_randomized_evidence(h: &Harness) {
    let hyp = h
        .engine
        .create_hypothesis(&h.alice, hypothesis(h))
        .await
        .unwrap();
    assert_eq!((hyp.status, hyp.version), (HypothesisStatus::Proposed, 1));

    // Strong passive correlation: still proposed, with the lift reported.
    trial(h, 6, 3, Assignment::Passive).await;
    h.tick(1);
    let hyp = h
        .engine
        .evaluate_hypothesis(&h.alice, hyp.id, 1)
        .await
        .unwrap();
    let e = hyp.evaluation.clone().unwrap();
    assert_eq!(hyp.status, HypothesisStatus::Proposed);
    assert!(e.observational.unwrap().lower > 0.1);
    assert!(e.interventions.is_empty() && e.effect.is_none());

    // Another user's randomized evidence in their own scope does not count.
    let mut foreign = intervention(h, "class-0", Arm::Treated, Assignment::Randomized);
    foreign.scope = cs_core::Scope::private(h.tenant, h.bob.user_id.unwrap());
    assert!(matches!(
        h.engine
            .record_intervention(&h.alice, foreign.clone())
            .await,
        Err(Error::Forbidden(_))
    ));
    h.engine.record_intervention(&h.bob, foreign).await.unwrap();

    // Randomized evidence decides.
    trial(h, 6, 3, Assignment::Randomized).await;
    h.tick(1);
    let hyp = h
        .engine
        .evaluate_hypothesis(&h.alice, hyp.id, 2)
        .await
        .unwrap();
    let e = hyp.evaluation.clone().unwrap();
    assert_eq!(hyp.status, HypothesisStatus::Supported, "{e:?}");
    assert_eq!(e.eligible_contexts, 6);
    assert_eq!(e.scope_contexts.len(), 6);
    assert_eq!(e.interventions.len(), 36);
    assert!(e.effect.unwrap().lower > 0.1);

    // Counterfactuals are computed on read, labelled inferred, and leave
    // nothing behind.
    let events_before = h
        .engine
        .events(
            &h.alice,
            cs_core::EventQuery {
                limit: Some(1000),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len();
    let mut treated = None;
    for id in &e.interventions {
        if h.engine.get_intervention(&h.alice, *id).await.unwrap().arm == Arm::Treated {
            treated = Some(*id);
            break;
        }
    }
    let treated = treated.unwrap();
    let cf = h
        .engine
        .counterfactual(&h.alice, treated, hyp.id)
        .await
        .unwrap();
    assert_eq!(cf.kind, "inferred");
    assert_eq!(cf.hypothesis_version, 3);
    match &cf.result {
        cs_engine::causal::CounterfactualResult::Bounds { bounds } => {
            assert_eq!(bounds.query, cs_policy::counterfactual::Query::Necessity);
            assert!(bounds.lower >= 0.0 && bounds.upper <= 1.0 && bounds.lower <= bounds.upper);
        }
        other => panic!("{other:?}"),
    }
    let passive = h
        .engine
        .record_intervention(
            &h.alice,
            intervention(h, "class-0", Arm::Treated, Assignment::Passive),
        )
        .await
        .unwrap();
    let refused = h
        .engine
        .counterfactual(&h.alice, passive.id, hyp.id)
        .await
        .unwrap();
    assert!(matches!(
        refused.result,
        cs_engine::causal::CounterfactualResult::Refused {
            reason: cs_policy::counterfactual::Refusal::NotRandomized
        }
    ));
    let events_after = h
        .engine
        .events(
            &h.alice,
            cs_core::EventQuery {
                limit: Some(1000),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len();
    assert_eq!(
        events_after,
        events_before + 1,
        "only the passive intervention was recorded"
    );

    // History keeps every version, with the evaluation that made it.
    let history = h.engine.hypothesis_history(&h.alice, hyp.id).await.unwrap();
    assert_eq!(
        history
            .iter()
            .map(|v| (v.version, v.status))
            .collect::<Vec<_>>(),
        vec![
            (1, HypothesisStatus::Proposed),
            (2, HypothesisStatus::Proposed),
            (3, HypothesisStatus::Supported)
        ]
    );
    let listed = h
        .engine
        .list_hypotheses(&h.alice, Some(HypothesisStatus::Supported), 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    // Stale versions conflict; other users cannot see it.
    assert!(matches!(
        h.engine.evaluate_hypothesis(&h.alice, hyp.id, 2).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        h.engine.get_hypothesis(&h.bob, hyp.id).await,
        Err(Error::NotFound { .. })
    ));

    // Outcomes are recorded once per key.
    let i = h
        .engine
        .record_intervention(
            &h.alice,
            intervention(h, "class-0", Arm::Treated, Assignment::Randomized),
        )
        .await
        .unwrap();
    let observe = |value| ObserveOutcome {
        outcome: "fast".into(),
        value,
        tier: OracleTier::Independent,
    };
    h.engine
        .observe_outcome(&h.alice, i.id, observe(true))
        .await
        .unwrap();
    assert!(matches!(
        h.engine
            .observe_outcome(&h.alice, i.id, observe(false))
            .await,
        Err(Error::Conflict(_))
    ));

    // A randomized assignment must record its draw and propensity.
    let mut bad = intervention(h, "class-0", Arm::Treated, Assignment::Randomized);
    bad.propensity = None;
    assert!(matches!(
        h.engine.record_intervention(&h.alice, bad).await,
        Err(Error::Validation(_))
    ));

    // Retiring is terminal.
    let hyp = h
        .engine
        .retire_hypothesis(&h.alice, hyp.id, 3)
        .await
        .unwrap();
    assert_eq!(hyp.status, HypothesisStatus::Retired);
    assert!(matches!(
        h.engine.evaluate_hypothesis(&h.alice, hyp.id, 4).await,
        Err(Error::Validation(_))
    ));
}
