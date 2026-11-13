# ADR-0017: Zero-downtime model migration and process roles

- Status: Accepted
- Date: 2026-11-13

## Context

Changing the embedding model must not silently lower recall quality while the new space is only partly backfilled. Background work also needs an operational boundary that is hard to misconfigure.

## Decision

### Model migration (shadow space)

1. **Dual-write.** Configure the new model as the shadow embedder (`CS_SHADOW_EMBEDDER*`). Every new revision commits a backfill job for the shadow space alongside the write. Recall and resolution keep using the active space at full coverage.
2. **Backfill.** `POST /v1/embeddings/reindex` enqueues `embed` jobs for current revisions that lack a vector in the target space, **newest first**, so the candidates most likely to rank (recency counts) are covered earliest. It is idempotent: rerunning it is the reconciliation pass for writes that raced the first run.
3. **Observe.** `GET /v1/processing/status` reports per-space coverage (active and shadow) and queue counts by kind and status, including the oldest due job.
4. **Calibrate.** If dense duplicate detection should run in the new space, add a calibrated threshold entry (ADR-0013) to `CS_RESOLUTION_CONFIG`. Uncalibrated spaces still serve recall, but never propose dense duplicates.
5. **Cut over.** Make the new model the active embedder. At startup the service refuses to serve an active space whose coverage across all tenants is below `CS_MIN_ACTIVE_COVERAGE` (default 99.5%), unless `CS_ALLOW_LOW_COVERAGE=true` is set explicitly.
6. **Rollback and replay.** Old vectors are never deleted on cutover. Switching back is a configuration change, and traces recorded before the cutover still replay in their original space.

### Process roles

`CS_ROLE` selects one of:

| Role | Serves HTTP | Claims jobs | Use |
|---|---|---|---|
| `api` | yes | **never** | Production API nodes |
| `worker` | no | yes | Production workers (`cognitive-substrate worker`) |
| `all` | yes | yes | Development, and the in-memory store |

The default is `api` with PostgreSQL and `all` with the in-memory store. The in-memory store cannot be shared between processes, so it refuses any other role. An `api` process logs that it never claims jobs. Workers stop claiming on shutdown; any leases they still hold expire and are reclaimed (ADR-0011).

Embedding configuration must be identical on API and worker processes.

## Consequences

The active space is a deployment setting rather than a stored record, which keeps one source of truth per deployment. The startup gate is what makes this setting safe.

## Verification

Conformance scenario `shadow_space_migration`: dual write, idempotent reindex, full shadow coverage, then recall served from the new space.
