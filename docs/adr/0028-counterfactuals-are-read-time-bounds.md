# ADR-0028: Counterfactuals are read-time bounds, and stay experimental

- Status: Accepted
- Date: 2026-11-17

## Context

Once a hypothesis is supported (ADR-0027), it is tempting to explain individual episodes with it: "the deploy was fast *because* the cache was warmed". That claim is about a unit's unobserved alternative, and data never identifies it.

A memory system that stores such explanations as facts lets one supported average become many confident, unverifiable stories. If those stories then feed back as evidence, they confirm themselves.

## Decision

### Computed on read, never kept

A counterfactual is computed when asked for (`GET /v1/interventions/{id}/counterfactual?hypothesis=`). It is:

- labelled `inferred`;
- tied to the hypothesis version it used;
- never stored, never evented, and never accepted as evidence.

When the hypothesis loses support, its counterfactuals simply stop being computable.

### Bounds, not stories

**What is answered.** Only a randomized unit, in a context that a supported hypothesis covers, with a strong-tier outcome:

| The unit | The question | Answer |
|---|---|---|
| Treated, outcome present | Was the treatment necessary? | Probability of necessity, PN |
| Control, outcome absent | Would it have been sufficient? | Probability of sufficiency, PS |

**How the bounds are computed.**
- Both are reported as Tian and Pearl's bounds under exogeneity, from the context's experimental counts.
- Each limit takes its own one-sided 97.5% Katz log-ratio limit.
- No monotonicity is assumed, whatever direction the hypothesis states.

**What every answer states:**
- the assumptions: exogeneity, consistency, no interference, and that the context's population represents this unit;
- whether the bounds are informative at all.

**What is refused, with a typed reason:**

| Request | Reason |
|---|---|
| A passive episode | `not_randomized` |
| A context out of scope | `out_of_scope` |
| An unsupported hypothesis | `not_supported` |
| An ineligible outcome | `no_eligible_outcome` |
| An arm and outcome with no question to answer | `outcome_mismatch` |
| An undefined bound | `insufficient_evidence` |

### Experimental status

[Experiment 012](../../benchmarks/012-counterfactual-bounds/RESULTS.md) ran on held-out seeds, through M6.2's own support decisions.

| Criterion | Result |
|---|---|
| Bounds contain the true PN | 99.9% |
| Bounds contain the true PS | 99.8% |
| Every family | ≥ 99.6% |
| Probes refused correctly | All 2,000 |
| Writes | None |
| Informative where the context should be | **78%**, against a gate of 80% |

Rare outcomes reached only 17% at 25 outcomes per arm. **The counterfactual endpoint is therefore experimental.** It is safe to call, but often says nothing. Callers must check `informative`.

A first development design, a box over two Wilson intervals, was even less informative. It was replaced before the held-out run, as disclosed in the preregistration.

## Consequences

- Explanations of single episodes are bounded, labelled and transient. They cannot become facts or evidence.
- Useful counterfactuals need more randomized data than support does: about 50 per arm, and more for rare outcomes.
- Moving out of experimental needs a new preregistered experiment, with a tighter interval or a sample-size-aware gate. This one is not re-run with a tuned method.
