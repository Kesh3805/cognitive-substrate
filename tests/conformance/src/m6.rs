//! M6.1 conformance: answer, investigate or abstain.

use cs_core::{Error, ExtractionInfo};
use cs_engine::{NewMemory, RecallRequest, ReplayRequest};
use cs_memory::{
    CandidateGeneration, Decision, MemoryContent, MetacognitivePolicy, Reason, RecallPolicy,
    SemanticMemory,
};

use crate::Harness;

fn triple(h: &Harness, subject: &str, predicate: &str, object: &str) -> NewMemory {
    let mut m = h.fact(
        h.alice_scope(),
        &format!("{subject} invoices {predicate} {object}"),
    );
    m.content = MemoryContent::Semantic(SemanticMemory {
        proposition: format!("{subject} invoices {predicate} {object}"),
        subject: Some(format!("{subject} invoices")),
        predicate: Some(predicate.into()),
        object: Some(object.into()),
    });
    m
}

fn deciding(query: &str, min_margin: Option<f64>, include_candidates: bool) -> RecallRequest {
    RecallRequest {
        query: query.into(),
        policy: RecallPolicy {
            candidates: CandidateGeneration::Channels {
                lexical: 100,
                dense: 0,
            },
            include_candidates,
            metacognition: Some(MetacognitivePolicy { min_margin }),
            ..RecallPolicy::default()
        },
        session: None,
    }
}

/// Covered, validated, agreeing evidence answers; uncovered questions
/// abstain; candidate-only or contradicting evidence is investigated. The
/// decision is in the trace and replays exactly.
pub async fn recalls_answer_investigate_or_abstain(h: &Harness) {
    for text in [
        "Deploys happen on Fridays",
        "Standup moved to ten",
        "Lunch is at noon",
        "The search team indexes products nightly",
    ] {
        h.tick(1);
        h.engine
            .create_memory(&h.alice, h.fact(h.alice_scope(), text))
            .await
            .unwrap();
    }
    h.tick(1);
    let cassandra = h
        .engine
        .create_memory(&h.alice, triple(h, "billing", "live in", "cassandra"))
        .await
        .unwrap();
    let mut unvalidated = h.fact(h.alice_scope(), "Atlas replicates snapshots to Oslo");
    unvalidated.provenance.extraction = Some(ExtractionInfo {
        model: None,
        model_version: None,
        prompt_version: None,
        extractor_version: "test".into(),
    });
    h.engine.create_memory(&h.alice, unvalidated).await.unwrap();
    h.tick(1);

    // Covered, validated, alone: answer.
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("where do billing invoices live", None, false),
        )
        .await
        .unwrap();
    let d = r.metacognition.clone().unwrap();
    assert_eq!(d.decision, Decision::Answer, "{d:?}");
    assert!(d.reasons.is_empty());
    assert_eq!(d.signals.covering_active, 1);
    let stored = h.engine.get_trace(&h.alice, r.trace_id).await.unwrap();
    assert_eq!(stored.metacognition.as_ref(), Some(&d));
    let replay = h
        .engine
        .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(replay.identical);
    assert_eq!(replay.replay.metacognition.as_ref(), Some(&d));

    // A margin demand the evidence cannot meet: investigate.
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("where do billing invoices live", Some(1.0), false),
        )
        .await
        .unwrap();
    let d = r.metacognition.unwrap();
    assert_eq!(
        (d.decision, d.reasons),
        (Decision::Investigate, vec![Reason::NarrowMargin])
    );

    // Nothing selected covers the question: abstain.
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("which team owns the kraken payroll service", None, false),
        )
        .await
        .unwrap();
    let d = r.metacognition.unwrap();
    assert_eq!(d.decision, Decision::Abstain, "{d:?}");
    assert!(d.signals.selected > 0, "recency still fills the candidates");

    // About billing invoices, but not about who maintains them: investigate.
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("who maintains billing invoices", None, false),
        )
        .await
        .unwrap();
    let d = r.metacognition.unwrap();
    assert_eq!(
        (d.decision, d.reasons.clone()),
        (Decision::Investigate, vec![Reason::AspectUnmatched]),
        "{d:?}"
    );
    assert!(d.signals.anchored >= 1);

    // Only an unvalidated memory covers it: investigate.
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("where does atlas replicate snapshots", None, true),
        )
        .await
        .unwrap();
    let d = r.metacognition.unwrap();
    assert_eq!(
        (d.decision, d.reasons.clone()),
        (Decision::Investigate, vec![Reason::OnlyUnvalidated]),
        "{d:?}"
    );
    // Without candidates in the search, nothing covers it.
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("where does atlas replicate snapshots", None, false),
        )
        .await
        .unwrap();
    assert_eq!(r.metacognition.unwrap().decision, Decision::Abstain);

    // Validated evidence that disagrees: investigate, naming the pair.
    h.tick(1);
    let postgres = h
        .engine
        .create_memory(&h.alice, triple(h, "billing", "live in", "postgres"))
        .await
        .unwrap();
    h.tick(1);
    let r = h
        .engine
        .recall(
            &h.alice,
            deciding("where do billing invoices live", None, false),
        )
        .await
        .unwrap();
    let d = r.metacognition.unwrap();
    assert_eq!(
        (d.decision, d.reasons.clone()),
        (Decision::Investigate, vec![Reason::Contradiction]),
        "{d:?}"
    );
    let mut pair = [cassandra.id, postgres.id];
    pair.sort();
    assert_eq!(d.signals.contradictions, vec![(pair[0], pair[1])]);

    // Not asked: no decision anywhere.
    let r = h
        .engine
        .recall(
            &h.alice,
            RecallRequest {
                query: "where do billing invoices live".into(),
                policy: RecallPolicy::default(),
                session: None,
            },
        )
        .await
        .unwrap();
    assert!(r.metacognition.is_none());
    assert!(
        h.engine
            .get_trace(&h.alice, r.trace_id)
            .await
            .unwrap()
            .metacognition
            .is_none()
    );

    // Out-of-range parameters are refused.
    let err = h
        .engine
        .recall(&h.alice, deciding("billing", Some(2.0), false))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err:?}");
}
