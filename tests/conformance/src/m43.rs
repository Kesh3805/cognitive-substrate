//! M4.3 conformance: knowledge compilation.

use std::sync::Arc;

use cs_core::{
    CognitiveEventType, Error, EventQuery, EventStreamId, MemoryId, SourceRef, TimeInterval,
};
use cs_engine::{
    Decision, NewMemory, RuleExtractor, StatusChange, Worker, WorkerConfig,
    entities::ProposeEntityMerge,
};
use cs_memory::{
    EpisodicMemory, MemoryContent, MemoryStatus,
    derivation::{Derivation, DerivationStatus, RECURRING_EPISODE_OPERATOR},
};
use time::Duration;

use crate::Harness;

fn tool(name: &str) -> SourceRef {
    SourceRef::ToolOutput {
        tool: name.into(),
        execution_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn user() -> SourceRef {
    SourceRef::UserInput { message_id: None }
}

/// An episode `s p o` from `source`, `day` days after the harness origin.
fn episode(h: &Harness, (s, p, o): (&str, &str, &str), source: SourceRef, day: i64) -> NewMemory {
    let mut req = h.fact(h.alice_scope(), "x");
    req.content = MemoryContent::Episodic(EpisodicMemory {
        description: format!("{s} {p} {o}"),
        event_id: None,
        participants: vec![],
        subject: Some(s.into()),
        predicate: Some(p.into()),
        object: Some(o.into()),
    });
    req.provenance.source = source;
    let base = time::macros::datetime!(2026-05-01 12:00 UTC);
    req.valid_time = Some(TimeInterval {
        start: base + Duration::days(day),
        end: Some(base + Duration::days(day) + Duration::hours(1)),
    });
    req
}

async fn add(h: &Harness, req: NewMemory) -> MemoryId {
    h.engine.create_memory(&h.alice, req).await.unwrap().id
}

async fn drain(h: &Harness) {
    Worker::new(
        h.engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    )
    .drain()
    .await
    .unwrap();
}

async fn derivations(h: &Harness) -> Vec<Derivation> {
    h.engine
        .list_derivations(&h.alice, None, None)
        .await
        .unwrap()
}

fn reason(r: &str) -> StatusChange {
    StatusChange { reason: r.into() }
}

const CALLS: (&str, &str, &str) = ("API", "called", "auth");

pub async fn recurring_episodes_compile_to_candidates(h: &Harness) {
    add(h, episode(h, CALLS, user(), 0)).await;
    add(h, episode(h, CALLS, tool("monitor"), 0)).await;
    drain(h).await;
    assert!(derivations(h).await.is_empty(), "two units are not enough");

    let third = add(h, episode(h, CALLS, tool("monitor"), 1)).await;
    drain(h).await;
    let ds = derivations(h).await;
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.operator, RECURRING_EPISODE_OPERATOR);
    assert_eq!(d.status, DerivationStatus::Active);
    assert_eq!(
        (
            d.support.units,
            d.support.origins,
            d.support.buckets,
            d.support.counter_units
        ),
        (3, 2, 2, 0)
    );
    assert_eq!(d.witnesses.len(), 3);
    assert!(d.roots.contains(&third));

    // The compiled memory is a semantic candidate, never trusted by itself.
    let m = h
        .engine
        .get_memory(&h.alice, d.derived_memory_id)
        .await
        .unwrap();
    assert_eq!(m.status, MemoryStatus::Candidate);
    let MemoryContent::Semantic(s) = &m.content else {
        panic!("compiled memory must be semantic");
    };
    assert_eq!(s.subject.as_deref(), Some("API"));
    assert_eq!(s.predicate.as_deref(), Some("called"));
    assert_eq!(s.object.as_deref(), Some("auth"));
    assert!(matches!(
        m.provenance.source,
        SourceRef::SystemDerived { .. }
    ));

    // Recompiling unchanged support writes nothing.
    h.engine
        .revalidate_derivation(&h.alice, d.id)
        .await
        .unwrap();
    drain(h).await;
    assert_eq!(
        h.engine
            .get_derivation(&h.alice, d.id)
            .await
            .unwrap()
            .version,
        d.version
    );

    let events = h
        .engine
        .events(
            &h.alice,
            EventQuery {
                stream_id: Some(EventStreamId(d.id.0)),
                ..EventQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, CognitiveEventType::DerivationRecorded);

    // Another user sees neither the derivation nor its memory.
    assert!(
        h.engine
            .list_derivations(&h.bob, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        h.engine.get_derivation(&h.bob, d.id).await,
        Err(Error::NotFound { .. })
    ));
}

pub async fn correlated_or_contradicted_support_does_not_compile(h: &Harness) {
    // One tool, one day: five records are one unit of support.
    for _ in 0..5 {
        add(
            h,
            episode(h, ("cache", "reported", "degraded"), tool("monitor"), 0),
        )
        .await;
    }
    // Enough independent support, but a counterexample.
    let backed = ("queue", "is backed by", "redis");
    add(h, episode(h, backed, user(), 0)).await;
    add(h, episode(h, backed, tool("monitor"), 1)).await;
    add(h, episode(h, backed, tool("pager"), 2)).await;
    add(h, episode(h, ("queue", "is backed by", "kafka"), user(), 3)).await;
    drain(h).await;
    assert!(derivations(h).await.is_empty());

    // Candidate (unvalidated) episodes never count either.
    let mut extracted = episode(h, ("dns", "resolved", "edge"), user(), 0);
    extracted.provenance.extraction = Some(cs_core::ExtractionInfo {
        model: None,
        model_version: None,
        prompt_version: None,
        extractor_version: "test".into(),
    });
    add(h, extracted).await;
    add(
        h,
        episode(h, ("dns", "resolved", "edge"), tool("monitor"), 1),
    )
    .await;
    add(h, episode(h, ("dns", "resolved", "edge"), tool("pager"), 2)).await;
    drain(h).await;
    assert!(derivations(h).await.is_empty());
}

pub async fn losing_support_demotes_compiled_knowledge(h: &Harness) {
    let a = add(h, episode(h, CALLS, user(), 0)).await;
    let b = add(h, episode(h, CALLS, tool("monitor"), 1)).await;
    add(h, episode(h, CALLS, tool("pager"), 2)).await;
    drain(h).await;
    let d = derivations(h).await.remove(0);
    h.engine
        .promote_memory(&h.alice, d.derived_memory_id, reason("reviewed"))
        .await
        .unwrap();

    // A tombstoned root no longer counts: support falls below threshold.
    h.clock.advance(Duration::seconds(10));
    h.engine
        .tombstone_memory(&h.alice, a, reason("wrong"))
        .await
        .unwrap();
    drain(h).await;
    let now = h.engine.get_derivation(&h.alice, d.id).await.unwrap();
    assert_eq!(now.status, DerivationStatus::Invalidated);
    assert_eq!(now.support.units, 2);
    let m = h
        .engine
        .get_memory(&h.alice, d.derived_memory_id)
        .await
        .unwrap();
    assert_eq!(m.status, MemoryStatus::Candidate, "demoted, not deleted");

    // New independent support restores the derivation (not the trust).
    add(h, episode(h, CALLS, tool("tracer"), 3)).await;
    drain(h).await;
    let now = h.engine.get_derivation(&h.alice, d.id).await.unwrap();
    assert_eq!(now.status, DerivationStatus::Active);
    assert_eq!(
        derivations(h).await.len(),
        1,
        "same signature, same derivation"
    );

    // A purge flags the derivation in the same transaction, then the
    // revalidation job decides.
    h.clock.advance(Duration::seconds(10));
    h.engine
        .tombstone_memory(&h.alice, b, reason("erase"))
        .await
        .unwrap();
    h.engine
        .purge_memory(&h.alice, b, reason("erasure request"))
        .await
        .unwrap();
    let flagged = h.engine.get_derivation(&h.alice, d.id).await.unwrap();
    assert_eq!(flagged.status, DerivationStatus::RevalidationRequired);
    drain(h).await;
    let now = h.engine.get_derivation(&h.alice, d.id).await.unwrap();
    assert_eq!(now.status, DerivationStatus::Invalidated);
    assert!(!now.roots.contains(&b));

    // Purging the compiled memory removes its derivation.
    h.engine
        .purge_memory(&h.alice, d.derived_memory_id, reason("not wanted"))
        .await
        .unwrap();
    assert!(derivations(h).await.is_empty());
}

pub async fn compilation_follows_entity_merges(h: &Harness) {
    add(h, episode(h, CALLS, user(), 0)).await;
    add(h, episode(h, CALLS, tool("monitor"), 1)).await;
    add(
        h,
        episode(h, ("API", "called", "auth service"), tool("pager"), 2),
    )
    .await;
    drain(h).await;
    assert!(derivations(h).await.is_empty(), "two names, two signatures");

    let find =
        |n: &'static str| async move { h.engine.find_entities(&h.alice, n).await.unwrap()[0].id };
    let (auth, auth_service) = (find("auth").await, find("auth service").await);
    let p = h
        .engine
        .propose_entity_merge(
            &h.alice,
            ProposeEntityMerge {
                survivor: auth,
                merged: auth_service,
                reason: "same service".into(),
            },
        )
        .await
        .unwrap();
    h.clock.advance(Duration::seconds(10));
    h.engine
        .accept_proposal(
            &h.alice,
            p.id,
            Decision {
                reason: "same service".into(),
            },
        )
        .await
        .unwrap();

    // The next episode compiles with support pooled across both names.
    add(h, episode(h, CALLS, user(), 3)).await;
    drain(h).await;
    let ds = derivations(h).await;
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].object, auth);
    assert_eq!(ds[0].support.units, 4);
}
