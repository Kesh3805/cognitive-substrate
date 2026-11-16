# 011 — Results (held-out seeds)

- **Run:** held-out seeds 701–705, with 1,400 hypotheses (7 types × 40 per seed). In-memory store; the evaluation is a pure function and the stores conform.
- **Data:** manifest, raw rows and summary are in [`results/held-out-memory/`](results/held-out-memory/).

## Verdict

**Adopted: every gate passed.** Causal evaluation version 1 ships as specified.

## Gates

| # | Criterion | Value | Pass |
|---|---|---|---|
| 1 | False-supported ≤ 0.01, Wilson upper bound ≤ 0.02 | 2 / 506 = 0.004 (upper bound 0.014) | ✅ |
| 2 | Detectable causes supported ≥ 0.70 (effect ≥ 0.25, ≥ 60 randomized outcomes per arm) | 68 / 70 = 0.971 | ✅ |
| 3 | False-refuted ≤ 0.01 | 3 / 894 = 0.003 | ✅ |
| 4 | Interval coverage ≥ 0.90 | 1,043 / 1,098 = 0.950 | ✅ |
| 5 | Naive baseline fooled (false-supported ≥ 0.20) | 230 / 506 = 0.455 | ✅ |
| 6 | Every evaluation reproduced exactly | all | ✅ |

## Statuses by type

Each type has 200 hypotheses.

| Type | supported | refuted | inconclusive | proposed | Naive baseline: supported |
|---|---|---|---|---|---|
| strong positive (+0.3) | 117 | 0 | 47 | 36 | 195 |
| near boundary (+0.12) | 13 | 1 | 145 | 41 | 94 |
| negative (−0.3, stated either way) | 58 | 74 | 33 | 35 | 21 |
| confounded null | **2** | 12 | 133 | 53 | **25** |
| reverse causation | **0** | 9 | 144 | 47 | **200** |
| context-dependent | 17 | 2 | 140 | 41 | 104 |
| harmful in one | 47 | 0 | 104 | 49 | 160 |

## What this shows

- **Confounding fools the naive reading, and not the evaluation.**
  - Read as if it were randomized, the agent's own evidence supported every reverse-causation hypothesis (200 of 200), 25 confounded nulls, and 45% of all non-causes.
  - The evaluation, which counts only randomized evidence, supported none of the reverse-causation hypotheses, 2 confounded nulls, and 0.4% of non-causes.
- **The intervals are honest, and conservative.**
  - The 95% intervals covered the true effect 95% of the time.
  - Where the budget is adequate, causes are found (97%). Across every cause, whatever its budget, only 28% were supported: most stay `inconclusive` or `proposed` rather than being guessed at.
  - Refutation needs a stated minimum effect, and wrongly refuted 0.3% of real effects.
- **Negative effects are handled in either direction.** Stated as `lowers`, they were supported; stated as `raises`, they were refuted.

## Limits

- **Harm can hide outside the sampled contexts.** 47 of 200 harmful-in-one hypotheses were supported. Support speaks only for `scope_contexts`. When the harmed context is not sampled, or has too few outcomes to show its own effect, nothing in the evidence shows the harm. The sign-consistency guard blocks support only when an eligible context establishes the opposite effect. Callers acting on `supported` must read `scope_contexts`.
- **Assignment is declared, not verified.** A caller that labels confounded choices as `randomized` defeats the evaluation. The record keeps the declared draw and propensity for audit.
- **One synthetic environment.** Binary outcomes, an additive effect, and independent units within a context. Effects that interact with the confounder, and correlated units, were not tested.
- **Repeated evaluation.** Evaluating after every new batch of evidence (optional stopping) was not measured. Each held-out hypothesis was evaluated once.
