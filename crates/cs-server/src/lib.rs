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
use cs_core::{
    BeliefId, CognitiveEventType, DerivationId, EntityId, EventQuery, EventStreamId, EvidenceId,
    ExecutionId, HypothesisId, InformationNeedId, InterventionId, JobId, MemoryId, ObservationId,
    ProposalId, RecallTraceId,
};
use cs_engine::{
    CognitiveEngine, Decision, NewBelief, NewDependency, NewEvidence, NewMemory, NewObservation,
    RecallRequest, ReplayRequest, ReviseBelief, ReviseMemory, StatusChange,
    capabilities::{
        CapabilityView, ChangeStage, FinishExecution, NewCase, StartExecution, VerifyExecution,
    },
    entities::{
        AddAlias, EntityView, Neighborhood, NeighborhoodRequest, ProposeEntityMerge, SetEntityKind,
    },
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
        .route("/debt", get(debt))
        .route("/observations", post(submit_observation))
        .route("/observations/{id}", get(get_observation))
        .route("/observations/{id}/purge", post(purge_observation))
        .route("/evidence/{id}/purge", post(purge_evidence))
        .route("/memories/{id}/resolve", post(resolve_memory))
        .route("/beliefs/{id}/resolve", post(resolve_belief))
        .route("/proposals", get(list_proposals))
        .route("/proposals/{id}", get(get_proposal))
        .route("/proposals/{id}/accept", post(accept_proposal))
        .route("/proposals/{id}/reject", post(reject_proposal))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(get_job))
        .route("/processing/status", get(processing_status))
        .route("/embeddings/reindex", post(reindex))
        .route("/entities", get(find_entities))
        .route("/entities/{id}", get(get_entity))
        .route("/entities/{id}/aliases", post(add_entity_alias))
        .route("/entities/{id}/kind", post(set_entity_kind))
        .route("/entities/{id}/neighborhood", post(entity_neighborhood))
        .route(
            "/entities/{id}/merge-candidates",
            post(detect_entity_merges),
        )
        .route("/entity-merges", post(propose_entity_merge))
        .route("/derivations", get(list_derivations))
        .route("/derivations/{id}", get(get_derivation))
        .route("/derivations/{id}/revalidate", post(revalidate_derivation))
        .route("/capabilities/{id}", get(get_capability))
        .route("/capabilities/{id}/stage", post(change_capability_stage))
        .route("/capabilities/{id}/cases", post(add_case))
        .route(
            "/capabilities/{id}/executions",
            get(list_executions).post(start_execution),
        )
        .route("/executions/{id}", get(get_execution))
        .route("/executions/{id}/finish", post(finish_execution))
        .route("/executions/{id}/verifications", post(verify_execution))
        .route("/information-needs", get(list_needs).post(ask_question))
        .route("/information-needs/{id}", get(get_need))
        .route("/information-needs/{id}/validate", post(validate_need))
        .route("/information-needs/{id}/confirm", post(confirm_gap))
        .route("/information-needs/{id}/dismiss", post(dismiss_need))
        .route("/information-needs/{id}/weights", post(weigh_need))
        .route("/information-needs/{id}/purge", post(purge_need))
        .route("/hypotheses", get(list_hypotheses).post(create_hypothesis))
        .route("/hypotheses/{id}", get(get_hypothesis))
        .route("/hypotheses/{id}/history", get(hypothesis_history))
        .route("/hypotheses/{id}/evaluate", post(evaluate_hypothesis))
        .route("/hypotheses/{id}/retire", post(retire_hypothesis))
        .route("/interventions", post(record_intervention))
        .route("/interventions/{id}", get(get_intervention))
        .route("/interventions/{id}/outcomes", post(observe_outcome))
        .route("/interventions/{id}/counterfactual", get(counterfactual));
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

// ---- governed processing (M3) ---------------------------------------------------

async fn submit_observation(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<NewObservation>,
) -> ApiResult<cs_engine::ObservationView> {
    // 202: the observation is stored; perception runs in the background.
    Ok((
        StatusCode::ACCEPTED,
        Json(s.engine.submit_observation(&ctx, req).await?),
    ))
}

