//! M4 conformance: entities and the source-linked knowledge graph.

use std::sync::Arc;

use cs_core::{Clock, CognitiveEventType, EntityId, Error, EventQuery, EventStreamId};
use cs_engine::{
    Decision, NewMemory, RuleExtractor, StatusChange, Worker, WorkerConfig,
    entities::{AddAlias, NeighborhoodRequest, ProposeEntityMerge, SetEntityKind, TraversalLimit},
};
use cs_memory::{
    MemoryContent, ProposalStatus, RelationalMemory, ResolutionKind, ResolutionOperation,
    entity::{EntityKind, EntityStatus},
};
use cs_storage::EdgeDirection;
use time::Duration;

use crate::Harness;

fn relation(h: &Harness, s: &str, p: &str, o: &str) -> NewMemory {
    let mut req = h.fact(h.alice_scope(), &format!("{s} {p} {o}"));
    req.content = MemoryContent::Relational(RelationalMemory {
        subject: s.into(),
        predicate: p.into(),
        object: o.into(),
        directed: true,
    });
    req
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

async fn entity(h: &Harness, name: &str) -> EntityId {
    let found = h.engine.find_entities(&h.alice, name).await.unwrap();
    assert_eq!(found.len(), 1, "exactly one entity named {name}: {found:?}");
    found[0].id
}

fn depth(d: u32) -> NeighborhoodRequest {
    NeighborhoodRequest {
        depth: Some(d),
        ..NeighborhoodRequest::default()
    }
}

fn reason(r: &str) -> Decision {
    Decision { reason: r.into() }
}

pub async fn relations_project_into_entities_and_edges(h: &Harness) {
    let a = h
        .engine
        .create_memory(&h.alice, relation(h, "API", "depends on", "auth-service"))
        .await
        .unwrap();
    let b = h
        .engine
        .create_memory(&h.alice, relation(h, "AuthService", "uses", "PostgreSQL"))
        .await
        .unwrap();
    drain(h).await;

    // Surface variants resolve to one entity.
    let auth = entity(h, "Auth Service").await;
    assert_eq!(entity(h, "auth-service").await, auth);
    let api = entity(h, "the API").await;

    let n = h
        .engine
        .entity_neighborhood(&h.alice, api, depth(2))
        .await
        .unwrap();
    assert_eq!(n.nodes.len(), 3);
    assert_eq!(n.edges.len(), 2);
    assert!(n.truncated_by.is_empty());
    let relations: Vec<&str> = n.edges.iter().map(|e| e.relation.as_str()).collect();
    assert_eq!(relations, ["depends_on", "uses"]);
    // Every edge names the exact revision it came from.
    assert_eq!(n.edges[0].revision_id, a.revision_id);
    assert_eq!(n.edges[1].revision_id, b.revision_id);
    let pg = entity(h, "postgresql").await;
    let path = n
        .paths
        .iter()
        .find(|p| p.entities.last() == Some(&pg))
        .unwrap();
    assert_eq!(path.entities, [api, auth, pg]);
    assert_eq!(path.edges, [n.edges[0].id, n.edges[1].id]);

    // Mentions record where a name occurred.
    let view = h.engine.get_entity(&h.alice, auth).await.unwrap();
    assert_eq!(view.mentions.len(), 2);
    assert_eq!(view.entity.kind, EntityKind::Unknown);

    // Another user sees nothing of Alice's private graph.
    assert!(
        h.engine
            .find_entities(&h.bob, "auth service")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        h.engine.get_entity(&h.bob, auth).await,
        Err(Error::NotFound { .. })
    ));

    let events = h
        .engine
        .events(
            &h.alice,
            EventQuery {
                stream_id: Some(EventStreamId(a.id.0)),
                ..EventQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == CognitiveEventType::ProjectionUpdated)
    );
}

