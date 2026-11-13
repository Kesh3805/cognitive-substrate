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

The set is ordered by transaction start descending, then revision id ascending, and truncated to `max_candidates`.

Defaults: `as_of_tx = now` and `as_of_valid = as_of_tx`. The resolved values are recorded in the trace.

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
