# ADR-0015: Belief recall alongside memory recall

- Status: Accepted
- Date: 2026-11-13

## Context

Agents need to know what the system *believes* about a query, how strongly, and on what evidence, not only which memories match.

## Decision

- Recall returns `beliefs` next to `results`. Beliefs pass through the same authorization, scope filter, as-of transaction time and valid time as memories. Policy fields `include_beliefs`, `max_belief_results`, `include_disputed_beliefs` and `include_retracted_beliefs` control which are returned.
- Belief relevance uses the **same scoring function and tie-breaking** as memories (`rank_items`). **Confidence is never blended into relevance.** Each recalled belief carries `relevance` and its own `confidence` as separate fields, plus its supporting and refuting evidence ids.
- Belief candidates and their scores are persisted in the recall trace. Exact replay compares belief selections as well as memory selections.
- Belief embeddings are computed on the fly from the configured embedder (propositions are short, and the built-in embedder is deterministic). Persisting them is deferred until a model-backed embedder makes it worthwhile.

## Verification

Conformance scenario `beliefs_are_recalled_with_evidence`, and the HTTP test `governed_processing_over_http`.
