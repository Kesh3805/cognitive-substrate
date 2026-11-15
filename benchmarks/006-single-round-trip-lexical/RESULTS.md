# 006 — Results (held-out seeds, PostgreSQL)

- Run: held-out seeds 201–205; store: PostgreSQL + pgvector, in a dedicated benchmark database; embedder: `cs/hashing@1` (384 d); strategy order rotated per query.
- Commit: the run started on `d3fb65d`, the preregistration commit. The manifest records `aad00e9` because the commit is read when the run finishes. `aad00e9` differs from `d3fb65d` only by a documentation note added to 005's results during the run. No code changed.
- Raw per-query output: [`results/held-out-postgres/raw.jsonl`](results/held-out-postgres/raw.jsonl). Aggregates and gates: [`summary.json`](results/held-out-postgres/summary.json). Manifest: [`manifest.json`](results/held-out-postgres/manifest.json).

## Verdict

**All eight preregistered criteria pass. `channels { lexical: 100, dense: 0 }` is adopted as the default candidate generation for new recall policies.** `recency_v1` stays selectable as the frozen control.

## Gates (`L` vs `v1`)

| Criterion | Result | Pass |
|---|---|---|
| nDCG@10 Δ ≥ +0.03, ≥ 5% relative, 95% CI lower bound > 0 | +0.3517 (+76.8%), CI [+0.3207, +0.3823] | ✅ |
| Candidate recall Δ ≥ +5 pp | +51.27 pp | ✅ |
| No family regresses nDCG@10 by > 0.02 | worst: semantic, +0.1808 | ✅ |
| Recent-stratum nDCG@10 does not regress by > 0.02 | +0.0080 | ✅ |
| MRR Δ 95% CI not entirely below 0 | CI [+0.2677, +0.3307] | ✅ |
| Zero scope and temporal violations | 0 | ✅ |
| Mean tokens ≤ 1.05× `v1` | 1.017× | ✅ |
| p95 latency ≤ 1.25× `v1` | 1.08× (33.58 ms vs 31.18 ms) | ✅ |

## Overall

| Strategy | nDCG@10 | MRR@10 | Recall@10 | Candidate recall | Tokens | p50 ms | p95 ms |
|---|---|---|---|---|---|---|---|
| `v1` | 0.4580 | 0.4623 | 0.4873 | 0.4873 | 198.6 | 22.40 | 31.18 |
| `L` | 0.8097 | 0.7615 | 0.9845 | 1.0000 | 202.1 | 23.54 | 33.58 |

## By age stratum (nDCG@10 / candidate recall)

| Stratum | `v1` | `L` |
|---|---|---|
| recent | 0.9045 / 0.965 | 0.9125 / 1.0 |
| old | 0.0115 / 0.010 | 0.7069 / 1.0 |

## By family (nDCG@10)

| Family | Queries | `v1` | `L` |
|---|---|---|---|
| direct | 250 | 0.4731 | 0.8911 |
| lexical_trap | 62 | 0.4940 | 0.9345 |
| semantic | 250 | 0.4094 | 0.5902 |
| temporal | 94 | 0.5000 | 1.0000 |
| scope | 52 | 0.5000 | 0.9808 |

## What this does and does not show

- **This is the same finding as 005, made affordable.** The gain comes from old targets that `v1` cannot score. On recent targets the two strategies are within 0.01. What changed is the cost: one store round trip instead of two, never more than `max_candidates` rows, and a rotated strategy order. p95 latency fell to 1.08× `v1`.
- **These are new seeds.** 005's held-out results were not reused. The relevance figures replicate closely across independent seeds: Δ nDCG@10 was +0.3515 for 005's `L` and +0.3517 here.
- **Limits that still apply** (unchanged from 005):
  - the benchmark is synthetic and uses one embedder;
  - the semantic family remains weak, at 0.59;
  - temporal correctness is enforced structurally by the valid-time filter;
  - 708 held-out queries, with fewer in the lexical-trap and scope families because the age strata are balanced.
- **Latency comes from one benchmark machine** with noticeable run-to-run variance. The gate passed with margin (1.08× against 1.25×), but these latency figures do not say how the system performs under production load.
