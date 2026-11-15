# 007 — HNSW vs exact nearest-neighbour search under per-scope filtering

- Status: **preregistered** (committed before any run on held-out seeds 301–303)
- Date: 2026-11-15

## Question

Every recall is authorized to one tenant's scope and filtered bitemporally. In this setting, is a global per-space HNSW index, queried with iterative scans, faster than exact nearest-neighbour search over the eligible vectors **without losing recall**? And for which scope selectivities does that hold?

Exact filtered kNN is the reference. It defines the eligible population and its ordering. HNSW is only an approximation of it and has to earn its place.

Context: in 005 the dense channel added nothing measurable with the hashing embedder, and it is not part of the default recall policy. **This experiment therefore does not change recall.** It decides whether an ANN path is worth building at all, and for which scopes.

## Setup (fixed)

### Store

- PostgreSQL 17 with pgvector 0.8.x, in a dedicated benchmark database.
- The schema is dropped and recreated for every seed, so the index only ever contains one seed's vectors.

### Corpus

There are 100,000 semantic memories per seed, written through `CognitiveEngine::create_memory` with inline `cs/hashing@1` embeddings (384 d). They belong to 115 tenants of skewed size, each held in one user's private scope:

| Tenant size | Tenants | Share of index | Stratum |
|---|---|---|---|
| 40,000; 15,000; 10,000 | 3 | 10–40% | **broad** (≥ 10%) |
| 5,000 ×2; 2,000 ×5; 1,000 ×5 | 12 | 1–5% | **medium** (1–10%) |
| 300 ×20; 100 ×30 | 50 | 0.1–0.3% | **narrow** (0.1–1%) |
| 20 ×50 | 50 | 0.02% | **tiny** (< 0.1%) |

- Texts are six random words from a 40-word vocabulary plus a unique suffix.
- Queries are four random words, each authorized to one tenant chosen at random within its stratum.
- There are 40 queries per stratum per seed.

### Index

- One partial expression index: `hnsw ((embedding::vector(384)) vector_cosine_ops) WHERE model_name = … AND model_version = …`.
- It is created explicitly after loading, with `maintenance_work_mem = 1GB`, a serial build (`max_parallel_maintenance_workers = 0`, because the database container's shared memory is 64 MB) and the default build settings (`m = 16`, `ef_construction = 64`), and followed by `ANALYZE`.

### Search

| | Exact (E) | HNSW (H) |
|---|---|---|
| Filters | the store's own eligibility filters (`memory_filters`) | the same filters |
| `k` | 50 | 50 |
| Distance expression | cannot use the index | uses the index |
| Settings | — | `SET LOCAL hnsw.ef_search = 100`, `hnsw.iterative_scan = strict_order` and `enable_sort = off` (other settings at their defaults, including `max_scan_tuples` and `scan_mem_multiplier`) |
| Ordering | `(distance, revision_id)` inside the scan, so the reference set is deterministic | `distance` inside the scan (a tie-breaker would stop Postgres using the index), then the `k` results re-sorted by `(distance, revision_id)` |

- **Index usage is verified per query** with `EXPLAIN` on the same statement.
- **Timing** covers the search statement only, measured client-side. E and H alternate which runs first.

### Seeds

| Use | Seeds |
|---|---|
| Development (debugging only) | 1 |
| **Held-out** | 301, 302, 303 |

That gives 480 held-out queries, 120 per stratum.

### Disclosure: what the development seed showed

The development seed (1) was used only to debug the pipeline. It led to three changes, all made before this preregistration was committed:

1. **Postgres never chose HNSW on its own.** In all 160 development queries, at every selectivity including the 40% tenant, the planner preferred exact search through the tenant index (B-tree scan, nested loop into embeddings, sort). `EXPLAIN` showed the index is usable when queried alone. H therefore sets `enable_sort = off` so that the ordered HNSW scan is what runs. The planner's own preference for exact search is itself reported as a finding.
2. **The exact reference now breaks ties inside its scan.** Without it, the reference set was not deterministic at the `LIMIT` boundary.
3. **Recall is tie-aware.**

The HNSW index build also failed for lack of shared memory in the database container, so it runs serially (see Index).

## Metrics, per query

- **Recall@k, tie-aware**: the share of E's size covered by H results whose distance is ≤ E's k-th distance (+1e-9), capped at |E|. Both sets are computed after identical authorization, scope, bitemporal, status, space and `embedded_at` filters. Treating equal distances as equivalent keeps ties, which are common with hashing embeddings, from being scored as approximation errors.
- **Underfill**: H returned fewer than min(k, eligible) rows.
- **Index used**: from `EXPLAIN`.
- **Latency**: of E and of H.
- **Authorization violations**: any returned revision outside the querying tenant's rows.

## Gates (evaluated per stratum; all must hold for a stratum to pass)

1. Mean recall@k ≥ **0.98**, with the 95% bootstrap CI lower bound ≥ **0.97**. In the **tiny** stratum the thresholds are ≥ **0.95** and ≥ **0.93**.
2. **Zero underfilled queries.** An underfill caused by the scan budget is still a failure: it ships degraded recall.
3. **HNSW actually ran:** the index was used in **every** H query of the stratum. If the planner silently fell back to exact search, the stratum did not test HNSW.
4. **p95(E) / p95(H) ≥ 2.0** within the stratum.
5. **Zero authorization violations.**

## Decision rule

- **If no stratum passes:** exact kNN remains the only dense path, and no ANN path is built.
- **If one or more strata pass:** a follow-up may preregister a routing policy (ANN only for the passing selectivities, exact otherwise) and build the product path. That path must include:
  - transaction-local settings;
  - exact fallback on underfill;
  - candidate pinning in traces, so that replay re-scores the recorded candidates (re-checking authorization and purges) instead of re-running ANN.

  It must also measure historical (as-of) queries, where vectors embedded later remain in the graph.

## Reporting

Results are reported per stratum, with recall CIs, underfill and index-use counts, p50 and p95 for E and H, and the speedup. Raw per-query rows and a manifest (commit, Postgres and pgvector versions, settings) are committed. Results that miss criteria are reported, not reframed.
