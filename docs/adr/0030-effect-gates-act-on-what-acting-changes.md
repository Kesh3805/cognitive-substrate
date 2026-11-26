# ADR-0030: Effect gates act on what acting changes

- Status: Accepted, experimental. Experiment 013 part B: [not adopted](../../benchmarks/013-prediction-gating/RESULTS.md). It failed every criterion; holdout cost and pooled support are the identified causes.
- Date: 2026-11-25

## Context

The gate of ADR-0029 acts when the treated arm's favourable rate is confidently high. That is a question about the action's **success rate**. It ignores what would have happened without the action.

Development runs of experiment 013 (part A) showed the consequence. Where refusing costs a flat penalty and nothing else, the causal layer adds nothing: the gate ties a per-context detector run on raw executions. Two external reviews agreed on the cause. Causal evidence has decision value only when the **untreated outcome matters**, that is, when acting has a cost and refusing still yields the control outcome. Then the right question is not "will it succeed?" but "will acting improve the outcome enough to pay for itself?".

A success-rate rule gets both directions wrong:
- it acts where the outcome happens anyway (control 85%, treated 87%), paying the cost for nothing;
- it refuses where the action rarely succeeds but helps a lot (control 15%, treated 55%).

The monitor of ADR-0029 has a second blind spot. It watches only the treated arm, so it cannot see the control rate rising until the action becomes redundant. A gate that acts collects no control outcomes at all.

## Decision

### Rules carry both arms and the effect

When the context's control arm has at least 20 outcomes, a rule also carries:
- the control arm's rate, with its Wilson interval;
- the **effect**: treated minus control favourable rate, with Newcombe's 95% interval.

Both arms accumulate, or re-earn after a demotion, by the same fixed-block rules (ADR-0029), each from its own randomized blocks.

### The gate can require an effect

`min_effect` makes the gate also require `effect.lower ≥ min_effect`.
- For an action costing `C`, where an outcome is worth `+R` and a failure `−P`, the caller sets `min_effect = C / (R + P)`.
- A threshold of 0 on the treated rate then leaves the decision to the effect alone.
- A context with no control estimate refuses (`no_effect_estimate`): the gate fails closed.

### Control holdouts are predicted and monitored

A control-arm intervention can now be predicted. Its null is the control arm's **upper** bound, and a second CUSUM e-detector on the complement tests that the control's favourable rate stays at or below it.
- Either detector demotes the lineage, and the demotion records which arm crossed.
- A rising control rate is therefore caught like a falling treated rate.
- The monitored contract per arm (treated at or above its lower bound, control at or below its upper bound) implies an effect of at least the difference of those bounds.

Holdouts are the caller's policy. The substrate monitors them when they are recorded, as it trusts declared assignment (ADR-0027).

### Alternatives considered

| Alternative | Why not |
|---|---|
| A two-sample sequential test on the effect itself | More powerful for slow drifts that shrink the effect within both arms' bounds. Rejected for this version: per-arm detectors reuse the validated detector, keep each prediction's null frozen, and catch the two failures that matter (the action stops working; the outcome starts happening anyway). Stated as a limit. |
| An absolute floor in addition to the effect | Kept optional: `threshold` still applies. It is not required, because an effect gate's purpose is to act where success is rare but valuable. |
| Substrate-scheduled holdouts | Deferred with substrate-owned randomization (ADR-0029). |

## Consequences

- Callers that value outcomes, not just successes, can gate on what acting changes.
- **Holdouts cost something.** A control holdout forgoes the action's benefit; experiment 013B charges it at its true utility.
- **A pooled hypothesis can hide harm.** Rules still need a supported hypothesis. A context where the action is mildly harmful can share a supported hypothesis, and the effect gate refuses it locally. If any context clearly establishes harm, M6.2 blocks support for the whole hypothesis, and every context is refused (ADR-0027).

## Verification

Policy unit tests:
- the effect gate refuses what happens anyway and allows rare but useful actions;
- an effect threshold without a control estimate refuses;
- rules carry both arms and requalify each from its own blocks;
- a rising control rate demotes the lineage, and a control arm behaving as predicted never does.

Conformance on both stores: `predictions_are_scored_and_demote_their_rules` (effect gate, control holdouts, control-rise demotion).

HTTP test: `predictions_over_http` (`min_effect`, and control holdouts recorded with status 201).

Experiment 013, part B, is preregistered in [benchmarks/013-prediction-gating](../../benchmarks/013-prediction-gating/PREREGISTRATION.md).
