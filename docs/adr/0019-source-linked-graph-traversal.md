# ADR-0019: Source-linked, bounded graph traversal

- Status: Accepted
- Date: 2026-11-14

## Context

Multi-hop traversal over a knowledge graph tends to break three properties this system depends on:

- **authorization**: an edge can be followed into data the caller cannot read;
- **time**: an edge outlives the fact that produced it;
- **cost**: one well-connected node can make a query explode.

The order in which results are returned also has to be reproducible across both stores.

## Decision

### An edge is exactly as visible as its source

Edges are projections, never independent facts. An edge is returned only when all of these hold:

- the caller can read the edge's scope (the scope of its source memory), checked **inside the storage query** before any budget is applied;
- its source revision is current at the as-of transaction time;
- the source revision's status is `active` (or `candidate`, if requested);
- its valid time contains the as-of valid time.

Tombstoning a fact hides its edges from then on. Superseding a fact replaces its edges with those of the new revision. Reads as of earlier times still see the old edges. Purging a fact removes its edges at every point in time.

### Bounded, deterministic breadth-first search

`POST /v1/entities/{id}/neighborhood` performs a breadth-first search from the entity, after first resolving it to its survivor as of the query time (ADR-0018). The request takes:

- `depth` 1–3 (default 2);
- `direction` `out` / `in` / `both`;
- an optional relation filter;
- `as_of` and `valid_at`;
- budgets: `max_nodes` (200, capped at 1,000), `max_edges` (1,000, capped at 5,000) and `max_paths` (100, capped at 1,000).

At each depth, the store returns visible edges for the whole frontier, ordered by relation (bytewise), then the other entity's id, then the edge id. Merged entities are folded into their survivors as of `as_of`. Edges are reported with survivor endpoints, and an edge whose two ends became one identity is dropped. Each entity is visited at most once, so cycles terminate: a back edge is reported but not expanded. Entities the caller cannot read are never expanded.

### Every result is explainable

The response lists the following:

- **nodes**: reached entities, ordered by id;
- **edges**: in discovery order, each with its source memory and revision;
- **paths**: the first-discovered shortest path to each reached entity, as entity ids and edge ids;
- **truncated_by**: the budgets that cut the search short, if any.

A truncated result says that it is truncated.

## Consequences

Each depth issues one edge query. The traversal resolves merge chains with per-entity lookups, which is acceptable at these budgets. A recursive SQL implementation could replace it later, provided the conformance scenarios still pass on both stores.

## Verification

- `traversal_is_bounded_and_deterministic`: a cycle terminates, results are identical across runs, `max_nodes` truncates and says so, and relation filters apply.
- `edges_follow_their_source_revisions`: tombstoned sources hide edges now but not as of earlier times.
- `entity_merges_are_governed`: as-of identity resolution in traversal.
- HTTP test `entity_graph_over_http`.
