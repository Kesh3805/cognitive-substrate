# 005 — Results (held-out seeds, PostgreSQL)

- Run: held-out seeds 101–105; store: PostgreSQL + pgvector (exact kNN); embedder: `cs/hashing@1` (384 d).
- Commit at run time: `26c739e` (see [`results/held-out-postgres/manifest.json`](results/held-out-postgres/manifest.json)).
- Raw per-query output: [`results/held-out-postgres/raw.jsonl`](results/held-out-postgres/raw.jsonl). Aggregates and gates: [`summary.json`](results/held-out-postgres/summary.json).

## Verdict

**`LD` is not adopted. `recency_v1` remains the default.** Six of the seven preregistered criteria, plus the amendment-1 gate, pass, but **the p95 latency criterion fails**: 1.70× `v1`, against a limit of 1.25×. Under the preregistered rule a single failure means no adoption. The channels strategy stays available as an opt-in policy.

## Gates (`LD` vs `v1`)

| Criterion | Result | Pass |
|---|---|---|
| nDCG@10 Δ ≥ +0.03, ≥ 5% relative, 95% CI lower bound > 0 | +0.3524 (+78.9%), CI [+0.3216, +0.3839] | ✅ |
| Candidate recall Δ ≥ +5 pp | +51.85 pp | ✅ |
| No family regresses nDCG@10 by > 0.02 | worst: semantic, +0.1597 | ✅ |
| Recent-stratum nDCG@10 does not regress by > 0.02 (amendment 1) | +0.0006 | ✅ |
| MRR Δ 95% CI not entirely below 0 | CI [+0.2683, +0.3324] | ✅ |
| Zero scope and temporal violations (all strategies) | 0 | ✅ |
| Mean tokens ≤ 1.05× `v1` | 1.023× | ✅ |
| p95 latency ≤ 1.25× `v1` | **1.70×** (61.51 ms vs 36.23 ms) | ❌ |

## Overall

| Strategy | nDCG@10 | MRR@10 | Recall@10 | Candidate recall | Tokens | p50 ms | p95 ms |
|---|---|---|---|---|---|---|---|
| `v1` | 0.4467 | 0.4469 | 0.4815 | 0.4815 | 197.6 | 24.83 | 36.23 |
| `L` | 0.7982 | 0.7453 | 0.9822 | 1.0000 | 202.3 | 35.29 | 50.41 |
| `D` | 0.7992 | 0.7470 | 0.9808 | 1.0000 | 202.3 | 38.15 | 54.89 |
| `LD` | 0.7991 | 0.7469 | 0.9808 | 1.0000 | 202.1 | 43.99 | 61.51 |

## By age stratum (nDCG@10, candidate recall)

| Stratum | `v1` | `L` | `D` | `LD` |
|---|---|---|---|---|
| recent | 0.8917 / 0.962 | 0.8928 / 1.0 | 0.8934 / 1.0 | 0.8923 / 1.0 |
| old | 0.0017 / 0.001 | 0.7037 / 1.0 | 0.7049 / 1.0 | 0.7058 / 1.0 |

## By family (nDCG@10)

| Family | Queries | `v1` | `L` | `D` | `LD` |
|---|---|---|---|---|---|
| direct | 250 | 0.4516 | 0.8945 | 0.9018 | 0.8966 |
| lexical_trap | 102 | 0.4891 | 0.9493 | 0.9481 | 0.9530 |
| semantic | 250 | 0.4032 | 0.5646 | 0.5648 | 0.5629 |
| temporal | 50 | 0.5000 | 1.0000 | 1.0000 | 1.0000 |
| scope | 50 | 0.5000 | 0.9752 | 0.9531 | 0.9779 |

## What this does and does not show

- **The gain is the blind spot, nothing more.** On recent targets all strategies tie, within 0.002. Every gain comes from old targets, which `v1` cannot score at all because they are outside its 200-newest window. The benchmark confirms the blind spot is real and that channels close it at equal budget. It does not show better *ranking* where `v1` can see the evidence.
- **Dense retrieval adds nothing here.** With the hashing embedder, `L`, `D` and `LD` are within 0.001 nDCG of each other, and the paired CIs overlap completely. This says nothing about learned embedders, which have not been run and would be reported separately.
- **The semantic family stays weak** (0.56) for every strategy. Paraphrased questions are not answered well by lexical overlap or by hashing embeddings.
- **The violation gate holds structurally.** Superseded facts are removed by the valid-time filter before scoring, so no strategy could select them. Scope is enforced inside every channel's query, and no out-of-scope record was ever a candidate.
- **Fewer queries than planned.** There are 702 held-out queries instead of the planned 1,250. Balancing the age strata (amendment 1) leaves 25 queries per stratum in the scope and temporal families, so their per-family numbers have wide uncertainty.
- **Why latency fails.** `channels` issues up to three store queries per recall (lexical, dense, recency fill) instead of one. `L` alone is 1.39× and would also fail the gate.

## Follow-up (needs its own preregistration)

A follow-up experiment should test whether a cheaper channel implementation can meet the latency budget while keeping this relevance gain. Candidates:

- one round trip;
- the lexical channel only;
- index-only term matching.

The relevance gain measured here is not reused as evidence for that experiment.
