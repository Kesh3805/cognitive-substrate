//! M5.4 conformance: information needs and knowledge gaps.

use cs_core::{Error, ExtractionInfo};
use cs_engine::{
    RecallRequest,
    needs::{AskQuestion, NeedChange},
};
use cs_memory::{
    RecallPolicy,
    need::{GapValidationOutcome, NeedOrigin, NeedStatus},
};
use time::{Duration, OffsetDateTime};

use crate::Harness;

fn recording(query: &str, session: &str) -> RecallRequest {
    RecallRequest {
        query: query.into(),
        policy: RecallPolicy {
            record_information_needs: true,
            ..RecallPolicy::default()
        },
        session: Some(session.into()),
    }
}

async fn remember(h: &Harness, text: &str) {
    h.engine
        .create_memory(&h.alice, h.fact(h.alice_scope(), text))
        .await
        .unwrap();
}

fn ask(q: &str) -> AskQuestion {
    AskQuestion {
        question: q.into(),
        impact: 1,
        cost: 1,
    }
}

pub async fn misses_become_needs_and_evidence_satisfies_them(h: &Harness) {
    for t in [
        "Billing stores invoices in Cassandra",
        "The gateway runs in eu-west-1",
        "Search indexes products nightly",
    ] {
        remember(h, t).await;
    }
    h.tick(1);
    let q = "where does helios store its data";

    // Reads never write unless asked.
    let plain = h
        .engine
        .recall(
            &h.alice,
            RecallRequest {
                query: q.into(),
                policy: RecallPolicy::default(),
                session: None,
            },
        )
        .await
        .unwrap();
    assert!(plain.information_need.is_none());
    assert!(
        h.engine
            .list_needs(&h.alice, None, None)
            .await
            .unwrap()
            .is_empty()
    );

    // A miss records a retrieval failure; the same session again only
    // adds an occurrence; a second session makes it unresolved.
    let first = h.engine.recall(&h.alice, recording(q, "a")).await.unwrap();
    let o = first.information_need.unwrap();
    assert!(o.missed);
    assert_eq!(o.status, NeedStatus::RetrievalFailure);
    h.engine.recall(&h.alice, recording(q, "a")).await.unwrap();
    let n = h.engine.get_need(&h.alice, o.need_id).await.unwrap().need;
    assert_eq!((n.sessions, n.occurrences), (1, 2));
    assert_eq!(n.origin, NeedOrigin::Observed);
    assert!(n.question.is_none(), "observed needs keep terms, not text");
    let third = h.engine.recall(&h.alice, recording(q, "b")).await.unwrap();
    assert_eq!(
        third.information_need.unwrap().status,
        NeedStatus::Unresolved
    );

    // A historical recall neither records nor satisfies.
    let past = h
        .engine
        .recall(
            &h.alice,
            RecallRequest {
                policy: RecallPolicy {
                    as_of_transaction_time: Some(OffsetDateTime::now_utc() - Duration::days(400)),
                    record_information_needs: true,
                    ..RecallPolicy::default()
                },
                ..recording(q, "c")
            },
        )
        .await
        .unwrap();
    assert!(past.information_need.is_none());

    // Bob sees nothing of Alice's needs.
    assert!(
        h.engine
            .list_needs(&h.bob, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        h.engine.get_need(&h.bob, o.need_id).await,
        Err(Error::NotFound { .. })
    ));

    // Evidence arrives: the next recall that opted in satisfies the need.
    h.tick(1);
    remember(h, "Helios stores its data in Postgres").await;
    h.tick(1);
    let found = h.engine.recall(&h.alice, recording(q, "d")).await.unwrap();
    let o = found.information_need.unwrap();
    assert!(!o.missed);
    assert_eq!(o.status, NeedStatus::Satisfied);
    let n = h.engine.get_need(&h.alice, o.need_id).await.unwrap().need;
    assert_eq!(n.resolved_by.len(), 1);
    assert_eq!(n.resolved_by[0].memory_id, found.results[0].memory.id);
}

pub async fn validation_is_complete_or_says_so(h: &Harness) {
    for _ in 0..6 {
        remember(h, "The current deployment status is green").await;
    }
    remember(h, "Ledger exports run every night").await;
    let mut unvalidated = h.fact(h.alice_scope(), "Atlas replicates data to three regions");
    unvalidated.provenance.extraction = Some(ExtractionInfo {
        model: None,
        model_version: None,
        prompt_version: None,
        extractor_version: "test".into(),
    });
    h.engine.create_memory(&h.alice, unvalidated).await.unwrap();
    h.tick(1);

    // Asking establishes demand, not absence.
    let gap = h
        .engine
        .ask(
            &h.alice,
            ask("What is the current deployment status of helios?"),
        )
        .await
        .unwrap();
    assert_eq!(gap.need.status, NeedStatus::Unresolved);
    assert_eq!(gap.need.origin, NeedOrigin::Explicit);

    // Framing words alone ("current deployment status") are not evidence:
    // helios appears nowhere, so the complete search finds nothing. The
    // detector does not propose a gap (experiment 008); status is kept.
    let v = h.engine.validate_need(&h.alice, gap.need.id).await.unwrap();
    assert_eq!(v.need.status, NeedStatus::Unresolved);
    assert_eq!(
        v.need.validation.as_ref().unwrap().outcome,
        GapValidationOutcome::CompleteNoEvidence
    );
    // A person may confirm a gap only after such a search.
    let unsearched = h
        .engine
        .ask(&h.alice, ask("Who audits the kiln registry?"))
        .await
        .unwrap();
    assert!(matches!(
        h.engine
            .confirm_gap(
                &h.alice,
                unsearched.need.id,
                NeedChange {
                    reason: "sure".into()
                }
            )
            .await,
        Err(Error::Conflict(_))
    ));
    let c = h
        .engine
        .confirm_gap(
            &h.alice,
            gap.need.id,
            NeedChange {
                reason: "checked the runbooks".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(c.need.status, NeedStatus::ValidatedGap);

    // Evidence present: satisfied.
    let present = h
        .engine
        .ask(&h.alice, ask("How often do ledger exports run?"))
        .await
        .unwrap();
    let v = h
        .engine
        .validate_need(&h.alice, present.need.id)
        .await
        .unwrap();
    assert_eq!(v.need.status, NeedStatus::Satisfied);

    // Evidence only in an unvalidated memory: pending promotion.
    let pending = h
        .engine
        .ask(&h.alice, ask("Which regions does atlas replicate data to?"))
        .await
        .unwrap();
    let v = h
        .engine
        .validate_need(&h.alice, pending.need.id)
        .await
        .unwrap();
    assert_eq!(v.need.status, NeedStatus::PendingPromotion);
    assert!(matches!(
        v.need.validation.unwrap().outcome,
        GapValidationOutcome::CompleteEvidenceFound {
            unvalidated_only: true,
            ..
        }
    ));

    // Priority ranks open needs above closed ones.
    let list = h.engine.list_needs(&h.alice, None, None).await.unwrap();
    assert_eq!(list.len(), 4);
    assert_eq!(
        list[0].need.id, gap.need.id,
        "the validated gap ranks first"
    );
    assert_eq!(list.last().unwrap().priority.priority, 0.0);

    // Erasure: needs are purged directly.
    h.engine.purge_need(&h.alice, gap.need.id).await.unwrap();
    assert!(matches!(
        h.engine.get_need(&h.alice, gap.need.id).await,
        Err(Error::NotFound { .. })
    ));
}
