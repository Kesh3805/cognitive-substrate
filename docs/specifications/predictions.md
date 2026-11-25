# Bounded predictions — version 1

## Rules (computed on read, never stored)

A **rule** is a supported hypothesis in one context. It predicts how often the **favourable** outcome follows the **treated** arm there: `true` for `raises`, `false` for `lowers`.

### Lineage

A rule's **lineage** is the claim it makes: the SHA-256 of `(scope, environment, treatment, outcome, direction, context)`.
- Every hypothesis that states the same claim in the same scope shares the lineage, and with it the lineage's predictions and demotions.
- Neither a new hypothesis version nor a new hypothesis id clears a demotion.

### Order of checks

The first check that applies decides:

| # | Condition | Result |
|---|---|---|
| 1 | The hypothesis is not `supported` | `not_supported` |
| 2 | The context is not in the evaluation's `scope_contexts` | `out_of_scope` |
| 3 | The context's own effect estimate is not > 0 | `locally_unsupported` |
| 4 | The lineage was demoted, and no complete re-earning block exists | `demoted` |
| 5 | The lineage was never demoted, and the context has fewer than 20 treated outcomes | `imprecise` |
| 6 | Otherwise | a rule |

### Re-earning a demoted lineage

After a demotion, the rule is re-earned from **fixed blocks** of 20 interventions.

**Which interventions.** Randomized, treated, in the hypothesis's scope and context, and **recorded** after the demotion. They are taken in intent order (`recorded_at`, then id) and cut into consecutive blocks of 20.

**Each outcome** follows the monitor's own semantics:
- a strong-tier outcome observed within an hour of the intervention's intent;
- otherwise unfavourable, once that hour has passed;
- otherwise pending.

**The latest complete block decides.** A block with a pending outcome is not complete. There is no checking after every new outcome, so a rule cannot be re-earned by stopping when the evidence happens to look good.

Passive evidence never re-earns a rule.

### Accumulating evidence before any demotion

A never-demoted lineage counts the evaluation's treated outcomes, plus **complete blocks** of 20 fresh randomized treated outcomes recorded after the evaluation's `as_of`. The blocks use the same rules as above. No regime change has been detected, so the evidence accumulates, and a rule that started below the threshold can rise above it. It does so only at block boundaries.

### Numbers

From the treated counts `n` and `k` favourable (taken from the evaluation plus accumulated blocks, or from the deciding re-earning block):

| Field | Value |
|---|---|
| `estimate` | `(k + ½) / (n + 1)` (Jeffreys) |
| `lower`, `upper` | Wilson 95% interval of `k / n` |
| `null` | `max(lower, 0.02)`: the live rate is monitored against the rule's own lower bound |
| `requalified` | the counts come from a re-earning block |

## Predictions (stored, immutable)

`POST /v1/interventions/{id}/predictions { hypothesis_id }` asks for a prediction.

**Refusals.** These are returned with status 200 and write nothing:

| Condition | Reason |
|---|---|
| The hypothesis is not supported | `not_supported` |
| Another environment, treatment or scope | `unrelated` |
| A control-arm intervention | `not_treated` |
| Any rule refusal | that refusal |

**Conflicts.** The outcome for the hypothesis's key is already recorded, or the intervention already has a prediction in the same lineage.

**What is recorded (status 201).** The rule's numbers, frozen, together with:
- the hypothesis id and version;
- the lineage;
- `recorded_at`;
- `deadline = recorded_at + 1 hour`.

The prediction is written atomically with a `PredictionRecorded` event. The event holds ids only.

Predictions are never evidence for a hypothesis.

### Resolution

| Condition | Resolution | At |
|---|---|---|
| A strong-tier outcome observed by the deadline | `observed { favourable }` | `observed_at` |
| Otherwise, once the deadline has passed | `missed`, which counts as unfavourable | `deadline` |
| Otherwise | pending | — |

Self-assessed outcomes never resolve a prediction. An outcome that arrives after the deadline does not change a miss.

## Monitor

**Folding order.** A lineage's predictions are folded in **intent order** (`recorded_at`, then id), up to the first prediction still pending.
- Later predictions wait, even when they have already resolved. That bounds the wait by the one-hour deadline.
- Folding in resolution order would let outcome-dependent latency (fast failures, slow successes) bias the detector.

**Likelihood ratio.** For each prediction with null `p0`, and each `Δ ∈ {0.1, 0.2, 0.3}`:
- the alternative is `p1 = max(p0 − Δ, p0 / 4)`;
- the ratio is `p1/p0` if the prediction was favourable, else `(1 − p1)/(1 − p0)`.

**Detector.** Each `Δ` keeps a CUSUM, `M ← max(M, 1) · LR`. The detector is the mean of the three.

**Demotion.** The lineage is demoted when the detector reaches `1/α = 1000`.
- The demotion is dated when it became knowable: the latest resolution time in the folded prefix.
- The detector then resets, and a new epoch counts only predictions **recorded** after the demotion.

**False alarms.** Under the null (the live favourable rate is at least each prediction's `null`), the average run length to a false alarm is at least 1000 resolved predictions. This is a per-lineage average run length, not a probability of no false alarm over a horizon or across lineages.

**One-sided.** Doing better than predicted never demotes.

**Not stored.** Demotions are a deterministic function of immutable predictions and once-only outcomes. They are recomputed on every read.

The monitor reports:
- every demotion (`at`, the prediction, the e-value, and resolved predictions in the epoch);
- for the current epoch: resolved, favourable, missed and waiting counts, the e-value, and the mean Brier score.

## Gate

`GET /v1/hypotheses/{id}/rule?context=&threshold=` returns `{ kind: "inferred", version, hypothesis_id, hypothesis_version, context, result, monitor, gate, as_of }`, where `result` is either a rule or a refusal.

With `threshold`, `gate` is `{ execute, reason, threshold }`:

| Condition | `execute` | `reason` |
|---|---|---|
| A rule with `lower ≥ threshold` | true | `clear` |
| A rule with `lower < threshold` | false | `below_threshold` |
| No rule | false | `no_rule`, with `refusal` |

**The gate fails closed.** No rule, including a demoted rule, never means "act".

## Events

| Event | Payload |
|---|---|
| `PredictionRecorded` | prediction, intervention and hypothesis ids, hypothesis version, `requalified` |

## Known limits (version 1)

| Limit | Consequence |
|---|---|
| Accumulating blocks are looked at once per block | A context just below the threshold gets one chance per block to cross it by noise. That is far fewer looks than checking after every outcome, but it is not none. |
| Callers declare assignment and record predictions | The substrate cannot stop an agent from acting off the record. As with ADR-0027, it keeps the record for audit. |
| Contexts are opaque | Padding a context with easy cases dilutes the monitor. Context taxonomy is the caller's responsibility. |
