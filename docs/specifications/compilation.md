# Knowledge compilation — operator `recurring-episode/1`

## Inputs

An episode with all of `subject`, `predicate` and `object` set is projected into an edge `subject —relation→ object` (see [entities & graph](entities-and-graph.md)). After projection, a `compile` job runs for that revision.

## Signature

`signature = "{subject_entity}|{relation}|{object_entity}"`. Both entities are canonical: merge redirects are followed as of the compilation time. Subject and object must differ.

## Roots

For a signature, the compiler reads the subject's outgoing edges with that relation. It includes entities merged into the subject as of now, considers any valid time, and caps the read at 5,000 edges. It keeps an edge's source memory as a root when all of these hold:

- the source revision is current and `active`;
- the memory is episodic;
- `origin(source)` is defined. An `observation` source is replaced by the observation's own source, and `system_derived` sources are excluded.

The root **supports** the signature if the edge's canonical object equals the signature's object. Otherwise it is a **counterexample**.

| Source | Origin |
|---|---|
| `user_input` | `user_input` |
| `agent_output` | `agent_run:{run_id}` |
| `tool_output` | `tool:{tool}` (the execution id is ignored) |
| `external_document` | `document:{uri}` |
| `human_annotation` | `human_annotation` |
| `system_derived` | none — never support |

- `bucket = floor(unix(valid_time.start) / bucket_seconds)`
- `origin_key = uuid5(NS, "{actor}\u{1f}{origin}")`
- `unit_key = uuid5(NS, "{actor}\u{1f}{origin}\u{1f}{bucket}")`

## Support and thresholds

There is one witness per distinct `unit_key`: the root with the lowest revision id. Witnesses are ordered by `(bucket, unit_key)`.

- `units` = number of witnesses;
- `origins` = number of distinct `origin_key`s;
- `buckets` = number of distinct buckets;
- `counter_units` = number of distinct `unit_key`s among counterexamples.

Support **meets** the thresholds when all of these hold:

- `units ≥ min_units` (3);
- `origins ≥ min_origins` (2);
- `buckets ≥ min_buckets` (2);
- `counter_units / (units + counter_units) ≤ max_counter_ratio` (0.1).

## Outcomes

| Existing derivation | Meets | Result |
|---|---|---|
| none | no | nothing |
| none | yes | New `candidate` semantic memory (`subject`, `predicate` = relation with spaces, `object`; valid from the earliest root), and a derivation `active` v1 |
| any | yes | Derivation `active`; written only if its support, witnesses, roots, status or signature changed |
| any | no | Derivation `invalidated` with a reason; its memory is revised to `candidate` if it was `active` |

A derivation whose names were merged into a signature that already has its own derivation is invalidated in favour of that one.

## Events

`DerivationRecorded` and `DerivationUpdated` go on the derivation's stream. Their payloads carry ids, status, support counts and witness revision ids, never names or content. `MemoryCreated` and `MemoryRevised` go on the compiled memory's stream.

## API

| Method | Path | Effect |
|---|---|---|
| GET | `/v1/derivations?status=&limit=` | Derivations the caller can read, newest first |
| GET | `/v1/derivations/{id}` | One derivation |
| POST | `/v1/derivations/{id}/revalidate` | Queue a recompilation (202) |
