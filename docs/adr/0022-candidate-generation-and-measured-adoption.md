# ADR-0022: Candidate generation, and adoption only by preregistered measurement

- Status: Accepted
- Date: 2026-11-14

## Context

v1 recall scores the newest `max_candidates` (200) eligible revisions. An older revision is never scored, however relevant it is. The obvious fix, hybrid candidate generation, is exactly the kind of change the predecessor project shipped on intuition: hybrid retrieval with reranking regressed on its own 40-query evaluation. This project's rule is that no subsystem earns inclusion without measured utility.

## Decision

### A strategy field, with v1 frozen as the control

`RecallPolicy.candidates` selects the candidate generation strategy:

- `recency_v1` is the default. It is **frozen**: it is never changed to make another strategy look better.
- `channels { lexical, dense }` adds a lexical channel and a dense channel, then fills the remaining budget by recency.

Every strategy fills **the same budget** of unique revisions, so strategies compare at equal cost. Each trace candidate records which channel proposed it. See [recall scoring](../specifications/recall-scoring.md).

### One lexical contract for every store

Lexical matching uses one tokenizer, `cs_memory::lexical` (tokenizer v1). It is the same one BM25 uses.

- Records store their distinct terms (`index_terms`), with a GIN index in Postgres.
- Both stores rank by the number of shared terms, then by recency.
- Postgres full-text search, with its own stemming and normalization, is deliberately not used, so the stores cannot disagree.
- Rows written before the column existed are backfilled at startup.

### Dense candidates are exact

Dense candidates come from exact cosine kNN over vectors that were available at the as-of time, so replay stays exact. ANN is a separate decision. It needs its own benchmark against exact kNN, and pinned candidate sets so that replay never re-runs an approximate search.

### Adoption requires a preregistered experiment

A strategy becomes a default only when an experiment says so:

- The experiment is committed under `benchmarks/` **before** any held-out run, fixing the data, seeds, metrics and adoption gates.
- Amendments are allowed only before held-out runs, must be dated, and may only make adoption harder.
- Results that miss a gate are reported as failures.

## Outcome of experiment 005

Channels close v1's blind spot:

- **Relevance:** nDCG@10 is +0.35 (CI +0.32 to +0.38) and candidate recall is +52 points.
- **No cost elsewhere:** recent targets are unchanged, and there were zero scope or temporal violations.
- **Latency:** p95 is **1.70× v1** on PostgreSQL, against a 1.25× gate.

**`recency_v1` therefore remains the default**, and `channels` stays an opt-in policy. With the hashing embedder, the dense channel added nothing over lexical alone. See [the results](../../benchmarks/005-candidate-generation/RESULTS.md).

## Consequences

- Callers who need old evidence can opt in to `channels` today, knowing the latency cost.
- The next step is a cheaper channel implementation tested under a **new** preregistration. 005's relevance result is not reused as evidence for it.
- Writes carry a small extra cost: computing and indexing `index_terms`.

## Verification

- **Conformance** (`channels_reach_beyond_the_recency_window`, both stores): equal budgets, the old fact found through each channel, channels recorded in the trace, identical replay, no out-of-scope candidates, and rejection of quotas over budget.
- **Benchmark crate:** unit tests for generator determinism and stratum balance, metric reference values, and the bootstrap.
