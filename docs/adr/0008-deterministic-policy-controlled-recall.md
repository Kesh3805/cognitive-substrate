# ADR-0008: Deterministic, policy-controlled recall

- Status: Accepted
- Date: 2026-11-13

## Context

In the predecessor's experiments, research-memory retrieval had a 0% hit rate, hybrid retrieval brought no improvement and reranking regressed. Retrieval complexity must earn its place, and that requires decisions that can be inspected and reproduced.

## Decision

- Recall runs under an explicit `RecallPolicy` value: domains, scope filter, as-of valid and transaction times, candidate and result limits, token budget, weights, recency half-life, minimum score, and inclusion of superseded or candidate revisions.
- Scoring version 1 is `S = w_d·D + w_l·L + w_r·R`, with weights summing to 1 (see `docs/specifications/recall-scoring.md`).
- Scoring is **exact over the filtered candidate set**. No ANN is used, so the candidate set does not vary between runs. Ties break on `(memory_id, revision_id)`.
- Every recall persists a `RecallTrace`: the resolved policy, query hash (and text, unless disabled), embedding model, data snapshot, and every candidate with its component scores and selection reason.
- **Exact replay** re-executes against the recorded as-of times. **Counterfactual replay** substitutes policy parameters and reports which memories were added or removed.
- The built-in embedder is a deterministic hashing baseline. Learned embedders are adapters behind the `Embedder` trait.

## Consequences

The candidate cap bounds latency. ANN preselection may be added in M5, but only once it has been benchmarked against this exact baseline.

## Verification

The determinism tests in `cs-policy::scoring` and the `bitemporal_recall_is_reproducible` scenario.
