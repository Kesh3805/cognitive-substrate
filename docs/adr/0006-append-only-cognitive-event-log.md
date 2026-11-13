# ADR-0006: Append-only cognitive event log

- Status: Accepted
- Date: 2026-11-13

## Context

Auditability and cognitive debugging require an immutable account of every change. That account must survive governance deletion of content.

## Decision

- Every authoritative mutation appends a `CognitiveEvent` in the same storage transaction as the mutation.
- Events are sequenced per stream (a memory id, belief lineage id, evidence id or trace id) starting at 1. Storage assigns sequences under a per-stream lock.
- Payloads contain **identifiers and metadata only, never memory content.** Purging content never requires rewriting the log.
- Append-only is enforced at the storage boundary. A PostgreSQL trigger rejects `UPDATE`, `DELETE` and `TRUNCATE`. Deployments should also deny those privileges to the application role.
- Events carry the scope of what they describe and are filtered by the same access rule as the records.

## Consequences

The log is an audit trail, not the query model (ADR-0004). Event schema changes bump `schema_version`.

## Verification

The `event_log_is_sequenced` and `tombstone_and_purge` scenarios, plus `postgres_event_log_rejects_mutation`.
