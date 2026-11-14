//! M5 conformance: candidate generation.

use cs_engine::{RecallRequest, ReplayRequest};
use cs_memory::{CandidateChannel, CandidateGeneration, RecallPolicy};

use crate::Harness;

fn policy(candidates: CandidateGeneration) -> RecallPolicy {
    RecallPolicy {
        max_candidates: 5,
        max_results: 3,
        min_score: 0.05,
        candidates,
        ..RecallPolicy::default()
    }
}

pub async fn channels_reach_beyond_the_recency_window(h: &Harness) {
    let old = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "Billing stores invoices in Cassandra"),
        )
        .await
        .unwrap();
    for i in 0..10 {
        h.tick(1);
        h.engine
            .create_memory(
                &h.alice,
                h.fact(h.alice_scope(), &format!("Deploy window {i} is on Friday")),
            )
            .await
            .unwrap();
    }
    // Bob's identical fact must never become Alice's candidate.
    h.engine
        .create_memory(
            &h.bob,
            cs_engine::NewMemory {
                scope: cs_core::Scope::private(h.tenant, h.bob.user_id.unwrap()),
                ..h.fact(h.alice_scope(), "Billing stores invoices in Cassandra")
            },
        )
        .await
        .unwrap();
    h.tick(1);
    let query = "where does billing store invoices";
    let recall = |c| RecallRequest {
        query: query.into(),
        policy: policy(c),
    };

    // v1 (frozen control): the old fact is outside the newest five.
    let v1 = h
        .engine
        .recall(&h.alice, recall(CandidateGeneration::RecencyV1))
        .await
        .unwrap();
    assert!(v1.results.iter().all(|r| r.memory.id != old.id));
    assert_eq!(v1.trace.candidates_considered, 5);

    for (strategy, channel) in [
        (
            CandidateGeneration::Channels {
                lexical: 2,
                dense: 0,
            },
            CandidateChannel::Lexical,
        ),
        (
            CandidateGeneration::Channels {
                lexical: 0,
                dense: 2,
            },
            CandidateChannel::Dense,
        ),
    ] {
        let r = h.engine.recall(&h.alice, recall(strategy)).await.unwrap();
        assert_eq!(r.trace.candidates_considered, 5, "equal budget");
        assert_eq!(r.results[0].memory.id, old.id, "{strategy:?}");
        let trace = h.engine.get_trace(&h.alice, r.trace_id).await.unwrap();
        let c = trace
            .candidates
            .iter()
            .find(|c| c.memory_id == old.id)
            .unwrap();
        assert!(c.channels.contains(&channel), "{:?}", c.channels);
        assert!(
            trace.candidates.iter().all(|c| !c.channels.is_empty()),
            "every candidate names its channel"
        );

        // Exact replay reproduces the same selection.
        let replay = h
            .engine
            .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
            .await
            .unwrap();
        assert!(replay.identical);
    }

    // Quotas cannot exceed the budget.
    assert!(
        h.engine
            .recall(
                &h.alice,
                recall(CandidateGeneration::Channels {
                    lexical: 4,
                    dense: 4,
                }),
            )
            .await
            .is_err()
    );
}
