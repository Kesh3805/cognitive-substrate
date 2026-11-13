# ADR-0007: PostgreSQL as the authoritative persistence layer

- Status: Accepted
- Date: 2026-11-13

## Context

We need durable, transactional, self-hostable storage with range types, JSON, full-text search and vector support. No proprietary or hosted dependency is allowed.

## Decision

- PostgreSQL is the single authoritative store, with `tstzrange` for bitemporal intervals, JSONB for domain payloads and `btree_gist` exclusion constraints.
- `pgvector` stores embeddings per revision and per model. The column is dimension-agnostic, with a checked `dimensions` field, so models can be upgraded without schema changes. **Embeddings are a derived index, never the source of truth.**
- The graph is a relational adjacency list (`belief_dependencies`). A graph database is added only when measured traversal workloads justify it. (Kùzu was rejected: the project was archived in 2025.)
- Migrations are embedded in the binary (`sqlx::migrate!`).
- Queries use the runtime `sqlx` API, so building does not need a live database.

## Alternatives

Hosted vector databases, a separate graph store, or Redis: all rejected for M0–M2.

## Consequences

The in-memory store in `cs-storage` is the executable reference for semantics. Both adapters must pass the conformance suite.

## Verification

`postgres_store_conforms` runs the full suite against PostgreSQL in CI.
