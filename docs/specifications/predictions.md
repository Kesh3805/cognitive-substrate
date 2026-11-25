# Bounded predictions — version 1

## Rules (computed on read, never stored)

A **rule** is a supported hypothesis in one context. It predicts how often the **favourable** outcome follows the **treated** arm there: `true` for `raises`, `false` for `lowers`. When the context's **control** arm has at least 20 outcomes, the rule also carries the control arm's rate and the **effect**: the treated-minus-control favourable rate, with Newcombe's 95% interval.

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

**Which interventions.** Randomized, in the hypothesis's scope and context, of the arm concerned, and **recorded** after the demotion. Each arm has its own blocks: they are taken in intent order (`recorded_at`, then id) and cut into consecutive blocks of 20. The treated arm re-earns the rule. The control arm re-earns the effect.

**Each outcome** follows the monitor's own semantics:
- a strong-tier outcome observed within an hour of the intervention's intent;
- otherwise unfavourable, once that hour has passed;
- otherwise pending.

**The latest complete block decides.** A block with a pending outcome is not complete. There is no checking after every new outcome, so a rule cannot be re-earned by stopping when the evidence happens to look good.

Passive evidence never re-earns a rule.

### Accumulating evidence before any demotion

A never-demoted lineage counts, for each arm, the evaluation's outcomes plus **complete blocks** of 20 fresh randomized outcomes of that arm recorded after the evaluation's `as_of`. The blocks use the same rules as above. No regime change has been detected, so the evidence accumulates, and a rule that started below the threshold can rise above it. It does so only at block boundaries.

### Numbers

From the treated counts `n` and `k` favourable (taken from the evaluation plus accumulated blocks, or from the deciding re-earning block):

| Field | Value |
|---|---|
| `estimate` | `(k + ½) / (n + 1)` (Jeffreys) |
| `lower`, `upper` | Wilson 95% interval of `k / n` |
| `null` | `lower` clamped to [0.02, 0.98]: the live rate is monitored against the rule's own lower bound. The cap stops near-perfect evidence from making one or two late outcomes decisive. |
| `requalified` | the counts come from a re-earning block |
| `control` | `{ n, favourable, estimate, lower, upper }` for the control arm, when it has at least 20 outcomes |
| `effect` | `{ estimate, lower, upper }`: treated minus control, Newcombe's 95% interval, when `control` is present |

## Predictions (stored, immutable)

`POST /v1/interventions/{id}/predictions { hypothesis_id }` asks for a prediction.

**Refusals.** These are returned with status 200 and write nothing:

| Condition | Reason |
|---|---|
| The hypothesis is not supported | `not_supported` |
| Another environment, treatment or scope, or a context outside the hypothesis's declared `contexts` | `unrelated` |
| A control-arm intervention and no control estimate | `imprecise` |
| Any rule refusal | that refusal |

**Control holdouts.** A control-arm intervention is predicted from the control arm. Its `null` is the control arm's **upper** bound (clamped to [0.02, 0.98], symmetric with the treated arm), and the monitor tests that the control's favourable rate stays at or below it.

**Conflicts (409).** The outcome for the hypothesis's key is already recorded, or the intervention already has a prediction in the same lineage.

