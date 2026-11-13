# ADR-0013: Governed resolution through version-bound proposals

- Status: Accepted
- Date: 2026-11-13

## Context

Accumulated knowledge collects duplicates, outdated values and contradictions. Repairing them automatically is how false memories spread. We need detection that is useful, together with change that is explicit.

## Decision

- A deterministic detector produces **`ResolutionProposal`s**, never mutations. Each proposal records:
  - its kind (duplicate, supersession or contradiction);
  - an explicit **operation**;
  - a rationale, made of the signals that matched;
  - detection metadata;
  - the exact revisions it was computed against (`based_on`).
- Detection precedence, per pair of current records in the same domain:
  1. normalized text is equal → **Duplicate** → `Merge`;
  2. structured `(subject, predicate, object)` triples are equal → **Duplicate**;
  3. same subject and predicate but a different value, for a single-valued predicate:
     - the later fact starts later in valid time → **Supersession** → `Supersede`, which closes the older fact's valid time;
     - otherwise, over overlapping valid time → **Contradiction** → `Quarantine`, which returns both facts to `candidate`;
  4. cosine similarity at or above a **per-model, per-domain** threshold → **Duplicate** candidate, but only if the **structural pre-veto** passes: the ordered sequence of numbers, versions, negations and identifiers must be identical.
- Relational predicates are multi-valued: "uses X" and "uses Y" never conflict.
- Beliefs that assert and deny the same proposition → **Contradiction** → `Dispute`. Both become `disputed`; no winner is picked.
- Records are compared only within **identical visibility** (tenant, project, owner, agent, visibility). A proposal can therefore never reveal a record to someone who could not already read it.
- **Lifecycle:** `pending` → `accepted` | `rejected` | `stale`. Acceptance requires write access to the proposal and every target. It re-checks every bound revision:
  - if any target changed, the proposal becomes **stale** and nothing is applied;
  - otherwise the operation's revisions, the decision and the events commit atomically.
  Repeating an identical decision is idempotent; a conflicting decision fails.
- **Merge is a provenance union.** The canonical record's new revision absorbs the duplicate's sources (as corroborations) and evidence, and refreshes recency; the duplicate is tombstoned.
- **Fingerprints bind exact revisions** and are unique across *all* statuses. A rejected finding is never re-proposed until one of its records actually changes.

## Alternatives

- **Auto-applying high-confidence merges.** Rejected: a false positive destroys information.
- **A single global cosine threshold.** Rejected: cosine geometry varies by model, and short templated records inflate similarity.

## Verification

- `cs-policy::resolve` unit tests.
- The calibration suite (`cs-engine/tests/resolution_calibration.rs`): hard negatives never merge.
- Conformance scenarios: `duplicate_merge_unions_provenance`, `stale_proposals_are_never_applied`, `rejected_findings_are_not_reproposed`, `contradiction_quarantines_memories` and `belief_contradictions_are_disputed`.
