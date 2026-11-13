# ADR-0002: Cognitive identity, scope and isolation model

- Status: Accepted
- Date: 2026-11-13

## Context

Persistent cognition needs stronger isolation than a conversational RAG system. Knowledge belongs to users, agents, projects and conversations, and leaking it across them is a correctness failure, not a cosmetic one.

## Decision

- All identifiers are typed UUID newtypes. New values are UUID v7, generated explicitly at the application boundary.
- Every memory and belief has a `Scope`: `tenant_id`, and optionally an owner user, agent, project and conversation, plus a `Visibility` of `private`, `agent`, `project` or `tenant`.
- **Visibility is not authorization.** The caller is an `AccessContext`, and access is decided by `AccessContext::can_read` and `can_write`:
  - the tenant must match, always;
  - the owner can always read their own records;
  - `private` without an owner is readable only by the owning agent;
  - `agent` is readable by that agent, and `project` by callers authorized for the project;
  - `tenant` is readable by anyone in the tenant;
  - writes may only name principals the caller actually is.
- A scope must name the principal its visibility refers to.
- Unreadable records are reported as `not_found`.

## Alternatives

- **Implicit scope inheritance** (conversation ⊂ project ⊂ tenant): convenient, but it makes access hard to reason about. Explicit narrowing is done with `ScopeFilter` instead.

## Consequences

Storage adapters must implement the same rule inside their queries. The PostgreSQL adapter has a single `access_sql` predicate.

## Verification

Unit tests in `cs-core::scope` and the `scope_isolation` conformance scenario, run against every adapter.
