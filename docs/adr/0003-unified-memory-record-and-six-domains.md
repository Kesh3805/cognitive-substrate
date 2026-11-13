# ADR-0003: Unified memory record and six-domain model

- Status: Accepted
- Date: 2026-11-13

## Context

The predecessor architecture had four fixed tiers. The substrate needs six logical domains: episodic, semantic, procedural, relational, temporal and working. They need shared indexing, scope, provenance and lifecycle behaviour without being flattened into one shape.

## Decision

One `MemoryRecord` envelope (identity, revision, scope, provenance, bitemporal interval, status, expiry) wraps a tagged `MemoryContent` payload (`{"domain": ..., "data": ...}`).

- Domains are logical. They are stored in one table, with the payload as JSONB.
- A revision cannot change a memory's domain.
- Working memory must have `expires_at`. Persisting it does not make it permanent.
- Procedural memory carries a capability lifecycle stage: `observed → candidate → evaluated → trusted → deprecated`. Only `evaluated` and `trusted` count as verified.
- Each payload defines a deterministic `index_text()` used for lexical scoring and embedding.

## Alternatives

- **Six tables**: this would duplicate envelope logic and complicate cross-domain recall.
- **Untyped JSON**: this would lose validation and meaning.

## Consequences

Fields are promoted from JSONB to columns only when indexing or measured query performance requires it.

## Verification

Serialization round-trip and validation tests in `cs-memory`. The domain-change rejection is covered by the `memory_lifecycle` scenario.
