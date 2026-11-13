# ADR-0009: Embedded engine and standalone service

- Status: Accepted
- Date: 2026-11-13

## Context

Consumers include Rust agents that want in-process memory and polyglot systems that want an API.

## Decision

- `cs_engine::CognitiveEngine` is the only implementation of cognitive operations. It is built from a `CognitiveStore`, an optional `Embedder` and a `Clock`.
- `cs-server` exposes the engine over HTTP under `/v1` with JSON contracts identical to the engine's request and response types. Handlers authenticate, deserialize, call one engine method and serialize.
- Errors use stable codes: `validation_failed` (422), `invalid_request` (400), `not_found` (404), `forbidden` (403), `conflict` (409), `unauthorized` (401) and `storage_failure` (500, with details withheld from clients).
- Authentication is a small integration point: principal headers plus an optional shared bearer key. Production deployments put an identity-aware proxy in front of the service or replace the extractor.
- Creates accept idempotency keys, and revisions accept `expected_revision`.
- Streaming endpoints are deferred until a concrete consumer needs them.

## Verification

The same scenarios run through the engine (conformance suite) and through HTTP (`cs-server/tests/http.rs`).
