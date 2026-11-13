//! HTTP transport for the engine. No cognitive logic lives here: every
//! handler authenticates the caller, deserializes a request, calls exactly
//! one `CognitiveEngine` operation and serializes the result.

pub mod auth;
pub mod error;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequest, Path, Query, Request, State, rejection::QueryRejection},
    http::StatusCode,
    routing::{get, post},
};
use cs_core::{BeliefId, CognitiveEventType, EventQuery, EventStreamId, MemoryId, RecallTraceId};
use cs_engine::{
    CognitiveEngine, NewBelief, NewDependency, NewEvidence, NewMemory, RecallRequest,
    ReplayRequest, ReviseBelief, ReviseMemory, StatusChange,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

pub use crate::auth::{AuthConfig, Caller};
pub use crate::error::ApiError;

#[derive(Debug, Clone)]
pub struct AppState {
    pub engine: Arc<CognitiveEngine>,
    pub auth: Arc<AuthConfig>,
}

impl AppState {
    pub fn new(engine: CognitiveEngine, auth: AuthConfig) -> Self {
        Self {
            engine: Arc::new(engine),
            auth: Arc::new(auth),
        }
    }
}

/// JSON body extractor whose rejections use the API error format.
#[derive(Debug)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Self(v)),
            Err(r) => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                r.body_text(),
            )),
        }
    }
}

type ApiResult<T> = Result<(StatusCode, Json<T>), ApiError>;

fn ok<T>(v: T) -> ApiResult<T> {
    Ok((StatusCode::OK, Json(v)))
}

fn created<T>(v: T) -> ApiResult<T> {
    Ok((StatusCode::CREATED, Json(v)))
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/memories", post(create_memory))
        .route("/memories/{id}", get(get_memory))
        .route("/memories/{id}/history", get(memory_history))
        .route("/memories/{id}/revisions", post(revise_memory))
        .route("/memories/{id}/promote", post(promote_memory))
        .route("/memories/{id}/reject", post(reject_memory))
        .route("/memories/{id}/tombstone", post(tombstone_memory))
        .route("/memories/{id}/purge", post(purge_memory))
        .route("/recall", post(recall))
        .route("/recall-traces/{id}", get(get_trace))
        .route("/recall-traces/{id}/replay", post(replay))
        .route("/evidence", post(record_evidence))
        .route("/beliefs", post(create_belief))
        .route("/beliefs/{id}", get(get_belief))
        .route("/beliefs/{id}/history", get(belief_history))
        .route("/beliefs/{id}/revisions", post(revise_belief))
        .route("/beliefs/{id}/retract", post(retract_belief))
        .route("/dependencies", post(add_dependency))
        .route("/events", get(events))
        .route("/debt", get(debt));
    Router::new()
        .nest("/v1", v1)
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route") })
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn ready(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    s.engine.ready().await.map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "storage is unavailable",
        )
    })?;
    Ok(Json(json!({ "status": "ready" })))
}

// ---- memories -----------------------------------------------------------------

async fn create_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<NewMemory>,
) -> ApiResult<cs_memory::MemoryRecord> {
    created(s.engine.create_memory(&ctx, req).await?)
}

async fn get_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::MemoryRecord> {
    ok(s.engine.get_memory(&ctx, MemoryId(id)).await?)
}

async fn memory_history(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_engine::MemoryHistory> {
    ok(s.engine.memory_history(&ctx, MemoryId(id)).await?)
}

async fn revise_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<ReviseMemory>,
) -> ApiResult<cs_memory::MemoryRecord> {
    created(s.engine.revise_memory(&ctx, MemoryId(id), req).await?)
}

async fn promote_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<StatusChange>,
) -> ApiResult<cs_memory::MemoryRecord> {
    ok(s.engine.promote_memory(&ctx, MemoryId(id), req).await?)
}

async fn reject_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<StatusChange>,
) -> ApiResult<cs_memory::MemoryRecord> {
    ok(s.engine.reject_memory(&ctx, MemoryId(id), req).await?)
}

async fn tombstone_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<StatusChange>,
) -> ApiResult<cs_memory::MemoryRecord> {
    ok(s.engine.tombstone_memory(&ctx, MemoryId(id), req).await?)
}

async fn purge_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<StatusChange>,
) -> ApiResult<cs_core::CognitiveEvent> {
    ok(s.engine.purge_memory(&ctx, MemoryId(id), req).await?)
}

// ---- recall -------------------------------------------------------------------

async fn recall(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<RecallRequest>,
) -> ApiResult<cs_engine::RecallResponse> {
    ok(s.engine.recall(&ctx, req).await?)
}

async fn get_trace(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::RecallTrace> {
    ok(s.engine.get_trace(&ctx, RecallTraceId(id)).await?)
}

async fn replay(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> ApiResult<cs_engine::ReplayReport> {
    // The body is optional: empty means "exact replay".
    let req: ReplayRequest = if body.iter().all(u8::is_ascii_whitespace) {
        ReplayRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", e.to_string()))?
    };
    ok(s.engine.replay_recall(&ctx, RecallTraceId(id), req).await?)
}

// ---- evidence & beliefs ---------------------------------------------------------

async fn record_evidence(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<NewEvidence>,
) -> ApiResult<cs_memory::Evidence> {
    created(s.engine.record_evidence(&ctx, req).await?)
}

async fn create_belief(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<NewBelief>,
) -> ApiResult<cs_memory::Belief> {
    created(s.engine.create_belief(&ctx, req).await?)
}

async fn get_belief(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_engine::BeliefView> {
    ok(s.engine.get_belief(&ctx, BeliefId(id)).await?)
}

async fn belief_history(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<cs_memory::Belief>> {
    ok(s.engine.belief_history(&ctx, BeliefId(id)).await?)
}

async fn revise_belief(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<ReviseBelief>,
) -> ApiResult<cs_engine::BeliefRevisionOutcome> {
    created(s.engine.revise_belief(&ctx, BeliefId(id), req).await?)
}

async fn retract_belief(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<StatusChange>,
) -> ApiResult<cs_engine::BeliefRevisionOutcome> {
    ok(s.engine.retract_belief(&ctx, BeliefId(id), req).await?)
}

async fn add_dependency(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<NewDependency>,
) -> ApiResult<cs_memory::BeliefDependency> {
    created(s.engine.add_dependency(&ctx, req).await?)
}

// ---- inspection -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EventParams {
    stream_id: Option<Uuid>,
    event_type: Option<String>,
    after_sequence: Option<u64>,
    limit: Option<u32>,
}

async fn events(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<EventParams>, QueryRejection>,
) -> ApiResult<Vec<cs_core::CognitiveEvent>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    let event_type = match p.event_type.as_deref() {
        None => None,
        Some(t) => Some(CognitiveEventType::parse(t).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "unknown event_type",
            )
        })?),
    };
    let query = EventQuery {
        tenant_id: None,
        stream_id: p.stream_id.map(EventStreamId),
        event_type,
        after_sequence: p.after_sequence,
        limit: p.limit,
    };
    ok(s.engine.events(&ctx, query).await?)
}

async fn debt(
    State(s): State<AppState>,
    Caller(ctx): Caller,
) -> ApiResult<cs_engine::CognitiveDebtReport> {
    ok(s.engine.cognitive_debt(&ctx).await?)
}
