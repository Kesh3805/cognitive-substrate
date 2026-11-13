# Architectural invariants

These rules are enforced in code review and, where noted, by tests.

1. **Core domain logic never depends on PostgreSQL, HTTP or a model provider.**
   Enforced by crate boundaries (ADR-0001).
2. **All authoritative mutations produce cognitive events, atomically.**
   Every mutating `CognitiveStore` method takes its event; the conformance suite checks the sequences.
3. **No model-generated content becomes trusted knowledge without an explicit validation path.**
   Model-derived memories start as `candidate` and are excluded from recall until promoted.
4. **Revisions preserve historical transaction state.**
   Superseding closes the transaction-time interval, and exclusion constraints forbid overlapping versions.
5. **Valid time and transaction time are never conflated.**
   Clients may set valid time. Only the engine assigns transaction time.
6. **Every memory and belief has provenance and scope.**
7. **Recall is policy-controlled and traceable.** Every recall persists a trace.
8. **Scope authorization is enforced before retrieval**, inside the storage query, never by filtering assembled results.
9. **Deletion accounts for derived data.** Purge removes revisions and embeddings, and event payloads never contain content.
10. **Every performance or quality claim is backed by a reproducible benchmark.**
11. **The event log is append-only at the storage boundary.** A PostgreSQL trigger rejects `UPDATE`, `DELETE` and `TRUNCATE`.
12. **Unreadable is indistinguishable from missing.** Reads of out-of-scope records return `not_found`, never `forbidden`.
