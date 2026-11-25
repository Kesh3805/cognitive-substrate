//! Tool handlers called directly against the in-memory store (no transport).

use std::sync::Arc;

use cs_core::{AccessContext, TenantId, UserId};
use cs_engine::{CognitiveEngine, RuleExtractor, Worker, WorkerConfig};
use cs_mcp::{
    AddObservationsParams, AssertBeliefParams, CreateEntitiesParams, CreateRelationsParams,
    EntityInput, ListBeliefsParams, ObservationInput, ObserveParams, PromoteParams, RecallParams,
    RelationInput, RememberParams, SearchNodesParams, SubstrateMcp, TraceParams,
};
use cs_storage::InMemoryStore;
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult};
use serde_json::Value;

fn server_on(engine: CognitiveEngine, ctx: AccessContext) -> SubstrateMcp {
    let worker = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    SubstrateMcp::new(engine, ctx, Some(worker)).expect("user principal")
}

fn server() -> SubstrateMcp {
    let engine = CognitiveEngine::new(Arc::new(InMemoryStore::new()));
    server_on(
        engine,
        AccessContext::user(TenantId::new_v7(), UserId::new_v7()),
    )
}

fn text(r: &CallToolResult) -> &str {
    r.content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.as_str())
        .expect("text content")
}

fn ok(r: CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "tool failed: {}", text(&r));
    serde_json::from_str(text(&r)).expect("json")
}

fn err(r: CallToolResult) -> Value {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected a tool error: {}",
        text(&r)
    );
    serde_json::from_str(text(&r)).expect("json")
}

fn remember(text: &str) -> Parameters<RememberParams> {
    Parameters(RememberParams {
        text: text.into(),
        subject: None,
        predicate: None,
        object: None,
        idempotency_key: None,
    })
}

fn recall(query: &str) -> Parameters<RecallParams> {
    Parameters(RecallParams {
        query: query.into(),
        ..RecallParams::default()
    })
}

#[tokio::test]
async fn remember_recall_trace_and_exact_replay() {
    let s = server();
    let m = ok(s.remember(remember("Deploys are frozen on Fridays")).await);
    assert_eq!(m["status"], "active");
    assert_eq!(m["domain"], "semantic");
    ok(s.remember(remember("The API depends on the auth service"))
        .await);

    let r = ok(s.recall(recall("are deploys frozen on fridays?")).await);
    let trace_id = r["trace_id"].as_str().expect("trace id").to_owned();
    let results = r["results"].as_array().expect("results");
    assert!(!results.is_empty());
    assert_eq!(results[0]["memory_id"], m["memory_id"]);
    assert_eq!(results[0]["rank"], 1);

    let t = ok(s
        .get_recall_trace(Parameters(TraceParams {
            trace_id: trace_id.clone(),
        }))
        .await);
    assert_eq!(t["trace_id"], trace_id.as_str());
    assert_eq!(t["query_retained"], true);
    assert_eq!(t["selected"][0]["memory_id"], m["memory_id"]);

    // A later write must not change an exact replay.
    ok(
        s.remember(remember("Friday deploys need a director's approval"))
            .await,
    );
    let replay = ok(s
        .replay_recall(Parameters(TraceParams {
            trace_id: trace_id.clone(),
        }))
        .await);
    assert_eq!(replay["identical"], true);
    assert_eq!(replay["original_selected"], replay["replayed_selected"]);
    assert!(replay["added"].as_array().expect("added").is_empty());
}

#[tokio::test]
async fn validation_errors_are_tool_errors() {
    let s = server();
    let e = err(s.remember(remember("   ")).await);
    assert_eq!(e["error"]["code"], "validation_failed");

    let e = err(s.recall(recall("")).await);
    assert_eq!(e["error"]["code"], "validation_failed");

    let e = err(s
        .get_recall_trace(Parameters(TraceParams {
            trace_id: "not-a-uuid".into(),
        }))
        .await);
    assert_eq!(e["error"]["code"], "validation_failed");

    let e = err(s
        .replay_recall(Parameters(TraceParams {
            trace_id: uuid::Uuid::now_v7().to_string(),
        }))
        .await);
    assert_eq!(e["error"]["code"], "not_found");

    let e = err(s
        .recall(Parameters(RecallParams {
            query: "x".into(),
            domains: Some(vec!["dreams".into()]),
            ..RecallParams::default()
        }))
        .await);
    assert_eq!(e["error"]["code"], "validation_failed");

    let e = err(s
        .assert_belief(Parameters(AssertBeliefParams {
            proposition: "Fridays are quiet".into(),
            confidence: 1.5,
            polarity: None,
            interpretation: None,
            reason: None,
            idempotency_key: None,
        }))
        .await);
    assert_eq!(e["error"]["code"], "validation_failed");
    let listed = ok(s
        .list_beliefs(Parameters(ListBeliefsParams::default()))
        .await);
    assert_eq!(
        listed["total"], 0,
        "a rejected assertion leaves nothing behind"
    );
}