**What is recorded (status 201).** The rule's numbers, frozen, together with:
- the hypothesis id and version;
- the lineage;
- the predicted `arm` (`treated_n` is that arm's count);
- `recorded_at`;
- `deadline = recorded_at + 1 hour`.

The prediction is written atomically with a `PredictionRecorded` event. The event holds ids only.

Predictions are never evidence for a hypothesis.

### Resolution

| Condition | Resolution | At |
|---|---|---|
| A strong-tier outcome observed by the deadline | `observed { favourable }` | `observed_at` |
| Otherwise, strictly after the deadline | `missed`, which counts as unfavourable | `deadline` |
| Otherwise | pending | — |

Self-assessed outcomes never resolve a prediction. At the deadline instant itself a prediction is still pending, so a miss is only ever declared once no outcome can count, and an outcome can never turn a miss back into an observation.

## Monitor

**Folding order.** A lineage's predictions are folded in **intent order** (`recorded_at`, then id), up to the first prediction still pending.
- Later predictions wait, even when they have already resolved. That bounds the wait by the one-hour deadline.
- Folding in resolution order would let outcome-dependent latency (fast failures, slow successes) bias the detector.

**Likelihood ratio.** For each prediction with null `p0`, and each `Δ ∈ {0.1, 0.2, 0.3}`:
- the alternative is `p1 = max(p0 − Δ, p0 / 4)`;
- the ratio is `p1/p0` if the prediction was favourable, else `(1 − p1)/(1 − p0)`.

**Detector.** Each `Δ` keeps a CUSUM, `M ← max(M, 1) · LR`. The detector is the mean of the three.

**Two arms.** Treated predictions feed one detector. Control predictions feed a second one on the complement: null `1 − p0`, outcome `not favourable`, so that it tests "the control's rate is at most `p0`". A control rate that rises, making the action redundant, is caught like a treated rate that falls.

**Demotion.** The lineage is demoted when either detector reaches `1/α = 1000`. The demotion records which arm crossed. Both detectors reset.
- The demotion is dated when it became knowable: the latest resolution time in the folded prefix.
- The detector then resets, and a new epoch counts only predictions **recorded** after the demotion.

**False alarms.** Under the null (the live favourable rate is at least each prediction's `null`), the average run length to a false alarm is at least 1000 resolved predictions. This is a per-lineage average run length, not a probability of no false alarm over a horizon or across lineages.

**One-sided.** Doing better than predicted never demotes.

**Not stored.** Demotions are a deterministic function of immutable predictions and once-only outcomes. They are recomputed on every read.

The monitor reports:
- `threshold` (1000);
- every demotion (`at`, `prediction_id`, `arm`, `e_value`, and `resolved` predictions in the epoch);
- for the current epoch: `resolved`, `favourable`, `missed` and `pending` (waiting) counts, `e_value` (treated), `control_e_value`, and the mean Brier score (`brier`).

## Gate

`GET /v1/hypotheses/{id}/rule?context=&threshold=&min_effect=` returns `{ kind: "inferred", version, hypothesis_id, hypothesis_version, context, result, monitor, gate, as_of }`, where `result` is either a rule or a refusal.

With `threshold`, `gate` is `{ execute, reason, threshold, min_effect }`. `min_effect` needs `threshold`; a threshold of 0 leaves the treated rate unconstrained. A `threshold` outside [0, 1], a `min_effect` outside [−1, 1], `min_effect` without `threshold`, or a `context` that is blank or over 200 characters is a validation error (422); a missing `context` is a 400. The first row that applies decides:

| Condition | `execute` | `reason` |
|---|---|---|
| No rule | false | `no_rule`, with `refusal` |
| A rule with `lower < threshold` | false | `below_threshold` |
| `min_effect` given, and the rule has no `effect` | false | `no_effect_estimate` |
| `min_effect` given, and `effect.lower < min_effect` | false | `effect_below_threshold` |
| Otherwise | true | `clear` |

**An effect gate.** With `min_effect`, the gate acts on what acting **changes**. An action whose outcome happens anyway is refused however often it succeeds, and an action that rarely succeeds but helps a lot is allowed. For an action costing `C`, with an outcome worth `+R` and a failure `−P`, acting pays when `(R + P) · Δ > C`, so `min_effect = C / (R + P)`.

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
| The effect is monitored arm by arm | The monitored contract is "treated ≥ its lower bound and control ≤ its upper bound", which implies an effect of at least their difference. That is more conservative than the Newcombe bound the gate uses. A drift that shrinks the effect while both arms stay inside their own bounds is not detected until it crosses one of them. |
| Holdouts are the caller's policy | Without control holdouts, a rising control rate is invisible. The substrate monitors holdouts when they are recorded; it does not schedule them. |
| Reads and writes are not serialized | An outcome or prediction written concurrently with a read can land on the far side of a demotion or deadline from what the read saw. The read is consistent with the records it saw; the next read reflects the commit. The window is the duration of one write. |
