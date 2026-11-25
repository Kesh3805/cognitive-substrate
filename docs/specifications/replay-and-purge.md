# Replay after purge

Exact replay (ADR-0008) and hard purge (ADR-0010, ADR-0014) pull against each other. Replay promises to reproduce a past recall from immutable history. Purge deliberately makes part of that history disappear. This page records what the code does today, the guarantee we intend, and the change that closes the gap.

## Current behaviour

**Replay re-executes against the live store.** `Engine::replay_recall` (`crates/cs-engine/src/lib.rs`, `replay_recall`) loads the trace, reuses its resolved policy and as-of times, and runs `execute_recall` again against whatever the store holds now. It reuses the recorded query vector (`query_vector`, "Exact replay: reuse the very vector the original recall used"), so the query side is unaffected. It then compares:

- the selected `(memory_id, revision_id)` list, in order, and
- the selected belief ids,

and sets `identical` only if both are equal. `added` and `removed` are the set differences of selected memory ids. Nothing else is compared, and nothing consults the purge ledger.

**Purge removes what replay would need.** A memory purge deletes every revision, its embeddings, its entity mentions and derived edges, and records `(memory, id, purged_at)` in the purge ledger (`InMemoryStore::purge_memory` in `crates/cs-storage/src/in_memory.rs`; `PgStore::purge_memory` in `crates/cs-postgres/src/lib.rs`). An observation purge does the same for derived memories that no other source supports (`purge_observation` in both stores). Recall traces are never modified: they keep memory and revision ids, component scores and, unless disabled, the query text and query vector.

**What a replay therefore reports:**

| Case | What happens | Report |
|---|---|---|
| A **selected** memory was purged | Its revisions are gone, so it cannot be a candidate. Another candidate may move into its place. | `identical: false`; the purged id is in `removed`, any replacement is in `added`. Correct outcome, but the report does not say the cause was a purge: it looks the same as a determinism bug. |
| A **non-selected candidate** was purged | The candidate set shrinks. Lexical scores are BM25 normalized over the candidate set ([recall-scoring](recall-scoring.md)), so surviving candidates' `L` can change. A smaller set can also let in a candidate that the `max_candidates` cap or a channel quota previously cut, and dropped edges change what the graph channel proposes. | If the selection happens to be unchanged, `identical: true` even though the candidate set and component scores differ. **This is a silent difference.** |
| A memory was **re-homed** by an observation purge (another source still supports it) | The old revision is closed, not deleted (`purge_observation` sets its transaction-time end), so an as-of read still sees it. | Identical, as expected. |
| An **evidence** record was purged | Belief scoring does not read evidence links (`execute_recall`, belief branch), and beliefs are never purged (`PurgeKind` has `Memory`, `Evidence`, `Observation` only). | Belief selection is unaffected. |

ADR-0014 states the first row ("reports `identical: false` when purged content had been selected"). No conformance scenario currently covers replay after a purge, so that statement is not yet tested.

## Intended guarantee

1. **Exact replay is guaranteed identical only while every revision in the original candidate set still exists** (all candidates, not only the selected ones).
2. **After a purge, replay says so instead of silently differing.** It names every original candidate whose memory is in the purge ledger, and marks which of them were selected.
3. **Any divergence not explained by a purge is a bug** (a determinism violation), and the report must make that distinguishable.
4. Replay never reconstructs, returns or logs purged content. It reports only identifiers that the trace already holds.

## The fix (not yet implemented)

1. **Classify candidates.** In `replay_recall`, collect the distinct memory ids of `original.candidates` and look them up in the purge ledger. `CognitiveStore::purged_at(kind, id)` already exists in both stores; add a batched form (for example `purged_among(PurgeKind::Memory, &[Uuid])`) so the lookup is one query, bounded by `max_candidates`.
2. **Extend `ReplayReport` additively** (new fields with `#[serde(default)]`, so `/v1` stays compatible, see [stability](../stability.md)):
   - `purged_candidates: Vec<MemoryId>`: original candidates that have since been purged;
   - `purged_selected: Vec<MemoryId>`: the subset that the original recall selected;
   - `fidelity`: `exact` (identical, nothing purged), `degraded_by_purge` (something in the candidate set was purged; `identical` may be true or false), or `diverged` (not identical and nothing purged: a determinism failure that should be investigated).
3. **Keep `identical` as it is.** It still means "same selection, same order", so existing clients keep working.
4. **Conformance scenario** `replay_after_purge_reports_purged_candidates`, run against every store:
   - purge a selected memory: `identical: false`, `purged_selected` names it, `fidelity: degraded_by_purge`;
   - purge only a non-selected candidate: `purged_candidates` names it and `fidelity: degraded_by_purge`, whatever `identical` says;
   - re-homed memory after an observation purge: `fidelity: exact`;
   - no purge: `fidelity: exact`.
5. **Update ADR-0014's consequence** to point here once the scenario passes.

## Related, out of scope here

- Traces keep the query text and query vector after a purge. A purge of a memory does not remove a query that mentioned the same content. Trace retention is a separate governance decision.
- Replay runs under the caller's `AccessContext`, not the original caller's. A different principal in the same tenant can see a different candidate set, which also shows up as a divergence; the `diverged` label should be read with that in mind.
