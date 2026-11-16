# 011 — Causal hypotheses under confounding

- Status: **preregistered** (committed before any run on held-out seeds 701–705)
- Date: 2026-11-16
- Concerns: M6.2, causal evaluation version 1 ([spec](../../docs/specifications/causal-evidence.md)).

## Question

When evidence comes from an agent whose choices are confounded, does the evaluation:

- support only hypotheses whose effect is real;
- find the real ones when the randomized budget is adequate;
- avoid refuting real effects;
- report intervals that cover the true effect?

The same rule applied to passive evidence should fall for the confounding.

## Evaluation (frozen)

As specified:

- **Evidence:** only randomized interventions with a strong-tier outcome count.
- **Contexts:** a context is eligible with at least 2 outcomes per arm.
- **Pooled effect:** a stratified Newcombe–Wilson interval over at least 5 eligible contexts with total weight at least 5.
- **Status:**
  - `supported` when the lower bound > `min_effect` and no eligible context's interval lies entirely below 0;
  - `refuted` when the upper bound < `min_effect`;
  - otherwise `inconclusive`;
  - without a pooled effect, `proposed`.

## Data

Every hypothesis has its own environment with 12 contexts, and states `min_effect = 0.05`.

**Outcome model:**

```
P(y) = 0.4 + effect·treated + 0.2·u
```

- `u` is a hidden confounder whose prevalence (0.2–0.8) differs by context and **varies within each context**.
- Nothing is clamped.

**Passive evidence:** 10 units per context, chosen by the agent.
- The agent treats with probability 0.85 when `u` is set and 0.15 otherwise.
- For reverse causation, it treats with probability 0.85 when the outcome is already present, and the treatment changes nothing.

**Randomized evidence:** probability 0.5, with an uneven budget per hypothesis:
- 3–12 randomized contexts;
- 2–8 outcomes per arm per context.

All outcomes are deterministic-tier.

**Types**, 40 per type per seed:

| Type | True effect per context |
|---|---|
| strong positive | +0.3 |
| near boundary | +0.12 |
| negative | −0.3, stated as `lowers` (true) or `raises` (false) at random |
| confounded null | 0 |
| reverse causation | 0 |
| context-dependent | +0.4 in a third of the contexts, 0 elsewhere |
| harmful in one | +0.3 everywhere, except −0.4 in one context |

**True effect:** the mean effect over the hypothesis's randomized contexts, in its stated direction. That is the estimator's own target, because arms are equal within a hypothesis.

**Seeds:** development 1–3; held-out 701–705.

**Store:** the in-memory store. The evaluation is a pure function, and the conformance suite requires both stores to return the same evidence and decisions.

## Measures

| Measure | Definition |
|---|---|
| **Non-cause** | true effect ≤ 0.05. Supporting one is a *false support*. |
| **Detectable cause** | true effect ≥ 0.25, with ≥ 60 randomized outcomes per arm in total |
| **False refutation** | refuting a hypothesis whose true effect > 0.05 |
| **Coverage** | share of pooled intervals that contain the true effect |
| **Naive baseline** | the same rule over the passive evidence, relabelled as randomized |

Reported, not gated:
- detection over every cause;
- how often a harmful-in-one hypothesis is supported.

## Adoption criteria (all must hold on held-out seeds)

1. False-supported rate ≤ **0.01**, with a Wilson 95% upper bound ≤ **0.02**.
2. Detectable causes supported ≥ **0.70**.
3. False-refuted rate ≤ **0.01**.
4. Interval coverage ≥ **0.90**.
5. The naive baseline is fooled: its false-supported rate ≥ **0.20**. Otherwise the environment does not exercise confounding.
6. Every evaluation is reproduced exactly by re-evaluating its own evidence.

## Disclosure: what development seeds changed

The evaluation design was reviewed before any code was written, and the review changed it:

- The planned Mantel–Haenszel and Sato-variance interval was replaced by a stratified Newcombe–Wilson interval, because Sato's variance collapses on all-0 or all-1 strata.
- A sign-consistency guard was added.
- `refuted` now means the upper bound is below the minimum effect.
- "probe" assignments were merged into `randomized`, which records its draw and propensity.
- Observational association was moved out of the status.

The first development run (seeds 1–3, 20 per type) changed the benchmark, not the evaluation:

- **Generator fix:** with a base of 0.3, the harmful context's −0.4 was clamped at 0, so its true effect was misstated. The base became 0.4 and the confounder lift 0.2, so nothing clamps.
- **Sample size:** with 152 non-causes, the gate's upper bound could not be met even at one false support. The count was doubled to 40 per type.
- **Detection range:** detection was 52% under the planned range (effect ≥ 0.15 with 5 contexts × 4 per arm). That range is underpowered by design; the evaluation's intervals are conservative.
  - The detection gate now applies to the declared range "effect ≥ 0.25 with ≥ 60 randomized outcomes per arm". There, detection was 91% in the first run.
  - Detection over every cause is reported.

On the second development run, all gates passed:

| Measure | Result |
|---|---|
| False support | 1 of 309 (upper bound 0.018) |
| Detection | 44 of 47 |
| False refutation | 3 of 531 |
| Coverage | 0.93 |
| Naive false support | 0.45 |

**These numbers are not evidence.** Only the held-out seeds decide.

## If adopted

- Hypotheses and interventions ship as specified.
- `supported` is documented as a claim about the sampled contexts, under randomized assignment declared by the caller.

If any criterion fails, the failure is reported and the status rules do not change within this experiment.
