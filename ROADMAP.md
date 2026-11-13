# Roadmap

Each milestone has an exit gate. The project does not start a milestone's successor until that gate is demonstrated.

## ✅ M0 — Architecture specification

ADRs 0001–0010, the Rust type contracts, the bitemporal and scope semantics, the PostgreSQL schema, the recall scoring specification and the event-log invariants.

**Exit gate:** no unresolved ambiguity about identity, scope, time, revision or authoritative writes.

## ✅ M1 — Rust foundation

Seven-crate workspace, core types and validation, migrations and the PostgreSQL adapter, append-only events, the conformance suite, and CI.

**Exit gate:** a memory can be created, read, revised and recovered from PostgreSQL with its history intact.

## ✅ M2 — Cognitive primitives

Six domain payloads; beliefs, evidence, revisions and dependency edges; embedding storage and baseline retrieval; deterministic recall with traces and exact and counterfactual replay; the HTTP and embedded APIs; the cognitive-debt evaluator.

**Exit gate:** the same scenarios pass through the embedded engine (in-memory and PostgreSQL) and the HTTP service, demonstrating scope isolation, temporal correctness, revision preservation and recall reproducibility.

## M3 — Cognitive processing

- Durable PostgreSQL-backed jobs.
- Perception: a structured-extraction adapter (local models first) producing *candidate* memories.
- Resolution: duplicate and contradiction detection, and supersession proposals.
- Belief recall alongside memory recall; evidence purge.
- A model-backed `Embedder` adapter (FastEmbed / ONNX).

## M4 — Learning systems

- Knowledge compilation: episodes → generalizations → semantic knowledge, reversibly linked to evidence.
- Procedural learning: execution traces → capabilities, with lifecycle evaluation including negative tests.
- Entity resolution and typed relationships; multi-hop traversal.

## M5 — Retrieval economics & active acquisition

- Hybrid retrieval and reranking, accepted only if they beat the v1 baseline in preregistered experiments.
- ANN preselection benchmarked against exact scoring.
- Adaptive retrieval budgets; knowledge-gap detection and information-gain estimates.

## M6 — Reasoning

Causal hypotheses and intervention tracking, explicitly labelled counterfactuals, bounded world models, and metacognitive policies (answer / investigate / abstain).

## M7 — Collective intelligence & interoperability

Agent-scoped memory with explicit sharing, disagreement tracking, knowledge handoff, trust policies, a cognitive-state exchange protocol, SDKs and framework adapters.

## Engineering rules

1. No speculative abstraction without a demonstrated requirement.
2. No database dependency in core domain logic.
3. No LLM-controlled authoritative writes.
4. Every memory has provenance and scope.
5. Every update preserves historical state.
6. Every background operation is recoverable and idempotent.
7. Every retrieval decision is inspectable.
8. Every major optimization has a benchmark.
9. No subsystem earns permanent inclusion without measurable utility.
10. No performance claim without a reproducible measurement.
