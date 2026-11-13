# ADR-0004: Bitemporal knowledge and revision semantics

- Status: Accepted
- Date: 2026-11-13

## Context

A long-lived system must answer two different questions: *when was this true?* and *when did we know it?* It must also never lose what it previously believed.

## Decision

- Every memory revision and belief revision carries two half-open `[start, end)` intervals:
  - **valid time**: when the proposition holds in the represented world. Clients may set it.
  - **transaction time**: when this version was present in the substrate. Only the engine assigns it, from its injected `Clock`, truncated to microseconds.
- A revision is **current** when its transaction interval is open.
- **Revising never overwrites.** In one storage transaction, the current revision's transaction interval is closed at `t` and a new revision opens at `t`. `t` is strictly after the previous start.
- "Superseded" is not a status. A closed revision keeps the status it had while it was current, so as-of-transaction-time queries are truthful.
- Storage rejects superseding a revision that is no longer current (optimistic concurrency). PostgreSQL also has an exclusion constraint against overlapping transaction intervals per logical record.
- Retraction and tombstoning are revisions too.

## Alternatives

- **Event sourcing only** (reconstruct state by replay): this makes every read a replay. We keep an event log for audit (ADR-0006) and bitemporal tables for state.
- **Status flag plus `updated_at`**: this loses history.

## Consequences

Recall can run "as of" any past transaction time, which is what makes exact replay possible (ADR-0008).

## Verification

The `memory_lifecycle` and `bitemporal_recall_is_reproducible` conformance scenarios, plus the PostgreSQL exclusion constraints.
