//! Model Context Protocol server for Cognitive Substrate.
//!
//! [`SubstrateMcp`] exposes a [`CognitiveEngine`] as MCP tools for exactly
//! one principal (tenant + user). Everything it writes is private to that
//! user; everything it reads is what that user may read.
//!
//! Tool results are compact JSON text. Engine errors (validation, not found,
//! forbidden, conflict, storage) become MCP *tool* errors (`isError: true`)
//! carrying `{"error": {"code", "message"}}`, never protocol errors or
//! panics.
//!
//! The reference memory server's tool names are supported where their
//! semantics fit (see `crates/cs-mcp/README.md` for the mapping):
//! `create_entities`, `create_relations`, `add_observations` and
//! `search_nodes`.

pub mod config;

use std::collections::BTreeMap;

use cs_core::{AccessContext, BeliefId, Error, MemoryId, RecallTraceId, Result, Scope, SourceRef};
use cs_engine::{
    CognitiveEngine, NewBelief, NewEvidence, NewMemory, NewObservation, ProvenanceInput,
    RecallRequest, ReplayRequest, StatusChange, Worker,
};
use cs_memory::{
    Belief, BeliefEvidence, BeliefPolarity, Confidence, ConfidenceInterpretation,
    EvidenceDirection, MemoryContent, MemoryDomain, MemoryRecord, RecallPolicy, RelationalMemory,
    SemanticMemory,
};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Upper bound on items in one batch tool call.
const MAX_BATCH: usize = 100;
/// Method recorded on beliefs asserted through MCP.
const ASSERT_METHOD: &str = "mcp:assert_belief";

