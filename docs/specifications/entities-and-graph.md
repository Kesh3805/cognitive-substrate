# Entities and the knowledge graph

Normalization version `1`, projection version `1`, merge detector `entity-stem/1`.

## Normalized keys

`normalize_entity_name(name)`:

1. Trim the name. Insert a word boundary at lower→Upper and digit→Upper transitions, and before the last capital of an acronym that is followed by a lowercase letter (`HTTPServer` → `HTTP Server`).
2. Keep `+` and `#` as they are, and keep a `.` that sits between two digits.
3. Turn other separators (`-`, `_`, `.`, `/`, `:`, `\`) into word boundaries. Drop every other non-alphanumeric character. Lowercase everything.
4. If more than one word remains and the first is `the`, drop it.
5. Join the words with no separator. An empty result means "no name".

| Input | Key |
|---|---|
| `auth-service`, `AuthService`, `The Auth Service` | `authservice` |
| `PostgreSQL` | `postgresql` |
| `C`, `C++`, `C#` | `c`, `c++`, `c#` |
| `v1.23`, `v12.3` | `v1.23`, `v12.3` |
| `Node.js` | `nodejs` |
| `A record` | `arecord` |

## Identity partition

`partition_key(scope)` = `{tenant}|p:{project}|u:{owner}|a:{agent}|v:{visibility}`. An absent component is `-`. The conversation is not part of it. Entity ids are `uuid5(ENTITY_NAMESPACE, "{partition}\u{1f}{key}")`.

## Relations

`normalize_relation(predicate)` lowercases the predicate and joins its alphanumeric words with `_` (`depends on` → `depends_on`). A predicate whose relation would exceed 64 characters produces no edge.

## Merge candidates (`entity-stem/1`)

`identity_stem(key)` removes the first suffix from this list that leaves at least three characters: `service`, `svc`, `inc`, `corp`, `corporation`, `ltd`, `llc`, `app`, `co`. Two active entities in the same partition are candidates when their stems are equal. The survivor is the entity with the shorter key; on a tie, the lower id wins.

## API

| Method | Path | Body / query | Effect |
|---|---|---|---|
| GET | `/v1/entities?name=` | | Entities the caller can read whose names normalize to the key |
| GET | `/v1/entities/{id}` | | Entity, plus aliases and the 50 most recent mentions (including those of merged entities) |
| POST | `/v1/entities/{id}/aliases` | `{ name, expected_version }` | Add an explicit alias |
| POST | `/v1/entities/{id}/kind` | `{ kind, expected_version }` | Set the kind |
| POST | `/v1/entities/{id}/merge-candidates` | | Record `entity_merge` proposals for stem matches |
| POST | `/v1/entity-merges` | `{ survivor, merged, reason }` | Record an explicit `entity_merge` proposal |
| POST | `/v1/entities/{id}/neighborhood` | `NeighborhoodRequest` | Bounded traversal (ADR-0019) |

Merge proposals are decided with the existing `/v1/proposals/{id}/accept` and `/reject` endpoints.

Errors: an unreadable entity is `not_found`. A version mismatch, or a name already taken in the partition, is `conflict`. Merging across partitions is `validation_failed`.

## Events

`EntityAliasAdded`, `EntityKindChanged` and `EntitiesMerged` go on the entity's stream. `ProjectionUpdated` goes on the memory's stream and carries counts only. No event payload contains an entity name.
