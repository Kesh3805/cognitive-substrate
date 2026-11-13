//! Contract tests against local mock servers for both protocols. CI never
//! depends on a real embedding endpoint.
#![allow(clippy::unwrap_used)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use cs_embedder_http::{HttpEmbedder, HttpEmbedderConfig, Protocol};
use cs_engine::{EmbedError, EmbedKind, Embedder, EmbeddingSpace, embedding::embed_texts};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<Value>>>,
}

fn vector_for(text: &str) -> Vec<f32> {
    // Deterministic, non-zero, unnormalized (the engine normalizes).
    vec![text.len() as f32 + 1.0, 2.0, 3.0]
}

/// OpenAI-style server: rejects any batch containing "POISON" with 400 and
/// returns items in reverse index order to exercise reordering.
async fn openai(State(s): State<Seen>, Json(req): Json<Value>) -> impl IntoResponse {
    s.requests.lock().unwrap().push(req.clone());
    let inputs: Vec<String> = serde_json::from_value(req["input"].clone()).unwrap();
    if inputs.iter().any(|t| t.contains("POISON")) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "bad input" })),
        );
    }
    let mut data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, t)| json!({ "index": i, "embedding": vector_for(t) }))
        .collect();
    data.reverse();
    (StatusCode::OK, Json(json!({ "data": data })))
}

async fn ollama(State(s): State<Seen>, Json(req): Json<Value>) -> impl IntoResponse {
    s.requests.lock().unwrap().push(req.clone());
    let inputs: Vec<String> = serde_json::from_value(req["input"].clone()).unwrap();
    let embeddings: Vec<Vec<f32>> = inputs.iter().map(|t| vector_for(t)).collect();
    (StatusCode::OK, Json(json!({ "embeddings": embeddings })))
}

async fn status(State(code): State<u16>) -> impl IntoResponse {
    (
        StatusCode::from_u16(code).unwrap(),
        Json(json!({ "error": "x" })),
    )
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn space() -> EmbeddingSpace {
    EmbeddingSpace {
        provider: "test".into(),
        model: "mock-embed".into(),
        revision: "1".into(),
        dimensions: 3,
        normalized: true,
        preprocessing: 1,
        document_prefix: "search_document: ".into(),
        query_prefix: "search_query: ".into(),
    }
}

fn embedder(protocol: Protocol, base_url: String, max_items: usize) -> HttpEmbedder {
    HttpEmbedder::new(HttpEmbedderConfig {
        protocol,
        base_url,
        api_key: Some("sk-test".into()),
        timeout: Duration::from_secs(2),
        max_batch_items: max_items,
        max_batch_bytes: 1_000_000,
        space: space(),
    })
    .unwrap()
}

#[tokio::test]
async fn openai_batches_reorders_and_normalizes() {
    let seen = Seen::default();
    let base = serve(
        Router::new()
            .route("/v1/embeddings", post(openai))
            .with_state(seen.clone()),
    )
    .await;
    let e = embedder(Protocol::OpenAi, format!("{base}/v1"), 2);
    let texts: Vec<String> = ["a", "bb", "ccc"].iter().map(|s| (*s).to_owned()).collect();
    let out = embed_texts(
        &e,
        &texts,
        EmbedKind::Document,
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();
    assert_eq!(out.len(), 3);
    // Two requests: items are batched 2 + 1.
    let reqs = seen.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0]["model"], "mock-embed");
    // The engine applied the document prefix before the adapter saw it.
    assert_eq!(reqs[0]["input"][0], "search_document: a");
    // Order preserved despite the server reversing items.
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm(&out[0].vector) - 1.0).abs() < 1e-5,
        "normalized in Rust"
    );
    assert!(out[0].vector[0] < out[2].vector[0]);
    assert!(out.iter().all(|r| r.content_hash.is_some()));
}

#[tokio::test]
async fn poison_items_are_isolated_by_bisection() {
    let base = serve(
        Router::new()
            .route("/v1/embeddings", post(openai))
            .with_state(Seen::default()),
    )
    .await;
    let e = embedder(Protocol::OpenAi, format!("{base}/v1"), 64);
    let texts: Vec<String> = ["ok one", "ok two", "POISON", "ok three"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let each = e.embed_each(&texts).await;
    assert!(
        each[0].is_ok() && each[1].is_ok() && each[3].is_ok(),
        "{each:?}"
    );
    assert!(matches!(each[2], Err(EmbedError::Permanent(_))));
    // The all-or-nothing trait method surfaces the failure.
    assert!(e.embed_batch(&texts).await.is_err());
}

#[tokio::test]
async fn ollama_native_disables_truncation() {
    let seen = Seen::default();
    let base = serve(
        Router::new()
            .route("/api/embed", post(ollama))
            .with_state(seen.clone()),
    )
    .await;
    let e = embedder(Protocol::OllamaNative, base, 16);
    let out = embed_texts(
        &e,
        &["hello".to_owned()],
        EmbedKind::Query,
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();
    assert_eq!(out.len(), 1);
    let req = seen.requests.lock().unwrap()[0].clone();
    assert_eq!(req["truncate"], false);
    assert_eq!(req["input"][0], "search_query: hello");
}

#[tokio::test]
async fn failures_are_classified() {
    for (code, retryable) in [
        (500, true),
        (503, true),
        (429, true),
        (408, true),
        (401, false),
        (404, false),
    ] {
        let base = serve(
            Router::new()
                .route("/v1/embeddings", post(status))
                .with_state(code),
        )
        .await;
        let err = embedder(Protocol::OpenAi, format!("{base}/v1"), 8)
            .embed_batch(&["x".to_owned()])
            .await
            .unwrap_err();
        assert_eq!(
            matches!(err, EmbedError::Retryable(_)),
            retryable,
            "{code}: {err}"
        );
    }
    let err = embedder(Protocol::OpenAi, "http://127.0.0.1:9/v1".into(), 8)
        .embed_batch(&["x".to_owned()])
        .await
        .unwrap_err();
    assert!(matches!(err, EmbedError::Retryable(_)), "{err}");
}

#[tokio::test]
async fn wrong_dimensions_are_rejected_by_the_engine_contract() {
    let base = serve(
        Router::new()
            .route("/api/embed", post(ollama))
            .with_state(Seen::default()),
    )
    .await;
    let mut cfg_space = space();
    cfg_space.dimensions = 8;
    let e = HttpEmbedder::new(HttpEmbedderConfig {
        protocol: Protocol::OllamaNative,
        base_url: base,
        api_key: None,
        timeout: Duration::from_secs(2),
        max_batch_items: 8,
        max_batch_bytes: 1_000,
        space: cfg_space,
    })
    .unwrap();
    let err = embed_texts(
        &e,
        &["x".to_owned()],
        EmbedKind::Document,
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, EmbedError::Permanent(_)), "{err}");
    // Empty input never reaches the network.
    let err = embed_texts(
        &e,
        &["  ".to_owned()],
        EmbedKind::Document,
        time::OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, EmbedError::Permanent(_)));
}
