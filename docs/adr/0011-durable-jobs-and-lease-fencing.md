# ADR-0011: Durable jobs with lease fencing and atomic effects

- Status: Accepted
- Date: 2026-11-13

## Context

M3 introduces background work (perception and resolution) that must survive restarts and run on several workers at once. It must also never apply a result after another worker has taken over the job. Exactly-once delivery cannot be achieved, so the design has to make the observable effects correct anyway.

## Decision

- Jobs live in PostgreSQL (`processing_jobs`), next to the knowledge they act on. Delivery is **at-least-once**.
- Claims use `FOR UPDATE SKIP LOCKED` and take a lease until `lease_until`. Every claim increments `lease_generation` and `attempts`. Expired leases become claimable again.
- **Fencing.** Heartbeat, release and completion all present the `(job_id, generation)` they were claimed with. A stale generation fails with `conflict` and writes nothing.
- **Atomic effects.** A handler does not write. It returns `JobEffects` (memories, extraction run, proposals, events, follow-up jobs), and `finish_job` commits those effects and the fenced job transition in one transaction. A worker that lost its lease therefore cannot apply effects.
- **Read-skew guards.** Effects carry preconditions: "this observation is still unpurged" and "these revisions are still current". They are checked with `FOR SHARE` inside the commit. If a precondition no longer holds, the effects are discarded and the job succeeds with `{"discarded": reason}`.
- **Constraint isolation.** In PostgreSQL, effects run under a savepoint. If they violate a constraint, the transaction rolls back to the savepoint and dead-letters the job, so the failure is still recorded.
- **Retries.** Retryable failures back off exponentially with bounded, deterministic jitter (derived from job id and attempt), up to `max_attempts`, then dead-letter. Permanent failures dead-letter immediately. A dead-letter appends `job_dead_lettered`.
- **Dedupe.** Only *active* jobs (queued or leased) are unique per `(tenant, kind, dedupe_key)`. Terminal jobs never block resubmission. Work-level idempotency lives in domain constraints (`extraction_runs`, proposal fingerprints).
- Jobs run **as the principal that submitted them**, so background work never sees or writes more than its submitter could.
- **Lock order:** jobs → observations → memory revisions (sorted) → beliefs (sorted) → proposals → events.

## Alternatives

- **A queue library or broker** (Apalis, Redis, NATS). Rejected because effects and job state must commit in the same transaction as the knowledge they change.
- **Idempotent handlers with no effect fencing.** Rejected because a slow worker could still apply results computed against stale state.

## Verification

Conformance scenarios: `leases_are_fenced`, `retries_back_off_then_dead_letter` and `purge_during_extraction_discards_effects`.
