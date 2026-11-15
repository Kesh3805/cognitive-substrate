//! `cs-bench`: run a preregistered experiment.
//!
//! ```text
//! cs-bench 005|006|008|009 [--store memory|postgres] [--dev] [--out DIR]
//! cs-bench 007 --reset [--dev] [--out DIR]
//! ```
//!
//! `--dev` runs the development seeds (never reported as results).
//! Postgres reads `CS_BENCH_DATABASE_URL`, which must point to a dedicated
//! database: runs leave their tenants, traces and queued background jobs
//! behind, which would disturb tests sharing the database.

use std::{io::Write, path::PathBuf, process::Command, sync::Arc};

use cs_bench::{exp005, exp006, exp007, exp008, exp009};
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
    if experiment == "007" {
        return run_007(&args).await;
    }
    if experiment == "008" {
        return run_008(&args).await;
    }
    if experiment == "009" {
        return run_009(&args).await;
    }
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

/// Experiment 007 needs an index containing only one seed's vectors, so
/// each seed starts from an empty schema. That is destructive, so it
/// requires `--reset` and a dedicated `CS_BENCH_DATABASE_URL`.
async fn run_007(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|a| a == "--reset") {
        return Err(
            "007 drops and recreates the schema of CS_BENCH_DATABASE_URL for every seed;              pass --reset to confirm it points to a dedicated benchmark database"
                .into(),
        );
    }
    let url = std::env::var("CS_BENCH_DATABASE_URL")
        .map_err(|_| "set CS_BENCH_DATABASE_URL".to_owned())?;
    let dev = args.iter().any(|a| a == "--dev");
    let seeds: Vec<u64> = if dev {
        exp007::DEV_SEEDS.to_vec()
    } else {
        exp007::HELD_OUT_SEEDS.to_vec()
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/007-ann-under-filtering/results/{}-postgres",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: resetting schema");
        let pg = cs_postgres::PgStore::connect(&url, 32)
            .await
            .map_err(|e| e.to_string())?;
        for sql in [
            "DROP SCHEMA public CASCADE",
            "CREATE SCHEMA public",
            "CREATE EXTENSION IF NOT EXISTS vector",
        ] {
            sqlx_exec(&pg, sql).await?;
        }
        pg.migrate().await.map_err(|e| e.to_string())?;
        rows.extend(exp007::run_seed(Arc::new(pg), *seed).await?);
    }
    let summary = exp007::summarize(&rows);
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
        "experiment": "007-ann-under-filtering",
        "seeds": seeds,
        "held_out": !dev,
        "store": "postgres",
        "embedder": "cs/hashing@1 (384d)",
        "vectors_per_seed": exp007::total(),
        "k": exp007::K,
        "ef_search": exp007::EF_SEARCH,
        "iterative_scan": "strict_order",
        "pgvector": sql_scalar(&url, "SELECT extversion FROM pg_extension WHERE extname = 'vector'").await,
        "postgres": sql_scalar(&url, "SHOW server_version").await,
        "commit": command_output("git", &["rev-parse", "HEAD"]),
        "rustc": command_output("rustc", &["--version"]),
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
        "{:<7} {:>4} {:>8} {:>9} {:>6} {:>6} {:>9} {:>9} {:>8}  verdict",
        "stratum", "n", "recall", "CI low", "under", "index", "exact p95", "hnsw p95", "speedup"
    );
    for (s, a) in &summary.strata {
        println!(
            "{:<7} {:>4} {:>8.4} {:>9.4} {:>6} {:>6} {:>9.2} {:>9.2} {:>7.2}x  {}",
            format!("{s:?}"),
            a.queries,
            a.recall,
            a.recall_ci_lower,
            a.underfill,
            a.index_used,
            a.exact_p95_ms,
            a.hnsw_p95_ms,
            a.speedup_p95,
            if a.passes {
                "pass".to_owned()
            } else {
                format!("FAIL: {}", a.failed_criteria.join("; "))
            }
        );
    }
    println!(
        "overall recall {:.4} (CI low {:.4}); any stratum passes: {}",
        summary.overall_recall, summary.overall_recall_ci_lower, summary.any_stratum_passes
    );
    Ok(())
}

