# 013 — Prediction-gated execution under regime shifts

- Status: **done: not adopted** (version 2, committed at `5890149` before any held-out run; [results](RESULTS.md))
- Date: 2026-11-25
- Concerns: M6.4, prediction version 1 (ADR-0029, ADR-0030, [specification](../../docs/specifications/predictions.md)), over causal evaluation version 1.

Version 1 of this preregistration had one part, now **part A**. Its development runs showed that, in a world where only the treated success rate matters, the causal layer cannot add value. Version 2 therefore:
- amends part A's criteria before any held-out run, keeping the original G1 as a reported, superseded criterion;
- adds **part B**, where the untreated outcome matters.

The disclosure section records why.

## Questions

**Part A, predictive governance.** Where refusing is a flat cost, does gating on bounded predictions:
- **cost nothing** against the existing capability filters that keep healthy work;
- contain harm, detect collapses and recover;
- cost nothing against the same detector without the causal layer?

**Part B, causal decision value.** Where acting has a cost and refusing still yields the control outcome, does gating on the **effect** (ADR-0030):
- beat success-rate gating;
- keep its value when the control rate rises or the effect reverses?

**The gate is adopted as the recommended decision path only if both parts pass:** it never costs (A), and it pays where effects matter (B).

## Part A — method (frozen)

### The environment

Each run is one environment with **6 contexts** and one hypothesis ("acting raises success", `min_effect` 0.1).

**Rates.**
- Base treated success rates are drawn uniformly from 0.80–0.95.
- Control rates are 0.30–0.50 lower, floored at 0.

**Burn-in.** 25 randomized outcomes per arm and context, deterministic tier. Every arm shares it. The hypothesis is evaluated once, after the burn-in.

**Opportunities.** Then **1,500 opportunities** arrive, each in a uniformly drawn context, with one uniform draw `u`.
- Acting succeeds iff `u` is below the current treated rate.
- Every arm sees the same stream and the same draws (common random numbers), including the probe draws.

**Utility per opportunity.**

| Event | Utility |
|---|---|
| Success | +1 |
| Failure | −2 |
| Refusal | −0.25 |

Acting is worth it above a 7/12 success rate.

### Families

8 runs per family per seed. The shift is at opportunity 400.

| Family | What changes |
|---|---|
| stationary | Nothing |
| cliff | 2 contexts drop to 0.25 |
| gradual | 2 contexts fall linearly by 0.40 over opportunities 400–700 |
| recovery | 2 contexts drop to 0.25, then return to base at 700 |
| upward | 2 contexts start at 0.55 and rise to 0.90 (reported, not gated) |
| global | Every context drops to 0.40 |

The **harmful** families are cliff, gradual, recovery and global.

### Arms

**Recovery, identical in every probing arm.** It applies in a context where evidence could restore acting:
- for the gate, a demoted lineage or a rule below the threshold;
- for `per_context_cusum`, the same;
- for `per_context_probing`, a quarantined context.

There, each refused opportunity is a **canary** with probability **1/8**. A canary is a treated run that never counts towards re-earning (for the gate, a passive intervention). After **3 consecutive canary successes**, a **burst** starts: refused opportunities are probed with probability **0.5** until exactly one fresh block of **20** treated outcomes is complete, and then canaries resume. For the gate, burst probes are randomized interventions with propensity 0.9 of treatment; a control probe is scored as a refusal.

**Re-earning** uses those **fixed blocks of 20**. After a demotion or quarantine, the latest complete block's Wilson lower bound must reach **τ = 0.6**. Before any demotion, the gate and `per_context_cusum` add complete blocks to their burn-in evidence.

