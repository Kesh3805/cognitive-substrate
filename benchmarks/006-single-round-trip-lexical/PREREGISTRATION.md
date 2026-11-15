# 006 — Lexical channel in one store round trip

- Status: **preregistered** (committed before any run on held-out seeds 201–205)
- Date: 2026-11-15
- Follows: [005](../005-candidate-generation/RESULTS.md). That experiment was not adopted because it failed its latency gate. Its results are **not reused** as evidence here.

## Question

Can lexical candidate generation (`channels { lexical: 100, dense: 0 }`) meet every gate of experiment 005, **including p95 latency ≤ 1.25× v1**, when the store answers it in one round trip?

## What changed since 005

1. **One round trip.** `CognitiveStore::lexical_then_recent` returns the lexical candidates and the recency fill together, never more than `max_candidates` rows. PostgreSQL answers it with one statement (CTEs over the same eligibility filters). The conformance scenario `lexical_channel_matches_its_reference` requires each store's answer to equal the reference semantics (the two orders, merged): the same set and the same channel labels.
2. **Rotated strategy order.** 005 ran strategies in a fixed order for every query, with `v1` always first. Here the order is rotated per query (query index modulo the number of strategies), so no strategy always pays first-query costs.
3. **No dense channel.** In 005 the dense channel added nothing measurable with the hashing embedder, so it is left out of this experiment.

### Disclosure: what development seeds showed

Changes 1 and 2 were engineered on development seeds 1–3, which 005 also used for development. With both changes, the development run showed:

- p95 latency of 1.06× v1;
- relevance identical to 005's `L` strategy on the same development seeds.

These development numbers are not evidence. Only the held-out seeds below decide.

## Strategies (fixed)

| Id | Policy | Role |
|---|---|---|
| `v1` | `recency_v1` | Frozen control |
| `L` | `channels { lexical: 100, dense: 0 }` (single round trip) | **Primary hypothesis** |

Everything else is identical to 005: `max_candidates = 200`, `max_results = 10`, v1 scoring and weights, `min_score = 0`, and no token budget.

## Data, metrics and statistics

These are identical to 005 (as amended), with these differences:

- **Held-out seeds: 201, 202, 203, 204, 205.** These are new, and never run before this preregistration was committed.
- **Development seeds:** 1, 2, 3. They are never reported as results.
- **Store:** PostgreSQL + pgvector, in a dedicated benchmark database.
- **Embedder:** `cs/hashing@1`.
- **Strategy order:** rotated per query.

## Adoption criteria for `L` (all must hold on held-out seeds)

1. Mean nDCG@10 delta ≥ **+0.03** absolute **and** ≥ **5%** relative, with the 95% CI lower bound > 0.
2. Mean candidate recall delta ≥ **+5 percentage points**.
3. No family's mean nDCG@10 regresses by more than **0.02**.
4. Recent-stratum mean nDCG@10 does not regress by more than **0.02**.
5. The 95% CI of the MRR delta is **not entirely below 0**.
6. **Zero** scope and temporal violations, for every strategy.
7. Mean tokens ≤ **1.05×** `v1`.
8. p95 latency ≤ **1.25×** `v1`.

## If adopted

- `channels { lexical: 100, dense: 0 }` becomes the default candidate generation for **new** recall policies. `recency_v1` stays selectable as the frozen control.
- Stored recall traces whose policy predates the `candidates` field are migrated to record `recency_v1` explicitly, so that their replays stay exact under the new default.

If any criterion fails, `recency_v1` remains the default and the failure is reported. Criteria are not changed after results exist.