/// The MCP server: one engine, one principal.
#[derive(Clone)]
pub struct SubstrateMcp {
    engine: CognitiveEngine,
    ctx: AccessContext,
    scope: Scope,
    /// In-process worker used to process observations synchronously. `None`
    /// when another process owns the job queue (`CS_ROLE=api`).
    worker: Option<Worker>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for SubstrateMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubstrateMcp")
            .field("tenant_id", &self.ctx.tenant_id)
            .field("user_id", &self.ctx.user_id)
            .field("inline_worker", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

// ---- parameters -----------------------------------------------------------------

/// An entity in the reference memory server's knowledge-graph shape.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EntityInput {
    /// Entity name, e.g. `"Alice"` or `"auth-service"`.
    pub name: String,
    /// Entity type, e.g. `"person"` or `"service"`.
    #[serde(rename = "entityType")]
    pub entity_type: String,
    /// Facts about the entity; each becomes one semantic memory.
    #[serde(default)]
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateEntitiesParams {
    /// Entities to create. Re-creating an identical entity or observation is
    /// a no-op (idempotent).
    pub entities: Vec<EntityInput>,
}

/// A directed relation between two entities.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RelationInput {
    /// Source entity name.
    pub from: String,
    /// Target entity name.
    pub to: String,
    /// Relation in active voice, e.g. `"depends_on"`.
    #[serde(rename = "relationType")]
    pub relation_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CreateRelationsParams {
    /// Relations to create (idempotent).
    pub relations: Vec<RelationInput>,
}

/// New observations about one entity.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ObservationInput {
    /// Entity the observations are about.
    #[serde(rename = "entityName")]
    pub entity_name: String,
    /// Observation texts; each becomes one semantic memory.
    pub contents: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AddObservationsParams {
    pub observations: Vec<ObservationInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchNodesParams {
    /// Natural-language query, matched against names, types and observations.
    pub query: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RememberParams {
    /// The fact to remember, as one self-contained sentence.
    pub text: String,
    /// Optional structured reading: subject (all three or none).
    #[serde(default)]
    pub subject: Option<String>,
    /// Optional structured reading: predicate.
    #[serde(default)]
    pub predicate: Option<String>,
    /// Optional structured reading: object.
    #[serde(default)]
    pub object: Option<String>,
    /// Optional dedupe key (1-255 bytes): repeating it returns the original.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct RecallParams {
    /// What to recall (1-8192 characters).
    pub query: String,
    /// Maximum memories returned (default 10).
    #[serde(default)]
    pub max_results: Option<u32>,
    /// Minimum score in [0, 1] (default 0).
    #[serde(default)]
    pub min_score: Option<f64>,
    /// Restrict to these domains: episodic, semantic, procedural,
    /// relational, temporal, working (default all).
    #[serde(default)]
    pub domains: Option<Vec<String>>,
    /// Include unvalidated `candidate` memories (default false).
    #[serde(default)]
    pub include_candidates: Option<bool>,
    /// Recall beliefs alongside memories (default true).
    #[serde(default)]
    pub include_beliefs: Option<bool>,
    /// See the substrate as it was at this RFC 3339 instant
    /// (transaction time; default now).
    #[serde(default)]
    pub as_of: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TraceParams {
    /// Recall trace id returned by `recall` or `search_nodes`.
    pub trace_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ObserveParams {
    /// Raw text (at most 64 KiB). Extracted facts enter as `candidate`.
    pub text: String,
    /// Optional media type, e.g. `text/plain`.
    #[serde(default)]
    pub media_type: Option<String>,
    /// Optional dedupe key (1-255 bytes).
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PromoteParams {
    /// Memory id of a `candidate` memory.
    pub memory_id: String,
    /// Why it is being validated.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ListBeliefsParams {
    /// Include retracted beliefs (default false).
    #[serde(default)]
    pub include_retracted: Option<bool>,
    /// Maximum beliefs returned, newest first (default 50, max 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AssertBeliefParams {
    /// The proposition believed.
    pub proposition: String,
    /// Confidence in [0, 1].
    pub confidence: f64,
    /// `supports` (the proposition holds, default) or `refutes`.
    #[serde(default)]
    pub polarity: Option<String>,
    /// What the confidence means: `human_assessment` (default),
    /// `model_estimate`, `heuristic_score` or `calibrated_probability`.
    #[serde(default)]
    pub interpretation: Option<String>,
    /// Why the belief is held.
    #[serde(default)]
    pub reason: Option<String>,
    /// Optional dedupe key (1-255 bytes).
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

// ---- helpers --------------------------------------------------------------------

fn tool_error(code: &str, message: impl Into<String>) -> CallToolResult {
    let body = json!({ "error": { "code": code, "message": message.into() } });
    CallToolResult::error(vec![ContentBlock::text(body.to_string())])
}

fn reply(result: Result<Value>) -> CallToolResult {
    match result {
        Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
        Err(e) => tool_error(e.code(), e.to_string()),
    }
}

fn parse_id<T: std::str::FromStr>(field: &str, value: &str) -> Result<T> {
    value
        .trim()
        .parse()
        .map_err(|_| Error::validation(format!("{field} must be a UUID")))
}

fn batch_len(field: &str, n: usize) -> Result<()> {
    if n == 0 || n > MAX_BATCH {
        return Err(Error::validation(format!(
            "{field} must contain 1..={MAX_BATCH} items"
        )));
    }
    Ok(())
}

/// Deterministic idempotency key for knowledge-graph writes, so repeating
/// a `create_entities` call does not duplicate memories.
fn graph_key(kind: &str, parts: &[&str]) -> String {
    format!(
        "mcp:{kind}:{}",
        cs_policy::query_hash(&parts.join("\u{1f}"))
    )
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

/// The text a client sees for a memory: the canonical index text.
pub fn memory_view(m: &MemoryRecord) -> Value {
    json!({
        "memory_id": m.id,
        "revision_id": m.revision_id,
        "domain": m.domain(),
        "status": m.status,
        "text": m.content.index_text(),
    })
}

fn belief_view(b: &Belief) -> Value {
    json!({
        "belief_id": b.id,
        "lineage_id": b.lineage_id,
        "proposition": b.proposition,
        "polarity": b.polarity,
        "confidence": b.confidence.value,
        "interpretation": b.confidence.interpretation,
        "status": b.status,
        "revision": b.revision_number,
    })
}

fn semantic(
    proposition: String,
    subject: Option<String>,
    predicate: Option<String>,
    object: Option<String>,
) -> MemoryContent {
    MemoryContent::Semantic(SemanticMemory {
        proposition,
        subject,
        predicate,
        object,
    })
}

/// Prefix used for observations attached to an entity, so the entity name
/// is part of the indexed text.
fn observation_text(entity: &str, content: &str) -> String {
    format!("{entity}: {content}")
}

impl SubstrateMcp {
    /// Serve `engine` for one user. `worker`, when set, processes observation
    /// jobs inline so `observe` can return the extracted candidates.
    pub fn new(
        engine: CognitiveEngine,
        ctx: AccessContext,
        worker: Option<Worker>,
    ) -> Result<Self> {
        let user = ctx
            .user_id
            .ok_or_else(|| Error::validation("the MCP principal must be a user"))?;
        Ok(Self {
            scope: Scope::private(ctx.tenant_id, user),
            engine,
            ctx,
            worker,
            tool_router: Self::tool_router(),
        })
    }

    pub fn engine(&self) -> &CognitiveEngine {
        &self.engine
    }

    pub fn access(&self) -> &AccessContext {
        &self.ctx
    }

    fn provenance() -> ProvenanceInput {
        ProvenanceInput {
            source: SourceRef::UserInput { message_id: None },
            observed_at: None,
            extraction: None,
            parent_evidence: Vec::new(),
        }
    }

    async fn write(&self, content: MemoryContent, key: Option<String>) -> Result<MemoryRecord> {
        self.engine
            .create_memory(
                &self.ctx,
                NewMemory {
                    scope: self.scope.clone(),
                    content,
                    provenance: Self::provenance(),
                    valid_time: None,
                    expires_at: None,
                    idempotency_key: key,
                },
            )
            .await
    }

    // ---- implementations (engine errors propagate with `?`) --------------------

    async fn do_create_entities(&self, p: CreateEntitiesParams) -> Result<Value> {
        batch_len("entities", p.entities.len())?;
        let mut out = Vec::with_capacity(p.entities.len());
        for e in p.entities {
            let name = e.name.trim();
            let kind = e.entity_type.trim();
            if name.is_empty() || kind.is_empty() {
                return Err(Error::validation("name and entityType must not be empty"));
            }
            let mut ids = Vec::new();
            let typed = self
                .write(
                    semantic(
                        format!("{name} is a {kind}"),
                        Some(name.to_owned()),
                        Some("is_a".into()),
                        Some(kind.to_owned()),
                    ),
                    Some(graph_key("entity", &[name, kind])),
                )
                .await?;
            ids.push(typed.id);
            for o in &e.observations {
                ids.push(self.write_observation(name, o).await?.id);
            }
            out.push(json!({ "name": name, "entityType": kind, "memory_ids": ids }));
        }
        Ok(json!({ "entities": out }))
    }

    async fn write_observation(&self, entity: &str, content: &str) -> Result<MemoryRecord> {
        let content = content.trim();
        if content.is_empty() {
            return Err(Error::validation("observations must not be empty"));
        }
        self.write(
            semantic(
                observation_text(entity, content),
                Some(entity.to_owned()),
                None,
                None,
            ),
            Some(graph_key("observation", &[entity, content])),
        )
        .await
    }

    async fn do_create_relations(&self, p: CreateRelationsParams) -> Result<Value> {
        batch_len("relations", p.relations.len())?;
        let mut out = Vec::with_capacity(p.relations.len());
        for r in p.relations {
            let (from, to, rel) = (r.from.trim(), r.to.trim(), r.relation_type.trim());
            let m = self
                .write(
                    MemoryContent::Relational(RelationalMemory {
                        subject: from.to_owned(),
                        predicate: rel.to_owned(),
                        object: to.to_owned(),
                        directed: true,
                    }),
                    Some(graph_key("relation", &[from, rel, to])),
                )
                .await?;
            out.push(json!({ "from": from, "to": to, "relationType": rel, "memory_id": m.id }));
        }
        Ok(json!({ "relations": out }))
    }

    async fn do_add_observations(&self, p: AddObservationsParams) -> Result<Value> {
        batch_len("observations", p.observations.len())?;
        let mut out = Vec::with_capacity(p.observations.len());
        for o in p.observations {
            let name = o.entity_name.trim();
            if name.is_empty() {
                return Err(Error::validation("entityName must not be empty"));
            }
            batch_len("contents", o.contents.len())?;
            let mut ids = Vec::new();
            for c in &o.contents {
                ids.push(self.write_observation(name, c).await?.id);
            }
            out.push(json!({ "entityName": name, "memory_ids": ids }));
        }
        Ok(json!({ "observations": out }))
    }

    async fn do_search_nodes(&self, p: SearchNodesParams) -> Result<Value> {
        let policy = RecallPolicy {
            max_results: 20,
            include_beliefs: false,
            ..RecallPolicy::default()
        };
        let r = self
            .engine
            .recall(
                &self.ctx,
                RecallRequest {
                    query: p.query,
                    policy,
                    session: None,
                },
            )
            .await?;
        let mut entities: BTreeMap<String, (Option<String>, Vec<String>)> = BTreeMap::new();
        let mut relations = Vec::new();
        let mut other = Vec::new();
        for hit in &r.results {
            match &hit.memory.content {
                MemoryContent::Semantic(s) if s.subject.is_some() => {
                    let name = s.subject.clone().unwrap_or_default();
                    let entry = entities.entry(name.clone()).or_default();
                    if s.predicate.as_deref() == Some("is_a") {
                        entry.0 = s.object.clone();
                    } else {
                        let prefix = format!("{name}: ");
                        let text = s
                            .proposition
                            .strip_prefix(&prefix)
                            .unwrap_or(&s.proposition);
                        entry.1.push(text.to_owned());
                    }
                }
                MemoryContent::Relational(rel) => relations.push(json!({
                    "from": rel.subject,
                    "to": rel.object,
                    "relationType": rel.predicate,
                })),
                _ => other.push(memory_view(&hit.memory)),
            }
        }
        let entities: Vec<Value> = entities
            .into_iter()
            .map(|(name, (kind, observations))| {
                json!({ "name": name, "entityType": kind, "observations": observations })
            })
            .collect();
        Ok(json!({
            "trace_id": r.trace_id,
            "entities": entities,
            "relations": relations,
            "memories": other,
        }))
    }

    async fn do_remember(&self, p: RememberParams) -> Result<Value> {
        let m = self
            .write(
                semantic(p.text.trim().to_owned(), p.subject, p.predicate, p.object),
                p.idempotency_key,
            )
            .await?;
        Ok(memory_view(&m))
    }

    async fn do_recall(&self, p: RecallParams) -> Result<Value> {
        let mut policy = RecallPolicy::default();
        if let Some(n) = p.max_results {
            policy.max_results = n;
        }
        if let Some(s) = p.min_score {
            policy.min_score = s;
        }
        if let Some(domains) = p.domains {
            policy.allowed_domains = domains
                .iter()
                .map(|d| {
                    serde_json::from_value::<MemoryDomain>(Value::String(d.clone()))
                        .map_err(|_| Error::validation(format!("unknown memory domain '{d}'")))
                })
                .collect::<Result<_>>()?;
        }
        if let Some(c) = p.include_candidates {
            policy.include_candidates = c;
        }
        if let Some(b) = p.include_beliefs {
            policy.include_beliefs = b;
        }
        if let Some(t) = p.as_of {
            let at = OffsetDateTime::parse(t.trim(), &Rfc3339)
                .map_err(|_| Error::validation("as_of must be an RFC 3339 timestamp"))?;
            policy.as_of_transaction_time = Some(at);
        }
        let r = self
            .engine
            .recall(
                &self.ctx,
                RecallRequest {
                    query: p.query,
                    policy,
                    session: None,
                },
            )
            .await?;
        let results: Vec<Value> = r
            .results
            .iter()
            .map(|h| {
                let mut v = memory_view(&h.memory);
                v["rank"] = json!(h.rank);
                v["score"] = json!(h.score);
                v
            })
            .collect();
        let beliefs: Vec<Value> = r
            .beliefs
            .iter()
            .map(|b| {
                let mut v = belief_view(&b.belief);
                v["rank"] = json!(b.rank);
                v["relevance"] = json!(b.relevance);
                v
            })
            .collect();
        Ok(json!({
            "trace_id": r.trace_id,
            "results": results,
            "beliefs": beliefs,
            "candidates_considered": r.trace.candidates_considered,
            "dense": r.trace.dense,
            "data_snapshot": r.trace.data_snapshot,
        }))
    }

    async fn do_get_recall_trace(&self, p: TraceParams) -> Result<Value> {
        let id: RecallTraceId = parse_id("trace_id", &p.trace_id)?;
        let t = self.engine.get_trace(&self.ctx, id).await?;
        let selected: Vec<Value> = t
            .selected()
            .map(|c| {
                json!({
                    "rank": c.rank,
                    "score": c.score,
                    "memory_id": c.memory_id,
                    "revision_id": c.revision_id,
                    "dense": c.dense_score,
                    "lexical": c.lexical_score,
                    "recency": c.recency_score,
                    "channels": c.channels,
                })
            })
            .collect();
        let beliefs: Vec<Value> = t
            .belief_candidates
            .iter()
            .filter(|c| c.selected)
            .map(|c| json!({ "rank": c.rank, "score": c.score, "belief_id": c.belief_id }))
            .collect();
        Ok(json!({
            "trace_id": t.id,
            "executed_at": rfc3339(t.executed_at),
            "query_hash": t.query_hash,
            "query_retained": t.query_text.is_some(),
            "scoring_version": t.scoring_version,
            "data_snapshot": t.data_snapshot,
            "dense": t.dense,
            "policy": t.policy,
            "candidates_considered": t.candidates.len(),
            "selected": selected,
            "beliefs_selected": beliefs,
            "metacognition": t.metacognition,
        }))
    }

    async fn do_replay_recall(&self, p: TraceParams) -> Result<Value> {
        let id: RecallTraceId = parse_id("trace_id", &p.trace_id)?;
        let r = self
            .engine
            .replay_recall(&self.ctx, id, ReplayRequest::default())
            .await?;
        let pairs = |v: &[(MemoryId, cs_core::MemoryRevisionId)]| -> Vec<Value> {
            v.iter()
                .map(|(m, rev)| json!({ "memory_id": m, "revision_id": rev }))
                .collect()
        };
        Ok(json!({
            "trace_id": r.trace_id,
            "identical": r.identical,
            "original_selected": pairs(&r.original_selected),
            "replayed_selected": pairs(&r.replayed_selected),
            "added": r.added,
            "removed": r.removed,
            "original_beliefs": r.original_beliefs,
            "replayed_beliefs": r.replayed_beliefs,
            "data_snapshot": r.replay.data_snapshot,
        }))
    }

    async fn do_observe(&self, p: ObserveParams) -> Result<Value> {
        if p.text.trim().is_empty() {
            return Err(Error::validation("text must not be empty"));
        }
        let view = self
            .engine
            .submit_observation(
                &self.ctx,
                NewObservation {
                    scope: self.scope.clone(),
                    content: p.text,
                    media_type: p.media_type,
                    source: SourceRef::UserInput { message_id: None },
                    observed_at: None,
                    idempotency_key: p.idempotency_key,
                },
            )
            .await?;
        let id = view.observation.id;
        let processed = match &self.worker {
            Some(w) => {
                w.drain().await?;
                true
            }
            None => false,
        };
        let view = self.engine.get_observation(&self.ctx, id).await?;
        let mut candidates = Vec::new();
        for run in &view.extraction_runs {
            for m in &run.candidate_ids {
                // A candidate may have been merged or purged since.
                if let Ok(m) = self.engine.get_memory(&self.ctx, *m).await {
                    candidates.push(memory_view(&m));
                }
            }
        }
        Ok(json!({
            "observation_id": id,
            "processed_inline": processed,
            "job_status": view.job.as_ref().map(|j| j.status),
            "candidates": candidates,
        }))
    }

    async fn do_promote(&self, p: PromoteParams) -> Result<Value> {
        let id: MemoryId = parse_id("memory_id", &p.memory_id)?;
        let reason = p
            .reason
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| "validated by the user via MCP".into());
        let m = self
            .engine
            .promote_memory(&self.ctx, id, StatusChange { reason })
            .await?;
        Ok(memory_view(&m))
    }

    async fn do_list_beliefs(&self, p: ListBeliefsParams) -> Result<Value> {
        let include_retracted = p.include_retracted.unwrap_or(false);
        let limit = p.limit.unwrap_or(50).clamp(1, 500) as usize;
        let mut beliefs: Vec<Belief> = self
            .engine
            .store()
            .beliefs_for(&self.ctx)
            .await?
            .into_iter()
            .filter(|b| b.is_current() && (include_retracted || b.status.is_live()))
            .collect();
        beliefs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        let total = beliefs.len();
        let out: Vec<Value> = beliefs.iter().take(limit).map(belief_view).collect();
        Ok(json!({ "total": total, "beliefs": out }))
    }

    async fn do_assert_belief(&self, p: AssertBeliefParams) -> Result<Value> {
        let polarity = match p.polarity.as_deref() {
            None => BeliefPolarity::Supports,
            Some(v) => BeliefPolarity::parse(v)
                .ok_or_else(|| Error::validation("polarity must be supports or refutes"))?,
        };
        let interpretation = match p.interpretation.as_deref() {
            None => ConfidenceInterpretation::HumanAssessment,
            Some(v) => ConfidenceInterpretation::parse(v)
                .ok_or_else(|| Error::validation("unknown confidence interpretation"))?,
        };
        let confidence = Confidence {
            value: p.confidence,
            interpretation,
            method: ASSERT_METHOD.into(),
        };
        // Validate before recording evidence so a bad call leaves nothing behind.
        confidence.validate()?;
        let proposition = p.proposition.trim().to_owned();
        if proposition.is_empty() {
            return Err(Error::validation("proposition must not be empty"));
        }
        let evidence = self
            .engine
            .record_evidence(
                &self.ctx,
                NewEvidence {
                    source: SourceRef::UserInput { message_id: None },
                    observed_at: None,
                    extraction: None,
                    content_hash: None,
                    summary: None,
                },
            )
            .await?;
        let belief = self
            .engine
            .create_belief(
                &self.ctx,
                NewBelief {
                    scope: self.scope.clone(),
                    proposition,
                    polarity,
                    confidence,
                    evidence: vec![BeliefEvidence {
                        evidence_id: evidence.id,
                        direction: EvidenceDirection::Supports,
                        strength: None,
                    }],
                    depends_on: Vec::new(),
                    valid_time: None,
                    revalidation_interval_days: None,
                    reason: p.reason.filter(|r| !r.trim().is_empty()),
                    idempotency_key: p.idempotency_key,
                },
            )
            .await?;
        Ok(belief_view(&belief))
    }

    /// Look up a belief by id (used by tests and future tools).
    pub async fn belief(&self, id: BeliefId) -> Result<Belief> {
        Ok(self.engine.get_belief(&self.ctx, id).await?.belief)
    }
}

#[tool_router]
impl SubstrateMcp {
    #[tool(
        description = "Create knowledge-graph entities (reference memory server compatible). Each entity becomes a semantic memory '<name> is a <entityType>' and each observation a semantic memory '<name>: <observation>'. Idempotent."
    )]
    pub async fn create_entities(
        &self,
        Parameters(p): Parameters<CreateEntitiesParams>,
    ) -> CallToolResult {
        reply(self.do_create_entities(p).await)
    }

    #[tool(
        description = "Create directed relations between entities (reference memory server compatible). Each becomes a relational memory (from, relationType, to). Idempotent."
    )]
    pub async fn create_relations(
        &self,
        Parameters(p): Parameters<CreateRelationsParams>,
    ) -> CallToolResult {
        reply(self.do_create_relations(p).await)
    }

    #[tool(
        description = "Add observations to entities (reference memory server compatible). Each observation becomes a semantic memory about the entity. Idempotent."
    )]
    pub async fn add_observations(
        &self,
        Parameters(p): Parameters<AddObservationsParams>,
    ) -> CallToolResult {
        reply(self.do_add_observations(p).await)
    }

    #[tool(
        description = "Search the knowledge graph (reference memory server compatible). Runs a traced recall and groups hits into entities, relations and other memories. Returns the recall trace id."
    )]
    pub async fn search_nodes(
        &self,
        Parameters(p): Parameters<SearchNodesParams>,
    ) -> CallToolResult {
        reply(self.do_search_nodes(p).await)
    }

