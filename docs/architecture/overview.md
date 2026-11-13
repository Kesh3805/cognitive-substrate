# Architecture overview

## Thesis

> Can we build an open, model-independent cognitive substrate that allows AI systems to accumulate, validate, revise, compress, transfer and act upon knowledge across long periods of time, while keeping the entire process inspectable, reproducible and under explicit control?

The project is a **universal cognitive engine** that agents, research assistants and autonomous systems embed. Multi-agent operating-system capabilities are built on top of it later, not into it.

## Three things kept distinct

| Concept | What it is | Versioning |
|---|---|---|
| **Memory record** | Something retained by the system (one of six domains) | Bitemporal revisions under a stable `memory_id` |
| **Belief** | A proposition accepted to some degree, based on evidence | Revisions in a lineage (`lineage_id` = first revision) |
| **Cognitive event** | An immutable record that something changed | Append-only, sequenced per stream |

Collapsing these into a single table is the most common way memory systems lose the ability to explain themselves.

## Layering

```
┌────────────────────────────────────────────────────────────────────┐
│ Interface        cs-server (HTTP)  ·  embedded Rust API            │
├────────────────────────────────────────────────────────────────────┤
│ Engine           cs-engine: authorization, orchestration, events   │
├───────────────────────────────┬────────────────────────────────────┤
│ Policy (pure)                 │ Storage contract                   │
│ cs-policy: scoring, debt      │ cs-storage: trait + in-memory      │
├───────────────────────────────┼────────────────────────────────────┤
│ Domain           cs-memory: domains, beliefs, evidence, recall     │
├────────────────────────────────────────────────────────────────────┤
│ Foundation       cs-core: identity, scope, provenance, time, events│
└────────────────────────────────────────────────────────────────────┘
        adapters:  cs-postgres (PostgreSQL + pgvector)
```

Each boundary is stable for a reason:

- **cs-core / cs-memory** define meaning. They contain no I/O.
- **cs-policy** is pure. The same inputs always produce the same outputs, which is what makes recall replayable and debt auditable.
- **cs-storage** defines atomicity: every mutation persists together with its event.
- **cs-engine** is the only place cognitive operations are implemented.
- **cs-server** adapts HTTP to engine calls and holds no logic of its own.

## Write path

1. Authorize the caller against the target scope (`AccessContext::ensure_write`).
2. Validate the payload and its envelope invariants.
3. Assign identifiers and transaction time from the engine clock.
4. Model-derived content enters as `candidate`.
5. Persist the revision, its derived embedding and its cognitive event in one storage transaction.

Revising closes the current revision's transaction-time interval and inserts a new revision. Storage rejects the operation if the revision is no longer current, which gives optimistic concurrency for free.

## Recall path

1. Resolve the policy: fill in as-of times and validate the weights.
2. Fetch candidates. Authorization, domain, status, scope filter, validity and transaction as-of time are all applied **inside** the storage query.
3. Score exactly over the candidate set (`cs-policy::score_candidates`).
4. Select under the result count, minimum score and token budget.
5. Persist a trace with every candidate's component scores and selection reason.

Because the as-of transaction time is recorded and history is never overwritten, replaying a trace reproduces the original decision.

## Belief revision

Revising a belief:

- carries its evidence forward, and new links replace old ones per evidence id;
- copies its incoming dependency edges (what it relies on) to the new revision;
- leaves outgoing edges pointing at the superseded revision, so dependents surface as *invalidated dependencies* until they are re-affirmed;
- returns the transitive set of live dependents and emits `belief_reevaluation_requested`.

Refuting evidence makes a belief `disputed` unless a status is set explicitly. Conflicting evidence is never averaged into a confident belief.

## What is intentionally *not* here yet

Model providers, LLM extraction, background workers, ANN indexes, graph databases, Redis and streaming are all deferred until a measured requirement exists. See the [roadmap](../../ROADMAP.md).
