//! Environment configuration, following the `cognitive-substrate` service's
//! conventions (`DATABASE_URL`, `CS_EMBEDDER*`, `CS_EXTRACTOR*`, ...) plus
//! the MCP identity (`CS_MCP_TENANT`, `CS_MCP_USER`).

use std::{sync::Arc, time::Duration};

use cs_core::{AccessContext, TenantId, UserId};
use cs_embedder_http::{HttpEmbedder, HttpEmbedderConfig, Protocol};
use cs_engine::{
    CognitiveEngine, Embedder, EmbeddingSpace, Extractor, HashingEmbedder, ProcessingConfig,
    RuleExtractor, Worker, WorkerConfig, embedding::PREPROCESSING_VERSION,
};
use cs_extractor_http::{HttpExtractor, HttpExtractorConfig};
use cs_policy::ResolutionConfig;
use cs_postgres::PgStore;
use cs_storage::{CognitiveStore, InMemoryStore};

pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn env_parse<T: std::str::FromStr>(name: &str) -> Result<Option<T>, String> {
    env(name)
        .map(|v| {
            v.trim()
                .parse()
                .map_err(|_| format!("{name} has an invalid value"))
        })
        .transpose()
}

pub fn flag(name: &str, default: bool) -> bool {
    match env(name).as_deref() {
        Some("true" | "1" | "yes") => true,
        Some("false" | "0" | "no") => false,
        _ => default,
    }
}

/// A built engine and how this process should treat the job queue.
#[derive(Debug)]
pub struct Built {
    pub engine: CognitiveEngine,
    /// Whether the store is PostgreSQL (durable) rather than in-memory.
    pub durable: bool,
    /// Whether this process claims jobs (`CS_ROLE=all`, the in-memory
    /// default) or leaves them to a separate `cognitive-substrate worker`
    /// (`CS_ROLE=api`, the PostgreSQL default).
    pub runs_jobs: bool,
}

/// Resolve the principal. With a durable store both ids are required,
/// since generated ids would make the data unreachable on the next start.
/// In memory, missing ids are generated and printed to stderr.
pub fn identity(durable: bool) -> Result<AccessContext, String> {
    let tenant: Option<TenantId> = env_parse("CS_MCP_TENANT")?;
    let user: Option<UserId> = env_parse("CS_MCP_USER")?;
    match (tenant, user) {
        (Some(t), Some(u)) => Ok(AccessContext::user(t, u)),
        _ if durable => {
            Err("CS_MCP_TENANT and CS_MCP_USER (UUIDs) are required with DATABASE_URL".into())
        }
        (t, u) => {
            let t = t.unwrap_or_else(TenantId::new_v7);
            let u = u.unwrap_or_else(UserId::new_v7);
            eprintln!("cs-mcp: in-memory identity CS_MCP_TENANT={t} CS_MCP_USER={u}");
            Ok(AccessContext::user(t, u))
        }
    }
}

type Configured = (Arc<dyn Embedder>, bool);

/// `hashing` (default), `openai`, `ollama` or `none`, configured exactly as
/// for the service. (`fastembed` requires the service binary's feature.)
fn embedder_from_env(
    prefix: &str,
    default_kind: Option<&str>,
) -> Result<Option<Configured>, String> {
    let var = |s: &str| env(&format!("{prefix}{s}"));
    let kind = var("").or_else(|| default_kind.map(str::to_owned));
    let Some(kind) = kind else { return Ok(None) };
    let dims: Option<u32> = env_parse(&format!("{prefix}_DIMENSIONS"))?;
    match kind.as_str() {
        "none" => Ok(None),
        "hashing" => Ok(Some((
            Arc::new(HashingEmbedder::new(dims.unwrap_or(384) as usize)),
            true,
        ))),
        "openai" | "ollama" => {
            let protocol = if kind == "ollama" {
                Protocol::OllamaNative
            } else {
                Protocol::OpenAi
            };
            let base_url = var("_URL").ok_or(format!("{prefix}_URL is required"))?;
            let model = var("_MODEL").ok_or(format!("{prefix}_MODEL is required"))?;
            let dimensions = dims.ok_or(format!("{prefix}_DIMENSIONS is required for {kind}"))?;
            let e = HttpEmbedder::new(HttpEmbedderConfig {
                protocol,
                base_url,
                api_key: var("_API_KEY"),
                timeout: Duration::from_secs(30),
                max_batch_items: env_parse(&format!("{prefix}_MAX_BATCH_ITEMS"))?.unwrap_or(64),
                max_batch_bytes: env_parse(&format!("{prefix}_MAX_BATCH_BYTES"))?.unwrap_or(96_000),
                space: EmbeddingSpace {
                    provider: kind.clone(),
                    model,
                    revision: var("_REVISION").unwrap_or_else(|| "unversioned".into()),
                    dimensions,
                    normalized: true,
                    preprocessing: PREPROCESSING_VERSION,
                    document_prefix: var("_DOCUMENT_PREFIX").unwrap_or_default(),
                    query_prefix: var("_QUERY_PREFIX").unwrap_or_default(),
                },
            })?;
            Ok(Some((Arc::new(e), false)))
        }
        "fastembed" => Err(format!(
            "{prefix}=fastembed is only available in the cognitive-substrate service built with `fastembed`"
        )),
        other => Err(format!("unknown {prefix} '{other}'")),
    }
}

