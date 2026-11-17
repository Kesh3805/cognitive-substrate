# 012 — Read-time counterfactual bounds

- Status: **preregistered** (committed before any run on held-out seeds 801–805)
- Date: 2026-11-17
- Concerns: M6.3, counterfactual version 1, over causal evaluation version 1.

## Question

For a randomized unit under a supported hypothesis, do the reported bounds on the probability of necessity (PN) and of sufficiency (PS):

- contain the true value;
- say something when the context has the evidence to;
- refuse every request they cannot answer;
- leave nothing behind?

## Method (frozen)

### Which questions are answered

A counterfactual is computed on request from one intervention and one hypothesis. The checks run in this order:

| Condition | Result |
|---|---|
| The hypothesis is not `supported` | Refused: `not_supported` |
| The intervention concerns another environment or treatment | Refused: `unrelated` |
| The intervention is passive | Refused: `not_randomized` |
| Its context is not in the evaluation's `scope_contexts` | Refused: `out_of_scope` |
| No strong-tier outcome for the hypothesis's key | Refused: `no_eligible_outcome` |
| Treated with the outcome present (for `raises`) | Answers PN |
| Control with the outcome absent (for `raises`) | Answers PS |
| Any other arm and outcome | Refused: `outcome_mismatch` |

For `lowers`, "present" means the outcome is absent.

### The bounds

The bounds are Tian and Pearl's bounds under exogeneity, from the context's experimental counts:

```
PN ∈ [1 − p0/p1, (1 − p0)/p1]
PS ∈ [1 − (1 − p1)/(1 − p0), p1/(1 − p0)]
```

- **Sampling uncertainty:** each limit is a ratio of two independent proportions, or one minus such a ratio. It is taken at its own one-sided 97.5% Katz log-ratio limit. When a cell is empty, 0.5 is added to each count and 1 to each total (Haldane).
- **Clipping:** results are clipped to [0, 1].
- **Undefined bounds:** `insufficient_evidence` is returned when a bound's denominator count is 0.

### What every result states

- `kind: inferred`;
- the hypothesis version;
- the context counts;
- the assumptions: exogeneity, consistency, no interference, and that the context's units represent this one;
- `informative`, meaning not [0, 1].

Counterfactuals are never stored, evented or used as evidence. No monotonicity is assumed.

## Data

Every unit has both potential outcomes, from a per-context mix of four response types, written as (outcome if treated, outcome if not):

| Type | Outcomes |
|---|---|
| always | (1, 1) |
| helped | (1, 0) |
| harmed | (0, 1) |
| never | (0, 0) |

The true values are:

```
PN = helped / (always + helped)
PS = helped / (helped + never)
```

**Families,** 20 hypotheses each per seed, with 8 contexts each:

| Family | always | helped | harmed |
|---|---|---|---|
| monotone | 0.1–0.3 | 0.25–0.5 | 0 |
| defiers | 0.05–0.2 | 0.35–0.55 | 0.05–0.15 |
| rare | 0.02 | 0.15–0.25 | 0.02 |
| boundary | 0.01 | 0.85–0.95 | 0 |
| null | 0.1–0.3 | h | h, with h in 0.1–0.2 |

In every family, `never` is the remainder.

**Per hypothesis:**
- one randomized sample size per arm, from {6, 12, 25, 50}, used in every context;
- all outcomes deterministic-tier;
- `min_effect` 0.05.

**Pipeline:** every hypothesis goes through M6.2's evaluation. Counterfactuals are asked only of what it supported, for one PN unit and one PS unit per context in scope. So hypotheses that were supported partly by chance are part of the test.

**Refusal probes,** four per hypothesis, recorded before evaluation:
- a passive treated unit;
- a randomized unit in an otherwise empty context;
- two self-assessed randomized units.

Their expected reasons are `not_randomized`, `out_of_scope` and `no_eligible_outcome`, or `not_supported` for every probe when the hypothesis was not supported.

**Seeds:** development 1–3; held-out 801–805. **Store:** in-memory, as the conformance suite fixes both stores' behaviour.

## Adoption criteria (all must hold on held-out seeds)

1. The PN bounds contain the true PN for ≥ **0.95** of answered (hypothesis, context) pairs.
2. The PS bounds contain the true PS for ≥ **0.95** of them.
3. **Sharpness:** where the context has ≥ 25 outcomes per arm and the true (population) lower bound is ≥ 0.3, the reported lower bound is > 0 in ≥ **0.80** of cases.
4. Every probe is refused with exactly the expected reason.
5. Computing counterfactuals writes nothing. Checked by unchanged hypothesis versions and unchanged event counts for every event type with fewer than 1,000 events. The exact all-events check is in the conformance suite.

Coverage is also reported per family and per question. Any family below 0.95 is reported as a limit, even when the overall gates pass.

## Disclosure: what development seeds changed

The design was reviewed twice before any held-out run:

| Review | What it changed |
|---|---|
| Before implementation | A Bonferroni (joint) construction; typed refusals; stated assumptions; no monotonicity; passive episodes refused |
| On the experiment | Added the defier, rare, boundary and null families; the sharpness gate; routing through M6.2 support; the refusal probes |

**Development run 1** (seeds 1–3) used a box over two 97.5% Wilson intervals, taking the extremes at its corners.
- Coverage was 1,785/1,786 (PN) and 1,800/1,800 (PS).
- The sharpness gate failed at 0.61. Even at 50 per arm, only 21% of rare-outcome lower bounds were informative.
- The box was far more conservative than its 95% target.

**Revision:** each bound got its own one-sided 97.5% ratio limit (Katz), as above.
- An external review accepted the disclosed change.
- It warned that Katz is an approximation that is not guaranteed conservative with sparse or empty cells. Its coverage is therefore claimed only as measured, per family.

**Development run 2:**

| Measure | Result |
|---|---|
| Coverage, PN | 1,781/1,786 (0.997) |
| Coverage, PS | 1,794/1,800 (0.997) |
| Every family | ≥ 0.992 |
| Sharpness | 1,440/1,752 (0.82) |
| Probes | 1,200/1,200 |
| Writes | Nothing written |

A second external review named three ways Katz could undercover on held-out data:

1. **An all-success arm** (an empty complement cell) must trigger the correction. It does: the correction fires on any empty cell, and an empty denominator is refused.
2. **Near the boundary,** the log-normal tail can be anti-conservative when one arm is one short of all-success.
3. **Very unbalanced arms** bias rare-outcome limits. This benchmark's arms are equal, so it does not test that case, and it is a stated limit.

The gates were not changed between runs. **These numbers are not evidence.** Only the held-out seeds decide.

## If adopted

Counterfactuals ship as a read-only endpoint: `GET /v1/interventions/{id}/counterfactual?hypothesis=`. If any criterion fails, the failure is reported and the method is not revised within this experiment.
