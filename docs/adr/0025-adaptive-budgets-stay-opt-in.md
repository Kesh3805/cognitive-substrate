# ADR-0025: Adaptive candidate budgets stay opt-in

- Status: Accepted
- Date: 2026-11-15

## Context

Every recall under the default policy loads and scores 200 candidates, even when the best lexical matches are only a handful. A smaller first pass would be cheaper, but cutting the lexical channel short loses old, relevant memories that sit behind many newer, better-matching records. Experiment 009's crowded family was built to show exactly that.

## Decision

### The budget can adapt, by a deterministic rule

`RecallPolicy.budget` is `Fixed` (the default) or `Adaptive { initial, initial_lexical }`.

**Requirements:**
- lexical-only channels;
- `0 < initial_lexical ≤ initial ≤ max_candidates`;
- `initial_lexical` within the lexical quota;
- `max_results ≤ initial`.

**The rule:**
1. A first pass would admit the first `initial_lexical` records of the lexical order.
2. The channel is **saturated** when all those slots are filled **and** the last shares as many query terms as the first. In other words, the cut falls inside the best-matching tier, so equally good matches may lie beyond it.
3. A saturated recall uses the full budget. It gets exactly what `Fixed` would return.
4. Any other recall uses the first pass.

Saturation records that the first pass was truncated. It does not claim that expanding helps.

### Decided and executed in one store operation

The engine owns the policy, and the store executes it:
- `CognitiveStore::adaptive_lexical_then_recent` takes the policy parameters and returns the candidates with the decision inputs.
- PostgreSQL answers it with one statement. Its answer must equal the Rust reference (set, channel labels and decision), as the conformance suite checks.

The decision (`initial`, `initial_lexical`, `lexical_found`, `best_overlap`, `cut_overlap`, `lexical_saturated`) is stored in the trace, and exact replay re-derives it.

An earlier version issued a second round trip to expand. On development seeds it failed the latency gates (p95 1.33×), so it was replaced before the held-out run, as disclosed in the preregistration.

### `Fixed` stays the default

Experiment 009 ([results](../../benchmarks/009-adaptive-budgets/RESULTS.md)) ran on held-out seeds:

- **Quality: non-inferior.** nDCG@10 +0.002, CI [−0.001, +0.005], and identical candidate recall.
- **Cost:** 0.48× the candidates scored.
- **The signal: exact.** No query that stayed small lost a relevant candidate, and 62% of the expansions recovered one that the first pass alone missed.
- **Replay:** 785/785 identical.
- **Latency:** recalls that stayed small were about 30% faster, but 31% of recalls expanded at full cost. The aggregate p95 was 0.96× against a preregistered 0.9×, and p99 was 0.98×.

**The latency gate failed, so `Adaptive` is not adopted as the default.** It remains available for policies that opt in.

## Consequences

- Callers whose workloads rarely tie in the best tier, or whose scoring is expensive, can opt in. They get the same candidate recall at about half the scoring work. No such workload has been measured yet.
- Old traces read as `Fixed`, so replays of earlier recalls are unchanged.
- Making adaptive budgets the default needs a new preregistered experiment, for example one with a heavier scorer or a different first-pass size. 009 is not re-run with a tuned implementation.
