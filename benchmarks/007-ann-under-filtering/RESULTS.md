# 007 — Results (held-out seeds, PostgreSQL)

- Run: held-out seeds 301–303, 100,000 vectors each; PostgreSQL 17.12 with pgvector 0.8.7; `k = 50`; `ef_search = 100`; `iterative_scan = strict_order`; `cs/hashing@1` (384 d).
- Commit: `d22f49b` ([`manifest.json`](results/held-out-postgres/manifest.json)). Raw per-query rows: [`raw.jsonl`](results/held-out-postgres/raw.jsonl). Aggregates: [`summary.json`](results/held-out-postgres/summary.json).

## Verdict

**No stratum passes. Exact filtered kNN remains the only dense path, and no ANN path is built.** HNSW was slower than exact search in every stratum. In narrow and tiny scopes it also lost most of its recall and underfilled.

## Per stratum (120 queries each)

| Stratum | Recall@50 (95% CI) | Min | Underfilled | HNSW used | Exact p50 / p95 ms | HNSW p50 / p95 ms | p95(E)/p95(H) | Pass |
|---|---|---|---|---|---|---|---|---|
| broad (≥ 10%) | 0.9840 (0.9777–0.9895) | 0.78 | 0 | 120 | 113.3 / 199.7 | 272.8 / 349.0 | 0.57× | ❌ speedup |
| medium (1–10%) | 0.9738 (0.9635–0.9820) | 0.56 | 0 | 120 | 17.4 / 44.6 | 270.1 / 375.8 | 0.12× | ❌ recall, speedup |
| narrow (0.1–1%) | 0.5490 (0.5100–0.5870) | 0.18 | 87 | 120 | 3.5 / 4.8 | 428.4 / 508.9 | 0.01× | ❌ recall, underfill, speedup |
| tiny (< 0.1%) | 0.1871 (0.1679–0.2067) | 0.00 | 120 | 120 | 1.2 / 1.5 | 395.5 / 489.0 | 0.003× | ❌ recall, underfill, speedup |

Overall recall: 0.6735 (CI lower 0.6414). Authorization violations: 0 for both methods.

## What this shows

- **Postgres's planner was right.** Left to itself, it chose exact search through the tenant index for every query at every selectivity (development seed, disclosed in the preregistration). When HNSW is forced, it is slower even for the 40% tenant: 349 ms against 200 ms at p95. Each candidate the graph yields must be joined to its revision and filtered, so the walk pays for every out-of-scope vector it visits.
- **Selective scopes break HNSW twice.** For a tenant holding 0.1% of the index, the graph walk visits mostly other tenants' vectors. At the default `max_scan_tuples` and scan-memory limits, the iterative scan stops early:
  - in the narrow stratum, 87 of 120 queries returned fewer than 50 rows, and recall was 0.55;
  - in the tiny stratum, every query underfilled, and recall was 0.19.

  Meanwhile, exact search over those scopes takes 1–5 ms.
- **Exact search scales with the caller's scope, not with the index.** p95 ranges from 1.5 ms (20 vectors) to 200 ms (40,000 vectors). This workload is per-tenant and per-user, and every query is authorized to one scope. So the size that matters is the scope's, and for most scopes it is small.

## Limits

- One synthetic corpus shape, one embedder (hashing, 384 d), one build configuration (`m = 16`, `ef_construction = 64`) and one search setting (`ef_search = 100`, `strict_order`, default scan limits). A parameter sweep, `relaxed_order`, or denormalizing tenant columns onto the embeddings table could change the numbers for broad scopes. None of those were preregistered here.
- One benchmark machine.
- The largest scope is 40,000 vectors. A deployment where single scopes reach millions of vectors is outside what was measured. There, exact search would eventually become too slow, and the question should be asked again with its own preregistration.
