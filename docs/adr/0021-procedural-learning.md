# ADR-0021: Procedural learning with honest evaluation

- Status: Accepted
- Date: 2026-11-14

## Context

A procedure that "worked" is easy to over-trust:

- A clean exit says nothing about whether the goal was achieved.
- A verifier that shares the actor's assumptions confirms its blind spots.
- Repeating one input many times inflates the sample size.
- Runs that abort or never report back quietly drop out of the denominator.
- A long success record reacts too slowly when the environment changes today.
- Suites written by whoever proposes the procedure get overfitted.

## Decision

### Capabilities are procedural memories

A capability is a procedural memory. Its lifecycle stage lives in the procedure and changes by revision (history preserved, evented as `CapabilityStageChanged`):

```
observed → candidate → evaluated → trusted
              ↑   ↘         ↘          ↘
              └── quarantined (by evidence or by hand; reinstated as candidate only)
any → deprecated (terminal)
```

Moving to `evaluated` or `trusted` requires the current evaluation to support it. Moving to `observed` is never allowed. Manual demotions are allowed. Evidence is tied to the **procedure fingerprint**: a hash of the name, preconditions, steps and expected outcome. It carries across revisions that only change the stage and resets when the procedure itself changes.

### Execution status is not outcome

- An **execution** is recorded **at intent** (`started`) with an opaque input class and an optional evaluation case. It is finished once, as `completed` or `aborted` with a reason.
- A **verification** is a separate record with a verdict and an **oracle tier**:
  - `deterministic`: tests, invariant or schema checks, external receipts, human sign-off;
  - `independent`: a different checker asserting over observed state;
  - `self_assessed`: the actor, or a model sharing its assumptions.
- An execution's outcome follows these rules:
  - An abort is a failure.
  - A run with no result after the timeout (1 hour) is a failure.
  - Among verifications, the strongest tier present decides, and a failure at that tier wins.
  - **Self-assessment can establish failure, never success.** A completed run with only self-assessed passes is *unverified*.

### Statistics that cannot be gamed by repetition

Field evidence is every execution not bound to a case, on the current procedure. The success rate is computed over **input classes**: each class contributes its own success rate once. The Wilson lower bound (stated `z = 1.96`) must clear a threshold with a minimum number of classes:

| Stage | Classes | Wilson lower bound |
|---|---|---|
| evaluated | ≥ 10 | ≥ 0.6 |
| trusted | ≥ 30 | ≥ 0.8 |

Forty passes on one input are one class. Aborts and timeouts stay in the denominator.

### Suites gate promotion, and failures ratchet

- Suites have positive, negative, boundary and regression cases. Each of positive, negative and boundary needs at least one case.
- Every required-kind case and every **locked** case must pass on the current procedure. "Pass" means the latest decisive run bound to the case succeeded. For a negative case, refusing on a failed precondition also counts as a pass.
- **Failure ratchet:** a completed field run that fails a deterministic or independent verification atomically creates a locked regression case for its input class (one per class). Any later version must pass it before promotion.

### Drift quarantines automatically

Separately from the lifetime record, the engine watches the most recent field runs. After each finish and each verification it quarantines a `candidate`, `evaluated` or `trusted` capability when either of these holds:

- **3 consecutive failures**;
- **among the last 25 runs** (once there are at least 10), the Wilson upper bound is below 0.6, meaning we are confident the capability is failing.

Quarantine is not deprecation. A quarantined capability can be reinstated as a `candidate` and must earn its evaluation again.

### Privacy

Inputs are represented only by an opaque class chosen by the client, never raw input. Executions, verifications and cases are deleted when their capability is purged.

## Consequences

- Automatic quarantine runs after each finish or verification. A run that simply times out shows up in the evaluation, but it does not quarantine anything until the next recorded event.
- Case executions gate promotion but are excluded from field statistics, so a suite cannot substitute for real-world evidence.

## Verification

Policy unit tests:

- Wilson reference values;
- repetition does not inflate evidence;
- suites gate eligibility;
- aborts count and trigger drift;
- a failing locked case blocks promotion.

Conformance on both stores:

- `capabilities_earn_evaluation_with_independent_evidence`;
- `self_assessment_never_promotes`;
- `failures_ratchet_into_locked_regressions`;
- `drift_quarantines_and_aborts_count` (also covers timeouts, terminal deprecation, scope isolation and purge).

HTTP test: `capabilities_over_http`.
