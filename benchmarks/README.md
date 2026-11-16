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

See [the research program](../docs/research/research-program.md).
