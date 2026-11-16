# ADR-0027: Causal hypotheses are decided by randomized evidence only

- Status: Accepted
- Date: 2026-11-16

## Context

An agent that acts and observes accumulates evidence that looks causal but usually is not:

- It chooses actions for reasons that also drive the outcome (confounding).
- It acts because the outcome is already there (reverse causation).
- It repeats the same situation many times (pseudoreplication).

A memory system that turns such co-occurrences into "X causes Y" will confidently recommend actions that do nothing, or that do harm.

## Decision

### Hypotheses are stated, not discovered

A **hypothesis** is created by a caller, never generated. It says that applying a treatment `raises` or `lowers` a binary outcome, in a declared environment, by at least `min_effect`, optionally within declared contexts.

- It is a versioned record, kept apart from beliefs.
- Its status changes only by evaluation or by a person retiring it.
- Every version is kept, each with the evaluation that set it.

### Interventions declare their assignment at intent

An **intervention** is written before its outcome is known. Its assignment is one of:

- **`randomized`:** it must record the draw and the propensity;
- **`passive`:** the agent's own choice.

Each outcome is recorded once per key, with an oracle tier. Self-assessed outcomes never count.

### Only randomized evidence decides

1. Each context is a stratum; it is eligible with at least 2 outcomes per arm.
2. The pooled effect is a stratified Newcombe–Wilson interval over at least 5 eligible contexts with Mantel–Haenszel weight at least 5. Unlike a Wald (Sato) variance, it does not collapse on all-0 or all-1 strata.
3. The status follows from the interval:

| Condition | Status |
|---|---|
| No pooled effect | `proposed` |
| Lower bound > `min_effect`, and no eligible context establishes the opposite effect | `supported` |
| Upper bound < `min_effect` | `refuted` |
| Otherwise | `inconclusive` |

- A decision speaks only for the contexts sampled (`scope_contexts`).
- Passive evidence is reported as an **observational** lift, to choose what to randomize next. It never sets the status.

### Alternatives considered

- **Mantel–Haenszel with Sato's variance:** collapses on sparse, extreme strata.
- **Treating "probe" schedules as causal:** a schedule invites assignment that depends on the state.
- **An `associated` status:** observational association is not a weaker form of causal support. It is a separate report.

## Evidence

[Experiment 011](../../benchmarks/011-causal-evidence/RESULTS.md) ran on held-out seeds, with 1,400 hypotheses under within-context confounding and reverse causation:

| Measure | Result |
|---|---|
| False support on non-causes | 0.4% (upper bound 1.4%) |
| Detectable causes supported | 97% |
| Causes wrongly refuted | 0.3% |
| 95% intervals covering the true effect | 95% |
| Naive baseline (the same rule on passive evidence) | 45% false support, including every reverse-causation hypothesis |

## Consequences

- The system can hold bounded causal claims that later reasoning (M6.3 counterfactuals, M6.4 predictions) may rely on, scoped to where they were tested.
- **Support is not safety.** Harm in a context that was not sampled stays invisible: 23.5% of harmful-in-one hypotheses were supported in experiment 011. Callers must read `scope_contexts`.
- **The system trusts the declared assignment.** It records the evidence for audit but cannot verify it.
- **Evaluation is conservative.** Small budgets leave hypotheses `inconclusive` or `proposed`; only 28% of all causes were supported. Repeated evaluation as evidence accumulates (optional stopping) is not yet measured.
