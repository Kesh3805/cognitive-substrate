# Causal evidence — evaluation version 1

## Records

**Hypothesis** (versioned; every version is kept):

| Field | Meaning |
|---|---|
| `environment`, `treatment`, `outcome` | Keys, 1–200 characters |
| `direction` | `raises` or `lowers` |
| `min_effect` | The smallest effect worth supporting, in proportion points, in `0..1` |
| `contexts` | The declared target contexts. Empty means whichever contexts are sampled |
| `statement` | Optional |
| `status` | One of the statuses below |
| `evaluation` | The evaluation that set the status |

**Intervention** (written at intent):

| Field | Meaning |
|---|---|
| `environment`, `treatment` | Keys |
| `arm` | `treated` or `control` |
| `assignment` | `randomized`, which needs `assignment_detail` (the draw) and a `propensity` in (0, 1); or `passive`, which has no propensity |
| `context` | The partition, for example a capability input class |
| `state` | Up to 32 initial-state features |
| `execution_id` | Optional; must be readable |

Each intervention's `outcomes` map records one observation per outcome key, once: `{ value, tier, observed_at }`. A second observation for the same key is a conflict.

## Which evidence counts

Interventions count for a hypothesis when they meet all of these:

- they are in the hypothesis's **own scope**;
- they have the same environment and treatment;
- their context is among the declared contexts, if any are declared;
- their outcome for the hypothesis's key was established by a `deterministic` or `independent` tier.

Self-assessed outcomes never count. Randomized interventions decide the status. Passive ones only feed the observational report.

## Effects

All effects are in the hypothesis's direction (positive supports it) and are treated-minus-control proportions.

### Per context

- **Interval:** Newcombe's hybrid score interval, from the two arms' Wilson intervals (z = 1.96).
- **Eligible:** both arms have at least **2** counted outcomes.

### Pooled over eligible contexts

The pooled effect is a stratified Newcombe–Wilson interval:

1. Each context `k` gets the Mantel–Haenszel weight `w_k = n_tk n_ck / (n_tk + n_ck)`, with `W = Σ w_k`.
2. For each arm: `p̂ = Σ (w_k / W) p̂_k`, with effective size `n_eff = 1 / Σ ((w_k / W)² / n_k)`. Its interval is the Wilson interval for `p̂` at `n_eff`.
3. The difference `p̂_t − p̂_c` gets Newcombe's square-and-add interval from the two arm intervals.

There is no pooled effect with fewer than **5** eligible contexts or with `W < 5`.

## Status

| Condition | Status |
|---|---|
| No pooled effect | `proposed` |
| lower > `min_effect`, and no eligible context's own interval lies entirely below 0 | `supported` |
| upper < `min_effect` | `refuted` |
| Otherwise | `inconclusive` |
| A person withdrew it | `retired` (terminal; not evaluated again) |

- A `supported` or `refuted` status speaks only for the eligible contexts listed in `scope_contexts`.
- With `min_effect = 0`, a true null stays `inconclusive`. That is by design: absence of an effect is refuted only against a stated minimum.

## Evaluation record

The evaluation records:

- `as_of`;
- the counted randomized intervention ids;
- per-context counts and intervals;
- `eligible_contexts`;
- the pooled `effect` (`estimate`, `lower`, `upper`, `weight`);
- `opposite_context`;
- `scope_contexts`;
- `observational`: the same estimator over passive evidence. It is reported to prioritise what to randomize next, and never decides the status.

Evaluating writes a new version, even when the status is unchanged. A stale `expected_version` is a conflict.

## API

| Method | Path | Effect |
|---|---|---|
| GET | `/v1/hypotheses?status=&limit=` | Hypotheses the caller can read, newest change first |
| POST | `/v1/hypotheses` | Create a hypothesis (201) |
| GET | `/v1/hypotheses/{id}` | Read a hypothesis |
| GET | `/v1/hypotheses/{id}/history` | Every version, oldest first |
| POST | `/v1/hypotheses/{id}/evaluate` | `{ expected_version }` |
| POST | `/v1/hypotheses/{id}/retire` | `{ expected_version }` |
| POST | `/v1/interventions` | Record an intervention at intent (201) |
| GET | `/v1/interventions/{id}` | Read an intervention |
| POST | `/v1/interventions/{id}/outcomes` | `{ outcome, value, tier }`, once per outcome key |
| GET | `/v1/interventions/{id}/counterfactual?hypothesis=` | Read-time bounds ([Counterfactuals](#counterfactuals-version-1-experimental)) |
| POST | `/v1/interventions/{id}/predictions` | `{ hypothesis_id }`: record a prediction at intent (201), or a refusal (200) ([predictions](predictions.md)) |
| GET | `/v1/hypotheses/{id}/rule?context=&threshold=&min_effect=` | The rule, its lineage's monitor and the gate ([predictions](predictions.md)) |

## Events

| Event | Payload |
|---|---|
| `HypothesisChanged` | Version, status, change (`created`, `evaluated`, `retired`) and evidence counts |
| `InterventionRecorded` | Arm and assignment |
| `InterventionObserved` | Tier |

## Counterfactuals (version 1, experimental)

`GET /v1/interventions/{id}/counterfactual?hypothesis={id}` computes, and never stores, the following.

### Order of checks

The first check that applies decides:

| # | Condition | Result |
|---|---|---|
| 1 | The hypothesis is not supported | `not_supported` |
| 2 | Another environment or treatment | `unrelated` |
| 3 | A passive assignment | `not_randomized` |
| 4 | The context is not in `scope_contexts` | `out_of_scope` |
| 5 | No strong-tier outcome | `no_eligible_outcome` |
| 6 | Treated with the outcome present | PN |
| 7 | Control with the outcome absent | PS |
| 8 | Anything else | `outcome_mismatch` |

For `lowers`, "present" means the outcome is absent.

### Bounds

From the context's counts:

```
PN ∈ [1 − p0/p1, (1 − p0)/p1]
PS ∈ [1 − (1 − p1)/(1 − p0), p1/(1 − p0)]
```

- Each ratio takes its one-sided 97.5% Katz upper limit: `log R + 1.96 · sqrt(1/a − 1/nₐ + 1/b − 1/n_b)`.
- With any empty cell, 0.5 is added to every count and 1 to every total (Haldane).
- An empty denominator count returns `insufficient_evidence`.
- Results are clipped to [0, 1].

### Response

`{ kind: "inferred", version, hypothesis_id, hypothesis_version, intervention_id, result }`, where `result` is either:

- `bounds { query, lower, upper, context, assumptions, informative }`, or
- `refused { reason }`.