async fn sqlx_exec(pg: &cs_postgres::PgStore, sql: &str) -> Result<(), String> {
    use sqlx::Executor;
    pg.pool()
        .execute(sql)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn sql_scalar(url: &str, sql: &str) -> String {
    let Ok(pg) = cs_postgres::PgStore::connect(url, 1).await else {
        return "unknown".into();
    };
    sqlx::query_scalar::<_, String>(sql)
        .fetch_one(pg.pool())
        .await
        .unwrap_or_else(|_| "unknown".into())
}

async fn run_008(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let seeds: Vec<u64> = if dev {
        exp008::DEV_SEEDS.to_vec()
    } else {
        exp008::HELD_OUT_SEEDS.to_vec()
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/008-gap-validation/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: generating, loading, validating");
        rows.extend(exp008::run_seed(store(&kind).await?, *seed).await?);
    }
    let summary = exp008::summarize(&rows);
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
        "experiment": "008-gap-validation",
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "detector": cs_memory::need::DETECTOR_VERSION,
        "signature_version": cs_memory::need::SIGNATURE_VERSION,
        "per_stratum": exp008::PER_STRATUM,
        "commit": command_output("git", &["rev-parse", "HEAD"]),
        "rustc": command_output("rustc", &["--version"]),
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
        "{:<20} {:>4} {:>8} {:>8} {:>6}  statuses",
        "stratum", "n", "error", "upper", "incmpl"
    );
    for (s, a) in &summary.strata {
        println!(
            "{:<20} {:>4} {:>8.4} {:>8.4} {:>6}  {:?}",
            format!("{s:?}"),
            a.questions,
            a.error_rate,
            a.error_upper_95,
            a.incomplete,
            a.statuses
        );
    }
    for (title, gates) in [
        ("automatic validation", &summary.auto_validate_gates),
        ("gap proposals", &summary.propose_only_gates),
    ] {
        println!("-- {title}");
        for g in gates {
            println!(
                "[{}] {} — {}",
                if g.passed { "pass" } else { "FAIL" },
                g.criterion,
                g.value
            );
        }
    }
    println!(
        "automatic validation permitted: {}; gap proposals permitted: {}",
        summary.automatic_validation_permitted, summary.gap_proposals_permitted
    );
    Ok(())
}

async fn run_009(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let seeds: Vec<u64> = if dev {
        exp009::DEV_SEEDS.to_vec()
    } else {
        exp009::HELD_OUT_SEEDS.to_vec()
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/009-adaptive-budgets/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: generating and loading");
        rows.extend(exp009::run_seed(store(&kind).await?, *seed).await?);
    }
    let summary = exp009::summarize(&rows);
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
        "experiment": "009-adaptive-budgets",
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "crowded_services": cs_bench::crowded::CROWDED_SERVICES,
        "notes_per_service": cs_bench::crowded::NOTES_PER_SERVICE,
        "commit": command_output("git", &["rev-parse", "HEAD"]),
        "rustc": command_output("rustc", &["--version"]),
        "bootstrap": { "resamples": exp009::BOOTSTRAP_RESAMPLES, "seed": exp009::BOOTSTRAP_SEED },
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
        "{:<6} {:>7} {:>7} {:>9} {:>7} {:>6} {:>8} {:>8} {:>8}",
        "", "nDCG", "MRR", "cand.R", "cands", "trips", "p50 ms", "p95 ms", "p99 ms"
    );
    for (n, a) in &summary.overall {
        println!(
            "{n:<6} {:>7.4} {:>7.4} {:>9.4} {:>7.1} {:>6.3} {:>8.2} {:>8.2} {:>8.2}",
            a.ndcg,
            a.mrr,
            a.candidate_recall,
            a.candidates,
            a.round_trips,
            a.latency_p50_ms,
            a.latency_p95_ms,
            a.latency_p99_ms
        );
    }
    for (f, m) in &summary.by_family {
        let a = &m[exp009::PRIMARY];
        println!(
            "{f:<13} expansion {:.3}  nDCG A {:.4} F200 {:.4} F50 {:.4}",
            a.expansion_rate.unwrap_or(0.0),
            a.ndcg,
            m[exp009::CONTROL].ndcg,
            m[exp009::ABLATION].ndcg
        );
    }
    println!("{:?}", summary.expansions);
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt A: {}", summary.adopt);
    Ok(())
}
