# Replay after purge

Exact replay (ADR-0008) and hard purge (ADR-0010, ADR-0014) pull against each other. Replay promises to reproduce a past recall from immutable history. Purge deliberately makes part of that history disappear. This page records what replay does, the guarantee it gives after a purge, and how the report makes a purge visible.

## How replay works

**Replay re-executes against the live store.** `Engine::replay_recall` (`crates/cs-engine/src/lib.rs`, `replay_recall`) loads the trace (only the principal that ran the recall may, see [Who may replay](#who-may-replay)), reuses its resolved policy and as-of times, and runs `execute_recall` again against whatever the store holds now. It reuses the recorded query vector (`query_vector`, "Exact replay: reuse the very vector the original recall used"), so the query side is unaffected. It then compares:

- the selected `(memory_id, revision_id)` list, in order, and
- the selected belief ids,

and sets `identical` only if both are equal. `added` and `removed` are the set differences of selected memory ids.

**Purge removes what replay would need.** A memory purge deletes every revision, its embeddings, its entity mentions and derived edges, and records `(memory, id, purged_at)` in the purge ledger (`InMemoryStore::purge_memory` in `crates/cs-storage/src/in_memory.rs`; `PgStore::purge_memory` in `crates/cs-postgres/src/lib.rs`). An observation purge does the same for derived memories that no other source supports (`purge_observation` in both stores). Recall traces are never modified: they keep memory and revision ids, component scores and, unless disabled, the query text and query vector.

**Replay consults the purge ledger.** After re-executing, replay collects the distinct memory ids of the original trace's candidates (selected or not) and looks them up in the purge ledger in one batched call, `CognitiveStore::purged_among(PurgeKind::Memory, &ids)`, bounded by `max_candidates`. Both stores implement it (`InMemoryStore`, and `PgStore` with a single `record_id = ANY($2)` query).

## The report

`ReplayReport` carries three additive fields (`#[serde(default)]`, so `/v1` clients that ignore them keep working, see [stability](../stability.md)):

- `purged_candidates: Vec<MemoryId>`: original candidates whose memory has since been purged, in original rank order;
- `purged_selected: Vec<MemoryId>`: the subset that the original recall selected;
- `fidelity`, one of:
  - `exact`: identical, and nothing in the original candidate set was purged;
  - `degraded_by_purge`: something in the original candidate set was purged; `identical` may be true or false;
  - `diverged`: not identical, and nothing was purged.

`identical` keeps its meaning: same selection, same order.

| Case | What happens | Report |
|---|---|---|
| No purge, same selection | | `identical: true`, `fidelity: exact`. |
| A **selected** memory was purged | Its revisions are gone, so it cannot be a candidate. Another candidate may move into its place. | `identical: false`; the purged id is in `removed`, `purged_candidates` and `purged_selected`; `fidelity: degraded_by_purge`. |
| A **non-selected candidate** was purged | The candidate set shrinks. Lexical scores are BM25 normalized over the candidate set ([recall-scoring](recall-scoring.md)), so surviving candidates' `L` can change. A smaller set can also let in a candidate that the `max_candidates` cap or a channel quota previously cut, and dropped edges change what the graph channel proposes. | The id is in `purged_candidates` and `fidelity: degraded_by_purge`, whatever `identical` says. Never silently `exact`. |
| A memory was **re-homed** by an observation purge (another source still supports it) | The old revision is closed, not deleted (`purge_observation` sets its transaction-time end), so an as-of read still sees it. The memory itself is not in the purge ledger. | Identical, `fidelity: exact`. |
| An **evidence** record was purged | Belief scoring does not read evidence links (`execute_recall`, belief branch), and beliefs are never purged (`PurgeKind` has `Memory`, `Evidence`, `Observation` only). | Belief selection is unaffected. |
| Not identical, nothing purged | | `fidelity: diverged`. |

## Guarantee

1. **Exact replay is guaranteed identical only while every revision in the original candidate set still exists** (all candidates, not only the selected ones).
2. **After a purge, replay says so instead of silently differing.** It names every original candidate whose memory is in the purge ledger, and marks which of them were selected.
3. **Any divergence not explained by a purge is reported as `diverged`.** For an exact replay that is a determinism failure to investigate. For a counterfactual replay (`counterfactual: true`) it is the expected effect of the substituted policy.
4. Replay never reconstructs, returns or logs purged content. It reports only identifiers that the trace already holds.

## Who may replay

A recall trace holds the query text, the query vector and the ids and component scores of every candidate the caller could read, so it is as sensitive as the query and the scopes behind it. Traces record the principal that ran the recall (`owner_user_id`, `owner_agent_id`, taken from the caller's `AccessContext`), and `get_trace` and `replay_recall` return `not_found` to anyone else (invariant 32, `RecallTrace::readable_by`):

- **same principal only**: same tenant, same user and same agent. A user acting through an agent is a different principal from the user alone. There is no tenant-wide or service principal that reads all traces: `AccessContext` has no such role.
- **no owner, no reader**: a trace that names neither a user nor an agent is readable by no one. That covers traces recorded before owners were kept (migration `20261127000015_trace_owner.sql` adds nullable columns and does not backfill them) and recalls by a caller that identified only its tenant. Opening such traces to the whole tenant would leak ids and scores from scopes other members cannot read.

Because replay re-runs under the caller's `AccessContext`, and only the original principal may replay, the candidate set differs from the original only if that principal's own access changed (for example, its `project_ids`). That also shows up as `diverged` or as removed ids.

## Verification

Conformance scenarios, run against every store: `replay_after_purge_reports_purged_candidates` (no purge, a non-selected candidate purged, a selected memory purged) and `traces_are_readable_only_by_their_principal`. The HTTP test `recall_traces_belong_to_their_principal` covers the same rule over `/v1`.

## Related, out of scope here

- Traces keep the query text and query vector after a purge. A purge of a memory does not remove a query that mentioned the same content. Trace retention is a separate governance decision.
- The re-homed case is argued above from `purge_observation`; no scenario replays across an observation purge yet.
