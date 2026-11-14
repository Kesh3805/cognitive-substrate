# ADR-0018: Entities and governed identity

- Status: Accepted
- Date: 2026-11-14

## Context

Memories name things: services, people, teams, technologies. To traverse relationships, those names must resolve to shared identities. Identity is also where silent errors do the most damage. A wrong automatic link merges two things for every future read, and an in-place merge rewrites history that earlier reads depended on.

## Decision

### Mentions are not identity

A **projection job** (`project`) turns the structured fields of one memory revision into **mentions** (a name, its role and its revision) and **edges** (subject → normalized relation → object). It never parses free text. Sources:

| Memory | Mentions | Edges |
|---|---|---|
| relational | subject, object | subject —predicate→ object |
| semantic with a full triple | subject, object | subject —predicate→ object |
| temporal | subject, value | subject —predicate→ value |
| episodic | participants; subject and object when the episode has a triple | subject —predicate→ object, when present |

Every write path plans its follow-ups in one place, so the projection job commits atomically with the revision that needs it. The job is idempotent per revision, and it skips revisions that were purged before it ran.

### Exact keys only, within a partition

A name resolves by **exact normalized key** (`normalize_entity_name`, version 1) within an **identity partition**, which is the tuple tenant, project, owner, agent and visibility. Normalization folds case and separators (`auth-service` = `AuthService` = `Auth Service`), drops a leading "the", and keeps symbols that carry meaning (`C`, `C++` and `C#` stay distinct, and so do `v1.23` and `v12.3`). A new key creates an entity with a deterministic id, `uuid5(partition, key)`, and a canonical alias.

The partition includes owner, agent and visibility because an entity shared across read scopes would leak that a name exists to someone who cannot read any memory mentioning it. The cost is that the same name in a private memory and in a project memory gives two entities. Linking them across partitions is a future, explicit operation; it never happens implicitly.

### Everything looser is a decision

- **Aliases** are added explicitly (`POST /v1/entities/{id}/aliases`). The call is version-checked and evented. It is refused if the name already identifies another entity in the partition, and it is capped at 64 aliases per entity so that no entity absorbs a partition's vocabulary.
- **Kinds** start as `unknown` and are set explicitly. Extraction never guesses a kind.
- **Merges** are resolution proposals (`entity_merge`) bound to both entities' versions. They can be explicit (`POST /v1/entity-merges`) or detected (`POST /v1/entities/{id}/merge-candidates`). The detector (`entity-stem/1`) pairs names whose keys share a stem after removing one corporate or service suffix (`auth` / `auth-service`). The shorter key survives, and ties go to the lower id. Accepting a merge re-checks both versions inside the commit. If either changed, the proposal becomes `stale`. Concurrent opposite merges cannot form a cycle: each one requires both entities to be active, and both rows are locked in id order.

### Merges are redirects, not rewrites

Accepting a merge records `merged_into` and `merged_at` on the merged entity and bumps both versions. **Nothing else moves.** Mentions, aliases and edges stay attached to the entity they were resolved to. Reads resolve identity **as of their transaction time**: a merge at time *T* folds the two entities together for reads as of *T* or later. Reads before *T* still see two entities, each with its own edges. Merges are therefore not retroactive. Undoing one is possible in principle (close the redirect) without reconstructing anything.

### Purges reach identity rows

Purging a memory removes its mentions and edges (they cascade with the revision). An entity left with no mentions and no edges is also deleted, together with its aliases, if it was never curated. Version 1 means no alias, kind or merge decision was ever made on it. This way a purged name does not survive in identity rows. Curated entities remain, since they are governed decisions. Entity names never appear in event payloads.

## Consequences

- Automatic linking has high precision and lower recall: spelling variants beyond the normalization fold become separate entities until someone aliases or merges them. This is deliberate, because a missed link can be repaired later and a wrong one cannot be undone cleanly.
- A merge proposal is bound to entity versions, not to the edges around the entities. A new edge on either entity does not make the proposal stale, because the question "are these the same thing" is about identity, not topology.

## Verification

Conformance scenarios on both stores:

- `relations_project_into_entities_and_edges`: name variants resolve to one entity; edges name their source revisions; another user sees nothing.
- `aliases_and_kinds_are_version_checked`: stale versions conflict; a name can identify only one entity; later mentions of an alias resolve to its entity.
- `entity_merges_are_governed`: detection is idempotent, a merge folds both names together from then on, reads as of before the merge still see two entities, and a version change makes a pending merge stale.
- `edges_follow_their_source_revisions`: purging removes edges at every point in time and deletes uncurated orphan entities.
