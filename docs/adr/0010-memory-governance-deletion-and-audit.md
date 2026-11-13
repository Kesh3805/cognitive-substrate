# ADR-0010: Memory governance, deletion and auditability

- Status: Accepted
- Date: 2026-11-13

## Context

A persistent cognitive system retains information about people and organizations. Inspection, correction and deletion have to be part of the foundation from the start, not added as a later enterprise feature.

## Decision

- **Inspection**: current revision, full history, belief lineage with evidence, events and recall traces are all queryable within the caller's scope.
- **Correction**: revisions with a mandatory reason; earlier states are preserved.
- **Tombstone** (logical deletion): a revision with status `tombstoned`. It is excluded from recall and its history is kept.
- **Purge** (governance hard delete): removes every revision and all derived embeddings in one transaction, and appends `memory_purged`. Events never contain content, so the log stays valid.
- **Retention**: working memory requires `expires_at`, and expired records are excluded from recall.
- **Isolation**: see ADR-0002. Evidence and dependencies cannot cross tenants.
- **Quarantine**: model-derived content is a `candidate` until validated (ADR-0005).

## Consequences

Recall traces keep memory identifiers (not content) after a purge. Belief and evidence purge will follow the same pattern in M3.

## Verification

The `tombstone_and_purge` and `working_memory_is_ephemeral` scenarios.
