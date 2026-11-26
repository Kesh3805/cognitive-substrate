//! `cs-bench`: run a preregistered experiment.
//!
//! ```text
//! cs-bench 005|006|008|009|010|011|012 [--store memory|postgres] [--dev] [--out DIR]
//! cs-bench 007 --reset [--dev] [--out DIR]
//! ```
//!
//! `--dev` runs the development seeds (never reported as results).
//! Postgres reads `CS_BENCH_DATABASE_URL`, which must point to a dedicated
//! database: runs leave their tenants, traces and queued background jobs
//! behind, which would disturb tests sharing the database.

use std::{io::Write, path::PathBuf, process::Command, sync::Arc};

use cs_bench::{exp005, exp006, exp007, exp008, exp009, exp010, exp011, exp012, exp013, exp013b};
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
    if experiment == "010" {
        return run_010(&args).await;
    }
    if experiment == "011" {
        return run_011(&args).await;
    }
    if experiment == "013" {
        return run_013(&args).await;
    }
    if experiment == "013b" {
        return run_013b(&args).await;
    }
    if experiment == "012" {
        return run_012(&args).await;
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
    for (k, l) in &summary.latency_by_expansion {
        println!(
            "{k:<16} n {:>4}  p50 {:>7.2}  p95 {:>7.2}  p99 {:>7.2}",
            l.queries, l.p50_ms, l.p95_ms, l.p99_ms
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
    println!("adopt A: {}", summary.adopt);
    Ok(())
}

async fn run_010(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let (seeds, mix): (Vec<u64>, exp010::Mix) = if dev {
        (exp010::DEV_SEEDS.to_vec(), exp010::DEV_MIX)
    } else {
        (exp010::HELD_OUT_SEEDS.to_vec(), exp010::HELD_OUT_MIX)
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/010-selective-answering/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: generating, loading, deciding");
        rows.extend(exp010::run_seed(store(&kind).await?, *seed, mix).await?);
    }
    let summary = exp010::summarize(&rows);
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
        "experiment": "010-selective-answering",
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "mix": mix,
        "metacognition_version": cs_memory::metacognition::METACOGNITION_VERSION,
        "margin_only_threshold": exp010::MARGIN_ONLY_THRESHOLD,
        "commit": command_output("git", &["rev-parse", "HEAD"]),
        "rustc": command_output("rustc", &["--version"]),
        "bootstrap": { "resamples": exp010::BOOTSTRAP_RESAMPLES, "seed": exp010::BOOTSTRAP_SEED },
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
        "{:<14} {:>6} {:>9} {:>7} {:>9}",
        "", "n", "coverage", "risk", "present"
    );
    for (n, s) in &summary.overall {
        println!(
            "{n:<14} {:>6} {:>9.4} {:>7.4} {:>9.4}",
            s.queries, s.coverage, s.risk, s.present_coverage
        );
    }
    for (stratum, d) in &summary.decisions {
        println!(
            "{stratum:<15} {d:?}  reasons {:?}",
            summary.reasons[stratum]
        );
    }
    for (f, m) in &summary.by_family {
        let p = &m["policy"];
        println!(
            "family {f:<13} answered {:.4} risk {:.4}",
            p.present_coverage, p.risk
        );
    }
    println!(
        "matched margin threshold {:.4}; wrong-answer delta {:?}",
        summary.matched_margin_threshold, summary.wrong_answer_delta
    );
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt: {}", summary.adopt);
    Ok(())
}

async fn run_011(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let seeds: Vec<u64> = if dev {
        exp011::DEV_SEEDS.to_vec()
    } else {
        exp011::HELD_OUT_SEEDS.to_vec()
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/011-causal-evidence/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: generating, intervening, evaluating");
        rows.extend(exp011::run_seed(store(&kind).await?, *seed).await?);
    }
    let summary = exp011::summarize(&rows);
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
        "experiment": "011-causal-evidence",
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "evaluation_version": cs_memory::causal::CAUSAL_EVALUATION_VERSION,
        "per_type": exp011::PER_TYPE,
        "contexts": exp011::CONTEXTS,
        "min_effect": exp011::MIN_EFFECT,
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
    for (k, m) in &summary.statuses {
        println!("{k:<18} {m:?}   naive {:?}", summary.naive_statuses[k]);
    }
    println!("harmful-in-one supported: {:?}", summary.harmed_supported);
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt: {}", summary.adopt);
    Ok(())
}

async fn run_012(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let seeds: Vec<u64> = if dev {
        exp012::DEV_SEEDS.to_vec()
    } else {
        exp012::HELD_OUT_SEEDS.to_vec()
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/012-counterfactual-bounds/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut runs = Vec::new();
    for seed in &seeds {
        eprintln!("seed {seed}: generating, evaluating, asking");
        runs.push(exp012::run_seed(store(&kind).await?, *seed).await?);
    }
    let summary = exp012::summarize(&runs);
    let mut raw = std::fs::File::create(out.join("raw.jsonl")).map_err(|e| e.to_string())?;
    for r in &runs {
        for a in &r.answers {
            writeln!(
                raw,
                "{}",
                serde_json::to_string(a).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
    }
    let mut probes = std::fs::File::create(out.join("probes.jsonl")).map_err(|e| e.to_string())?;
    for r in &runs {
        for p in &r.probes {
            writeln!(
                probes,
                "{}",
                serde_json::to_string(p).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
    }
    let manifest = json!({
        "experiment": "012-counterfactual-bounds",
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "counterfactual_version": cs_policy::counterfactual::COUNTERFACTUAL_VERSION,
        "evaluation_version": cs_memory::causal::CAUSAL_EVALUATION_VERSION,
        "per_family": exp012::PER_FAMILY,
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
    println!("supported by family: {:?}", summary.supported);
    for (f, m) in &summary.coverage_by_family {
        println!("{f:<10} {m:?}");
    }
    println!(
        "in-scope refusals (insufficient evidence): {}",
        summary.insufficient
    );
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt: {}", summary.adopt);
    Ok(())
}

async fn run_013(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let all: Vec<u64> = if dev {
        exp013::DEV_SEEDS.to_vec()
    } else {
        exp013::HELD_OUT_SEEDS.to_vec()
    };
    // `--seed S` runs one of them (seeds are independent, so they can run as
    // separate processes and be merged with `--merge DIR...`).
    let seeds: Vec<u64> = match arg(args, "--seed") {
        Some(s) => {
            let s: u64 = s.parse().map_err(|_| "--seed takes a number".to_owned())?;
            if !all.contains(&s) {
                return Err(format!("seed {s} is not in this split"));
            }
            vec![s]
        }
        None => all,
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/013-prediction-gating/results/{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    if let Some(i) = args.iter().position(|a| a == "--merge") {
        // Summarize raw rows already written by per-seed runs.
        for dir in &args[i + 1..] {
            let text = std::fs::read_to_string(PathBuf::from(dir).join("raw.jsonl"))
                .map_err(|e| format!("{dir}: {e}"))?;
            for line in text.lines() {
                rows.push(serde_json::from_str(line).map_err(|e| e.to_string())?);
            }
        }
    } else {
        for seed in &seeds {
            eprintln!("seed {seed}: burn-in, evaluation, opportunities");
            rows.extend(exp013::run_seed(store(&kind).await?, *seed).await?);
        }
    }
    let summary = exp013::summarize(&rows);
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
        "experiment": "013-prediction-gating",
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "prediction_version": cs_policy::prediction::PREDICTION_VERSION,
        "evaluation_version": cs_memory::causal::CAUSAL_EVALUATION_VERSION,
        "per_family": exp013::PER_FAMILY,
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
    for f in &summary.families {
        println!(
            "{:<10} supported {}/{} false-demotion runs {} detected {}/{} median {:?} p90 {:?} requalified {}",
            format!("{:?}", f.family),
            f.supported,
            f.runs,
            f.runs_with_false_demotion,
            f.detected,
            f.shifted_contexts,
            f.median_delay,
            f.p90_delay,
            f.requalified
        );
        for (a, u) in &f.utility {
            let d = f
                .gate_minus
                .get(a)
                .map(|i| {
                    format!(
                        "gate-this {:+.3} [{:+.3}, {:+.3}]",
                        i.mean, i.lower, i.upper
                    )
                })
                .unwrap_or_default();
            println!("    {:<20} utility {u:+.3}  {d}", format!("{a:?}"));
        }
    }
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt: {}", summary.adopt);
    Ok(())
}

async fn run_013b(args: &[String]) -> Result<(), String> {
    let kind = arg(args, "--store").unwrap_or_else(|| "memory".into());
    let dev = args.iter().any(|a| a == "--dev");
    let all: Vec<u64> = if dev {
        exp013b::DEV_SEEDS.to_vec()
    } else {
        exp013b::HELD_OUT_SEEDS.to_vec()
    };
    let seeds: Vec<u64> = match arg(args, "--seed") {
        Some(s) => {
            let s: u64 = s.parse().map_err(|_| "--seed takes a number".to_owned())?;
            if !all.contains(&s) {
                return Err(format!("seed {s} is not in this split"));
            }
            vec![s]
        }
        None => all,
    };
    // `--holdout H` overrides the holdout rate, on development seeds only.
    let holdout = match arg(args, "--holdout") {
        Some(h) if dev => h
            .parse::<f64>()
            .ok()
            .filter(|h| (0.0..1.0).contains(h))
            .ok_or_else(|| "--holdout takes a rate in [0, 1)".to_owned())?,
        Some(_) => return Err("--holdout is for development seeds only".into()),
        None => exp013b::HOLDOUT,
    };
    let out = PathBuf::from(arg(args, "--out").unwrap_or_else(|| {
        format!(
            "benchmarks/013-prediction-gating/results/b-{}-{kind}",
            if dev { "dev" } else { "held-out" }
        )
    }));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    if let Some(i) = args.iter().position(|a| a == "--merge") {
        for dir in &args[i + 1..] {
            let text = std::fs::read_to_string(PathBuf::from(dir).join("raw.jsonl"))
                .map_err(|e| format!("{dir}: {e}"))?;
            for line in text.lines() {
                rows.push(serde_json::from_str(line).map_err(|e| e.to_string())?);
            }
        }
    } else {
        for seed in &seeds {
            eprintln!("seed {seed}: burn-in, evaluation, opportunities");
            rows.extend(exp013b::run_seed_with(store(&kind).await?, *seed, holdout).await?);
        }
    }
    let summary = exp013b::summarize(&rows);
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
        "experiment": "013b-effect-gating",
        "holdout": holdout,
        "seeds": seeds,
        "held_out": !dev,
        "store": kind,
        "prediction_version": cs_policy::prediction::PREDICTION_VERSION,
        "evaluation_version": cs_memory::causal::CAUSAL_EVALUATION_VERSION,
        "per_family": exp013b::PER_FAMILY,
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
    for f in &summary.families {
        println!(
            "{:<15} supported {}/{} exposed shifted {} detected {}",
            format!("{:?}", f.family),
            f.supported,
            f.runs,
            f.exposed_shifted,
            f.detected
        );
        for (a, u) in &f.utility {
            let d = f
                .effect_gate_minus
                .get(a)
                .map(|i| {
                    format!(
                        "effect-this {:+.3} [{:+.3}, {:+.3}]",
                        i.mean, i.lower, i.upper
                    )
                })
                .unwrap_or_default();
            println!("    {:<16} utility {u:+.3}  {d}", format!("{a:?}"));
        }
    }
    for g in &summary.gates {
        println!(
            "[{}] {} — {}",
            if g.passed { "pass" } else { "FAIL" },
            g.criterion,
            g.value
        );
    }
    println!("adopt: {}", summary.adopt);
    Ok(())
}
