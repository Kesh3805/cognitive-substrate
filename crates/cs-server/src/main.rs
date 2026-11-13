//! `cognitive-substrate` — service binary (composition root).
//!
//! ```text
//! cognitive-substrate [serve]   # run with the role from CS_ROLE
//! cognitive-substrate worker    # background worker only (no HTTP)
//! cognitive-substrate migrate   # apply database migrations and exit
//! ```
//!
//! Configuration (environment):
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `CS_BIND` | `0.0.0.0:8080` | Listen address |
//! | `DATABASE_URL` | — | PostgreSQL URL; in-memory store if unset |
//! | `CS_DB_MAX_CONNECTIONS` | `10` | |
//! | `CS_AUTO_MIGRATE` | `true` | Run migrations on start |
//! | `CS_ROLE` | `api` with Postgres, `all` in memory | `api` (HTTP only, never claims jobs), `worker` (jobs only), `all` (both; development) |
//! | `CS_API_KEY` | — | Require `Authorization: Bearer <key>` |
//! | `CS_LOG_FORMAT` | `pretty` | `pretty` or `json` |
//! | `CS_WORKER_POLL_MS` | `1000` | Idle poll interval |
//! | `CS_AUTO_RESOLVE` | `true` | Queue resolution on every write |
//! | `CS_EXTRACTOR` | `rules` | `rules` or `openai` |
//! | `CS_EXTRACTOR_URL` / `_MODEL` / `_API_KEY` | — | For `openai` |
//! | `CS_EMBEDDER` | `hashing` | `hashing`, `openai`, `ollama`, `fastembed`, `none` |
//! | `CS_EMBEDDER_URL` / `_MODEL` / `_API_KEY` | — | For `openai` / `ollama` |
//! | `CS_EMBEDDER_DIMENSIONS` | `384` | Vector size (required for remote models) |
//! | `CS_EMBEDDER_REVISION` | `unversioned` | Model revision or digest (part of the space id) |
//! | `CS_EMBEDDER_QUERY_PREFIX` / `_DOCUMENT_PREFIX` | — | Asymmetric model prefixes |
//! | `CS_EMBEDDER_MAX_BATCH_ITEMS` / `_MAX_BATCH_BYTES` | `64` / `96000` | HTTP batch limits |
//! | `CS_SHADOW_EMBEDDER*` | — | Same settings for a migration's shadow space |
//! | `CS_EMBED_INLINE` | `true` locally, `false` for HTTP | Embed during writes or always defer to jobs |
//! | `CS_RESOLUTION_CONFIG` | — | JSON file of calibrated duplicate thresholds |
//! | `CS_MIN_ACTIVE_COVERAGE` | `0.995` | Refuse to serve an active space below this coverage |
//! | `CS_ALLOW_LOW_COVERAGE` | `false` | Override the coverage gate |
//! | `RUST_LOG` | `info` | |

use std::{process::ExitCode, sync::Arc, time::Duration};

use cs_embedder_http::{HttpEmbedder, HttpEmbedderConfig, Protocol};
use cs_engine::{
    CognitiveEngine, Embedder, EmbeddingSpace, Extractor, HashingEmbedder, ProcessingConfig,
    RuleExtractor, Worker, WorkerConfig, embedding::PREPROCESSING_VERSION,
};
use cs_extractor_http::{HttpExtractor, HttpExtractorConfig};
use cs_policy::ResolutionConfig;
use cs_postgres::PgStore;
use cs_server::{AppState, AuthConfig, router};
use cs_storage::{CognitiveStore, InMemoryStore};
use tracing_subscriber::{EnvFilter, fmt};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Result<Option<T>, String> {
    env(name)
        .map(|v| {
            v.parse()
                .map_err(|_| format!("{name} has an invalid value"))
        })
        .transpose()
}

