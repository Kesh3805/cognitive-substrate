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

See [the research program](../docs/research/research-program.md).
