# 005 — Candidate generation: channels vs. recency (v1)

- Status: **preregistered** (committed before any run on held-out seeds)
- Date: 2026-11-14

## Question

v1 recall scores only the newest `max_candidates` (200) revisions, so an older relevant memory is never scored. Do lexical and dense **candidate channels** recover relevant evidence well enough to justify adoption, at the **same candidate budget**, without breaking temporal or authorization guarantees?

The synthetic benchmark validates controlled retrieval behaviour and implementation correctness. **It cannot establish real-world semantic superiority**, and results are not reported as such.

## Strategies (fixed)

All strategies use `max_candidates = 200`, `max_results = 10`, the v1 scoring function and weights (`dense 0.55`, `lexical 0.30`, `recency 0.15`, 30-day half-life), `min_score = 0`, no token budget, and identical stores and data.

| Id | Policy | Role |
|---|---|---|
| `v1` | `recency_v1` | Frozen control |
| `L` | `channels { lexical: 100, dense: 0 }` | Ablation |
| `D` | `channels { lexical: 0, dense: 100 }` | Ablation |
| `LD` | `channels { lexical: 50, dense: 50 }` | **Primary hypothesis** |

The remaining budget is filled with the newest revisions. Quotas were fixed before any run and are not tuned.

## Data (generated deterministically)

The generator is `crates/cs-bench`, seeded with SplitMix64. It makes no network calls and uses no external datasets.

### Corpus

There are 2,000 memories per seed, written in chronological order over 365 days of transaction time, mostly in one user's private scope (Alice). They consist of:

- **Facts** (semantic memories with structured triples, about 60% of the corpus). Each is about a generated service (150 per seed), using these relations:
  - stores data in a datastore;
  - is owned by a team;
  - runs in a region;
  - depends on another service;
  - exposes a port.
- **Background noise** (about 40%): episodic notes with shared vocabulary.
- **Lexical neighbours**: service names that share tokens (for example `orion ledger` and `orion ledger replica`).
- **Temporal changes**: a service's datastore changes. The old fact's valid time ends where the new one begins.
- **Out-of-scope copies**: the same facts stored in another user's (Bob's) private scope.

### Query families

There are 50 queries per family per seed, so 250 per seed.

| Family | Query | Gold (grade) |
|---|---|---|
| `direct` | Asks a relation of a service in the fact's own words | The current fact (2) |
| `lexical_trap` | Names a service whose name is contained in a longer service name | The exact service's fact (2); the longer-named service's fact is 0 |
| `semantic` | Asks in different words ("who maintains", "where is it hosted") | The current fact (2) |
| `temporal` | Asks the current datastore of a service whose datastore changed | The current fact (2); the superseded fact is a **temporal violation** if selected |
| `scope` | Asks about a fact that exists in Alice's scope and is copied in Bob's | Alice's fact (2); Bob's copy is a **scope violation** if it is ever a candidate |

**Age stratum.** In every family, half of the query targets are drawn from facts with fewer than 200 newer visible revisions (`recent`) and half from older facts (`old`), so recency is not made to look deliberately foolish.

### Seeds

| Use | Seeds |
|---|---|
| Development (debugging only; never reported as results) | 1, 2, 3 |
| **Held-out** (the only reported results) | 101, 102, 103, 104, 105 |

That gives 1,250 held-out queries.

### Embedder and store

- **Embedder:** the deterministic hashing embedder (`cs/hashing@1`, 384 dimensions). Hashing embeddings are weak semantically, and the `D` and `LD` results only speak for this embedder. Other embedders, if run later, are reported separately and never aggregated with these results.
- **Store:** PostgreSQL with pgvector (the production store), and exact kNN only (no ANN index). The in-memory store is used for development.

## Metrics (per query, then the mean over queries)

- **nDCG@10** with graded gains `2^g − 1`. The ideal ranking is built from the gold grades.
- **MRR@10**: the reciprocal rank of the first result with grade ≥ 1.
- **Recall@10**: the share of grade ≥ 1 gold items among the selected results.
- **Candidate recall**: the share of grade ≥ 1 gold items in the candidate set.
- **Tokens**: the estimated tokens of the selected results.
- **Latency**: wall-clock time of the recall call, reported as p50 and p95.
- **Violations**: scope (an out-of-scope record is a candidate) and temporal (a superseded fact is selected).

Deltas against `v1` are paired per query. 95% confidence intervals use the paired bootstrap: 10,000 resamples of queries, seed 7.

## Adoption criteria for `LD` (all must hold on held-out seeds)

1. Mean nDCG@10 delta ≥ **+0.03** absolute **and** ≥ **5%** relative, **and** the lower bound of its 95% CI is > 0.
2. Mean candidate recall delta ≥ **+5 percentage points**.
3. No family's mean nDCG@10 regresses by more than **0.02** absolute.
4. MRR: the 95% CI of the delta is **not entirely below 0**.
5. **Zero** scope violations and **zero** temporal violations, for every strategy.
6. Mean tokens ≤ **1.05×** `v1`.
7. p95 latency ≤ **1.25×** `v1`.

If every criterion holds, `LD` may become the default for **new** recall policies, and `v1` stays selectable as the frozen control. If any criterion fails, `v1` remains the default and the result is reported as a failure. Criteria are not changed after results exist.

`L` and `D` are exploratory ablations. They answer whether dense retrieval contributes anything beyond lexical channels, and do not on their own trigger adoption.

## Reporting

`results/` contains:

- the raw per-query output;
- the aggregate JSON;
- a manifest with the commit, toolchain, store and embedder versions, and seeds.

Results are reported overall, by family and by age stratum. Results that miss criteria are reported, not reframed.
