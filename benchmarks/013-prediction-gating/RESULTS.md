# 013 — Results (held-out seeds)

- **Run:** held-out seeds 901–905 (part A) and 911–915 (part B), with 40 runs per family in each part. In-memory store.
- **Build:** the binary was built at commit `5890149`, which is where the preregistration was frozen. The manifests record `e8ce42c`, because documentation commits landed while the runs were in progress. `git diff 5890149 e8ce42c -- crates tests migrations Cargo.toml Cargo.lock` is empty, so the code is identical.
- **Data:** in [`results/held-out-memory/`](results/held-out-memory/) (part A) and [`results/b-held-out-memory/`](results/b-held-out-memory/) (part B). Each holds `raw.jsonl`, `summary.json` and `manifest.json`.

## Verdict

**Not adopted, in either part.** The gate stays available, documented, and not recommended. It is not revised within this experiment.

- **Part A.** Prediction gating is safe: no false demotions, it detects collapses, and it never costs anything against the same detector without the causal layer. But it stays about 0.02–0.03 utility per opportunity **below** the strongest existing per-context filter in every family. Two detection and recovery gates missed narrowly.
- **Part B.** The effect gate failed every criterion. Its holdouts cost more than they returned, and in runs where the hypothesis was not supported after burn-in it refused everything.

## Part A — gates

| Gate | Binding | Criterion | Value | Pass |
|---|---|---|---|---|
| G1 (v1) | no | Beat every existing arm in every harmful family | Loses to `per_context_probing` in all four | ❌ (reported) |
| G1 | yes | Non-inferior (CI lower > −0.02) to every admissible arm in every harmful family | Admissible: `per_context_probing` only. Cliff −0.021 [−0.031, −0.011]; gradual, recovery and global also fail | ❌ |
| G2 | yes | Stationary: non-inferior to every existing arm | vs `per_context_probing` −0.030 [−0.043, −0.018] | ❌ |
| G3 | yes | Stationary: runs with any false demotion ≤ 5% | 0/40 (Wilson upper bound 0.088) | ✅ |
| G4 | yes | Cliff and global: ≥ 95% of exposed shifted contexts demoted; median delay ≤ 20 (cliff), ≤ 40 (global) | Cliff 73/73, median **21**, p90 33; global 217/217, median 31, p90 72 | ❌ (cliff median by 1) |
| G5 | yes | Recovery: ≥ 80% of exposed returned contexts re-earned | 59/75 = **78.7%** | ❌ |
| G6 | yes | Every family: non-inferior to `per_context_cusum` | All 6 families | ✅ |

### Part A — utility per opportunity

| Family | Gate | `per_context_probing` | `per_context_cusum` | `per_context` | `global` | Oracle |
|---|---|---|---|---|---|---|
| Stationary | 0.587 | 0.617 | 0.590 | 0.419 | −0.035 | 0.617 |
| Cliff | 0.337 | 0.357 | 0.338 | 0.262 | −0.106 | 0.412 |
| Gradual | 0.393 | 0.415 | 0.396 | 0.322 | −0.039 | 0.440 |
| Recovery | 0.427 | 0.460 | 0.434 | 0.239 | −0.106 | 0.553 |
| Upward | 0.447 | 0.528 | 0.454 | 0.240 | −0.237 | 0.572 |
| Global | −0.174 | −0.146 | −0.175 | −0.064 | −0.142 | −0.020 |

Against the existing capability filter (`global`) and its per-context version, the gate gains 0.07 to 0.68 per opportunity outside the global family. Those filters quarantine at three consecutive failures, so they give up healthy work. In stationary, `per_context_probing` matched the oracle exactly in every development run, so G2 amounts to asking the gate to match perfect play.

## Part B — gates

| Gate | Criterion | Value | Pass |
|---|---|---|---|
| B1 | Stationary: beat always-act, refuse, success filter, absolute gate | Loses to always-act (−0.116), the success filter (−0.026) and the absolute gate (−0.069) | ❌ |
| B2 | Every family: non-inferior to the static effect baseline | Stationary −0.216, control rise −0.082, effect reversal −0.088 | ❌ |
| B3 | Control rise: beat the static effect baseline and the absolute gate | −0.082 against the baseline; +0.091 against the absolute gate | ❌ |
| B4 | Effect reversal: beat the static effect baseline | −0.088 [−0.112, −0.067] | ❌ |
| B5 | Control rise: ≥ 90% of exposed shifted contexts demoted | 56/69 = 81% | ❌ |

Hypotheses supported after burn-in: 39/40 (stationary), 35/40 (control rise), 33/40 (effect reversal).

## What this shows

- **Sequential demotion works as a safety mechanism.**
  - No false demotion occurred in 40 stationary runs.
  - Every exposed collapse was caught: 73/73 cliff contexts at a median of 21 opportunities, and 217/217 global contexts at a median of 31.
  - The causal layer cost nothing against the same detector on raw executions (G6).
- **It does not improve decisions over a well-tuned per-context filter.** In the development data, the gate's deficit to `per_context_probing` followed its probing. The gate probes demoted and below-threshold contexts and pays for every probe at its true utility. The window filter re-enters on the same canary schedule but probes less.
- **Effect gating, as designed here, loses to a static effect estimate.**
  - In development, about half of the deficit came from runs where the M6.2 hypothesis was not supported. There the gate refuses every context, while the static baseline needs no pooled support.
  - The rest scales with the holdout rate. Extrapolated to a holdout near 0, the effect gate tied the static baseline in stationary and beat it under effect reversal. It still beat the success filter and the absolute gate by about 0.12–0.18 per opportunity on supported runs.
  - Monitoring the control arm needs about 28 control observations (median) to catch a control rise, which a cheap holdout cannot supply.
- **The development caveats were confirmed.** Part A's held-out numbers sit within about 0.01 of the corrected development numbers, and part B's deficits match their sign and rough size.

## Limits

- **Simulated environments.** Real agents have correlated outcomes, delayed verification and non-stationary context mixes.
- **One threshold, one utility function.** No risk–coverage frontier was swept.
- **No confounded observational evidence.** Experiment 011 tests that.
- **Recall untested.** The causal layer's value in decisions that depend on recall was not measured.

## Next

The mechanism stays available as experimental. These results point to a redesign, which needs a new preregistered experiment:
- **Per-context support:** decide support per context, instead of a pooled hypothesis gating every context.
- **Cheaper control monitoring:** for example, sparse randomized audits on a schedule instead of a constant holdout.
- **Fewer probes:** gate probing on the canary signal alone.
