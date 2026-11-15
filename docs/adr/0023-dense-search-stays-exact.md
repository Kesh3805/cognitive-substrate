# ADR-0023: Dense candidate search stays exact under per-scope filtering

- Status: Accepted
- Date: 2026-11-15

## Context

The roadmap called for "ANN preselection benchmarked against exact scoring". In this system, every recall is authorized to one tenant's scope and filtered bitemporally, so the eligible vectors are usually a small fraction of an embedding space. A global HNSW index over the space has to walk past every vector the caller cannot use. Exact search over the eligible rows, on the other hand, scales with the scope.

## Decision

**The dense channel uses exact cosine kNN over the eligible revisions. No ANN path is built.**

Experiment 007 ([results](../../benchmarks/007-ann-under-filtering/RESULTS.md)) compared exact search with a per-space HNSW index, using iterative scans, on the same eligibility filters. It ran on three held-out seeds of 100,000 vectors with skewed tenant sizes, and no selectivity stratum passed its gates:

| Stratum | HNSW recall@50 | p95 exact → HNSW |
|---|---|---|
| broad, ≥ 10% of the index | 0.98 | 200 → 349 ms |
| medium | 0.97 | 45 → 376 ms |
| narrow | 0.55 (87 of 120 underfilled) | 5 → 509 ms |
| tiny | 0.19 (all underfilled) | 1.5 → 489 ms |

Postgres's planner, left alone, also chose exact search for every query.

The experimental search code (`cs_postgres::ann`) stays, clearly marked as experimental and unused by the engine, so that the measurement can be repeated.

## Consequences

- Dense retrieval stays exact, deterministic and replayable without candidate pinning. No index maintenance cost is paid.
- The dense channel is not part of the default recall policy anyway (experiment 005). This decision costs nothing today.
- **When to revisit.** If single scopes grow far beyond the measured 40,000 vectors, or a learned embedder makes the dense channel useful, ANN should be measured again under a new preregistration. That experiment should test the conditions 007 deliberately fixed:
  - other HNSW build and search settings;
  - `relaxed_order`;
  - tenant columns denormalized onto the embeddings table;
  - per-tenant indexes for the largest tenants.

  Any ANN path that is eventually built must include exact fallback on underfill and candidate pinning for replay, as the 007 decision rule requires.