| Arm | Rule |
|---|---|
| `oracle` | Acts iff the true rate makes acting worth it. The upper bound. |
| `refuse` | Never acts. |
| `global` | The existing capability drift filter over all runs: 3 consecutive failures, or a window-25 Wilson upper bound below 0.6 (at least 10 runs). Quarantine is sticky. The burn-in's treated outcomes are its history. |
| `per_context` | The same filter, run separately in each context. |
| `per_context_probing` | Per context, the window rule only, with recovery. |
| `per_context_cusum` | The gate's own detector and null, without the causal layer. Per context, it acts while not demoted and the lower bound of its current evidence is ≥ τ, with recovery. |
| `gate` | The M6.4 gate at τ = 0.6, through the real engine. When it clears, a passive treated intervention and a prediction are recorded, and the outcome is observed one minute later. |

### Part A criteria (all binding ones must hold on held-out seeds 901–905)

Comparisons are paired by run, with bootstrap percentile intervals (10,000 resamples, seed 13). Each gate's level is Bonferroni-adjusted over its own comparisons.

The **existing arms** are `refuse`, `global`, `per_context` and `per_context_probing`. An existing arm is **admissible** when its mean stationary utility is within **0.02** of the oracle's. That excludes an arm that buys harm containment by giving up healthy work. 0.02 is the same margin G2 and G6 use.

| Gate | Binding | Criterion |
|---|---|---|
| **G1 (v1)** | No (superseded, reported) | In each harmful family, the gate beats each existing arm: CI lower bound **> 0** (16 comparisons). |
| **G1** | Yes | In **each** harmful family, the gate is non-inferior to **each admissible** existing arm: CI lower bound **> −0.02**. |
| **G2** | Yes | Stationary: the gate is non-inferior to each existing arm: CI lower bound **> −0.02** (4 comparisons). |
| **G3** | Yes | Stationary: at most **5%** of runs have any demotion. The Wilson upper bound is reported. |
| **G4** | Yes | Cliff and global: at least **95%** of exposed shifted contexts are demoted before the end. Median in-context delay **≤ 20** (cliff) and **≤ 40** (global). Exposed and censoring as in version 1; the p90 is reported. |
| **G5** | Yes | Recovery: at least **80%** of exposed returned contexts are acted on again from a re-earned rule after opportunity 700. |
| **G6** | Yes | In each family, the gate is non-inferior to `per_context_cusum`: CI lower bound **> −0.02** (6 comparisons). |

**G4's global target** comes from Wald's approximation, not from development results: `ln(1/α) / KL(0.40 ‖ 0.70) ≈ 36` opportunities for a typical null of 0.70, rounded up to 40. A median of 20 at a 0.40 success rate is incompatible with α = 0.001.

## Part B — method (frozen)

### The environment

Each run has **6 contexts**, one of each kind below and two benefit contexts. Both rates get uniform jitter of ±0.03.

| Kind | Control | Treated | Worth acting |
|---|---|---|---|
| benefit (×2) | 0.35 | 0.85 | yes |
| low success, useful | 0.15 | 0.55 | yes |
| happens anyway | 0.85 | 0.87 | no |
| mild backfire | 0.80 | 0.72 | no |
| insufficient | 0.60 | 0.65 | no |

**Utility.** An outcome is worth +1 and a failure −2. Acting yields the treated outcome minus a cost of **0.3**. Not acting yields the control outcome. Acting is worth it exactly when `3 · (p_treated − p_control) > 0.3`, so the effect threshold is **`min_effect` = 0.1**.

**Burn-in.** 40 randomized outcomes per arm and context, recorded identically in both gates' environments (each gate has its own environment, so its probes never feed the other). Each environment's hypothesis ("acting raises success", `min_effect` 0.05) is evaluated once.

**Opportunities.** 1,200, each in a uniformly drawn context, with independent uniform draws for the treated outcome, the control outcome and the probe. Common random numbers across arms.

### Families

8 runs per family per seed. The shift is at opportunity 400, in the two benefit contexts.

| Family | What changes |
|---|---|
| stationary | Nothing |
| control rise | The control rate rises to 0.85: acting becomes useless while still "succeeding" |
| effect reversal | The treated rate falls to 0.35 |