    #[tool(
        description = "Remember a fact: creates an active semantic memory with provenance user_input, private to this user."
    )]
    pub async fn remember(&self, Parameters(p): Parameters<RememberParams>) -> CallToolResult {
        reply(self.do_remember(p).await)
    }

    #[tool(
        description = "Policy-controlled recall of memories and beliefs. Ranking is deterministic and every recall is traced: the result includes trace_id for get_recall_trace and replay_recall."
    )]
    pub async fn recall(&self, Parameters(p): Parameters<RecallParams>) -> CallToolResult {
        reply(self.do_recall(p).await)
    }

    #[tool(
        description = "Inspect a recorded recall: resolved policy, data snapshot, scores and channels of the selected memories."
    )]
    pub async fn get_recall_trace(&self, Parameters(p): Parameters<TraceParams>) -> CallToolResult {
        reply(self.do_get_recall_trace(p).await)
    }

    #[tool(
        description = "Exactly replay a recorded recall against the substrate as it was, and report whether it selected the same revisions."
    )]
    pub async fn replay_recall(&self, Parameters(p): Parameters<TraceParams>) -> CallToolResult {
        reply(self.do_replay_recall(p).await)
    }

    #[tool(
        description = "Submit raw text as an immutable observation. Extracted facts enter as unvalidated candidates (returned when processed inline); validate them with promote_memory."
    )]
    pub async fn observe(&self, Parameters(p): Parameters<ObserveParams>) -> CallToolResult {
        reply(self.do_observe(p).await)
    }

    #[tool(description = "Validate a candidate memory so recall treats it as trusted knowledge.")]
    pub async fn promote_memory(&self, Parameters(p): Parameters<PromoteParams>) -> CallToolResult {
        reply(self.do_promote(p).await)
    }

    #[tool(description = "List current beliefs, newest first.")]
    pub async fn list_beliefs(
        &self,
        Parameters(p): Parameters<ListBeliefsParams>,
    ) -> CallToolResult {
        reply(self.do_list_beliefs(p).await)
    }

    #[tool(
        description = "Assert a belief with an explicit confidence and interpretation, backed by user_input evidence."
    )]
    pub async fn assert_belief(
        &self,
        Parameters(p): Parameters<AssertBeliefParams>,
    ) -> CallToolResult {
        reply(self.do_assert_belief(p).await)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SubstrateMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("cs-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Cognitive Substrate memory for one user. Use `remember` for facts the user states, \
                 `recall` before answering from memory (cite the trace_id), and `observe` for raw text \
                 whose facts need validation (`promote_memory`). Beliefs carry explicit confidence.",
            )
    }
}
