# ADR-0020: Knowledge compilation without laundering evidence

- Status: Accepted
- Date: 2026-11-14

## Context

Generalizing from episodes ("the API called auth" on three occasions) to knowledge ("the API calls auth") is how memory becomes useful over time. It is also how weak evidence becomes strong-looking knowledge. Fifty records from one retry loop, one document or one agent run can look like broad support while amounting to one source. A generalization can also outlive the episodes that justified it.

## Decision

### One narrow, structured operator first

`recurring-episode/1` compiles episodes that carry the same structured triple into one **semantic candidate**. Free text is never generalized.

- Episodes gain an optional `subject` / `predicate` / `object` (all three or none). The triple is projected into source-linked edges like any other triple (ADR-0018).
- The **signature** is (subject entity, relation, object entity), with identity resolved through merges as of compilation time. Episodes that name the same thing differently pool their support once the names are merged.

### Support is counted in independent units

- A root is a **validated** (`active`), current episode whose source is not derived. Candidate episodes never count, and neither does anything with a `system_derived` source, so compiled memories can never support further compilation.
- A root's **origin** is where its information came from: user input, an agent run, a tool (by name, not by execution), a document (by URI) or a human annotation. A root extracted from an observation takes the observation's own source.
- A **unit** is `H(actor, origin, valid-time bucket)` (one day by default). Re-extractions, retries and duplicate records that share these keys count once.

### Thresholds

| Threshold | Default |
|---|---|
| Independent units | ≥ 3 |
| Distinct (actor, origin) pairs | ≥ 2 |
| Distinct valid-time buckets | ≥ 2 (temporal dispersion) |
| Counterexample share | ≤ 10% |

A **counterexample** is a validated episode with the same subject and relation but a different object. One contradicting unit against three supporting ones (25%) blocks compilation. This is conservative by design: a recurring transient state is not a timeless fact.

### Compilation proposes; promotion decides

The compiled memory starts as a `candidate` with a `system_derived` source and the operator recorded as its extraction. Like any candidate, it is excluded from recall until someone promotes it explicitly.

### A derivation record, and revalidation by recompilation

A `Derivation` stores:

- the operator and the thresholds it applied;
- the support it found (units, origins, buckets, counterexamples);
- one **witness** per unit (root memory and revision ids, plus opaque unit and origin keys);
- **every** supporting root memory, so it can be looked up later.

It stores no content and no names.

**Revalidation is recompiling the signature from scratch**, so compiling and revalidating cannot diverge. It runs when:

- **a root changes.** Any new revision of an episode (including a tombstone) queues a compile job, and that job recompiles every derivation that cites the memory.
- **a root is purged.** The purge transaction itself flags citing derivations as `revalidation_required`. The engine then queues revalidation jobs. A compiled memory can therefore never silently keep support it lost.
- **on request**, via `POST /v1/derivations/{id}/revalidate`.

Outcomes:

- **Support still holds:** the derivation stays `active`. If it was invalidated, it returns to `active`, but its memory stays `candidate`; trust is never restored automatically.
- **Support falls below the thresholds:** the derivation becomes `invalidated` with a reason. Its compiled memory, if it had been promoted, is revised back to `candidate`. It is demoted, not deleted, and its history is preserved.
- **The compiled memory is purged:** its derivation is deleted with it.

### Concurrency

Derivation writes are version-checked inside the job's commit, and a demotion requires the compiled memory's revision to still be current. If another job got there first, the job **retries** rather than failing. Every compile job recomputes from the full current state, so the last writer is always correct.

## Consequences

- Feedback loops are only partly closed. A compiled memory cannot support compilation, but an agent that acts on a promoted compiled memory and writes new episodes from those actions could reinforce it. Tracking which beliefs an episode was conditioned on is future work.
- Extraction confidence is not weighted yet. Unvalidated extracted episodes are excluded outright instead.

## Verification

- `recurring_episodes_compile_to_candidates`: two units are not enough and three are. The output is a semantic candidate with derived provenance. Recompiling unchanged support writes nothing. Other users see nothing.
- `correlated_or_contradicted_support_does_not_compile`: correlated records, a counterexample and unvalidated episodes each block compilation (each would compile without its guard).
- `losing_support_demotes_compiled_knowledge`: a tombstone invalidates the derivation and demotes the promoted memory. New support restores the derivation but not trust. A purge flags the derivation atomically before the revalidation job runs. Purging the compiled memory removes its derivation.
- `compilation_follows_entity_merges`: support pools across names once they are merged.
- HTTP test `compilation_over_http`.
