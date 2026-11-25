//! M6.4 conformance: bounded predictions.

use cs_core::{Error, EventQuery, HypothesisId};
use cs_engine::{
    causal::{NewHypothesis, NewIntervention, ObserveOutcome},
    predictions::{PredictionResult, RuleResult},
};
use cs_memory::{
    capability::OracleTier,
    causal::{Arm, Assignment, Direction, HypothesisStatus, Intervention},
};
use cs_policy::prediction::{GateReason, RuleRefusal};

use crate::Harness;

const ENV: &str = "deploy@1";
const TREATMENT: &str = "warm-cache";
const OUTCOME: &str = "fast";

fn intervention(h: &Harness, context: &str, arm: Arm, assignment: Assignment) -> NewIntervention {
    let randomized = assignment == Assignment::Randomized;
    NewIntervention {
        scope: h.alice_scope(),
        environment: ENV.into(),
        treatment: TREATMENT.into(),
        arm,
        assignment,
        assignment_detail: randomized.then(|| "seed 11".into()),
        propensity: randomized.then_some(0.5),
        context: context.into(),
        state: Default::default(),
        execution_id: None,
    }
}

async fn record(h: &Harness, context: &str, arm: Arm, assignment: Assignment) -> Intervention {
    h.tick(1);
    h.engine
        .record_intervention(&h.alice, intervention(h, context, arm, assignment))
        .await
        .unwrap()
}

async fn observe(h: &Harness, i: &Intervention, value: bool) {
    h.engine
        .observe_outcome(
            &h.alice,
            i.id,
            ObserveOutcome {
                outcome: OUTCOME.into(),
                value,
                tier: OracleTier::Deterministic,
            },
        )
        .await
        .unwrap();
}

/// A hypothesis supported by 25 randomized outcomes per arm in each of six
/// contexts (treated always fast, control never).
async fn supported(h: &Harness) -> HypothesisId {
    let hyp = h
        .engine
        .create_hypothesis(
            &h.alice,
            NewHypothesis {
                scope: h.alice_scope(),
                environment: ENV.into(),
                treatment: TREATMENT.into(),
                outcome: OUTCOME.into(),
                direction: Direction::Raises,
                min_effect: 0.1,
                contexts: Vec::new(),
                statement: None,
            },
        )
        .await
        .unwrap();
    for c in 0..6 {
        for _ in 0..25 {
            for (arm, value) in [(Arm::Treated, true), (Arm::Control, false)] {
                let i = record(h, &format!("class-{c}"), arm, Assignment::Randomized).await;
                observe(h, &i, value).await;
            }
        }
    }
    let hyp = h
        .engine
        .evaluate_hypothesis(&h.alice, hyp.id, 1)
        .await
        .unwrap();
    assert_eq!(hyp.status, HypothesisStatus::Supported);
    hyp.id
}

