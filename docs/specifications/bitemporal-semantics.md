# Bitemporal semantics

| Clock | Question | Who sets it |
|---|---|---|
| Valid time | When was this true in the world? | Client (defaults to `[now, ∞)`) |
| Transaction time | When did the substrate hold this version? | Engine only |
| `observed_at` (provenance) | When was the source encountered? | Client (defaults to now) |
| `recorded_at` (provenance) | When did the substrate record it? | Engine |

All intervals are half-open `[start, end)`, and `end = None` means unbounded. Timestamps are UTC, truncated to microseconds.

## Operations

| Operation | Effect |
|---|---|
| Create | Inserts revision 1 with `transaction_time = [t, ∞)` |
| Revise / promote / reject / tombstone | Closes the current revision at `t′` (`t′ > start`) and inserts revision n+1 at `[t′, ∞)` |
| Purge | Deletes all revisions and embeddings, and appends `memory_purged` |

A closed revision keeps the status it had while current, and `superseded_by` links it forward.

## Queries

- **Current state**: `upper_inf(transaction_time)`.
- **As of transaction time T**: `transaction_time ∋ T`.
- **True at valid time V**: `valid_time ∋ V`.
- **Historical correction**: a revision whose valid time starts in the past while its transaction time starts now. This records "we have learned that X was true since March".

## Concurrency

Superseding names the revision expected to be current. If another writer got there first, storage returns `conflict`. Clients can pass `expected_revision` to get the same guarantee end to end.
