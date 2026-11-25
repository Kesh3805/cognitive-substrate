# 013 — Prediction-gated execution under regime shifts

- Status: **preregistered** (to be committed before any run on held-out seeds 901–905)
- Date: 2026-11-25
- Concerns: M6.4, prediction version 1 (ADR-0029, [specification](../../docs/specifications/predictions.md)), over causal evaluation version 1.

## Question

Does gating actions on bounded predictions do better than the existing capability reliability filter and its strengthened per-context variants?

We measure five things:
- whether it **contains harm** when a regime shifts;
- whether it **leaves healthy work alone** when nothing shifts;
- whether it **detects** collapses quickly;
- whether it **recovers** when the world returns;
- whether its causal layer **costs** anything against the same detector run on raw executions.

## Method (frozen)

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

**Probing.** Where an arm probes, it does so per refused opportunity in a context where evidence could restore acting:
- for the gate, a demoted lineage or a rule below the threshold;
- for `per_context_cusum`, the same;
- for `per_context_probing`, a quarantined context.

The probe rate starts at **0.5** and halves after each complete block of 20 treated probes that does not restore acting, at most four times (down to 1/32).

**Re-earning** uses **fixed blocks of 20** treated outcomes. After a demotion or quarantine, the latest complete block's Wilson lower bound must reach **τ = 0.6**. Before any demotion, the gate and `per_context_cusum` add complete blocks to their burn-in evidence.

| Arm | Rule |
|---|---|
| `oracle` | Acts iff the true rate makes acting worth it. The upper bound. |
| `refuse` | Never acts. |
| `global` | The existing capability drift filter over all runs: 3 consecutive failures, or a window-25 Wilson upper bound below 0.6 (at least 10 runs). Quarantine is sticky. The burn-in's treated outcomes are its history. |
| `per_context` | The same filter, run separately in each context. |
| `per_context_probing` | Per context, the window rule only. Probes are treated runs (a non-causal filter needs no control), and it re-earns from fixed blocks. |
| `per_context_cusum` | The gate's own detector and null, without the causal layer. Per context, it acts while not demoted and the lower bound of its current evidence is ≥ τ. That evidence is the burn-in, then the latest re-earning block. Probes are treated, and it re-earns from fixed blocks. |
| `gate` | The M6.4 gate at τ = 0.6, through the real engine. When it clears, a passive treated intervention and a prediction are recorded, and the outcome is observed one minute later. A probe is a randomized intervention with propensity **0.9** of treatment: a treated probe is scored as an action, a control probe as a refusal. Other refusals do nothing. |

## Adoption criteria (all must hold on held-out seeds 901–905)

Comparisons are paired by run, with bootstrap percentile intervals (10,000 resamples, seed 13). Each gate's level is Bonferroni-adjusted over its own comparisons.

The **existing arms** are `refuse`, `global`, `per_context` and `per_context_probing`.

| Gate | Criterion |
|---|---|
| **G1** | In **each** harmful family, the gate beats **each** existing arm: utility difference CI lower bound **> 0** (16 comparisons). |
| **G2** | Stationary: the gate is non-inferior to each existing arm: CI lower bound **> −0.02** (4 comparisons). |
| **G3** | Stationary: at most **5%** of runs have any demotion. The Wilson upper bound of that rate is reported. |
| **G4** | Cliff and global: at least **95%** of **exposed** shifted contexts are demoted before the end, and the median in-context delay from the shift is **≤ 20** opportunities in each family. A context is exposed when the gate's last decision there before the shift was to act; a context the gate was not acting in has nothing to detect. Undetected contexts count as never detected, and the p90 is reported. |
| **G5** | Recovery: at least **80%** of exposed returned contexts are acted on again from a re-earned rule after opportunity 700. |
| **G6** | In each family, the gate is non-inferior to `per_context_cusum`: CI lower bound **> −0.02** (6 comparisons). |

Reported but not gated:
- every arm's utility per family;
- the upward family;
- false demotions in unshifted contexts in every family;
- how many hypotheses were supported after the burn-in.

## What this cannot show