#[tokio::test]
async fn memories_are_private_to_the_configured_user() {
    let engine = CognitiveEngine::new(Arc::new(InMemoryStore::new()));
    let tenant = TenantId::new_v7();
    let alice = server_on(
        engine.clone(),
        AccessContext::user(tenant, UserId::new_v7()),
    );
    let bob = server_on(engine, AccessContext::user(tenant, UserId::new_v7()));

    ok(alice
        .remember(remember("Alice's locker code is in the vault"))
        .await);
    let r = ok(bob.recall(recall("locker code vault")).await);
    assert!(r["results"].as_array().expect("results").is_empty());

    let promoted = err(bob
        .promote_memory(Parameters(PromoteParams {
            memory_id: uuid::Uuid::now_v7().to_string(),
            reason: None,
        }))
        .await);
    assert_eq!(promoted["error"]["code"], "not_found");
}

#[tokio::test]
async fn reference_graph_tools_map_onto_memories() {
    let s = server();
    let created = ok(s
        .create_entities(Parameters(CreateEntitiesParams {
            entities: vec![EntityInput {
                name: "auth-service".into(),
                entity_type: "service".into(),
                observations: vec!["issues session tokens".into()],
            }],
        }))
        .await);
    assert_eq!(
        created["entities"][0]["memory_ids"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    // Idempotent: the same entity maps to the same memories.
    let again = ok(s
        .create_entities(Parameters(CreateEntitiesParams {
            entities: vec![EntityInput {
                name: "auth-service".into(),
                entity_type: "service".into(),
                observations: vec!["issues session tokens".into()],
            }],
        }))
        .await);
    assert_eq!(
        again["entities"][0]["memory_ids"],
        created["entities"][0]["memory_ids"]
    );

    ok(s.add_observations(Parameters(AddObservationsParams {
        observations: vec![ObservationInput {
            entity_name: "auth-service".into(),
            contents: vec!["rotates signing keys weekly".into()],
        }],
    }))
    .await);
    ok(s.create_relations(Parameters(CreateRelationsParams {
        relations: vec![RelationInput {
            from: "api".into(),
            to: "auth-service".into(),
            relation_type: "depends_on".into(),
        }],
    }))
    .await);

    let found = ok(s
        .search_nodes(Parameters(SearchNodesParams {
            query: "auth-service signing keys".into(),
        }))
        .await);
    assert!(found["trace_id"].is_string());
    let entities = found["entities"].as_array().expect("entities");
    let auth = entities
        .iter()
        .find(|e| e["name"] == "auth-service")
        .expect("auth-service entity");
    assert_eq!(auth["entityType"], "service");
    let observations = auth["observations"].as_array().expect("observations");
    assert!(
        observations
            .iter()
            .any(|o| o == "rotates signing keys weekly")
    );
    assert!(
        found["relations"]
            .as_array()
            .expect("relations")
            .iter()
            .any(|r| r["relationType"] == "depends_on")
    );
}

#[tokio::test]
async fn observe_yields_candidates_that_can_be_promoted() {
    let s = server();
    let o = ok(s
        .observe(Parameters(ObserveParams {
            text: "The API depends on the auth service. We decided to freeze deploys on Fridays."
                .into(),
            media_type: None,
            idempotency_key: None,
        }))
        .await);
    assert_eq!(o["processed_inline"], true);
    let candidates = o["candidates"].as_array().expect("candidates");
    assert!(!candidates.is_empty(), "rule extractor found nothing: {o}");
    assert!(candidates.iter().all(|c| c["status"] == "candidate"));

    let id = candidates[0]["memory_id"].as_str().expect("id").to_owned();
    let p = ok(s
        .promote_memory(Parameters(PromoteParams {
            memory_id: id,
            reason: Some("confirmed".into()),
        }))
        .await);
    assert_eq!(p["status"], "active");
}

#[tokio::test]
async fn beliefs_can_be_asserted_and_listed() {
    let s = server();
    let b = ok(s
        .assert_belief(Parameters(AssertBeliefParams {
            proposition: "Friday deploys cause most incidents".into(),
            confidence: 0.7,
            polarity: None,
            interpretation: Some("model_estimate".into()),
            reason: Some("incident review".into()),
            idempotency_key: None,
        }))
        .await);
    assert_eq!(b["status"], "active");
    assert_eq!(b["interpretation"], "model_estimate");

    let listed = ok(s
        .list_beliefs(Parameters(ListBeliefsParams::default()))
        .await);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["beliefs"][0]["belief_id"], b["belief_id"]);

    let r = ok(s.recall(recall("friday deploy incidents")).await);
    assert_eq!(r["beliefs"][0]["belief_id"], b["belief_id"]);
}
