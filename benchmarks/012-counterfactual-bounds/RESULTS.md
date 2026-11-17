# 012 — Results (held-out seeds)

- **Run:** held-out seeds 801–805, with 500 hypotheses (5 families × 100). Counterfactuals were asked of the 374 that M6.2 supported, giving 5,965 answered bounds and 2,000 refusal probes. In-memory store.
- **Data:** in [`results/held-out-memory/`](results/held-out-memory/): answers in `raw.jsonl`, refusals in `probes.jsonl`, plus `summary.json` and `manifest.json`.

## Verdict

**Not adopted: the sharpness gate failed** (0.779 against a gate of ≥ 0.80). Every safety criterion passed:

- the bounds contained the truth;
- every request that should be refused was refused for the right reason;
- nothing was written.

The counterfactual endpoint stays available, documented as **experimental**. The method is not revised within this experiment.

## Gates

| # | Criterion | Value | Pass |
|---|---|---|---|
| 1 | PN bounds contain the true PN ≥ 0.95 | 2,970 / 2,973 = 0.999 | ✅ |
| 2 | PS bounds contain the true PS ≥ 0.95 | 2,987 / 2,992 = 0.998 | ✅ |
| 3 | Sharpness: lower bound > 0 in well-powered contexts ≥ 0.80 | 1,958 / 2,515 = **0.779** | ❌ |
| 4 | Every probe refused with the expected reason | 2,000 / 2,000 | ✅ |
| 5 | Computing counterfactuals writes nothing | yes | ✅ |

## Coverage by family

| Family | Supported | PN coverage | PS coverage |
|---|---|---|---|
| monotone | 98 / 100 | 781 / 784 (0.996) | 781 / 784 (0.996) |
| defiers (some units harmed) | 96 / 100 | 766 / 766 (1.000) | 768 / 768 (1.000) |
| rare outcome | 80 / 100 | 623 / 623 (1.000) | 638 / 640 (0.997) |
| boundary | 100 / 100 | 800 / 800 (1.000) | 800 / 800 (1.000) |
| null (no average effect) | 0 / 100 | — | — |

No family fell below 0.95, including the families that M6.2 supported partly by chance.

## Where the bounds say nothing

Share of well-powered contexts with a lower bound > 0:

| Family | 25 per arm | 50 per arm |
|---|---|---|
| boundary | 1.00 | 1.00 |
| monotone | 0.65 | 0.91 |
| defiers | 0.68 | 0.90 |
| rare outcome | **0.17** | 0.64 |

Across every answered bound, 57% were informative (not the vacuous [0, 1]).

## What this shows

- **The bounds are safe and conservative.** With two one-sided 97.5% limits, coverage of the true value was 99.8–99.9%, well above the 95% target. That holds when units are harmed (defiers), where the true PN sits above what monotonicity would give.
- **They are not yet useful at the sample sizes M6.2 supports with.**
  - At 25 outcomes per arm, a third of the monotone and defier contexts, and five in six rare-outcome contexts, give a lower bound of 0.
  - Information arrives at about 50 per arm, except for rare outcomes.
  - Tian–Pearl bounds divide by a proportion, so a rare outcome makes them wide whatever the interval method.
- **The development run overstated sharpness** (0.82 against 0.78 held out). Its margin over the gate was small. Sharpness, like coverage, depends on the mix of sample sizes.

## Limits

- One structural model with independent units, binary treatment and outcome, and equal arms. Unbalanced arms and units one short of all-success (near-boundary) were not stressed.
- Coverage is measured, not guaranteed. Katz's method is an approximation.

## What would change the verdict

A new, preregistered experiment, with either:

- **a tighter interval:** for example, a score-based ratio interval, or bounds that use the joint distribution rather than per-bound limits;
- **or a sharpness gate tied to sample size:** for example, declaring counterfactuals informative only from 50 per arm.

Re-running this one with a tuned method would not.