async fn get_observation(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_engine::ObservationView> {
    ok(s.engine.get_observation(&ctx, ObservationId(id)).await?)
}

async fn purge_observation(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<Decision>,
) -> ApiResult<cs_engine::PurgeReport> {
    ok(s.engine
        .purge_observation(&ctx, ObservationId(id), req)
        .await?)
}

async fn purge_evidence(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<Decision>,
) -> ApiResult<cs_engine::PurgeReport> {
    ok(s.engine.purge_evidence(&ctx, EvidenceId(id), req).await?)
}

async fn resolve_memory(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::Job> {
    Ok((
        StatusCode::ACCEPTED,
        Json(
            s.engine
                .request_memory_resolution(&ctx, MemoryId(id))
                .await?,
        ),
    ))
}

async fn resolve_belief(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::Job> {
    Ok((
        StatusCode::ACCEPTED,
        Json(
            s.engine
                .request_belief_resolution(&ctx, BeliefId(id))
                .await?,
        ),
    ))
}

#[derive(Debug, Deserialize)]
struct ListParams {
    status: Option<String>,
    limit: Option<u32>,
}

fn parse_status<T>(v: Option<&str>, f: impl Fn(&str) -> Option<T>) -> Result<Option<T>, ApiError> {
    v.map(|s| {
        f(s).ok_or_else(|| {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", "unknown status")
        })
    })
    .transpose()
}

async fn list_proposals(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<ListParams>, QueryRejection>,
) -> ApiResult<Vec<cs_memory::ResolutionProposal>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    let status = parse_status(p.status.as_deref(), cs_memory::ProposalStatus::parse)?;
    ok(s.engine.list_proposals(&ctx, status, p.limit).await?)
}

async fn get_proposal(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::ResolutionProposal> {
    ok(s.engine.get_proposal(&ctx, ProposalId(id)).await?)
}

async fn accept_proposal(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<Decision>,
) -> ApiResult<cs_memory::ResolutionProposal> {
    ok(s.engine.accept_proposal(&ctx, ProposalId(id), req).await?)
}

async fn reject_proposal(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<Decision>,
) -> ApiResult<cs_memory::ResolutionProposal> {
    ok(s.engine.reject_proposal(&ctx, ProposalId(id), req).await?)
}

async fn list_jobs(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<ListParams>, QueryRejection>,
) -> ApiResult<Vec<cs_memory::Job>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    let status = parse_status(p.status.as_deref(), cs_memory::JobStatus::parse)?;
    ok(s.engine.list_jobs(&ctx, status, p.limit).await?)
}

async fn get_job(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::Job> {
    ok(s.engine.get_job(&ctx, JobId(id)).await?)
}

// ---- embeddings & operations (M3.x) ---------------------------------------------

async fn processing_status(
    State(s): State<AppState>,
    Caller(ctx): Caller,
) -> ApiResult<cs_engine::ProcessingStatus> {
    ok(s.engine.processing_status(&ctx).await?)
}

async fn reindex(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    body: axum::body::Bytes,
) -> ApiResult<cs_engine::ReindexReport> {
    let req: cs_engine::ReindexRequest = if body.iter().all(u8::is_ascii_whitespace) {
        cs_engine::ReindexRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", e.to_string()))?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(s.engine.request_reembedding(&ctx, req).await?),
    ))
}

// ---- entities & graph -------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EntityParams {
    name: String,
}

async fn find_entities(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<EntityParams>, QueryRejection>,
) -> ApiResult<Vec<cs_memory::entity::Entity>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    ok(s.engine.find_entities(&ctx, &p.name).await?)
}

