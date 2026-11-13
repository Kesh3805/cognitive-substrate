# Research program

The novelty is not any individual feature. Knowledge graphs, vector retrieval, planning and causal inference already exist. The contribution is integrating them under one shared, inspectable and experimentally testable cognitive model. The project works toward five properties:

1. **Knowledge is revisable.** Beliefs evolve without erasing their history.
2. **Experience becomes capability.** Validated outcomes turn into reusable procedures.
3. **Reasoning is traceable.** Conclusions can be reconstructed from evidence and dependencies.
4. **Intelligence is resource-aware.** The system decides what to remember, retrieve, investigate and execute under explicit budgets.
5. **Cognition is cumulative.** Knowledge transfers across tasks, sessions, agents and model generations.

## Research tracks

| Track | Core question | Status |
|---|---|---|
| A. Belief systems & epistemic state | Can a system keep a coherent, revisable model of what it knows without reconstructing its whole history? | **Foundation in M2**: beliefs, evidence, lineage, dependency graph |
| B. Causal & counterfactual memory | Do accumulated execution histories improve causal diagnosis? Counterfactuals must stay labelled as inferred. | Planned (M6) |
| C. Procedural learning | Can reusable capabilities be learned from experience without brittle rules? | Lifecycle stages in M2; learning in M4 |
| D. Active knowledge acquisition | What information should be acquired next to reduce uncertainty? | Planned (M5) |
| E. Temporal cognition | What did the system believe at time T? What became stale? | **Foundation in M2**: bitemporal store, as-of recall |
| F. Multi-agent federation | Shared substrate with attributed, disagreeing perspectives, without merging every agent's memories | Planned (M7) |
| G. Metacognition & resource allocation | When should a system answer, investigate or abstain? | Planned (M6) |
| H. Knowledge compilation | How much experience can be compressed while preserving future utility? Must be reversible. | Planned (M4) |
| I. Internal world models | Do explicit, revisable environment models reduce execution failures? | Planned (M6) |
| J. Deterministic replay & cognitive debugging | Can every decision be reconstructed and re-run under counterfactual conditions? | **Foundation in M2**: recall traces, exact and counterfactual replay |
| K. Governance, privacy & trust | Distinguishing what was encountered from what may be retained | **Foundation in M2**: isolation, candidate quarantine, purge |
| L. Cognitive interoperability | A stable protocol for exchanging structured cognitive state | HTTP API in M2; protocol in M7 |

## Experimental directions

| Direction | Measure |
|---|---|
| Memory utility prediction | Utility per stored token and retrieval cost |
| Knowledge decay | Stale-memory error rate |
| Hypothesis ecology | Calibration and discovery rate |
| Attention allocation | Task performance under fixed context budgets |
| **Cognitive debt** | Downstream failure rate vs. CDI |
| Provenance propagation | Unsupported-conclusion rate |
| Experience recombination | Success on compositional tasks |
| Forgetting as optimization | Storage reduction vs. capability loss |
| Cross-model knowledge transfer | Transfer gain and negative transfer |

## Preregistered experiments

Each experiment fixes its dataset, baselines, metrics and success criteria in `benchmarks/` **before** it is run.

- **001 — Memory utility.** Compares a sliding window, vector RAG, structured episodic memory, consolidated semantic memory and the full substrate on accuracy, recall, tokens, latency and retrieval precision.
- **002 — Belief evolution.** Feeds the system changing and contradictory information, and measures contradiction detection, supersession accuracy, historical preservation, current-state accuracy and false-belief propagation.
- **003 — Long-horizon accumulation.** Tests whether knowledge becomes more useful over many sessions, or whether noise and accumulated error overwhelm it.
- **004 — Cognitive debt validity.** Tests whether a lower CDI predicts better decision accuracy and fewer stale-memory failures.

## Lessons carried forward

The predecessor architecture reported 0.724 thread cohesion, an 18% lower full-pipeline P95 than baseline RAG, a 35% higher mean latency, **a 0% research-memory hit rate** over 47 queries, and no gain (and a regression with reranking) from hybrid retrieval over 40 queries. Structured extraction is not automatically useful. **No subsystem earns permanent inclusion without measured utility.**