- **The value of the causal layer.** There is no confounding and no arm choice here. The causal layer is expected to tie with a per-context detector on raw executions (G6), not beat it. Testing what M6.2's randomized evidence adds over observational filters needs a confounded family. That is future work.
- **False alarms near the boundary.** In the stationary family the true rates sit well above each rule's null. False-alarm control at the null boundary is covered by a seeded policy test (average run length ≥ 1000), not by this experiment.

## If adopted

The gate becomes the recommended decision path for actions with a supported hypothesis. If any criterion fails, the failure is reported, the mechanism stays available but not recommended, and it is not revised within this experiment.

## Disclosure

The design was reviewed externally twice, by two independent reviewers each time, before any held-out run.

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
- 1,500 opportunities instead of 1,200, because the expected re-earning evidence after the return was barely one block.

**Development runs.** A first development run of the pre-review implementation was stopped before completion, and its results were discarded unread.

**Development run 1 (seeds 1–3, the reviewed implementation).** It failed G1, G2 and G4, and passed G3 (0/24 stationary runs with a false demotion), G5 (42/48) and G6.

| Family | Gate | Best existing arm | Gate − per-context CUSUM |
|---|---|---|---|
| Stationary | 0.547 | 0.661 (`per_context_probing`) | +0.000 |
| Cliff | 0.260 | 0.291 (`per_context`) | +0.012 |
| Gradual | 0.336 | 0.388 (`per_context_probing`) | +0.000 |
| Recovery | 0.379 | 0.468 (`per_context_probing`) | −0.005 |
| Global | −0.219 | −0.041 (`per_context`) | +0.016 |

Three mechanisms explain the failures. None of them is the causal layer, which tied the CUSUM baseline everywhere.

1. **Burn-in trap.** With 25 burn-in outcomes, a Wilson lower bound of 0.6 needs 20 or more successes. Healthy contexts at 0.80–0.85 were often refused, and a never-demoted rule below the threshold never received new evidence.
2. **Probe bleed.** A flat 0.5 probe rate in permanently degraded contexts (global at 0.40) cost more than refusing.
3. **Measurement.** G4 counted contexts the gate was never acting in as undetected.

**Revision before development run 2:**
- never-demoted rules accumulate fixed blocks of fresh randomized evidence;
- below-threshold rules are probed;
- probing backs off exponentially, identically in every probing arm;
- G4 and G5 count exposed contexts only.

Thresholds (τ, the e-value, block size, utilities) and the other gates were not changed.

**Development run 2 (seeds 1–3, the revised version).** It passed G2, G3 (0/24) and G6, and failed G1, G4 and G5.

| Family | Gate | `per_context_probing` | `per_context` | Per-context CUSUM | Oracle |
|---|---|---|---|---|---|
| Stationary | 0.649 | 0.661 | 0.555 | 0.653 | 0.661 |
| Cliff | 0.337 | 0.339 | 0.291 | 0.335 | 0.427 |
| Gradual | 0.382 | 0.391 | 0.293 | 0.380 | 0.425 |
| Recovery | 0.392 | 0.407 | 0.246 | 0.394 | 0.553 |
| Global | −0.174 | −0.157 | −0.041 | −0.175 | −0.008 |

- **G1.** The gate ties `per_context_probing` in cliff and gradual rather than beating it. It loses slightly in recovery (−0.015). In global, the existing three-consecutive-failures rule quarantines faster at a 0.40 success rate than a detector calibrated against the rule's own lower bound.
- **G4.** Cliff: 48/48 exposed contexts detected, median 17, p90 34. Global: 141/141 detected, but median 28 against a gate of 20 (p90 62).
- **G5.** 21/48 returned contexts acted on again. Backoff slows re-earning after a long outage.
- **G6.** The causal layer never costs anything against the same detector on raw executions.

On development seeds, the gate therefore matches the strongest per-context heuristic. It does not beat it. Whether to revise again or to freeze this version and run the held-out seeds, which would then be expected to report "not adopted", is decided outside this document. No change has been made after development run 2. Constants are not changed after held-out results exist.
