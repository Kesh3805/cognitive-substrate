# Stability, versioning and MSRV

Cognitive Substrate is pre-1.0 (`0.1.0`). This page says what may change and how changes are signalled. It is a statement of intent, not a support contract.

## Versioning

The workspace follows [Semantic Versioning](https://semver.org/) with the usual pre-1.0 reading:

- a **minor** bump (`0.1` → `0.2`) may contain breaking changes to any public API listed below;
- a **patch** bump (`0.1.0` → `0.1.1`) contains only compatible fixes and additions;
- all workspace crates share one version (`[workspace.package]` in `Cargo.toml`).

Breaking changes are listed in the release notes with a migration note. After 1.0, breaking changes to the public API need a major version.

## What counts as public API

| Surface | Compatibility intent |
|---|---|
| **HTTP API under `/v1`** (`cs-server`) | The most stable surface. Within `/v1`, fields and endpoints are added, not removed or repurposed. Clients must ignore unknown response fields. A change that cannot be made additively goes to a new prefix (`/v2`), served alongside `/v1` for at least one minor release. |
| **Engine crate API** (`cs-engine`: `Engine` and its request/response types) | Public for embedding, but still evolving: may break in a minor release, with release notes. |
| **Domain crates** (`cs-core`, `cs-memory`, `cs-policy`, `cs-storage`) | Public types used by the engine API follow the engine's intent. The `CognitiveStore` trait may gain methods in any minor release; external store implementations should expect to follow it. |
| **Database migrations** (`migrations/`) | Forward-only. A released migration is never edited or removed; fixes are new migrations. There are no down migrations: roll back by restoring a backup. |
| **Recall traces and scoring** | Stored traces stay readable and replayable across upgrades (old traces are migrated, e.g. ADR-0022). A change to scoring semantics gets a new `scoring_version`. |
| **Event log payloads** | Event types and fields are added, not changed; payloads never carry content (ADR-0006). |

Not public API: `cs-bench`, `tests/conformance`, anything `#[doc(hidden)]` or behind an experimental flag, log and tracing output, metrics names, and the Docker and Compose files under `deploy/`.

## Experimental features

Endpoints and features that `ROADMAP.md` marks experimental, usually because their preregistered experiment did not support adoption (for example M6.3 counterfactuals and M6.4 predictions), stay opt-in and may change or be removed in any minor release, even when served under `/v1`.

## Deprecation

Before removing a public item, we deprecate it for at least one minor release where practical: `#[deprecated]` on Rust items, and a note in the release notes and API documentation for HTTP fields and endpoints. Security fixes may skip the deprecation period.

## MSRV

The minimum supported Rust version is the `rust-version` in the workspace `Cargo.toml` (currently **1.85**, the first release with edition 2024).

- Raising the MSRV is allowed in a minor release, is noted in the release notes, and is never done in a patch release.
- The optional `cs-embedder-fastembed` crate (and `cs-server`'s `fastembed` feature) depends on `ort`, which currently needs a newer Rust (1.92). The MSRV applies to the default workspace members.
- CI builds with the latest stable toolchain (`rust-toolchain.toml`). The MSRV is not yet checked by a dedicated CI job.
