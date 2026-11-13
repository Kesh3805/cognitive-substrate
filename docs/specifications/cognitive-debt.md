# Cognitive debt — model version 1

Software accumulates technical debt. Cognitive systems accumulate **epistemic debt**: unverified assumptions, unresolved contradictions, stale beliefs, weakly supported conclusions, and procedures never tested outside their original conditions.

**Definition.** Cognitive debt is the accumulated risk carried by retained knowledge or capabilities that are unreliable, unresolved, stale, unsupported, or dependent on questionable assumptions.

Uncertainty alone is not debt. Debt exists when an unresolved condition creates material risk to future decisions.

## Detection (deterministic)

The evaluator considers *live* beliefs (current revision, status `active` or `disputed`) and active procedural memories within the caller's scope.

| Category | Condition | Issue confidence |
|---|---|---|
| `unsupported_belief` | No `supports` evidence link | 1.0 |
| `unresolved_contradiction` | Same normalized proposition with opposite polarity, same tenant and project, and overlapping valid time; or an explicit `contradicts` edge | 0.9 / 1.0 |
| `stale_belief` | Older than its `revalidation_interval_days` (or the configured default) | 1.0 |
| `invalidated_dependency` | Relies (`supports`, `requires`, `derived_from` or `specializes`) on a belief whose current revision is retracted, or on a superseded revision without a re-affirmed edge from the current revision | 1.0 / 0.8 |
| `unverified_procedure` | Procedural memory at stage `observed` or `candidate` | 1.0 |

## Per-item score

```
D_i = I_i · C_i · (1 + log2(1 + A_i / H_i)) · E_i
E_i = min(3, 1 + ln(1 + N_i))
```

| Symbol | Meaning |
|---|---|
| `I` | Impact (1–5), configured per category |
| `C` | Confidence that the issue is real |
| `A` | Age in days since the condition arose |
| `H` | Category half-life in days, the age at which the age multiplier reaches 2 |
| `N` | Weighted count of live beliefs relying on the affected belief |

Default parameters (impact / half-life in days):

- unsupported 3 / 7
- contradiction 4 / 3
- stale 2 / 30
- invalidated 4 / 3
- unverified 3 / 14

**Worked example.** I = 5, C = 0.9, A = 14, H = 7, N = 3 gives `5 · 0.9 · (1 + log2 3) · (1 + ln 4) ≈ 27.76`.

## Aggregate

```
CDI = 1000 · Σ D_i / max(1, active_assets)
```

Active assets are live beliefs plus active procedural memories. The CDI is debt points per 1,000 active cognitive assets. The report also includes the raw total, a per-category breakdown, and every item with all of its calculation inputs, so the number can be audited.

## Caveats

- This is a heuristic risk score for prioritising remediation. It is **not** a probability of failure.
- CDIs are not comparable across systems until thresholds, impact scales and half-lives are standardized.
- If a lower CDI does not correlate with better decisions in Experiment 004, the metric is revised. The system is not tuned to make the number look good.
