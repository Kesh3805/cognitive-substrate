# ADR-0001: Rust workspace and layered dependency architecture

- Status: Accepted
- Date: 2026-11-13

## Context

The substrate must run embedded in other Rust applications and also as a standalone service. Its core semantics (identity, time, revision, recall) must stay testable without a database, a network stack or a model provider.

## Decision

Use a Cargo workspace with seven crates and a strict dependency direction:

```
cs-core ← cs-memory ← cs-policy
                    ← cs-storage ← cs-postgres
          cs-policy, cs-storage ← cs-engine ← cs-server
```

- `cs-core` depends only on `serde`, `serde_json`, `uuid`, `time` and `thiserror`.
- `cs-memory` and `cs-policy` have no I/O. `cs-policy` is pure and deterministic.
- `cs-engine` receives its store, embedder and clock through constructor injection.
- `cs-server` is the composition root for the binary. It chooses the store (PostgreSQL or in-memory) and holds no cognitive logic.
- Model SDKs, worker frameworks, `tantivy`, Redis and graph databases are excluded from M0–M2.

A crate is justified by a stable architectural boundary, not by the number of modules it contains.

## Alternatives

- **One crate with modules**: simpler, but nothing stops a domain type from importing `sqlx`.
- **One crate per memory domain**: premature. The domains share one envelope (ADR-0003).

## Consequences

- An embedded user can depend on `cs-engine` and `cs-storage` alone.
- Recall traces and policies are data contracts in `cs-memory`, so `cs-storage` can persist them without depending on `cs-policy`.

## Verification

`cargo tree` shows no database or HTTP crates under `cs-core`, `cs-memory` or `cs-policy`.