### Arms

| Arm | Rule |
|---|---|
| `oracle` | Acts iff acting is worth it at the true rates. |
| `refuse` | Never acts. |
| `always_act` | Always acts. |
| `success_filter` | Per context, the window rule on treated success (window 25, at least 10 runs, Wilson upper below 0.6 quarantines, sticky), from the burn-in. |
| `absolute_gate` | The M6.4 gate at τ = 0.6, without `min_effect`. No probing. |
| `effect_baseline` | Acts where the burn-in's Newcombe lower bound of the effect is ≥ 0.1. Never updates. |
| `effect_gate` | The M6.4 gate with threshold 0 and `min_effect` 0.1, through the real engine. On a cleared opportunity, it runs a randomized **holdout** (treated or control at 0.5) with probability **0.8** (chosen on development seeds by the rule in the disclosure; originally 0.1), else a passive treated action, and predicts it either way. On a refused opportunity, it runs a randomized probe (treated or control at 0.5) with probability **0.25** if the lineage is demoted, or **0.05** if a rule exists but its effect is short. Every randomized run is scored at its true utility. |

### Part B criteria (all must hold on held-out seeds 911–915)

| Gate | Criterion |
|---|---|
| **B1** | Stationary: `effect_gate` beats `always_act`, `refuse`, `success_filter` and `absolute_gate`: CI lower bound **> 0** (Bonferroni over 4). |
| **B2** | Every family: `effect_gate` is non-inferior to `effect_baseline`: CI lower bound **> −0.02** (Bonferroni over 3). |
| **B3** | Control rise: `effect_gate` beats `effect_baseline` and `absolute_gate` (Bonferroni over 2). |
| **B4** | Effect reversal: `effect_gate` beats `effect_baseline`. |
| **B5** | Control rise: at least **90%** of exposed shifted contexts are demoted by the end. |

Reported but not gated: every arm's utility per family, and how many runs had both hypotheses supported.

## What this cannot show

- **Confounded observational evidence.** Neither part has an agent whose own choices are confounded. Part B tests the value of the effect, not M6.2's protection against confounding (experiment 011 covers that).
- **False alarms near the boundary.** In part A's stationary family the true rates sit well above each rule's null. False-alarm control at the null boundary is covered by a seeded policy test (average run length ≥ 1000).
- **Slow effect drifts within both arms' bounds.** Part B's shifts each move one arm across its bound (ADR-0030, stated limit).

## If adopted

The gate, with an effect threshold where outcomes have value, becomes the recommended decision path for actions with a supported hypothesis. If any binding criterion fails, the failure is reported, the mechanism stays available but not recommended, and it is not revised within this experiment.

## Disclosure

The design was reviewed externally three times before any held-out run, by two independent reviewers each time.

**Review of the draft.** It replaced:
- the rolling window-versus-interval ratchet, with the e-detector;
- the fall-through on "no rule", with fail-closed refusal;
- two-outcome rules, with a local support and precision requirement.

It added misses as failures, lineage-level demotion, fresh-randomized requalification, and the per-context and probing baselines.

**Review of the first implementation.** It replaced:
- resolution-order folding, with intent order up to the first pending prediction;
- the 0.05 slack below the lower bound, with the lower bound itself;
- hypothesis-id lineages, with claim-keyed lineages;
- requalification by a growing sample, with fixed blocks that count misses.

It also brought these changes to the experiment:
- probing continues while a re-earned rule is below the threshold;
- the gate's probes are 90% treated;
- the always-refuse and per-context CUSUM arms;
- per-family, every-baseline gates (G1, G2 and G6);
- 1,500 opportunities instead of 1,200.

**Development runs.** A first development run of the pre-review implementation was stopped before completion, and its results were discarded unread.

