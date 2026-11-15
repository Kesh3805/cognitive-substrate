# Recall scoring — version 1

## Candidate set

The storage adapter returns revisions that satisfy **all** of the following:

- readable by the caller (`AccessContext::can_read`, evaluated inside the query);
- domain ∈ `allowed_domains`;
- status `active`, plus `candidate` when `include_candidates` is set;
- matching `scope_filter`, if one is given;
- `lower(transaction_time) ≤ as_of_tx`, and, unless `include_superseded`, `transaction_time ∋ as_of_tx`;
- `valid_time ∋ as_of_valid`;
- not expired at `as_of_tx`.

Defaults: `as_of_tx = now` and `as_of_valid = as_of_tx`. The resolved values are recorded in the trace.

### Candidate generation

`policy.candidates` chooses which eligible revisions fill the budget of `max_candidates` unique revisions. Every strategy fills the same budget.

| Strategy | Candidate set |
|---|---|
| `recency_v1` (**frozen control**) | The newest eligible revisions: transaction start DESC, then revision id ASC |
| `channels { lexical: L, dense: D }` | In this order, adding only revisions not already present: up to `L` **lexical** candidates, up to `D` **dense** candidates, then the newest revisions until the budget is full. Quotas are upper bounds: the set never exceeds `max_candidates`. |

The default is `channels { lexical: 100, dense: 0 }`, adopted by the preregistered [experiment 006](../../benchmarks/006-single-round-trip-lexical/RESULTS.md). With `dense: 0`, a store answers the lexical candidates and the recency fill in **one** operation (`lexical_then_recent`). PostgreSQL does this in one round trip, and its answer is tested against the reference semantics (the two orders, merged). Traces recorded before this field existed record `recency_v1`, so their replays stay exact.

- **Lexical channel.** Takes revisions sharing at least one term with the query, ordered by the number of shared distinct terms DESC, then the recency order. Terms follow the shared lexical contract, `cs_memory::lexical` (tokenizer v1): lowercased alphanumeric tokens, stopwords removed, distinct. Records store their terms (`index_terms`), so every store matches identically. Postgres full-text search is not used.
- **Dense channel.** Takes revisions with a vector in the active space that was available at `as_of_tx`, ordered by exact cosine distance ASC, then revision id ASC. If the query cannot be embedded, the channel contributes nothing.
- **Channels in the trace.** Every candidate records the channels that proposed it (`recent`, `lexical`, `dense`).

### Candidate budget

`policy.budget` is `fixed` (default) or `adaptive { initial, initial_lexical }`.

**Requirements:**
- lexical-only channels `{ lexical: L, dense: 0 }`;
- `0 < initial_lexical ≤ initial ≤ max_candidates`;
- `initial_lexical ≤ L`;
- `max_results ≤ initial`.

**Rule.** Let `best` and `cut` be the shared-term counts of the first and the last of the first `initial_lexical` records in the lexical order.

| Condition | Candidates |
|---|---|
| All `initial_lexical` slots are filled **and** `cut == best` (**saturated**) | The fixed result: up to `L` lexical candidates, then the recency fill, `max_candidates` in total |
| Otherwise | Those lexical candidates, then the recency fill, `initial` in total |

**Execution.** One store operation, `adaptive_lexical_then_recent`, decides and executes the budget. PostgreSQL uses one statement, tested against `adaptive_lexical_then_recent_reference`.

**Trace.** The trace records `budget: { initial, initial_lexical, lexical_found, best_overlap, cut_overlap, lexical_saturated }`, and exact replay re-derives it. Fixed budgets record no decision.

Experiment 009 found adaptive budgets non-inferior in quality at 0.48× the candidates scored, but p95 latency was 0.96× against a 0.9× gate. They are therefore not the default ([ADR-0025](../adr/0025-adaptive-budgets-stay-opt-in.md)).

The BM25 component is computed over whichever candidate set was generated, so changing the strategy can change lexical scores as well as membership.

## Score

```
S(d, q) = w_d · D(d, q) + w_l · L(d, q) + w_r · R(d)        w_d + w_l + w_r = 1
```

| Component | Definition |
|---|---|
| `D` | `(cos(e_q, e_d) + 1) / 2`, using embeddings from the active model. It is 0 when the record has no embedding from that model. |
| `L` | BM25 (`k1 = 1.2`, `b = 0.75`) of `index_text()` over the candidate set, divided by the set's maximum (0 if nothing matches). Tokens are lowercased and alphanumeric, with stopwords removed. |
| `R` | `0.5 ^ (age_days / recency_half_life_days)`, where age runs from `provenance.observed_at` to `as_of_tx` and is clamped to ≥ 0. |

## Ranking and selection

Candidates are sorted by `(S DESC, memory_id ASC, revision_id ASC)` and walked in order:

1. `S < min_score` gives `below_min_score`;
2. once `max_results` are selected, the rest get `max_results_reached`;
3. if adding the candidate would exceed `max_context_tokens` (estimate: `ceil(chars / 4)`), it gets `context_budget_exceeded`;
4. otherwise it is `selected`.

## Determinism

Given the same stored data, policy, query and embedder, the output is identical. This holds because:

- the candidate set is defined by the recorded as-of times over immutable history;
- scoring is exact (no ANN);
- tie-breaking is total.

The trace's `data_snapshot` string (`tx=…;valid=…;scoring=v1;embedder=name@version`) identifies these inputs.

## Defaults

`dense 0.55 · lexical 0.30 · recency 0.15`, a 30-day half-life, 200 candidates and 10 results. These are a **baseline to be evaluated**, not a claim of optimality.