pub async fn edges_follow_their_source_revisions(h: &Harness) {
    let m = h
        .engine
        .create_memory(&h.alice, relation(h, "Gateway", "routes to", "Billing"))
        .await
        .unwrap();
    drain(h).await;
    let gateway = entity(h, "gateway").await;
    let before = h.clock.now();
    assert_eq!(
        h.engine
            .entity_neighborhood(&h.alice, gateway, depth(1))
            .await
            .unwrap()
            .edges
            .len(),
        1
    );

    // Tombstoning the source hides the edge now, but not as of before.
    h.clock.advance(Duration::seconds(10));
    h.engine
        .tombstone_memory(
            &h.alice,
            m.id,
            StatusChange {
                reason: "retired".into(),
            },
        )
        .await
        .unwrap();
    drain(h).await;
    let now = h
        .engine
        .entity_neighborhood(&h.alice, gateway, depth(1))
        .await
        .unwrap();
    assert!(now.edges.is_empty());
    let then = h
        .engine
        .entity_neighborhood(
            &h.alice,
            gateway,
            NeighborhoodRequest {
                as_of: Some(before),
                ..depth(1)
            },
        )
        .await
        .unwrap();
    assert_eq!(then.edges.len(), 1);
    assert_eq!(then.edges[0].revision_id, m.revision_id);

    // Purging the source removes its projection from every point in time;
    // curated identity remains, uncurated names it alone supported go.
    let g = h.engine.get_entity(&h.alice, gateway).await.unwrap().entity;
    h.engine
        .set_entity_kind(
            &h.alice,
            gateway,
            SetEntityKind {
                kind: EntityKind::Service,
                expected_version: g.version,
            },
        )
        .await
        .unwrap();
    let billing = entity(h, "billing").await;
    h.engine
        .purge_memory(
            &h.alice,
            m.id,
            StatusChange {
                reason: "erasure request".into(),
            },
        )
        .await
        .unwrap();
    let then = h
        .engine
        .entity_neighborhood(
            &h.alice,
            gateway,
            NeighborhoodRequest {
                as_of: Some(before),
                ..depth(1)
            },
        )
        .await
        .unwrap();
    assert!(then.edges.is_empty());
    let view = h.engine.get_entity(&h.alice, gateway).await.unwrap();
    assert!(view.mentions.is_empty());
    assert!(matches!(
        h.engine.get_entity(&h.alice, billing).await,
        Err(Error::NotFound { .. })
    ));
    assert!(
        h.engine
            .find_entities(&h.alice, "billing")
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn aliases_and_kinds_are_version_checked(h: &Harness) {
    h.engine
        .create_memory(
            &h.alice,
            relation(h, "Ledger", "stores data in", "PostgreSQL"),
        )
        .await
        .unwrap();
    drain(h).await;
    let pg = h
        .engine
        .get_entity(&h.alice, entity(h, "postgresql").await)
        .await
        .unwrap();
    let ledger = entity(h, "ledger").await;

    let stale = h
        .engine
        .add_entity_alias(
            &h.alice,
            pg.entity.id,
            AddAlias {
                name: "Postgres".into(),
                expected_version: pg.entity.version + 7,
            },
        )
        .await;
    assert!(matches!(stale, Err(Error::Conflict(_))));

    let e = h
        .engine
        .add_entity_alias(
            &h.alice,
            pg.entity.id,
            AddAlias {
                name: "Postgres".into(),
                expected_version: pg.entity.version,
            },
        )
        .await
        .unwrap();
    assert_eq!(e.version, pg.entity.version + 1);
    assert_eq!(entity(h, "postgres").await, pg.entity.id);

    // A name already identifying another entity cannot be taken.
    let taken = h
        .engine
        .add_entity_alias(
            &h.alice,
            pg.entity.id,
            AddAlias {
                name: "ledger".into(),
                expected_version: e.version,
            },
        )
        .await;
    assert!(matches!(taken, Err(Error::Conflict(_))));

    let e = h
        .engine
        .set_entity_kind(
            &h.alice,
            pg.entity.id,
            SetEntityKind {
                kind: EntityKind::Technology,
                expected_version: e.version,
            },
        )
        .await
        .unwrap();
    assert_eq!(e.kind, EntityKind::Technology);

    // Bob cannot change Alice's entities.
    assert!(
        h.engine
            .set_entity_kind(
                &h.bob,
                ledger,
                SetEntityKind {
                    kind: EntityKind::Service,
                    expected_version: 1,
                },
            )
            .await
            .is_err()
    );

    // New mentions of the alias resolve to the same entity.
    h.engine
        .create_memory(&h.alice, relation(h, "Reports", "read from", "Postgres"))
        .await
        .unwrap();
    drain(h).await;
    let n = h
        .engine
        .entity_neighborhood(
            &h.alice,
            pg.entity.id,
            NeighborhoodRequest {
                direction: EdgeDirection::In,
                ..depth(1)
            },
        )
        .await
        .unwrap();
    assert_eq!(n.edges.len(), 2);
}

pub async fn entity_merges_are_governed(h: &Harness) {
    h.engine
        .create_memory(&h.alice, relation(h, "API", "depends on", "auth"))
        .await
        .unwrap();
    h.engine
        .create_memory(&h.alice, relation(h, "Gateway", "calls", "auth service"))
        .await
        .unwrap();
    drain(h).await;
    let auth = entity(h, "auth").await;
    let auth_service = entity(h, "auth-service").await;
    assert_ne!(auth, auth_service, "different keys are never auto-merged");

    let found = h
        .engine
        .detect_entity_merges(&h.alice, auth_service)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    let p = &found[0];
    assert_eq!(p.kind, ResolutionKind::EntityMerge);
    assert_eq!(
        p.operation,
        ResolutionOperation::MergeEntities {
            survivor: auth,
            merged: auth_service,
        }
    );
    // Detection is idempotent.
    let again = h.engine.detect_entity_merges(&h.alice, auth).await.unwrap();
    assert_eq!(again[0].id, p.id);
    // Nothing changed yet.
    assert_eq!(entity(h, "auth-service").await, auth_service);

    let before = h.clock.now();
    h.clock.advance(Duration::seconds(10));
    h.engine
        .accept_proposal(&h.alice, p.id, reason("same service"))
        .await
        .unwrap();
    let merged = h.engine.get_entity(&h.alice, auth_service).await.unwrap();
    assert!(matches!(
        merged.entity.status,
        EntityStatus::Merged { into, .. } if into == auth
    ));
    assert_eq!(entity(h, "auth-service").await, auth);
    let survivor = h.engine.get_entity(&h.alice, auth).await.unwrap();
    assert_eq!(survivor.aliases.len(), 2, "aliases of both names resolve");
    assert_eq!(survivor.mentions.len(), 2);
    let inbound = |as_of| NeighborhoodRequest {
        direction: EdgeDirection::In,
        as_of,
        ..depth(1)
    };
    let n = h
        .engine
        .entity_neighborhood(&h.alice, auth, inbound(None))
        .await
        .unwrap();
    assert_eq!(n.edges.len(), 2, "edges of both names now meet");
    assert!(n.edges.iter().all(|e| e.to == auth));
    // Asking for the merged entity lands on the survivor.
    let via_merged = h
        .engine
        .entity_neighborhood(&h.alice, auth_service, inbound(None))
        .await
        .unwrap();
    assert_eq!(via_merged.root, auth);

    // The merge is not retroactive: as of before it, the names were
    // separate and each had its own edge.
    let then = h
        .engine
        .entity_neighborhood(&h.alice, auth, inbound(Some(before)))
        .await
        .unwrap();
    assert_eq!(then.edges.len(), 1);
    let then = h
        .engine
        .entity_neighborhood(&h.alice, auth_service, inbound(Some(before)))
        .await
        .unwrap();
    assert_eq!(then.root, auth_service);
    assert_eq!(then.edges.len(), 1);

    // A proposal is bound to entity versions: a change makes it stale.
    let api = entity(h, "api").await;
    let gateway = entity(h, "gateway").await;
    let p = h
        .engine
        .propose_entity_merge(
            &h.alice,
            ProposeEntityMerge {
                survivor: api,
                merged: gateway,
                reason: "testing staleness".into(),
            },
        )
        .await
        .unwrap();
    let g = h.engine.get_entity(&h.alice, gateway).await.unwrap().entity;
    h.engine
        .set_entity_kind(
            &h.alice,
            gateway,
            SetEntityKind {
                kind: EntityKind::Service,
                expected_version: g.version,
            },
        )
        .await
        .unwrap();
    let accepted = h
        .engine
        .accept_proposal(&h.alice, p.id, reason("too late"))
        .await;
    assert!(matches!(accepted, Err(Error::Conflict(_))));
    let p = h.engine.get_proposal(&h.alice, p.id).await.unwrap();
    assert_eq!(p.status, ProposalStatus::Stale);
    assert_eq!(
        h.engine
            .get_entity(&h.alice, gateway)
            .await
            .unwrap()
            .entity
            .status,
        EntityStatus::Active
    );
}

pub async fn traversal_is_bounded_and_deterministic(h: &Harness) {
    // A cycle with a tail: a → b → c → a, c → d.
    for (s, o) in [
        ("alpha", "beta"),
        ("beta", "gamma"),
        ("gamma", "alpha"),
        ("gamma", "delta"),
    ] {
        h.engine
            .create_memory(&h.alice, relation(h, s, "feeds", o))
            .await
            .unwrap();
    }
    drain(h).await;
    let alpha = entity(h, "alpha").await;

    let full = h
        .engine
        .entity_neighborhood(&h.alice, alpha, depth(3))
        .await
        .unwrap();
    assert_eq!(full.nodes.len(), 4);
    assert_eq!(full.edges.len(), 4, "the cycle edge is reported once");
    assert_eq!(full.paths.len(), 3);
    assert!(full.truncated_by.is_empty());
    let again = h
        .engine
        .entity_neighborhood(&h.alice, alpha, depth(3))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&full).unwrap(),
        serde_json::to_value(&again).unwrap()
    );

    let capped = h
        .engine
        .entity_neighborhood(
            &h.alice,
            alpha,
            NeighborhoodRequest {
                max_nodes: Some(2),
                ..depth(3)
            },
        )
        .await
        .unwrap();
    assert_eq!(capped.nodes.len(), 2);
    assert!(capped.truncated_by.contains(&TraversalLimit::MaxNodes));

    let shallow = h
        .engine
        .entity_neighborhood(&h.alice, alpha, depth(1))
        .await
        .unwrap();
    assert_eq!(shallow.nodes.len(), 2);

    let filtered = h
        .engine
        .entity_neighborhood(
            &h.alice,
            alpha,
            NeighborhoodRequest {
                relations: Some(vec!["owns".into()]),
                ..depth(3)
            },
        )
        .await
        .unwrap();
    assert!(filtered.edges.is_empty());
}