async fn event_count(h: &Harness) -> usize {
    h.engine
        .events(
            &h.alice,
            EventQuery {
                limit: Some(10_000),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len()
}

async fn predict(h: &Harness, i: &Intervention, hyp: HypothesisId) -> PredictionResult {
    h.engine.predict(&h.alice, i.id, hyp).await.unwrap()
}

fn refused(r: &PredictionResult) -> Option<RuleRefusal> {
    match r {
        PredictionResult::Refused { reason } => Some(*reason),
        PredictionResult::Recorded { .. } => None,
    }
}

/// Rules are computed on read and gate fail-closed; predictions are made at
/// intent, once; an over-optimistic rule is demoted by its own scored
/// predictions (silence counts as failure); re-evaluating the hypothesis
/// does not clear a demotion; only fresh randomized evidence re-earns it.
pub async fn predictions_are_scored_and_demote_their_rules(h: &Harness) {
    let hyp = supported(h).await;

    // A rule, on read, writes nothing.
    let before = event_count(h).await;
    let view = h
        .engine
        .prediction_rule(&h.alice, hyp, "class-0", Some(0.8))
        .await
        .unwrap();
    assert_eq!(view.kind, "inferred");
    let RuleResult::Rule { rule } = view.result else {
        panic!("expected a rule: {view:?}")
    };
    assert!(rule.lower > 0.8 && !rule.requalified && rule.treated_n == 25);
    assert!(view.gate.unwrap().execute);
    let out = h
        .engine
        .prediction_rule(&h.alice, hyp, "class-unseen", Some(0.0))
        .await
        .unwrap();
    assert_eq!(
        out.gate.unwrap().reason,
        GateReason::NoRule(RuleRefusal::OutOfScope),
        "the gate fails closed"
    );
    assert!(!out.gate.unwrap().execute);
    assert_eq!(event_count(h).await, before);

    // Other users cannot read the rule.
    assert!(matches!(
        h.engine.prediction_rule(&h.bob, hyp, "class-0", None).await,
        Err(Error::NotFound { .. })
    ));

    // Predictions: treated only, at intent, once.
    let control = record(h, "class-0", Arm::Control, Assignment::Passive).await;
    assert_eq!(
        refused(&predict(h, &control, hyp).await),
        Some(RuleRefusal::NotTreated)
    );
    let i = record(h, "class-0", Arm::Treated, Assignment::Passive).await;
    let PredictionResult::Recorded { prediction } = predict(h, &i, hyp).await else {
        panic!("expected a prediction")
    };
    assert!(prediction.favourable && (prediction.lower - rule.lower).abs() < 1e-12);
    assert!(matches!(
        h.engine.predict(&h.alice, i.id, hyp).await,
        Err(Error::Conflict(_))
    ));
    observe(h, &i, true).await;
    let late = record(h, "class-0", Arm::Treated, Assignment::Passive).await;
    observe(h, &late, true).await;
    assert!(
        matches!(
            h.engine.predict(&h.alice, late.id, hyp).await,
            Err(Error::Conflict(_))
        ),
        "no prediction after the outcome is known"
    );

    // The regime shifts: the cache no longer helps in class-0.
    let mut demoted_after = None;
    for n in 1..=20 {
        let i = record(h, "class-0", Arm::Treated, Assignment::Passive).await;
        if refused(&predict(h, &i, hyp).await).is_some() {
            demoted_after = Some(n);
            break;
        }
        h.tick(1);
        observe(h, &i, false).await;
    }
    let demoted_after = demoted_after.expect("the rule is demoted");
    assert!(demoted_after <= 12, "demoted after {demoted_after}");
    let view = h
        .engine
        .prediction_rule(&h.alice, hyp, "class-0", Some(0.0))
        .await
        .unwrap();
    assert_eq!(view.monitor.demotions.len(), 1);
    assert!(matches!(
        view.result,
        RuleResult::Refused {
            reason: RuleRefusal::Demoted
        }
    ));
    assert!(!view.gate.unwrap().execute);

    // Re-evaluating the hypothesis does not launder the demotion.
    let current = h.engine.get_hypothesis(&h.alice, hyp).await.unwrap();
    h.engine
        .evaluate_hypothesis(&h.alice, hyp, current.version)
        .await
        .unwrap();
    let view = h
        .engine
        .prediction_rule(&h.alice, hyp, "class-0", None)
        .await
        .unwrap();
    assert!(matches!(
        view.result,
        RuleResult::Refused {
            reason: RuleRefusal::Demoted
        }
    ));

    // Nor does stating the same claim as a new hypothesis: demotions attach
    // to the claim's lineage, not to a hypothesis record.
    let twin = h
        .engine
        .create_hypothesis(
            &h.alice,
            NewHypothesis {
                scope: h.alice_scope(),
                environment: ENV.into(),
                treatment: TREATMENT.into(),
                outcome: OUTCOME.into(),
                direction: Direction::Raises,
                min_effect: 0.1,
                contexts: Vec::new(),
                statement: Some("the same claim, restated".into()),
            },
        )
        .await
        .unwrap();
    let twin = h
        .engine
        .evaluate_hypothesis(&h.alice, twin.id, 1)
        .await
        .unwrap();
    assert_eq!(twin.status, HypothesisStatus::Supported);
    let view = h
        .engine
        .prediction_rule(&h.alice, twin.id, "class-0", Some(0.0))
        .await
        .unwrap();
    assert!(matches!(
        view.result,
        RuleResult::Refused {
            reason: RuleRefusal::Demoted
        }
    ));
    assert_eq!(view.monitor.demotions.len(), 1);

    // Silence is failure: predictions in class-1 whose outcomes never come.
    for _ in 0..12 {
        let i = record(h, "class-1", Arm::Treated, Assignment::Passive).await;
        if refused(&predict(h, &i, hyp).await).is_some() {
            break;
        }
    }
    h.tick(61);
    let view = h
        .engine
        .prediction_rule(&h.alice, hyp, "class-1", None)
        .await
        .unwrap();
    assert_eq!(view.monitor.demotions.len(), 1, "{:?}", view.monitor);

    // Passive successes do not re-earn class-0; fresh randomized ones do.
    for _ in 0..25 {
        let i = record(h, "class-0", Arm::Treated, Assignment::Passive).await;
        observe(h, &i, true).await;
    }
    let view = h
        .engine
        .prediction_rule(&h.alice, hyp, "class-0", None)
        .await
        .unwrap();
    assert!(matches!(
        view.result,
        RuleResult::Refused {
            reason: RuleRefusal::Demoted
        }
    ));
    for _ in 0..20 {
        let i = record(h, "class-0", Arm::Treated, Assignment::Randomized).await;
        observe(h, &i, true).await;
    }
    let i = record(h, "class-0", Arm::Treated, Assignment::Passive).await;
    let PredictionResult::Recorded { prediction } = predict(h, &i, hyp).await else {
        panic!("re-earned from fresh randomized evidence")
    };
    assert!(prediction.requalified && prediction.treated_n == 20);
}
