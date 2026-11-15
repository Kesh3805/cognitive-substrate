//! `cs-bench`: run a preregistered experiment.
//!
//! ```text
//! cs-bench 005|006 [--store memory|postgres] [--dev] [--out DIR]
//! ```
//!
//! `--dev` runs the development seeds (never reported as results).
//! Postgres reads `CS_BENCH_DATABASE_URL`, which must point to a dedicated
//! database: runs leave their tenants, traces and queued background jobs
//! behind, which would disturb tests sharing the database.

use std::{io::Write, path::PathBuf, process::Command, sync::Arc};

use cs_bench::{exp005, exp006};
use cs_storage::{CognitiveStore, InMemoryStore};
use serde_json::json;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn command_output(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

async fn store(kind: &str) -> Result<Arc<dyn CognitiveStore>, String> {
    match kind {
        "memory" => Ok(Arc::new(InMemoryStore::new())),
        "postgres" => {
            let url = std::env::var("CS_BENCH_DATABASE_URL")
                .map_err(|_| "set CS_BENCH_DATABASE_URL".to_owned())?;
            let pg = cs_postgres::PgStore::connect(&url, 8)
                .await
                .map_err(|e| e.to_string())?;
            pg.migrate().await.map_err(|e| e.to_string())?;
            Ok(Arc::new(pg))
        }
        other => Err(format!("unknown store {other}")),
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let experiment = args.get(1).cloned().unwrap_or_default();
    let (dir, dev_seeds, held_out): (&str, &[u64], &[u64]) = match experiment.as_str() {
        "005" => (
            "005-candidate-generation",
            &exp005::DEV_SEEDS,
            &exp005::HELD_OUT_SEEDS,
        ),
        "006" => (
            "006-single-round-trip-lexical",
            &exp006::DEV_SEEDS,
            &exp006::HELD_OUT_SEEDS,
        ),
        _ => {
            return Err(
                "usage: cs-bench 005|006 [--store memory|postgres] [--dev] [--out DIR]".into(),
            );
        }
    };
    let kind = arg(&args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let seeds: Vec<u64> = if dev {
        dev_seeds.to_vec()
    } else {
        held_out.to_vec()
    };
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/{dir}/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    let mut rows = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: generating and loading");
        // A fresh store per seed (Postgres: a fresh tenant in one database).
        let s = store(&kind).await?;
        rows.extend(match experiment.as_str() {
            "005" => exp005::run_seed(s, *seed).await?,
            _ => exp006::run_seed(s, *seed).await?,
        });
    }
    let summary = match experiment.as_str() {
        "005" => exp005::summarize(&rows),
        _ => exp006::summarize(&rows),
    };

    let mut raw = std::fs::File::create(out.join("raw.jsonl")).map_err(|e| e.to_string())?;
    for r in &rows {
        writeln!(
            raw,
            "{}",
            serde_json::to_string(r).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
    }
    let manifest = json!({
        "experiment": dir,
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "embedder": "cs/hashing@1 (384d)",
        "commit": command_output("git", &["rev-parse", "HEAD"]),
        "rustc": command_output("rustc", &["--version"]),
        "bootstrap": { "resamples": exp005::BOOTSTRAP_RESAMPLES, "seed": exp005::BOOTSTRAP_SEED },
    });
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    println!(
        "{:<6} {:>7} {:>7} {:>7} {:>9} {:>7} {:>9}",
        "", "nDCG", "MRR", "R@10", "cand.R", "tokens", "p95 ms"
    );
    for (n, a) in &summary.overall {
        println!(
            "{n:<6} {:>7.4} {:>7.4} {:>7.4} {:>9.4} {:>7.1} {:>9.2}",
            a.ndcg, a.mrr, a.recall, a.candidate_recall, a.tokens, a.latency_p95_ms
        );
    }
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt {}: {}", summary.primary, summary.adopt);
    Ok(())
}
