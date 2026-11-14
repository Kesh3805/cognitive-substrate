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

## ✅ M3.x — Embeddings & operations

- Async, batch-capable embedders with an enforced contract; immutable embedding spaces including prefixes; deterministic projection capping and content hashes (ADR-0016).
- OpenAI-compatible and Ollama-native HTTP embedder (2D batching, poison bisection, index ordering, no silent truncation) and an optional local FastEmbed/ONNX embedder.
- Embedder-independent writes with atomic backfill jobs; persisted belief vectors; availability-time filtering so replays stay exact; explicit dense fallback and neutral imputation.
- Shadow-space migration with newest-first reindex, coverage reporting and a startup cutover gate; calibration provenance for duplicate thresholds; `api` / `worker` / `all` process roles (ADR-0017).

**Exit gate:** 27 conformance scenarios pass on both stores (including provider outage, backfill-vs-replay, fallback replay, shadow migration and purge-before-backfill). Both embedder adapters pass mock-server contract tests, and FastEmbed runs real local inference behind an opt-in test.

## M4 — Learning systems

### ✅ M4.1 — Entities

- Projection jobs turn structured fields of every revision into mentions and typed edges, committed with the write and idempotent per revision.
- Exact-key identity within a read-scope partition; deterministic ids; symbol-preserving normalization.
- Explicit, version-checked aliases and kinds; merges only through version-bound proposals, recorded as timestamped redirects so they are never retroactive (ADR-0018).
- Purges remove derived edges and uncurated orphan entities; entity names never enter the event log.

### ✅ M4.2 — Bounded traversal

- Edges visible exactly when their source revision is (scope, transaction time, status, valid time).
- Breadth-first neighborhoods with depth, node, edge and path budgets, deterministic ordering, as-of identity resolution and explicit truncation reporting (ADR-0019).

**Exit gate:** 32 conformance scenarios pass on both stores, including non-retroactive merges, purge erasure of orphan names, and stale merge proposals.

### ✅ M4.3 — Knowledge compilation

- Episodes may carry a structured triple, projected into source-linked edges.
- Operator `recurring-episode/1`: recurring episode triples become a semantic **candidate**, with a derivation record (operator, thresholds, support, witnesses, roots; no content).
- Support counted in independent units (actor, origin, valid-time bucket) over validated, non-derived roots; origin diversity, temporal dispersion and a counterexample veto (ADR-0020).
- Revalidation is recompilation: root revisions and tombstones recompile citing derivations; purges flag them atomically; lost support invalidates the derivation and demotes its memory; contended writes retry.

**Exit gate:** 36 conformance scenarios pass on both stores, including correlated-support, counterexample and candidate-root vetoes, purge-triggered revalidation, and merge-pooled support.

Open: weighting by extraction confidence, and excluding episodes conditioned on the compiled knowledge they would reinforce.

### M4.4 — Procedural learning

- Execution traces → capabilities, with outcome verification separate from execution status and verifier tiers that keep self-judgment from promoting a procedure.
- Lifecycle evaluation with positive, negative, boundary and regression suites; stated confidence bounds and minimum sample sizes; quarantine distinct from deprecation.

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
