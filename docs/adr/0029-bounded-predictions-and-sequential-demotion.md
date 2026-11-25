# ADR-0029: Bounded predictions, sequentially monitored, gated fail-closed

- Status: Accepted (experiment 013 preregistered; adoption of the gate as a recommended decision path awaits its held-out result)
- Date: 2026-11-25

## Context

A supported hypothesis (ADR-0027) says that a treatment changes an outcome on average over the sampled contexts. An agent wants something different: whether acting **here, now** is likely to work, and whether that is still true.

Turning causal support into predictions closes a loop. Predictions decide which actions are taken, and only the actions taken produce outcomes. Without care, that loop fails in four ways:

1. **Statistical invalidity.** Re-checking a rolling window after every outcome, across many rules, inflates false alarms without bound.
2. **Starvation.** A refused or demoted rule never sees the evidence that could restore it.
3. **Self-confirmation.** A system that refuses everything difficult looks reliable.
4. **Laundering.** A new hypothesis version or id, a withheld outcome, or outcome-dependent latency escapes a bad record.

The design was reviewed externally twice before any held-out run: once as a draft and once as implemented. Two independent reviewers named largely the same issues each time. This ADR records the result.

## Decision

### Three objects, kept apart

| Object | Question | Decided by |
|---|---|---|
| Hypothesis (M6.2) | Does T change Y on average? | Randomized evaluation |
| Rule (M6.4) | How often does the favourable outcome follow T in context c? | The hypothesis's evaluation, plus local requirements |
| Gate | Should this agent act now? | The rule's lower bound against the caller's threshold |

- A predictive failure does not refute the hypothesis.
- A supported hypothesis does not authorize action by itself.

### A rule needs local support and precision

M6.2 lets a context into `scope_contexts` with two outcomes per arm, and pooled support says nothing about one context. A rule therefore needs both:
- the context's **own** effect estimate pointing the hypothesis's way;
- at least **20** treated outcomes.

Rules predict the treated arm (ADR-0030 adds the control arm and the effect). Rules are computed on read and never stored.

### Predictions are made at intent and frozen

- A prediction records, before the outcome, which rule was relied on and with what numbers.
- There is one prediction per intervention and lineage.
- None is accepted once the outcome is known.
- **Silence is failure.** No strong-tier outcome within an hour resolves the prediction as unfavourable, and a late outcome does not undo that.

### Demotion is sequential, one-sided and anytime-valid

A mixture of Bernoulli CUSUM e-detectors tests whether the live favourable rate is at least each prediction's frozen null, which is the rule's own lower bound:
- three alternatives, 0.1, 0.2 and 0.3 below the null;
- demotion at an e-value of 1000.

| Property | What it means |
|---|---|
| Valid under continuous monitoring | Under the null, the per-lineage average run length to a false alarm is at least 1000 resolved predictions. It is not a horizon-wide or cross-lineage error probability. |
| No staleness | The detector resets at 1, so a long good history cannot hide a late collapse. |
| One-sided | Doing better than predicted never demotes. |
| Intent order | Predictions are folded in the order they were made, up to the first pending one. Fast failures and slow successes cannot reorder the evidence. |

**Lineage, not record.** A lineage is the claim: scope, environment, treatment, outcome, direction and context. Neither re-evaluating a hypothesis nor restating it under a new id clears a demotion.

**Accumulating in fixed blocks.** Until a demotion, a rule counts its evaluation's treated outcomes plus complete blocks of 20 fresh randomized treated outcomes, so a rule that started below the threshold can earn its way above it. Without this, a context refused at burn-in stays refused forever: development run 1 of experiment 013 showed this costing about 0.11 utility per opportunity even when nothing changed.

**Re-earning in fixed blocks.** A demoted lineage serves no rule until a complete **block** of 20 fresh randomized, treated outcomes exists, recorded after the demotion. The latest complete block decides.
- Fixed blocks, not a bound re-checked after every outcome, so recovery is not optional stopping.
- Block outcomes use the monitor's deadline semantics.
- Passive outcomes never count.

**Demotions are derived, not stored.** They are a deterministic function of append-only records: immutable predictions and once-only outcomes. Every read recomputes them identically, and there is no write to race.

### The gate fails closed

No rule, a demoted rule, or a lower bound below the threshold all refuse. "No rule" is never a fall-through to acting. Exploration happens through the existing randomized interventions: an explicit probe lane whose costs are counted.

### Alternatives considered

| Alternative | Why not |
|---|---|
| Window-versus-interval comparison (the draft) | Invalid under repeated checking. Low power at a window of 25. Two-sided, so it would demote improvement. |
| Folding in resolution order (first implementation) | Outcome-dependent latency biases it. |
| A slack below the lower bound for the null (first implementation) | Double-counts conservatism and slows detection several-fold. |
| Demotion keyed by hypothesis id (first implementation) | Laundered by restating the claim. |
| Re-earning when a growing sample's bound crosses the threshold (first implementation) | Optional stopping. |
| Never-demoted rules fixed at the evaluation's counts (second implementation) | Contexts refused at burn-in could never earn their way back. |
| Storing demotions | Adds a write that races with outcomes. Derivation is exact and replayable. |
| Gating on causal effect | The effect can be real while the action still usually fails, for example 10% → 30%. |
| A substrate-owned random draw for probes; atomic gate-and-record tokens | Deferred. Assignment and recording are declared by the caller, as in ADR-0027. |

## Consequences

- A rule always answers to its own predictions. A cliff to near zero demotes a confident rule within about ten predictions.
- Recovery costs randomized probes, and the evidence needed to re-earn a rule is deliberately the same as minting one.
- **Selection within a context is not modelled.** If production chooses harder cases than the randomized trial did, the monitor reads that as drift and demotes. This is conservative by design.
- **Probing is a cost and must be bounded.** A flat probe rate bled utility in permanently degraded contexts in development run 1 of experiment 013. Exponential backoff, tried next, could not recover in time, so version 2 of the preregistration probes through canary-triggered bursts that collect exactly one fresh block.
- Opportunity-level utility, not failure rate among executed actions, is the measure. A gate that refuses everything scores badly (experiment 013, which includes an always-refuse arm).
- **Without confounding, the causal layer is not expected to beat a per-context detector on raw executions.** Experiment 013 only requires it not to lose to one.

## Verification

Policy unit tests:
- rules need local support and precision;
- `lowers` predicts absence;
- fresh, randomized-only, fixed-block requalification, with misses counted;
- identical claims share a lineage;
- latency cannot reorder evidence;
- a quick demotion on collapse;
- no demotion on improvement;
- the average run length at the null is at least 1000 (seeded simulation);
- epochs ignore stragglers;
- the gate fails closed.

Conformance on both stores: `predictions_are_scored_and_demote_their_rules`, which includes restating the claim as a new hypothesis.

HTTP test: `predictions_over_http`.

Experiment 013 is preregistered in [benchmarks/013-prediction-gating](../../benchmarks/013-prediction-gating/PREREGISTRATION.md).
