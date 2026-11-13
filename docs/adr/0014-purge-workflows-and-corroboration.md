# ADR-0014: Purge workflows, the purge ledger and corroboration

- Status: Accepted
- Date: 2026-11-13

## Context

Deleting a source must erase what was derived from it. It must not destroy facts that other sources independently support, and it must leave an audit trail without retaining content.

## Decision

- **Purge ledger.** Every purge of a memory, evidence record or observation records `(kind, id, purged_at)`, never content. Purges are idempotent: repeating one returns `already_purged` with the original time.
- **Evidence purge** deletes the evidence and its belief links in one transaction. Live beliefs that lost a link get `belief_reevaluation_requested`, and they surface as *unsupported* cognitive debt if no evidence remains. They are not retracted automatically.
- **Observation purge:**
  1. erases the observation's content (its hash is retained);
  2. finds every memory derived from it: the ones its extraction runs created, plus any whose current provenance names it as source or corroboration;
  3. decides what survives:
     - if another unpurged source still stands behind a memory, the memory gets a new revision that drops the purged source (re-homing its primary source if needed);
     - otherwise all of its revisions and embeddings are erased.
- A purge that races an in-flight extraction cannot be undone by it, because the extraction's effects carry a read-skew guard (ADR-0011).
- Event payloads contain identifiers and hashes only (ADR-0006), so purges never conflict with the append-only log.

## Consequences

- Exact replay of a recall made before a purge reports `identical: false` when purged content had been selected. The original trace still holds the original component scores.
- BM25 statistics over the candidate set naturally reflect the purge.

## Verification

Conformance scenarios: `evidence_purge_flags_beliefs`, `observation_purge_keeps_corroborated_facts` and `purge_during_extraction_discards_effects`.
