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

## ✅ M4 — Learning systems

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

### ✅ M4.4 — Procedural learning

- Executions recorded at intent and finished once; verifications separate, tagged deterministic, independent or self-assessed; self-assessment can fail a run but never pass it (ADR-0021).
- Class-weighted Wilson lower bounds (z = 1.96) with minimum class counts for `evaluated` and `trusted`; aborts and timeouts stay in the denominator; evidence tied to the procedure fingerprint.
- Positive, negative, boundary and regression suites gate promotion; failing production runs ratchet into locked regression cases.
- Drift detection (consecutive failures, windowed Wilson upper bound) quarantines automatically; quarantine is reinstatable, deprecation is terminal.

**Exit gate:** 40 conformance scenarios pass on both stores, including self-assessment, repetition, ratchet, drift, timeout and purge cases.

## ✅ M5 — Retrieval economics & active acquisition

### ✅ M5.1 — Evaluation harness and frozen baseline

- `cs-bench`: a seeded, offline corpus and query generator (direct, lexical-trap, semantic, temporal and scope families; balanced recent/old targets; graded gold from generator knowledge), IR metrics and paired bootstrap CIs, and a runner that writes raw results, aggregates, preregistered gate verdicts and an environment manifest.
- `recency_v1` frozen as the selectable control (ADR-0022).

### ✅ M5.2 — Candidate generation (005 not adopted; 006 adopted)

- `channels` strategy: lexical (shared tokenizer, stored index terms) and exact-dense channels, recency fill, equal budget, channels recorded in traces.
- Held-out result: nDCG +0.35, candidate recall +52 pp, no regressions, but p95 latency 1.70× (gate 1.25×). **Not adopted**; `v1` stays the default ([results](benchmarks/005-candidate-generation/RESULTS.md)).
- Experiment 006: the lexical channel answered in **one** store round trip (tested against its reference on both stores), with the strategy order rotated per query, on new held-out seeds. Every gate passed (nDCG@10 +0.35, p95 latency 1.08×). **`channels { lexical: 100, dense: 0 }` is the default**; v1 stays selectable as the frozen control ([results](benchmarks/006-single-round-trip-lexical/RESULTS.md)).

### ✅ M5.3 — ANN economics (experiment 007: not adopted)

- Per-space HNSW with iterative scans against exact filtered kNN on the same eligibility filters, with 100k vectors, skewed tenants and per-query plan verification. **No selectivity stratum passed.** HNSW was slower everywhere (p95 0.57× at the 40% tenant, down to 0.003× for tiny scopes) and lost recall in selective scopes (0.55 and 0.19, with underfill). Dense search stays exact (ADR-0023, [results](benchmarks/007-ann-under-filtering/RESULTS.md)).

### ✅ M5.4 — Information needs and knowledge gaps (experiment 008: gap proposals withdrawn)

- Retrieval failures, unresolved needs and gaps kept apart; demand (sessions, occurrences) separate from availability (status, evidence); opt-in, present-time, private recording; versioned lexical signatures, purgeable (ADR-0024).
- Detector `idf-coverage/1` (smoothed IDF over the searched population); complete-or-incomplete validation; `pending_promotion` when evidence is only unvalidated; acquisition priority v1 as an inspectable heuristic.
- Experiment 008 (held-out, about 2,300 questions): never wrong about absence (0 missed gaps), but 54% false gaps on direct questions about present facts (every verbose one) and 94% on paraphrases. **The detector does not propose gaps**; a person confirms one after a complete, empty search ([results](benchmarks/008-gap-validation/RESULTS.md)).

### ✅ M5.5 — Adaptive budgets (experiment 009: not adopted)

- `CandidateBudget::Adaptive { initial, initial_lexical }`: a small first pass, or the full fixed result when the lexical cut falls inside the best-matching tier. The store decides and executes it in one operation (PostgreSQL uses one statement, tested against its reference), and the decision is in the trace and replays exactly (ADR-0025).
- Experiment 009 ran on held-out seeds, with a crowded family built to cut gold facts off. Results:
  - **Quality:** non-inferior (nDCG@10 +0.002, CI [−0.001, +0.005]), with identical candidate recall at 0.48× the candidates scored.
  - **The signal:** 0 misses. 62% of the expansions recovered a candidate the first pass alone lost.
  - **Latency:** p95 0.96× against a 0.9× gate. **Not adopted**: `Fixed` stays the default and adaptive stays opt-in ([results](benchmarks/009-adaptive-budgets/RESULTS.md)).

## M6 — Reasoning

Bounded causal claims, predictions within declared conditions, measured predictive failure, and the ability to decline to answer when the evidence is inadequate. M6 is not an autonomous scientist: it does no open-ended causal discovery, no general world simulation, and model-generated explanations never become evidence.

### ✅ M6.1 — Metacognitive policy (experiment 010: adopted)

- A pure, deterministic policy over signals the recall already has:
  - evidence coverage of the question;
  - evidence that is only unvalidated (`candidate`);
  - open contradictions among the selected evidence;
  - the score margin.
- Its decision and reasons are recorded in the trace, and it replays exactly. Score margins are signals, not probabilities.
- **Experiment 010:** selective answering, as risk–coverage.
  - Baselines: answer-always and margin-only.
  - Results are stratified by absence, conflict, candidate-only evidence and weak retrieval.
  - Thresholds are frozen on development seeds; held-out seeds shift the distractor and conflict rates.
- **Result:**
  - The coverage detector moved from IDF coverage (it leaked absent services on development seeds) to `accounted-terms/1`: a memory must name the question's subject and account for every other term.
  - Held out, across 1,032 questions: **no wrong answers** among the 423 answered, every literal question answered, and every paraphrase, absent, conflicting, unvalidated-only and qualified question held back. Margin-only was wrong on 43% of its answers at the same coverage (ADR-0026, [results](benchmarks/010-selective-answering/RESULTS.md)).

### M6.2 — Causal hypotheses and interventions

- A hypothesis is directional and versioned: "doing X raises/lowers Y", scoped to a declared environment and context partition (capability input classes).
- Interventions record their **assignment mechanism** at intent:
  - `randomized` or `probe` interventions count as causal evidence;
  - interventions chosen by a policy (`passive`) count only as association.
- **Status:** `proposed` → `associated` (observational only) → `supported`, `refuted` or `inconclusive`.
  - Support needs a stratified lower bound on the treatment–control difference above zero, across at least K independent contexts.
  - A non-significant result is `inconclusive`, never `refuted`.
- **Experiment 011:**
  - The environment includes true causes, confounded correlates, reverse causation, null and negative effects, context-dependent effects, a confounded agent policy and noisy context features.
  - The gate is the false-supported rate, with a minimum detection rate for true causes.

### M6.3 — Counterfactuals, computed and labelled

- "Had X not been done in episode E, Y would likely not have happened" is computed **at read time** from a supported hypothesis and an episode, and is never stored.
- It is labelled inferred, is never evidence for a belief or a hypothesis, and disappears when its hypothesis loses support.

### M6.4 — Bounded predictions

- Supported hypotheses act as explicit transition rules.
- A prediction is recorded at an execution's intent and scored after verification. Rules that mispredict are demoted by a ratchet.
- **Experiment 012:** prediction-gated execution against the existing capability reliability filter, not against nothing.
  - Actions are paired, and the regime shifts partway through.
  - Measured: failure rate, completion rate and how often a gate refuses to act.

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
