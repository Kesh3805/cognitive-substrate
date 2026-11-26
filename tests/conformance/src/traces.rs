//! Recall traces: who may read them, and what replay says after a purge
//! (`docs/specifications/replay-and-purge.md`).

use cs_core::{AccessContext, AgentId, Error, MemoryId, Scope, Visibility};
use cs_engine::{RecallRequest, ReplayFidelity, ReplayRequest, StatusChange};
use cs_memory::RecallPolicy;

use crate::Harness;

fn recall(query: &str, max_results: u32) -> RecallRequest {
    RecallRequest {
        query: query.into(),
        policy: RecallPolicy {
            max_results,
            min_score: 0.05,
            ..RecallPolicy::default()
        },
        session: None,
    }
}

fn purge(reason: &str) -> StatusChange {
    StatusChange {
        reason: reason.into(),
    }
}

/// A trace holds the query, its vector and candidate ids and scores. Another
/// principal in the same tenant must not learn that it exists, even when it
/// can read every memory the trace names.
pub async fn traces_are_readable_only_by_their_principal(h: &Harness) {
    let shared = Scope::tenant(h.tenant).with_visibility(Visibility::Tenant);
    let m = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(shared, "the release train leaves on Tuesdays"),
        )
        .await
        .unwrap();
    h.tick(1);
    let r = h
        .engine
        .recall(&h.alice, recall("when does the release train leave", 5))
        .await
        .unwrap();
    assert_eq!(r.results[0].memory.id, m.id);

    // Bob can read the memory itself, but not alice's trace or its replay.
    h.engine.get_memory(&h.bob, m.id).await.unwrap();
    assert!(matches!(
        h.engine.get_trace(&h.bob, r.trace_id).await,
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        h.engine
            .replay_recall(&h.bob, r.trace_id, ReplayRequest::default())
            .await,
        Err(Error::NotFound { .. })
    ));

    // A different agent acting for alice is a different principal, and so
    // is a caller that names only the tenant.
    let alice_agent = AccessContext {
        agent_id: Some(AgentId::new_v7()),
        ..h.alice.clone()
    };
    let anonymous = AccessContext {
        user_id: None,
        ..h.alice.clone()
    };
    for other in [&alice_agent, &anonymous] {
        assert!(matches!(
            h.engine.get_trace(other, r.trace_id).await,
            Err(Error::NotFound { .. })
        ));
    }

    // The principal that ran the recall still can, and the trace says who.
    let trace = h.engine.get_trace(&h.alice, r.trace_id).await.unwrap();
    assert_eq!(trace.owner_user_id, h.alice.user_id);
    assert_eq!(trace.owner_agent_id, None);
    let replay = h
        .engine
        .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(replay.identical, "{replay:?}");
    assert_eq!(replay.fidelity, ReplayFidelity::Exact);

    // An agent's own traces are readable by that agent.
    let agent = AccessContext::agent(h.tenant, AgentId::new_v7());
    let ra = h
        .engine
        .recall(&agent, recall("release train", 5))
        .await
        .unwrap();
    let ta = h.engine.get_trace(&agent, ra.trace_id).await.unwrap();
    assert_eq!(ta.owner_agent_id, agent.agent_id);
    assert!(matches!(
        h.engine.get_trace(&h.alice, ra.trace_id).await,
        Err(Error::NotFound { .. })
    ));
}

/// Replay after a purge names the purged candidates instead of differing
/// silently, whether or not the purged memory had been selected.
pub async fn replay_after_purge_reports_purged_candidates(h: &Harness) {
    let selected = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(
                h.alice_scope(),
                "postgres primary database failover runbook",
            ),
        )
        .await
        .unwrap();
    let runner_up = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "postgres database replica lag alert"),
        )
        .await
        .unwrap();
    h.tick(1);
    let r = h
        .engine
        .recall(
            &h.alice,
            recall("postgres primary database failover runbook", 1),
        )
        .await
        .unwrap();
    let trace = h.engine.get_trace(&h.alice, r.trace_id).await.unwrap();
    let selected_ids: Vec<MemoryId> = trace.selected().map(|c| c.memory_id).collect();
    assert_eq!(selected_ids, vec![selected.id]);
    assert!(
        trace
            .candidates
            .iter()
            .any(|c| c.memory_id == runner_up.id && !c.selected),
        "{trace:?}"
    );

    // No purge: exact.
    let before = h
        .engine
        .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(before.identical);
    assert_eq!(before.fidelity, ReplayFidelity::Exact);
    assert!(before.purged_candidates.is_empty());
    assert!(before.purged_selected.is_empty());

    // Purge a candidate that was not selected. The selection survives, but
    // the candidate set does not: never silently `exact`.
    h.tick(1);
    h.engine
        .purge_memory(&h.alice, runner_up.id, purge("user deletion request"))
        .await
        .unwrap();
    let after_runner_up = h
        .engine
        .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(after_runner_up.identical, "{after_runner_up:?}");
    assert_eq!(after_runner_up.fidelity, ReplayFidelity::DegradedByPurge);
    assert_eq!(after_runner_up.purged_candidates, vec![runner_up.id]);
    assert!(after_runner_up.purged_selected.is_empty());

    // Purge the selected memory: not identical, and the report says why.
    h.tick(1);
    h.engine
        .purge_memory(&h.alice, selected.id, purge("user deletion request"))
        .await
        .unwrap();
    let after_selected = h
        .engine
        .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(!after_selected.identical);
    assert_eq!(after_selected.removed, vec![selected.id]);
    assert_eq!(after_selected.fidelity, ReplayFidelity::DegradedByPurge);
    assert_eq!(after_selected.purged_selected, vec![selected.id]);
    let mut purged = after_selected.purged_candidates.clone();
    purged.sort();
    let mut expected = vec![selected.id, runner_up.id];
    expected.sort();
    assert_eq!(purged, expected);

    // The replay saw neither purged memory, and the report carries their
    // identifiers only, never their content.
    assert!(
        after_selected
            .replay
            .candidates
            .iter()
            .all(|c| c.memory_id != selected.id && c.memory_id != runner_up.id)
    );
    let body = serde_json::to_string(&after_selected).unwrap();
    assert!(!body.contains("replica lag"));

    // A recall made after the purges replays exactly.
    let fresh = h
        .engine
        .recall(&h.alice, recall("postgres failover", 1))
        .await
        .unwrap();
    let replay = h
        .engine
        .replay_recall(&h.alice, fresh.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(replay.identical);
    assert_eq!(replay.fidelity, ReplayFidelity::Exact);
}
