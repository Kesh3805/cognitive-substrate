# Benchmarks

Every experiment is **preregistered**. The dataset, baselines, metrics and success criteria are committed here before any results are produced. Results that miss their criteria are reported, not reframed.

## Layout

```
benchmarks/
  <nnn>-<name>/
    PREREGISTRATION.md   hypothesis, dataset, baselines, metrics, success criteria
    data/                fixed, versioned datasets (or a script that regenerates them deterministically)
    results/             raw outputs plus environment manifest (commit, hardware, versions)
```

## Planned

| ID | Experiment | Status |
|---|---|---|
| 001 | Memory utility vs. sliding window and vector RAG | Preregistration drafting |
| 002 | Belief evolution under changing and contradictory input | Preregistration drafting |
| 003 | Long-horizon knowledge accumulation | Planned |
| 004 | Cognitive Debt Index validity | Planned |
| 005 | [Candidate generation: channels vs. recency](005-candidate-generation/PREREGISTRATION.md) | **Done: not adopted** (latency gate failed; [results](005-candidate-generation/RESULTS.md)) |
| 006 | [Lexical channel in one store round trip](006-single-round-trip-lexical/PREREGISTRATION.md) | **Done: adopted** ([results](006-single-round-trip-lexical/RESULTS.md)) |
| 007 | [HNSW vs exact search under per-scope filtering](007-ann-under-filtering/PREREGISTRATION.md) | **Done: not adopted** (no stratum passed; [results](007-ann-under-filtering/RESULTS.md)) |
| 008 | [How far gap validation can be trusted](008-gap-validation/PREREGISTRATION.md) | **Done: gap proposals withdrawn** (both tiers failed; [results](008-gap-validation/RESULTS.md)) |
| 009 | [Adaptive candidate budgets](009-adaptive-budgets/PREREGISTRATION.md) | **Done: not adopted** (p95 gate failed, 0.96×; [results](009-adaptive-budgets/RESULTS.md)) |
| 010 | [Answer, investigate or abstain](010-selective-answering/PREREGISTRATION.md) | **Done: adopted** (0 wrong answers among 423; [results](010-selective-answering/RESULTS.md)) |
| 011 | [Causal hypotheses under confounding](011-causal-evidence/PREREGISTRATION.md) | **Done: adopted** (0.4% false support vs 45% naive; [results](011-causal-evidence/RESULTS.md)) |
| 012 | [Read-time counterfactual bounds](012-counterfactual-bounds/PREREGISTRATION.md) | **Done: not adopted** (sharpness 0.78 < 0.80; coverage 99.8%; [results](012-counterfactual-bounds/RESULTS.md)) |
| 013 | [Prediction-gated execution under regime shifts](013-prediction-gating/PREREGISTRATION.md) | Preregistered, version 2 (part A predictive governance, part B causal decision value); **Done: not adopted** (part A: safe, 0/40 false demotions, but about 0.02–0.03 below the best per-context filter; part B: failed every criterion; [results](013-prediction-gating/RESULTS.md)) |

See [the research program](../docs/research/research-program.md).