async fn get_entity(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<EntityView> {
    ok(s.engine.get_entity(&ctx, EntityId(id)).await?)
}

async fn add_entity_alias(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<AddAlias>,
) -> ApiResult<cs_memory::entity::Entity> {
    ok(s.engine.add_entity_alias(&ctx, EntityId(id), req).await?)
}

async fn set_entity_kind(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<SetEntityKind>,
) -> ApiResult<cs_memory::entity::Entity> {
    ok(s.engine.set_entity_kind(&ctx, EntityId(id), req).await?)
}

async fn entity_neighborhood(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> ApiResult<Neighborhood> {
    let req: NeighborhoodRequest = if body.iter().all(u8::is_ascii_whitespace) {
        NeighborhoodRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", e.to_string()))?
    };
    ok(s.engine
        .entity_neighborhood(&ctx, EntityId(id), req)
        .await?)
}

async fn detect_entity_merges(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<cs_memory::ResolutionProposal>> {
    ok(s.engine.detect_entity_merges(&ctx, EntityId(id)).await?)
}

async fn propose_entity_merge(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<ProposeEntityMerge>,
) -> ApiResult<cs_memory::ResolutionProposal> {
    Ok((
        StatusCode::CREATED,
        Json(s.engine.propose_entity_merge(&ctx, req).await?),
    ))
}

// ---- knowledge compilation --------------------------------------------------

async fn list_derivations(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<ListParams>, QueryRejection>,
) -> ApiResult<Vec<cs_memory::derivation::Derivation>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    let status = parse_status(
        p.status.as_deref(),
        cs_memory::derivation::DerivationStatus::parse,
    )?;
    ok(s.engine.list_derivations(&ctx, status, p.limit).await?)
}

async fn get_derivation(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::derivation::Derivation> {
    ok(s.engine.get_derivation(&ctx, DerivationId(id)).await?)
}

async fn revalidate_derivation(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::Job> {
    Ok((
        StatusCode::ACCEPTED,
        Json(
            s.engine
                .revalidate_derivation(&ctx, DerivationId(id))
                .await?,
        ),
    ))
}

// ---- procedural learning ----------------------------------------------------

#[derive(Debug, Deserialize)]
struct LimitParams {
    limit: Option<u32>,
}

async fn get_capability(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<CapabilityView> {
    ok(s.engine.get_capability(&ctx, MemoryId(id)).await?)
}

async fn change_capability_stage(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<ChangeStage>,
) -> ApiResult<cs_memory::MemoryRecord> {
    ok(s.engine
        .change_capability_stage(&ctx, MemoryId(id), req)
        .await?)
}

async fn add_case(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<NewCase>,
) -> ApiResult<cs_memory::capability::EvaluationCase> {
    Ok((
        StatusCode::CREATED,
        Json(s.engine.add_case(&ctx, MemoryId(id), req).await?),
    ))
}

async fn start_execution(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<StartExecution>,
) -> ApiResult<cs_memory::capability::Execution> {
    Ok((
        StatusCode::CREATED,
        Json(s.engine.start_execution(&ctx, MemoryId(id), req).await?),
    ))
}

async fn list_executions(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    params: Result<Query<LimitParams>, QueryRejection>,
) -> ApiResult<Vec<cs_memory::capability::Execution>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    ok(s.engine
        .list_executions(&ctx, MemoryId(id), p.limit)
        .await?)
}

async fn get_execution(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::capability::Execution> {
    ok(s.engine.get_execution(&ctx, ExecutionId(id)).await?)
}

async fn finish_execution(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<FinishExecution>,
) -> ApiResult<cs_memory::capability::Execution> {
    ok(s.engine
        .finish_execution(&ctx, ExecutionId(id), req)
        .await?)
}

async fn verify_execution(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<VerifyExecution>,
) -> ApiResult<cs_memory::capability::Execution> {
    Ok((
        StatusCode::CREATED,
        Json(
            s.engine
                .verify_execution(&ctx, ExecutionId(id), req)
                .await?,
        ),
    ))
}

// ---- information needs ------------------------------------------------------

async fn list_needs(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<ListParams>, QueryRejection>,
) -> ApiResult<Vec<cs_engine::needs::NeedView>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    let status = parse_status(p.status.as_deref(), cs_memory::need::NeedStatus::parse)?;
    ok(s.engine.list_needs(&ctx, status, p.limit).await?)
}

async fn ask_question(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<cs_engine::needs::AskQuestion>,
) -> ApiResult<cs_engine::needs::NeedView> {
    Ok((StatusCode::CREATED, Json(s.engine.ask(&ctx, req).await?)))
}

async fn get_need(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_engine::needs::NeedView> {
    ok(s.engine.get_need(&ctx, InformationNeedId(id)).await?)
}

async fn validate_need(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_engine::needs::NeedView> {
    ok(s.engine.validate_need(&ctx, InformationNeedId(id)).await?)
}

async fn confirm_gap(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<cs_engine::needs::NeedChange>,
) -> ApiResult<cs_engine::needs::NeedView> {
    ok(s.engine
        .confirm_gap(&ctx, InformationNeedId(id), req)
        .await?)
}

async fn dismiss_need(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<cs_engine::needs::NeedChange>,
) -> ApiResult<cs_engine::needs::NeedView> {
    ok(s.engine
        .dismiss_need(&ctx, InformationNeedId(id), req)
        .await?)
}

async fn weigh_need(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<cs_engine::needs::NeedWeights>,
) -> ApiResult<cs_engine::needs::NeedView> {
    ok(s.engine
        .weigh_need(&ctx, InformationNeedId(id), req)
        .await?)
}

async fn purge_need(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    s.engine.purge_need(&ctx, InformationNeedId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- M6.2: causal hypotheses and interventions --------------------------------

#[derive(Debug, Deserialize)]
struct ExpectedVersion {
    expected_version: u64,
}

async fn list_hypotheses(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    params: Result<Query<ListParams>, QueryRejection>,
) -> ApiResult<Vec<cs_memory::causal::Hypothesis>> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    let status = parse_status(
        p.status.as_deref(),
        cs_memory::causal::HypothesisStatus::parse,
    )?;
    ok(s.engine
        .list_hypotheses(&ctx, status, p.limit.unwrap_or(100))
        .await?)
}

async fn create_hypothesis(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<cs_engine::causal::NewHypothesis>,
) -> ApiResult<cs_memory::causal::Hypothesis> {
    Ok((
        StatusCode::CREATED,
        Json(s.engine.create_hypothesis(&ctx, req).await?),
    ))
}

async fn get_hypothesis(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::causal::Hypothesis> {
    ok(s.engine.get_hypothesis(&ctx, HypothesisId(id)).await?)
}

async fn hypothesis_history(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<cs_memory::causal::Hypothesis>> {
    ok(s.engine.hypothesis_history(&ctx, HypothesisId(id)).await?)
}

async fn evaluate_hypothesis(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<ExpectedVersion>,
) -> ApiResult<cs_memory::causal::Hypothesis> {
    ok(s.engine
        .evaluate_hypothesis(&ctx, HypothesisId(id), req.expected_version)
        .await?)
}

async fn retire_hypothesis(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<ExpectedVersion>,
) -> ApiResult<cs_memory::causal::Hypothesis> {
    ok(s.engine
        .retire_hypothesis(&ctx, HypothesisId(id), req.expected_version)
        .await?)
}

async fn record_intervention(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    ApiJson(req): ApiJson<cs_engine::causal::NewIntervention>,
) -> ApiResult<cs_memory::causal::Intervention> {
    Ok((
        StatusCode::CREATED,
        Json(s.engine.record_intervention(&ctx, req).await?),
    ))
}

async fn get_intervention(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<cs_memory::causal::Intervention> {
    ok(s.engine.get_intervention(&ctx, InterventionId(id)).await?)
}

async fn observe_outcome(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    ApiJson(req): ApiJson<cs_engine::causal::ObserveOutcome>,
) -> ApiResult<cs_memory::causal::Intervention> {
    ok(s.engine
        .observe_outcome(&ctx, InterventionId(id), req)
        .await?)
}

#[derive(Debug, Deserialize)]
struct CounterfactualParams {
    hypothesis: Uuid,
}

async fn counterfactual(
    State(s): State<AppState>,
    Caller(ctx): Caller,
    Path(id): Path<Uuid>,
    params: Result<Query<CounterfactualParams>, QueryRejection>,
) -> ApiResult<cs_engine::causal::CounterfactualView> {
    let Query(p) = params
        .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text()))?;
    ok(s.engine
        .counterfactual(&ctx, InterventionId(id), HypothesisId(p.hypothesis))
        .await?)
}
