//! End-to-end tests of the HTTP adapter over the in-memory store.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use cs_engine::CognitiveEngine;
use cs_server::{AppState, AuthConfig, router};
use cs_storage::InMemoryStore;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Client {
    app: Router,
    tenant: Uuid,
    user: Uuid,
    key: Option<&'static str>,
}

impl Client {
    fn new(key: Option<&'static str>) -> Self {
        let engine = CognitiveEngine::new(Arc::new(InMemoryStore::new()));
        let app = router(AppState::new(
            engine,
            AuthConfig {
                api_key: key.map(str::to_owned),
            },
        ));
        Self {
            app,
            tenant: Uuid::now_v7(),
            user: Uuid::now_v7(),
            key,
        }
    }

    fn as_user(&self, user: Uuid) -> Self {
        Self {
            app: self.app.clone(),
            tenant: self.tenant,
            user,
            key: self.key,
        }
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("x-cs-tenant", self.tenant.to_string())
            .header("x-cs-user", self.user.to_string());
        if let Some(k) = self.key {
            req = req.header("authorization", format!("Bearer {k}"));
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    fn private_scope(&self) -> Value {
        json!({ "tenant_id": self.tenant, "owner_user_id": self.user, "visibility": "private" })
    }
}

#[tokio::test]
async fn health_needs_no_auth() {
    let c = Client::new(Some("k"));
    let res = c
        .app
        .clone()
        .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let (status, _) = c.send("GET", "/v1/ready", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn api_key_is_enforced() {
    let c = Client::new(Some("secret"));
    let mut wrong = c.as_user(c.user);
    wrong.key = Some("nope");
    let (status, body) = wrong.send("GET", "/v1/debt", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
    let (status, _) = c.send("GET", "/v1/debt", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn tenant_header_is_required() {
    let c = Client::new(None);
    let res = c
        .app
        .clone()
        .oneshot(Request::get("/v1/debt").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn memory_recall_replay_roundtrip() {
    let c = Client::new(None);
    let (status, mem) = c
        .send(
            "POST",
            "/v1/memories",
            Some(json!({
                "scope": c.private_scope(),
                "content": { "domain": "semantic", "data": {
                    "proposition": "The project uses PostgreSQL as its authoritative store.",
                    "subject": "cognitive-substrate", "predicate": "uses", "object": "PostgreSQL"
                }},
                "provenance": { "source": { "type": "user_input" } }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{mem}");
    let id = mem["id"].as_str().unwrap().to_owned();
    assert_eq!(mem["status"], "active");

    let (status, got) = c.send("GET", &format!("/v1/memories/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["revision_id"], mem["revision_id"]);

    let (status, rev) = c
        .send(
            "POST",
            &format!("/v1/memories/{id}/revisions"),
            Some(json!({ "reason": "clarified", "expected_revision": mem["revision_id"] })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{rev}");
    assert_eq!(rev["revision_number"], 2);

    let (_, history) = c
        .send("GET", &format!("/v1/memories/{id}/history"), None)
        .await;
    assert_eq!(history["revisions"].as_array().unwrap().len(), 2);

    let (status, recall) = c
        .send(
            "POST",
            "/v1/recall",
            Some(
                json!({ "query": "Why did we choose PostgreSQL?", "policy": { "max_results": 5 } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recall}");
    assert_eq!(recall["results"].as_array().unwrap().len(), 1);
    let trace_id = recall["trace_id"].as_str().unwrap().to_owned();

    let (status, trace) = c
        .send("GET", &format!("/v1/recall-traces/{trace_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(trace["candidates"][0]["reason"], "selected");

    let (status, replay) = c
        .send(
            "POST",
            &format!("/v1/recall-traces/{trace_id}/replay"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["identical"], true);

    // Another user cannot see Alice's private memory.
    let bob = c.as_user(Uuid::now_v7());
    let (status, body) = bob.send("GET", &format!("/v1/memories/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (status, events) = c
        .send(
            "GET",
            &format!("/v1/events?stream_id={id}&event_type=memory_revised"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(events.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn beliefs_and_debt() {
    let c = Client::new(None);
    let (status, ev) = c
        .send(
            "POST",
            "/v1/evidence",
            Some(json!({ "source": { "type": "tool_output", "tool": "bench", "execution_id": "run-7" },
                         "summary": "p95 latency 41ms" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ev}");

    let belief = |prop: &str, evidence: Value| {
        json!({
            "scope": c.private_scope(),
            "proposition": prop,
            "confidence": { "value": 0.7, "interpretation": "heuristic_score", "method": "bench" },
            "evidence": evidence,
        })
    };
    let (status, b) = c
        .send(
            "POST",
            "/v1/beliefs",
            Some(belief(
                "p95 latency is under 50ms",
                json!([{ "evidence_id": ev["id"], "direction": "supports" }]),
            )),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let (_, unsupported) = c
        .send(
            "POST",
            "/v1/beliefs",
            Some(belief("caching is unnecessary", json!([]))),
        )
        .await;

    let (status, debt) = c.send("GET", "/v1/debt", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(debt["active_assets"], 2);
    assert_eq!(debt["debt_items"], 1);
    assert_eq!(debt["items"][0]["category"], "unsupported_belief");

    let (status, dep) = c
        .send(
            "POST",
            "/v1/dependencies",
            Some(json!({ "from": b["id"], "to": unsupported["id"], "relation": "supports" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{dep}");

    let bid = b["id"].as_str().unwrap();
    let (status, out) = c
        .send(
            "POST",
            &format!("/v1/beliefs/{bid}/revisions"),
            Some(json!({ "confidence": { "value": 0.9, "interpretation": "heuristic_score", "method": "bench-v2" },
                         "reason": "re-measured" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["needs_reevaluation"][0], unsupported["id"]);

    let (_, view) = c.send("GET", &format!("/v1/beliefs/{bid}"), None).await;
    assert_eq!(view["current_revision_id"], out["belief"]["id"]);
    let (_, hist) = c
        .send("GET", &format!("/v1/beliefs/{bid}/history"), None)
        .await;
    assert_eq!(hist.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn malformed_and_invalid_requests() {
    let c = Client::new(None);
    let (status, body) = c
        .send("POST", "/v1/memories", Some(json!({ "nope": 1 })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");

    let (status, body) = c
        .send(
            "POST",
            "/v1/recall",
            Some(json!({ "query": "x", "policy": { "dense_weight": 0.9 } })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");

    let (status, _) = c.send("GET", "/v1/nowhere", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn governed_processing_over_http() {
    use cs_engine::{ProcessingConfig, RuleExtractor, Worker, WorkerConfig};
    let engine = CognitiveEngine::builder(Arc::new(InMemoryStore::new()))
        .processing(ProcessingConfig {
            auto_resolve: true,
            ..ProcessingConfig::default()
        })
        .build();
    let worker = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let c = Client {
        app: router(AppState::new(engine, AuthConfig::default())),
        tenant: Uuid::now_v7(),
        user: Uuid::now_v7(),
        key: None,
    };

    // Observation intake is asynchronous.
    let obs = |text: &str| {
        json!({
            "scope": c.private_scope(),
            "content": text,
            "source": { "type": "user_input" }
        })
    };
    let (status, view) = c
        .send(
            "POST",
            "/v1/observations",
            Some(obs("The primary database is PostgreSQL.")),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    assert_eq!(view["job"]["status"], "queued");
    let o1 = view["observation"]["id"].as_str().unwrap().to_owned();
    worker.drain().await.unwrap();
    let (_, view) = c
        .send(
            "POST",
            "/v1/observations",
            Some(obs("The primary database is PostgreSQL.")),
        )
        .await;
    let o2 = view["observation"]["id"].as_str().unwrap().to_owned();
    worker.drain().await.unwrap();

    let (status, view) = c.send("GET", &format!("/v1/observations/{o1}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["job"]["status"], "succeeded");
    assert_eq!(
        view["extraction_runs"][0]["candidate_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let (_, jobs) = c.send("GET", "/v1/jobs?status=succeeded", None).await;
    assert!(jobs.as_array().unwrap().len() >= 2, "{jobs}");

    // The two candidates are proposed as duplicates; accept the merge.
    let (_, proposals) = c.send("GET", "/v1/proposals?status=pending", None).await;
    let proposals = proposals.as_array().unwrap();
    assert_eq!(proposals.len(), 1, "{proposals:?}");
    assert_eq!(proposals[0]["kind"], "duplicate");
    let pid = proposals[0]["id"].as_str().unwrap();
    let (status, body) = c
        .send(
            "POST",
            &format!("/v1/proposals/{pid}/accept"),
            Some(json!({ "reason": "same" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "accepted");
    let (status, _) = c
        .send(
            "POST",
            &format!("/v1/proposals/{pid}/reject"),
            Some(json!({ "reason": "late" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Purge the first source: the corroborated fact survives.
    let (status, report) = c
        .send(
            "POST",
            &format!("/v1/observations/{o1}/purge"),
            Some(json!({ "reason": "request" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["purged_memories"].as_array().unwrap().len(), 0);
    let (_, report) = c
        .send(
            "POST",
            &format!("/v1/observations/{o2}/purge"),
            Some(json!({ "reason": "request" })),
        )
        .await;
    assert_eq!(report["purged_memories"].as_array().unwrap().len(), 2);
    let (_, again) = c
        .send(
            "POST",
            &format!("/v1/observations/{o2}/purge"),
            Some(json!({ "reason": "again" })),
        )
        .await;
    assert_eq!(again["already_purged"], true);

    // Evidence purge flags the belief that relied on it.
    let (_, ev) = c
        .send(
            "POST",
            "/v1/evidence",
            Some(json!({ "source": { "type": "human_annotation", "annotation_id": "n1" } })),
        )
        .await;
    let (_, b) = c.send("POST", "/v1/beliefs", Some(json!({
        "scope": c.private_scope(),
        "proposition": "Batch size 500 is optimal",
        "confidence": { "value": 0.6, "interpretation": "heuristic_score", "method": "bench" },
        "evidence": [{ "evidence_id": ev["id"], "direction": "supports" }]
    }))).await;
    let eid = ev["id"].as_str().unwrap();
    let (status, report) = c
        .send(
            "POST",
            &format!("/v1/evidence/{eid}/purge"),
            Some(json!({ "reason": "withdrawn" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["beliefs_needing_reevaluation"][0], b["id"]);

    // Recall returns beliefs alongside memories.
    let (_, recall) = c
        .send(
            "POST",
            "/v1/recall",
            Some(json!({ "query": "optimal batch size" })),
        )
        .await;
    assert_eq!(recall["beliefs"][0]["belief"]["id"], b["id"]);
    assert!(recall["beliefs"][0]["relevance"].as_f64().unwrap() > 0.0);

    // Unknown filters are rejected.
    let (status, _) = c.send("GET", "/v1/proposals?status=bogus", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn entity_graph_over_http() {
    use cs_engine::{RuleExtractor, Worker, WorkerConfig};
    let engine = CognitiveEngine::new(Arc::new(InMemoryStore::new()));
    let worker = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let c = Client {
        app: router(AppState::new(engine, AuthConfig::default())),
        tenant: Uuid::now_v7(),
        user: Uuid::now_v7(),
        key: None,
    };
    for (s, p, o) in [
        ("API", "depends on", "auth"),
        ("Gateway", "calls", "auth service"),
    ] {
        let (status, _) = c
            .send(
                "POST",
                "/v1/memories",
                Some(json!({
                    "scope": c.private_scope(),
                    "content": { "domain": "relational", "data": {
                        "subject": s, "predicate": p, "object": o
                    }},
                    "provenance": { "source": { "type": "user_input" } }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    worker.drain().await.unwrap();

    let (status, found) = c
        .send("GET", "/v1/entities?name=Auth%20Service", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let auth_service = found[0]["id"].as_str().unwrap().to_owned();
    let (_, found) = c.send("GET", "/v1/entities?name=auth", None).await;
    let auth = found[0]["id"].as_str().unwrap().to_owned();

    let (status, view) = c.send("GET", &format!("/v1/entities/{auth}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["aliases"][0]["key"], "auth");

    let (status, proposals) = c
        .send(
            "POST",
            &format!("/v1/entities/{auth}/merge-candidates"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = proposals[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(proposals[0]["kind"], "entity_merge");
    let (status, _) = c
        .send(
            "POST",
            &format!("/v1/proposals/{id}/accept"),
            Some(json!({ "reason": "same service" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, merged) = c
        .send("GET", &format!("/v1/entities/{auth_service}"), None)
        .await;
    assert_eq!(merged["entity"]["status"]["state"], "merged");
    assert_eq!(merged["entity"]["status"]["into"], auth.as_str());

    let (status, n) = c
        .send(
            "POST",
            &format!("/v1/entities/{auth}/neighborhood"),
            Some(json!({ "direction": "in", "depth": 1 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(n["edges"].as_array().unwrap().len(), 2);

    let (status, _) = c
        .as_user(Uuid::now_v7())
        .send("GET", &format!("/v1/entities/{auth}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn compilation_over_http() {
    use cs_engine::{RuleExtractor, Worker, WorkerConfig};
    let engine = CognitiveEngine::new(Arc::new(InMemoryStore::new()));
    let worker = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let c = Client {
        app: router(AppState::new(engine, AuthConfig::default())),
        tenant: Uuid::now_v7(),
        user: Uuid::now_v7(),
        key: None,
    };
    for (day, source) in [
        (1, json!({ "type": "user_input" })),
        (
            1,
            json!({ "type": "tool_output", "tool": "monitor", "execution_id": "a" }),
        ),
        (
            2,
            json!({ "type": "tool_output", "tool": "monitor", "execution_id": "b" }),
        ),
    ] {
        let (status, _) = c
            .send(
                "POST",
                "/v1/memories",
                Some(json!({
                    "scope": c.private_scope(),
                    "content": { "domain": "episodic", "data": {
                        "description": "API called auth",
                        "subject": "API", "predicate": "called", "object": "auth"
                    }},
                    "provenance": { "source": source },
                    "valid_time": { "start": format!("2026-05-0{day}T12:00:00Z") }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    worker.drain().await.unwrap();

    let (status, list) = c.send("GET", "/v1/derivations?status=active", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let id = list[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(list[0]["support"]["units"], 3);

    let (status, d) = c.send("GET", &format!("/v1/derivations/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let memory = d["derived_memory_id"].as_str().unwrap().to_owned();
    let (_, m) = c.send("GET", &format!("/v1/memories/{memory}"), None).await;
    assert_eq!(m["status"], "candidate");

    let (status, job) = c
        .send("POST", &format!("/v1/derivations/{id}/revalidate"), None)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(job["kind"], "compile");

    let (status, _) = c
        .as_user(Uuid::now_v7())
        .send("GET", &format!("/v1/derivations/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn capabilities_over_http() {
    let c = Client::new(None);
    let (status, m) = c
        .send(
            "POST",
            "/v1/memories",
            Some(json!({
                "scope": c.private_scope(),
                "content": { "domain": "procedural", "data": {
                    "name": "rotate credentials",
                    "steps": ["issue new key", "deploy", "revoke old key"]
                }},
                "provenance": { "source": { "type": "user_input" } }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = m["id"].as_str().unwrap().to_owned();

    let (status, _) = c
        .send(
            "POST",
            &format!("/v1/capabilities/{id}/stage"),
            Some(json!({ "stage": "evaluated", "reason": "too early" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, case) = c
        .send(
            "POST",
            &format!("/v1/capabilities/{id}/cases"),
            Some(json!({ "kind": "regression", "input_class": "edge" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(case["locked"], true);

    let (status, exec) = c
        .send(
            "POST",
            &format!("/v1/capabilities/{id}/executions"),
            Some(json!({ "input_class": "a" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let exec = exec["id"].as_str().unwrap().to_owned();
    let (status, _) = c
        .send(
            "POST",
            &format!("/v1/executions/{exec}/verifications"),
            Some(json!({ "tier": "deterministic", "verdict": "passed" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "not finished yet");
    let (status, _) = c
        .send(
            "POST",
            &format!("/v1/executions/{exec}/finish"),
            Some(json!({ "status": "completed" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, e) = c
        .send(
            "POST",
            &format!("/v1/executions/{exec}/verifications"),
            Some(json!({ "tier": "deterministic", "verdict": "failed" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(e["verifications"].as_array().unwrap().len(), 1);

    let (status, view) = c.send("GET", &format!("/v1/capabilities/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        view["cases"].as_array().unwrap().len(),
        2,
        "authored + ratchet"
    );
    assert_eq!(view["evaluation"]["field"]["failures"], 1);

    let (status, _) = c
        .as_user(Uuid::now_v7())
        .send("GET", &format!("/v1/executions/{exec}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