**Development run 1 (seeds 1–3, the reviewed implementation).** It failed G1, G2 and G4, and passed G3 (0/24 stationary runs with a false demotion), G5 (42/48) and G6.

| Family | Gate | Best existing arm | Gate − per-context CUSUM |
|---|---|---|---|
| Stationary | 0.547 | 0.661 (`per_context_probing`) | +0.000 |
| Cliff | 0.260 | 0.291 (`per_context`) | +0.012 |
| Gradual | 0.336 | 0.388 (`per_context_probing`) | +0.000 |
| Recovery | 0.379 | 0.468 (`per_context_probing`) | −0.005 |
| Global | −0.219 | −0.041 (`per_context`) | +0.016 |

Three mechanisms explained the failures. None of them was the causal layer, which tied the CUSUM baseline everywhere.

1. **Burn-in trap.** Healthy contexts refused at burn-in never received new evidence.
2. **Probe bleed.** A flat 0.5 probe rate in permanently degraded contexts cost more than refusing.
3. **Measurement.** G4 counted contexts the gate was never acting in.

**Revision before development run 2:**
- accumulating blocks;
- probing below-threshold rules;
- exponential backoff;
- exposed-context G4 and G5.

**Development run 2 (seeds 1–3).** It passed G2, G3 (0/24) and G6, and failed G1, G4 and G5.

| Family | Gate | `per_context_probing` | `per_context` | Per-context CUSUM | Oracle |
|---|---|---|---|---|---|
| Stationary | 0.649 | 0.661 | 0.555 | 0.653 | 0.661 |
| Cliff | 0.337 | 0.339 | 0.291 | 0.335 | 0.427 |
| Gradual | 0.382 | 0.391 | 0.293 | 0.380 | 0.425 |
| Recovery | 0.392 | 0.407 | 0.246 | 0.394 | 0.553 |
| Global | −0.174 | −0.157 | −0.041 | −0.175 | −0.008 |

**Review of development run 2, and amendment to version 2.** Both reviewers concluded the following.

- **G1 (v1) is infeasible, not merely failed.** The three-consecutive-failures filters win the global family only by giving up about 0.1 per opportunity of healthy work in stationary. No policy that passes G2 can beat them there. G1 (v1) is therefore kept, reported and superseded. The amended G1 compares only against admissible arms, with the margin G2 and G6 already use. As one reviewer cautioned, this is a development-informed amendment, not a confirmatory finding, and it is disclosed as such.
- **G4's global target of 20 is incompatible with α = 0.001.** Wald's bound gives about 36. The threshold is not lowered to pass. The target is restated from the bound.
- **Backoff cannot recover in time.** Halving to 1/32 leaves about one treated probe per context after the return. A floor of 1/8 still leaves about five. Recovery is replaced by canary-triggered bursts that collect exactly one fresh block, identically in every probing arm. Canary runs never count towards re-earning.
- **Without a meaningful untreated outcome, the causal layer has nothing to show.** Part B and the effect gate (ADR-0030) were added, with a static randomized-effect baseline so that beating success-rate filters is not mistaken for the value of the substrate.

Thresholds τ, α, block size and utilities were not changed.

**A seeding defect affected every development run above.** Each run's generator seed XORed the seed number and the run index into the same low bits. Seeds 1–3 crossed with runs 0–7 gave the same eight values, so every development run had 8 distinct runs per family, each counted three times. The held-out splits collided the same way.
- The means reported above are the means of those 8 runs. Their confidence intervals, and every gate decided from them, are too narrow and are not evidence.
- The seed is now built from disjoint bits, and a test checks that every (seed, family, run) of both splits gets its own generator.

**Development run 3 of part A and development run 1 of part B** ran before the fix. Their verdicts are recorded only for the record, with the defect above.
- Part A passed G3, G4 (cliff median 17, global median 27) and G5 (42/45), and failed G1 and G2: the gate sat about 0.03 below `per_context_probing` in stationary, cliff, gradual and recovery.
- Part B: the effect gate beat the success filter (+0.155) and the absolute gate (+0.172) in stationary, and beat the static effect baseline under effect reversal (B4). It failed B1 (against always-act, after correction), B2 (−0.04 against the static baseline), B3, and B5 (0/39).

