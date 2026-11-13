# ADR-0016: Embedding spaces, asynchronous backfill and dense fallback

- Status: Accepted
- Date: 2026-11-13

## Context

M3 used one in-process, deterministic embedder. Real models are remote or heavy. They fail, they change behind stable names, and two models with the same dimensions still produce incomparable vectors. Embedding availability must never decide whether knowledge can be stored, and it must not silently change what a recall returns.

## Decision

**Identity.** Every vector belongs to an immutable **embedding space**, written as `provider/model@revision+dN+l2|raw+ppN[+pxHASH]`. A space is defined by:

- the provider, model and revision (or digest);
- the dimensions;
- whether vectors are L2-normalized;
- the preprocessing version;
- the query and document prefixes, which asymmetric models such as nomic, e5 and bge require.

Vectors from different spaces are never compared. Duplicate thresholds are bound to space ids (ADR-0013).

**The engine owns the input.** It:

- caps the canonical projection deterministically (head plus tail, 8,192 characters, part of preprocessing v1);
- applies the space's prefix;
- hashes the exact text that is embedded (`content_hash`).

Adapters only map strings to vectors. The engine checks every returned vector for count, dimensions, finite values and non-zero norm, and L2-normalizes it in Rust when the space is normalized.

**Writes never wait on the embedder for correctness.** The engine embeds a new revision inline, best effort and *outside* the storage transaction. If that fails, or if inline embedding is disabled (the default for remote embedders), an `embed` job is committed **in the same transaction as the write**. That job is deduplicated by (revision, space), retried with backoff and dead-lettered like any other job. A backfilled vector:

- attaches to its exact revision;
- is skipped if that revision has since been purged;
- re-queues dense resolution, because a new vector can reveal duplicates the structural pass could not see.

**Availability time.** Each vector records when it became available (`embedded_at`). Recall as of time *T* uses only vectors available at *T*, so exact replay is unaffected by later backfills.

**Recall** records a `DenseStatus` in the trace:

- `used(space)`: the dense channel ran in that space.
- `unavailable(space, reason)`: the query could not be embedded. Scoring falls back to lexical plus recency, with those weights renormalized to sum to 1. It does not score every candidate as dense-dissimilar.
- `disabled`: no embedder is configured, or the policy's dense weight is 0.

A candidate still awaiting its vector receives the batch's **median dense score**. Its `dense_score` stays `None`, which marks the imputation. It neither sinks to 0 nor floats above its embedded peers. The response reports `dense_coverage`.

**Exact replay** reuses the trace's recorded query vector, and replays a fallback recall as a fallback. When query text is not retained, the query vector is dropped too, because it encodes the query.

## Alternatives

- **Synchronous embedding in the write transaction.** Rejected: it holds locks and connections for the length of network timeouts.
- **Treating a missing vector as dense 0.** Rejected: it buries new knowledge until the backfill catches up.

## Verification

Conformance scenarios `writes_survive_embedder_outage`, `backfill_never_changes_a_replay`, `fallback_recalls_replay_as_fallback` and `purged_revisions_are_never_re_embedded`, plus the scoring and embedding contract unit tests.