pub fn extractor() -> Result<Arc<dyn Extractor>, String> {
    match env("CS_EXTRACTOR").as_deref().unwrap_or("rules") {
        "rules" => Ok(Arc::new(RuleExtractor)),
        "openai" => {
            let base_url = env("CS_EXTRACTOR_URL").ok_or("CS_EXTRACTOR_URL is required")?;
            let model = env("CS_EXTRACTOR_MODEL").ok_or("CS_EXTRACTOR_MODEL is required")?;
            Ok(Arc::new(HttpExtractor::new(HttpExtractorConfig {
                base_url,
                model,
                api_key: env("CS_EXTRACTOR_API_KEY"),
                timeout: Duration::from_secs(60),
            })?))
        }
        other => Err(format!(
            "unknown CS_EXTRACTOR '{other}' (expected rules or openai)"
        )),
    }
}

fn resolution_config() -> Result<ResolutionConfig, String> {
    match env("CS_RESOLUTION_CONFIG") {
        None => Ok(ResolutionConfig::default()),
        Some(path) => {
            let json = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            ResolutionConfig::from_json(&json).map_err(|e| format!("{path}: {e}"))
        }
    }
}

/// Build the store and engine from the environment.
pub async fn build() -> Result<Built, String> {
    let durable;
    let store: Arc<dyn CognitiveStore> = match env("DATABASE_URL") {
        Some(url) => {
            let max = env_parse("CS_DB_MAX_CONNECTIONS")?.unwrap_or(10);
            let pg = PgStore::connect(&url, max)
                .await
                .map_err(|e| e.to_string())?;
            if flag("CS_AUTO_MIGRATE", true) {
                pg.migrate().await.map_err(|e| e.to_string())?;
            }
            tracing::info!("using PostgreSQL store");
            durable = true;
            Arc::new(pg)
        }
        None => {
            tracing::warn!("DATABASE_URL not set: using the in-memory store (data is not durable)");
            durable = false;
            Arc::new(InMemoryStore::new())
        }
    };
    let runs_jobs = match env("CS_ROLE").as_deref() {
        Some("all") => true,
        Some("api") => false,
        Some(other) => return Err(format!("unknown CS_ROLE '{other}' for cs-mcp (api or all)")),
        None => !durable,
    };
    if !durable && !runs_jobs {
        return Err("the in-memory store requires CS_ROLE=all (workers cannot share it)".into());
    }

    let active = embedder_from_env("CS_EMBEDDER", Some("hashing"))?;
    let shadow = embedder_from_env("CS_SHADOW_EMBEDDER", None)?;
    let resolution = resolution_config()?;
    let mut builder = CognitiveEngine::builder(store).without_embedder();
    let mut inline_default = true;
    if let Some((e, local)) = &active {
        if !resolution.is_calibrated(&e.space().id()) {
            tracing::warn!(space = %e.space().id(), "no calibrated duplicate thresholds for this space: dense duplicate detection is disabled");
        }
        inline_default = *local;
        builder = builder.embedder(e.clone());
    }
    if let Some((s, _)) = shadow {
        builder = builder.shadow_embedder(s);
    }
    let engine = builder
        .processing(ProcessingConfig {
            auto_resolve: flag("CS_AUTO_RESOLVE", true),
            embed_inline: flag("CS_EMBED_INLINE", inline_default),
            resolution,
            ..ProcessingConfig::default()
        })
        .build();

    // Same cutover gate as the service: never recall from an
    // under-backfilled embedding space.
    if let Some((space, coverage)) = engine
        .active_space_coverage()
        .await
        .map_err(|e| e.to_string())?
    {
        let min: f64 = env_parse("CS_MIN_ACTIVE_COVERAGE")?.unwrap_or(0.995);
        let ratio = coverage.ratio();
        if ratio < min && !flag("CS_ALLOW_LOW_COVERAGE", false) {
            return Err(format!(
                "active embedding space {space} covers {:.1}% of current revisions (minimum {:.1}%); \
                 backfill it with the service first, or set CS_ALLOW_LOW_COVERAGE=true",
                ratio * 100.0,
                min * 100.0
            ));
        }
    }
    Ok(Built {
        engine,
        durable,
        runs_jobs,
    })
}

/// The in-process worker, when this process runs jobs.
pub fn worker(built: &Built) -> Result<Option<Worker>, String> {
    if !built.runs_jobs {
        return Ok(None);
    }
    Ok(Some(Worker::new(
        built.engine.clone(),
        extractor()?,
        WorkerConfig::default(),
    )))
}