fn flag(name: &str, default: bool) -> bool {
    match env(name).as_deref() {
        Some("true" | "1" | "yes") => true,
        Some("false" | "0" | "no") => false,
        _ => default,
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = fmt().with_env_filter(filter);
    if env("CS_LOG_FORMAT").as_deref() == Some("json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

async fn postgres() -> Result<Option<PgStore>, String> {
    let Some(url) = env("DATABASE_URL") else {
        return Ok(None);
    };
    let max = env_parse("CS_DB_MAX_CONNECTIONS")?.unwrap_or(10);
    PgStore::connect(&url, max)
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = terminate => {} }
    tracing::info!("shutting down");
}

/// A configured embedder and whether it runs in-process (cheap enough to
/// embed inline during writes).
type Configured = (Arc<dyn Embedder>, bool);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Api,
    Worker,
    All,
}

/// Build an embedder from `{prefix}`, `{prefix}_URL`, `{prefix}_MODEL`, …
/// Returns `None` for `none` or an unset shadow.
fn embedder_from_env(
    prefix: &str,
    default_kind: Option<&str>,
) -> Result<Option<Configured>, String> {
    let var = |s: &str| env(&format!("{prefix}{s}"));
    let kind = var("").or_else(|| default_kind.map(str::to_owned));
    let Some(kind) = kind else { return Ok(None) };
    let dims: Option<u32> = env_parse(&format!("{prefix}_DIMENSIONS"))?;
    let space = |provider: &str, model: String, dimensions: u32| EmbeddingSpace {
        provider: provider.into(),
        model,
        revision: var("_REVISION").unwrap_or_else(|| "unversioned".into()),
        dimensions,
        normalized: true,
        preprocessing: PREPROCESSING_VERSION,
        document_prefix: var("_DOCUMENT_PREFIX").unwrap_or_default(),
        query_prefix: var("_QUERY_PREFIX").unwrap_or_default(),
    };
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
                space: space(&kind, model, dimensions),
            })?;
            Ok(Some((Arc::new(e), false)))
        }
        "fastembed" => fastembed(var("_MODEL")),
        other => Err(format!("unknown {prefix} '{other}'")),
    }
}

#[cfg(feature = "fastembed")]
fn fastembed(model: Option<String>) -> Result<Option<Configured>, String> {
    use cs_embedder_fastembed::{FastEmbedder, LocalModel};
    let name = model.unwrap_or_else(|| "bge-small-en-v1.5".into());
    let m = LocalModel::parse(&name).ok_or(format!("unsupported fastembed model '{name}'"))?;
    let cache = env("CS_FASTEMBED_CACHE").map(std::path::PathBuf::from);
    Ok(Some((Arc::new(FastEmbedder::new(m, cache)?), true)))
}

#[cfg(not(feature = "fastembed"))]
fn fastembed(_: Option<String>) -> Result<Option<Configured>, String> {
    Err("this binary was built without the `fastembed` feature".into())
}

