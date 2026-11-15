# 009 — Adaptive candidate budgets

- Status: **preregistered** (committed before any run on held-out seeds 501–505)
- Date: 2026-11-15
- Follows: [006](../006-single-round-trip-lexical/RESULTS.md), which made `channels { lexical: 100, dense: 0 }` with a 200-candidate budget the default.

## Question

Can a recall start with a small candidate budget and use the full budget only when its lexical channel was truncated inside the best-matching tier, while scoring far fewer candidates, staying non-inferior in quality, and lowering tail latency against the fixed full budget?

## Policy semantics (fixed independently of the implementation)

`CandidateBudget::Adaptive { initial, initial_lexical }`, with lexical-only channels `{ lexical: L, dense: 0 }` and `max_candidates: M`:

1. **First pass.** Take the first `initial_lexical` records of the lexical order: shared query terms (tokenizer v1) DESC, then transaction start DESC, then revision id ASC. Records are in scope and eligible at the as-of times.
2. **Decision.** Let `best` and `cut` be the shared-term counts of the first and the last of them. The channel is **saturated** when all `initial_lexical` slots are filled **and** `cut == best`, so equally good matches may lie beyond the cut.
3. **Candidates.**
   - **Saturated:** exactly the fixed result, up to `L` lexical candidates then the recency fill, `M` in total.
   - **Not saturated:** the first pass, then the recency fill, `initial` in total.
4. **Trace.** The decision (`initial`, `initial_lexical`, `lexical_found`, `best_overlap`, `cut_overlap`, `lexical_saturated`) is recorded in the trace. Exact replay must re-derive the same decision.

Saturation says the first pass was truncated. It does not say that expanding helps; this experiment measures that.

## Implementation (frozen)

`CognitiveStore::adaptive_lexical_then_recent` decides and executes the budget in **one store operation**:

- PostgreSQL answers it with one statement.
- The Rust reference, `adaptive_lexical_then_recent_reference`, defines it, and the conformance scenario `lexical_channel_matches_its_reference` requires the store's answer to match it: the same set, channel labels and decision.
- The engine passes the policy parameters, records the decision, and scores what comes back.

### Disclosure: what development seeds showed

The design was revised once, on development seeds 1–3 (PostgreSQL):

| Version | nDCG@10 A / F200 / F50 | Candidates scored A / F200 | Expansions | p95 A / F200 | p99 A / F200 |
|---|---|---|---|---|---|
| 1: expansion is a second store round trip | 0.779 / 0.774 / 0.665 | 87.1 / 200 (0.44×) | 121 of 489 | 76.2 / 57.4 ms (1.33×) | 108.2 / 93.7 ms (1.15×) |
| 2: one store operation (this one) | 0.779 / 0.774 / 0.665 | 87.1 / 200 (0.44×) | 121 of 489 | 49.0 / 53.9 ms (0.908×) | 55.4 / 65.0 ms (0.85×) |

- **Why version 1 failed:** about 25% of recalls expanded, and each expansion paid for a second full store round trip, so the latency gates failed.
- **What version 2 changed:** only execution. Candidates, decisions and relevance were identical on the development seeds, and replay was 489/489 identical in both versions.
- **What version 2 still showed:** it missed the p95 gate narrowly on the development seeds (0.908×).
- **What stayed fixed:** the gates were set before either run and are **unchanged**.

These development numbers are not evidence. Only the held-out seeds below decide. If the held-out run fails, the implementation is not optimised again within this experiment; a further change would need a new experiment.

## Strategies (fixed)

| Id | Policy | Role |
|---|---|---|
| `F200` | `max_candidates: 200`, `channels { lexical: 100, dense: 0 }`, `Fixed` (the current default) | Control |
| `A` | the same, with `Adaptive { initial: 50, initial_lexical: 25 }` | **Primary hypothesis** |
| `F50` | `max_candidates: 50`, `channels { lexical: 25, dense: 0 }`, `Fixed` (A's first pass, never expanded) | Ablation |

All strategies use `max_results = 10`, v1 scoring and weights, `min_score = 0`, no token budget and no beliefs.

## Data

005's generator (seeded; gold labels from the generator, never from text similarity), plus a **crowded** family (`crates/cs-bench/src/crowded.rs`):

- **15 services** are chosen per seed. Each has exactly one still-valid store fact and no replica name.
- **30 crowding notes per service:** "standup note c{n}: the {service} data store came up again". The notes are placed after the fact, in the second half of the year. They share 4 terms with the store question, while the fact shares 3, so a 25-slot first pass fills with notes and cuts the fact off.
- **One crowded query per service:** the direct store question, whose gold is the fact.

Other details:

- **Families:** direct, lexical trap, semantic, temporal, scope (005's) and crowded.
- **Held-out seeds:** 501, 502, 503, 504, 505, new and never run before this preregistration was committed. Development seeds: 1, 2, 3.
- **Store:** PostgreSQL + pgvector, in a dedicated benchmark database. A fresh tenant is used per seed.
- **Strategy order:** rotated per query.
- **Latency:** wall time of `recall`, per query and strategy. Replays and trace reads are outside the timing.

## Metrics

- **Quality:** nDCG@10, MRR@10, recall@10, candidate recall.
- **Cost:** candidates scored, and latency p50, p95 and p99.
- **Paired deltas:** A − F200 and A − F50. 95% CIs come from a paired bootstrap: 10,000 resamples, seed 7.

## Adoption criteria for `A` (all must hold on held-out seeds)

1. **nDCG@10 non-inferior:** 95% CI lower bound of A − F200 ≥ **−0.01**.
2. **Candidate recall non-inferior:** mean A − F200 ≥ **−0.01**.
3. **Mean candidates scored** ≤ **0.6×** F200.
4. **p95 latency** ≤ **0.9×** F200.
5. **p99 latency** ≤ **1.0×** F200.
6. **Exact replay** reproduces every adaptive selection **and** budget decision (100%).
7. **Zero** scope and temporal violations, for every strategy.

## Reported, not gated

- Expansion rate, overall and per family.
- Expansions that recovered a relevant candidate F50 missed.
- Non-expanded queries where F200's candidates held more relevant memories than A's (misses the signal did not see).
- Latency of A and F200 on the expanded and on the non-expanded queries, separately.

## If adopted

- `Adaptive { initial: 50, initial_lexical: 25 }` becomes the default budget for **new** recall policies, and `Fixed` stays selectable.
- Stored traces whose policy predates the `budget` field already read as `Fixed`, so their replays stay exact.

If any criterion fails, `Adaptive` remains an opt-in policy, `Fixed` remains the default, and the failure is reported. Criteria are not changed after results exist.