**B5 and the holdout rate.** A holdout of 0.1 gives about 7 control observations per benefit context after the shift. A seeded simulation of the control detector shows a median of 28, and a p90 of 52, observations to detect a control rise from 0.35 to 0.85. So B5 could not pass at 0.1.

The holdout rate is chosen on the development seeds, by a rule fixed **before** seeing those results: the smallest of 0.3, 0.5 and 0.8 that passes B5 becomes `HOLDOUT`. If none passes, 0.8 is used and B5 is expected to fail.

**Development runs with corrected seeding (seeds 1–3, 24 distinct runs per family).** These are the first development results that are evidence. Results are in `results/dev-memory` (part A) and `results/b-dev-memory-holdout-{0.3,0.5,0.8}` (part B).

*Part A* passed G3 (0/24), G4 and G5. It failed G1, G2 and G6.

| Family | Gate | `per_context_probing` | Gate − `per_context_probing` (95% CI) |
|---|---|---|---|
| Stationary | 0.618 | 0.637 | −0.019 [−0.037, −0.006] |
| Cliff | 0.362 | 0.384 | −0.021 [−0.037, −0.009] |
| Gradual | 0.389 | 0.410 | −0.020 [−0.038, −0.007] |
| Recovery | 0.453 | 0.489 | −0.036 [−0.062, −0.017] |
| Global | −0.181 | −0.146 | −0.035 [−0.049, −0.022] |

- **G4.** Cliff: 42/42 exposed contexts detected, median 17, p90 31. Global: 134/134, median 27, p90 64.
- **G5.** 34/42 re-earned.
- **G6.** Fails only in recovery: −0.0135, CI lower −0.031.

With correct replication, the gate is about 0.02 per opportunity **below** the strongest per-context heuristic in every family. That is just past the 0.02 margin.

*Part B* failed every criterion at every candidate holdout.

| Holdout | B5 detected | Effect gate − static effect baseline: stationary / control rise / reversal | Effect gate − success filter (stationary) |
|---|---|---|---|
| 0.3 | 10/39 | −0.137 / −0.044 / −0.071 | +0.034 [−0.042, +0.101] |
| 0.5 | 22/39 | −0.181 / −0.053 / −0.087 | −0.011 |
| 0.8 | 33/39 | −0.244 / −0.060 / −0.111 | −0.074 |

- **Holdouts cost more than they buy.** Every control holdout in a benefit context forgoes about 1.2 utility. Detecting a control rise needs a median of 28 control observations, and a p90 of 52.
- **Hypothesis support gates everything.** After the burn-in, the hypothesis was supported in only 21/24 (stationary), 20/24 (control rise) and 16/24 (effect reversal) runs. Without support the gate refuses every context, while the static effect baseline needs no support. The mildly harmful context weakens or blocks the pooled claim (ADR-0027, as ADR-0030 anticipated).
- **The earlier, pseudo-replicated advantage over the success filter does not survive.** At a holdout of 0.3 it is +0.034 with a CI crossing 0; at higher holdouts it reverses.

**By the rule fixed before these results, no candidate passed B5, so `HOLDOUT` = 0.8.** B5 is expected to fail on held-out seeds, and on development evidence so are B1–B4.

**Where this leaves M6.4.** On development evidence, the gate is a sound and well-tested governance mechanism. It is sequentially valid, fails closed, resists laundering and recovers. But it does not improve decisions over simpler per-context baselines in either part. Running the held-out seeds now would be expected to confirm "not adopted". Whether to run them as preregistered, or to treat these findings as the result and redesign, is decided outside this document. Constants are not changed after held-out results exist.
