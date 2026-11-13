//! `cognitive-substrate` — standalone service binary (composition root).
//!
//! ```text
//! cognitive-substrate [serve]   # run the HTTP service (default)
//! cognitive-substrate migrate   # apply database migrations and exit
//! ```
//!
//! Configuration (environment):
//!
//! | Variable                  | Default        |                                         |
//! |---------------------------|----------------|-----------------------------------------|
//! | `CS_BIND`                 | `0.0.0.0:8080` | Listen address                          |
//! | `DATABASE_URL`            | —              | PostgreSQL URL; in-memory store if unset |
//! | `CS_DB_MAX_CONNECTIONS`   | `10`           |                                         |
//! | `CS_AUTO_MIGRATE`         | `true`         | Run migrations on start                 |
//! | `CS_API_KEY`              | —              | Require `Authorization: Bearer <key>`   |
//! | `CS_EMBEDDING_DIMENSIONS` | `384`          | Hashing embedder dimensionality         |
//! | `CS_LOG_FORMAT`           | `pretty`       | `pretty` or `json`                      |
//! | `CS_WORKER`               | `true`         | Run the background worker in-process    |
//! | `CS_WORKER_POLL_MS`       | `1000`         | Idle poll interval                      |
//! | `CS_AUTO_RESOLVE`         | `true`         | Queue resolution on every write         |
//! | `CS_EXTRACTOR`            | `rules`        | `rules` or `openai` (any compatible API) |
//! | `CS_EXTRACTOR_URL`        | —              | e.g. `http://localhost:11434/v1`        |
//! | `CS_EXTRACTOR_MODEL`      | —              | Model name for `openai`                 |
//! | `CS_EXTRACTOR_API_KEY`    | —              | Bearer key for `openai`                 |
//! | `RUST_LOG`                | `info`         |                                         |

use std::{process::ExitCode, sync::Arc};

use cs_engine::{
    CognitiveEngine, Extractor, HashingEmbedder, ProcessingConfig, RuleExtractor, Worker,
    WorkerConfig,
};
use cs_extractor_http::{HttpExtractor, HttpExtractorConfig};
use cs_postgres::PgStore;
use cs_server::{AppState, AuthConfig, router};
use cs_storage::{CognitiveStore, InMemoryStore};
use tracing_subscriber::{EnvFilter, fmt};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
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
    let max = env("CS_DB_MAX_CONNECTIONS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
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
        "serve" => serve().await,
        "-h" | "--help" | "help" => {
            println!("usage: cognitive-substrate [serve|migrate]");
            Ok(())
        }
        other => Err(format!(
            "unknown command '{other}' (expected serve or migrate)"
        )),
    }
}

async fn serve() -> Result<(), String> {
    let store: Arc<dyn CognitiveStore> = match postgres().await? {
        Some(pg) => {
            if env("CS_AUTO_MIGRATE").as_deref() != Some("false") {
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
    let dims = env("CS_EMBEDDING_DIMENSIONS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(384);
    let engine = CognitiveEngine::builder(store)
        .embedder(Arc::new(HashingEmbedder::new(dims)))
        .processing(ProcessingConfig {
            auto_resolve: env("CS_AUTO_RESOLVE").as_deref() != Some("false"),
            ..ProcessingConfig::default()
        })
        .build();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let worker = if env("CS_WORKER").as_deref() == Some("false") {
        None
    } else {
        let w = Worker::new(engine.clone(), extractor()?, WorkerConfig::default());
        tracing::info!(owner = %w.config().owner, "background worker enabled");
        Some(tokio::spawn(run_worker(w, stop_rx)))
    };
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
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string());
    let _ = stop_tx.send(true);
    if let Some(handle) = worker {
        let _ = handle.await;
    }
    served
}

fn extractor() -> Result<Arc<dyn Extractor>, String> {
    match env("CS_EXTRACTOR").as_deref().unwrap_or("rules") {
        "rules" => Ok(Arc::new(RuleExtractor)),
        "openai" => {
            let base_url = env("CS_EXTRACTOR_URL").ok_or("CS_EXTRACTOR_URL is required")?;
            let model = env("CS_EXTRACTOR_MODEL").ok_or("CS_EXTRACTOR_MODEL is required")?;
            let ex = HttpExtractor::new(HttpExtractorConfig {
                base_url,
                model,
                api_key: env("CS_EXTRACTOR_API_KEY"),
                timeout: std::time::Duration::from_secs(60),
            })?;
            Ok(Arc::new(ex))
        }
        other => Err(format!("unknown CS_EXTRACTOR '{other}' (expected rules or openai)")),
    }
}

/// Poll for work until shutdown. A pass that finds work runs again
/// immediately; an idle pass waits `CS_WORKER_POLL_MS`. In-flight jobs
/// finish (or their leases expire and are reclaimed) on shutdown.
async fn run_worker(worker: Worker, mut stop: tokio::sync::watch::Receiver<bool>) {
    let idle = std::time::Duration::from_millis(
        env("CS_WORKER_POLL_MS").and_then(|v| v.parse().ok()).unwrap_or(1000),
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