fn extractor() -> Result<Arc<dyn Extractor>, String> {
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

async fn build(role_override: Option<Role>) -> Result<(CognitiveEngine, Role, bool), String> {
    let pg = postgres().await?;
    let durable = pg.is_some();
    let store: Arc<dyn CognitiveStore> = match pg {
        Some(pg) => {
            if flag("CS_AUTO_MIGRATE", true) {
                pg.migrate().await.map_err(|e| e.to_string())?;
            }
            tracing::info!("using PostgreSQL store");
            Arc::new(pg)
        }
        None => {
            tracing::warn!("DATABASE_URL not set: using the in-memory store (data is not durable)");
            Arc::new(InMemoryStore::new())
        }
    };
    let role = match role_override {
        Some(r) => r,
        None => match env("CS_ROLE").as_deref() {
            Some("api") => Role::Api,
            Some("worker") => Role::Worker,
            Some("all") => Role::All,
            Some(other) => return Err(format!("unknown CS_ROLE '{other}' (api, worker or all)")),
            // A separate worker process cannot share an in-memory store.
            None if durable => Role::Api,
            None => Role::All,
        },
    };
    if !durable && role != Role::All {
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
        tracing::info!(space = %s.space().id(), "shadow embedding space enabled (dual write)");
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

    // Cutover gate: never serve recall from a space that has not been
    // backfilled, which would silently degrade relevance.
    if let Some((space, coverage)) = engine
        .active_space_coverage()
        .await
        .map_err(|e| e.to_string())?
    {
        let min: f64 = env_parse("CS_MIN_ACTIVE_COVERAGE")?.unwrap_or(0.995);
        let ratio = coverage.ratio();
        tracing::info!(%space, ratio, "active embedding space coverage");
        if ratio < min && !flag("CS_ALLOW_LOW_COVERAGE", false) {
            return Err(format!(
                "active embedding space {space} covers {:.1}% of current revisions (minimum {:.1}%). \
                 Backfill it first as a shadow space (CS_SHADOW_EMBEDDER + POST /v1/embeddings/reindex), \
                 or set CS_ALLOW_LOW_COVERAGE=true",
                ratio * 100.0,
                min * 100.0
            ));
        }
    }
    Ok((engine, role, durable))
}

async fn run() -> Result<(), String> {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_owned());
    match command.as_str() {
        "migrate" => {
            let store = postgres()
                .await?
                .ok_or("DATABASE_URL is required for migrate")?;
            store.migrate().await.map_err(|e| e.to_string())?;
            tracing::info!("migrations applied");
            Ok(())
        }
        "serve" => serve(None).await,
        "worker" => serve(Some(Role::Worker)).await,
        "-h" | "--help" | "help" => {
            println!("usage: cognitive-substrate [serve|worker|migrate]");
            Ok(())
        }
        other => Err(format!(
            "unknown command '{other}' (expected serve, worker or migrate)"
        )),
    }
}

async fn serve(role_override: Option<Role>) -> Result<(), String> {
    let (engine, role, _) = build(role_override).await?;
    tracing::info!(?role, "starting");
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let worker = if matches!(role, Role::Worker | Role::All) {
        let w = Worker::new(engine.clone(), extractor()?, WorkerConfig::default());
        tracing::info!(owner = %w.config().owner, "background worker enabled");
        Some(tokio::spawn(run_worker(w, stop_rx)))
    } else {
        tracing::warn!(
            "CS_ROLE=api: this process never claims jobs; run `cognitive-substrate worker` alongside it"
        );
        None
    };

    if role == Role::Worker {
        shutdown_signal().await;
    } else {
        let api_key = env("CS_API_KEY");
        if api_key.is_none() {
            tracing::warn!("CS_API_KEY not set: the API accepts unauthenticated principal headers");
        }
        let app = router(AppState::new(engine, AuthConfig { api_key }));
        let bind = env("CS_BIND").unwrap_or_else(|| "0.0.0.0:8080".to_owned());
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(%bind, "cognitive-substrate listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| e.to_string())?;
    }
    let _ = stop_tx.send(true);
    if let Some(handle) = worker {
        let _ = handle.await;
    }
    Ok(())
}

/// Poll for work until shutdown. A pass that finds work runs again
/// immediately; an idle pass waits `CS_WORKER_POLL_MS`. On shutdown the
/// worker stops claiming; leases still held expire and are reclaimed.
async fn run_worker(worker: Worker, mut stop: tokio::sync::watch::Receiver<bool>) {
    let idle = Duration::from_millis(
        env("CS_WORKER_POLL_MS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000),
    );
    loop {
        if *stop.borrow() {
            break;
        }
        let busy = match worker.run_once().await {
            Ok(r) => {
                if r.claimed > 0 {
                    tracing::debug!(?r, "worker pass");
                }
                r.claimed > 0
            }
            Err(e) => {
                tracing::error!(error = %e, "worker pass failed");
                false
            }
        };
        if !busy {
            tokio::select! {
                _ = tokio::time::sleep(idle) => {}
                _ = stop.changed() => {}
            }
        }
    }
    tracing::info!("background worker stopped");
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal");
            ExitCode::FAILURE
        }
    }
}
