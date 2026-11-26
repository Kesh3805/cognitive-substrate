//! `cs-mcp`: Cognitive Substrate over the Model Context Protocol.
//!
//! ```text
//! cs-mcp                 # stdio transport (default)
//! cs-mcp --http [ADDR]   # Streamable HTTP at http://ADDR/mcp
//! ```
//!
//! Configuration (environment), in addition to the service's store,
//! embedder and extractor variables (`DATABASE_URL`, `CS_EMBEDDER*`,
//! `CS_EXTRACTOR*`, `CS_RESOLUTION_CONFIG`, ...):
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `CS_MCP_TENANT` / `CS_MCP_USER` | generated (in memory only) | The principal, as UUIDs; required with `DATABASE_URL` |
//! | `CS_MCP_TRANSPORT` | `stdio` | `stdio` or `http` |
//! | `CS_MCP_BIND` | `127.0.0.1:8765` | HTTP listen address |
//! | `CS_MCP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Accepted `Host` headers (DNS-rebinding protection) |
//! | `CS_MCP_API_KEY` | — | Require `Authorization: Bearer <key>` over HTTP |
//! | `CS_ROLE` | `api` with Postgres, `all` in memory | `all` processes jobs in this process |
//! | `CS_WORKER_POLL_MS` | `1000` | Idle poll interval of the background worker |
//! | `RUST_LOG` | `info` | Logs go to stderr and never contain memory content |

use std::{process::ExitCode, time::Duration};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use cs_engine::Worker;
use cs_mcp::{SubstrateMcp, config};
use rmcp::{
    ServiceExt,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use tracing_subscriber::{EnvFilter, fmt};

const USAGE: &str = "usage: cs-mcp [--stdio | --http [ADDR]]";

#[derive(Debug, PartialEq, Eq)]
enum Transport {
    Stdio,
    Http(String),
}

fn transport() -> Result<Transport, String> {
    let default_bind = || config::env("CS_MCP_BIND").unwrap_or_else(|| "127.0.0.1:8765".into());
    let mut args = std::env::args().skip(1);
    let mut chosen = match config::env("CS_MCP_TRANSPORT").as_deref() {
        None | Some("stdio") => Transport::Stdio,
        Some("http") => Transport::Http(default_bind()),
        Some(other) => {
            return Err(format!(
                "unknown CS_MCP_TRANSPORT '{other}' (stdio or http)"
            ));
        }
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdio" => chosen = Transport::Stdio,
            "--http" => chosen = Transport::Http(args.next().unwrap_or_else(default_bind)),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument '{other}'\n{USAGE}")),
        }
    }
    Ok(chosen)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // The SDK's debug logs include request payloads (memory content): cap them.
    let filter = match "rmcp=warn".parse() {
        Ok(d) => filter.add_directive(d),
        Err(_) => filter,
    };
    // stdout carries the stdio transport: logs must go to stderr.
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Poll for background work (resolution, embedding backfill) until stopped.
async fn run_worker(worker: Worker, mut stop: tokio::sync::watch::Receiver<bool>) {
    let idle = Duration::from_millis(
        config::env_parse("CS_WORKER_POLL_MS")
            .ok()
            .flatten()
            .unwrap_or(1000),
    );
    loop {
        if *stop.borrow() {
            break;
        }
        let busy = match worker.run_once().await {
            Ok(r) => r.claimed > 0,
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
}

async fn require_key(State(key): State<String>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|v| v == key);
    if ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
    }
}

async fn serve_http(server: SubstrateMcp, bind: String) -> Result<(), String> {
    let mut cfg = StreamableHttpServerConfig::default();
    if let Some(hosts) = config::env("CS_MCP_ALLOWED_HOSTS") {
        cfg = cfg.with_allowed_hosts(hosts.split(',').map(|h| h.trim().to_owned()));
    }
    let cancel = cfg.cancellation_token.clone();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        cfg,
    );
    let mut app = axum::Router::new().nest_service("/mcp", service);
    match config::env("CS_MCP_API_KEY") {
        Some(key) => app = app.layer(middleware::from_fn_with_state(key, require_key)),
        None => tracing::warn!("CS_MCP_API_KEY not set: the HTTP endpoint is unauthenticated"),
    }
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(%bind, "cs-mcp listening on http://{bind}/mcp");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            cancel.cancel();
        })
        .await
        .map_err(|e| e.to_string())
}

async fn run() -> Result<(), String> {
    let transport = transport()?;
    let built = config::build().await?;
    let ctx = config::identity(built.durable)?;
    let inline = config::worker(&built)?;
    if inline.is_none() {
        tracing::warn!(
            "CS_ROLE=api: observations are processed by a separate `cognitive-substrate worker`"
        );
    }
    let server = SubstrateMcp::new(built.engine, ctx, inline.clone()).map_err(|e| e.to_string())?;

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let background = inline.map(|w| tokio::spawn(run_worker(w, stop_rx)));

    let result = match transport {
        Transport::Stdio => {
            tracing::info!("cs-mcp serving over stdio");
            match server.serve(rmcp::transport::stdio()).await {
                Ok(running) => running
                    .waiting()
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        Transport::Http(bind) => serve_http(server, bind).await,
    };
    let _ = stop_tx.send(true);
    if let Some(handle) = background {
        let _ = handle.await;
    }
    result
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
