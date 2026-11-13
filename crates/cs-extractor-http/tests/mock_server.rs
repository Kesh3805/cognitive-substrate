//! Adapter contract tests against a local mock OpenAI-compatible server.
//! CI never depends on a real model endpoint.
#![allow(clippy::unwrap_used)]

use std::{sync::Arc, time::Duration};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use cs_core::{ActorRef, ObservationId, Scope, SourceRef, TenantId, UserId};
use cs_engine::{ExtractError, Extractor};
use cs_extractor_http::{HttpExtractor, HttpExtractorConfig};
use cs_memory::Observation;
use serde_json::{Value, json};
use time::OffsetDateTime;

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: Value,
    seen: Arc<std::sync::Mutex<Option<Value>>>,
}

async fn chat(State(r): State<Reply>, Json(req): Json<Value>) -> impl IntoResponse {
    *r.seen.lock().unwrap() = Some(req);
    (r.status, Json(r.body.clone()))
}

async fn serve(status: StatusCode, body: Value) -> (String, Arc<std::sync::Mutex<Option<Value>>>) {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(Reply {
            status,
            body,
            seen: seen.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1"), seen)
}

fn extractor(base_url: String) -> HttpExtractor {
    HttpExtractor::new(HttpExtractorConfig {
        base_url,
        model: "test-model".into(),
        api_key: Some("sk-test".into()),
        timeout: Duration::from_secs(2),
    })
    .unwrap()
}

fn observation(text: &str) -> Observation {
    let now = OffsetDateTime::now_utc();
    let tenant = TenantId::new_v7();
    let user = UserId::new_v7();
    Observation {
        id: ObservationId::new_v7(),
        scope: Scope::private(tenant, user),
        content: Some(text.into()),
        content_hash: "h".into(),
        media_type: None,
        source: SourceRef::UserInput { message_id: None },
        actor: ActorRef::User { user_id: user },
        observed_at: now,
        recorded_at: now,
        purged_at: None,
    }
}

fn completion(content: &str) -> Value {
    json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
}

#[tokio::test]
async fn extracts_and_sends_a_deterministic_request() {
    let (url, seen) = serve(
        StatusCode::OK,
        completion(r#"{"memories":[{"domain":"semantic","data":{"proposition":"The API uses gRPC","subject":"API","predicate":"uses","object":"gRPC"},"confidence":0.9}]}"#),
    )
    .await;
    let ex = extractor(url);
    let out = ex
        .extract(&observation("The API uses gRPC."))
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(ex.model(), Some("test-model"));
    let req = seen.lock().unwrap().clone().unwrap();
    assert_eq!(req["model"], "test-model");
    assert_eq!(req["temperature"], 0);
    assert_eq!(req["messages"][1]["content"], "The API uses gRPC.");
}

#[tokio::test]
async fn server_errors_and_rate_limits_are_retryable() {
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::BAD_GATEWAY,
    ] {
        let (url, _) = serve(status, json!({ "error": "busy" })).await;
        let err = extractor(url)
            .extract(&observation("x y z"))
            .await
            .unwrap_err();
        assert!(matches!(err, ExtractError::Retryable(_)), "{status}: {err}");
    }
}

#[tokio::test]
async fn client_errors_and_bad_output_are_permanent() {
    let (url, _) = serve(StatusCode::BAD_REQUEST, json!({ "error": "bad" })).await;
    assert!(matches!(
        extractor(url).extract(&observation("x")).await,
        Err(ExtractError::Permanent(_))
    ));

    let (url, _) = serve(StatusCode::OK, completion("I think the API uses gRPC")).await;
    assert!(matches!(
        extractor(url).extract(&observation("x")).await,
        Err(ExtractError::Permanent(_))
    ));

    let (url, _) = serve(StatusCode::OK, json!({ "unexpected": true })).await;
    assert!(matches!(
        extractor(url).extract(&observation("x")).await,
        Err(ExtractError::Permanent(_))
    ));
}

#[tokio::test]
async fn unreachable_endpoint_is_retryable() {
    let err = extractor("http://127.0.0.1:9/v1".into())
        .extract(&observation("x"))
        .await
        .unwrap_err();
    assert!(matches!(err, ExtractError::Retryable(_)), "{err}");
}

#[tokio::test]
async fn purged_observation_is_permanent() {
    let (url, _) = serve(StatusCode::OK, completion(r#"{"memories":[]}"#)).await;
    let mut o = observation("x");
    o.content = None;
    assert!(matches!(
        extractor(url).extract(&o).await,
        Err(ExtractError::Permanent(_))
    ));
}
