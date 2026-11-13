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

## ✅ M3 — Governed cognitive processing

The substrate can observe, extract, detect, propose and recall. Authoritative knowledge still changes only through validated, version-checked decisions.

- Durable PostgreSQL jobs: at-least-once delivery, `SKIP LOCKED` leases, generation fencing, effects committed atomically with the fenced transition, read-skew guards, savepoint-isolated effects, backoff with deterministic jitter, and dead-lettering (ADR-0011).
- Immutable observations stored atomically with their perception job. A precision-first rule extractor, plus an optional OpenAI-compatible / Ollama extractor. Everything extracted is a *candidate* (ADR-0012).
- Version-bound resolution proposals for duplicates, supersessions and contradictions, with a structural pre-veto, per-model thresholds calibrated against hard negatives, stale detection, and provenance-union merges (ADR-0013).
- Evidence and observation purge with a purge ledger, re-evaluation flags, and re-homing of corroborated facts (ADR-0014).
- Belief recall, with relevance kept separate from confidence (ADR-0015).

**Exit gate:** 22 conformance scenarios pass on both stores. The calibration suite proves no hard negative merges. The HTTP flow is covered end to end, and the extractor contract is tested against a mock server.

## M3.x — Deferred from M3

- A model-backed `Embedder` adapter (FastEmbed / ONNX), with per-model threshold calibration.
- A dedicated worker binary and horizontal worker deployment guidance.
- Persisted belief embeddings.

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
