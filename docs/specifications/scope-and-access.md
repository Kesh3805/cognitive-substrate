# Scope and access

## Read rule (`AccessContext::can_read`)

```
tenant(scope) == tenant(caller)
AND (
     owner(scope) IS NOT NULL AND owner(scope) == user(caller)
  OR visibility == tenant
  OR visibility == private AND owner(scope) IS NULL AND agent(scope) == agent(caller)
  OR visibility == agent   AND agent(scope) == agent(caller)
  OR visibility == project AND project(scope) ∈ projects(caller)
)
```

Every storage adapter evaluates exactly this predicate inside its queries. In PostgreSQL this is `cs_postgres::access_sql`, applied to both the record columns and event scope JSON.

## Write rule (`AccessContext::can_write`)

The caller may only write into a scope that:

- has the caller's tenant;
- names no owner other than the caller's user;
- names no agent other than the caller's agent;
- names only projects the caller is authorized for;
- passes the read rule.

The scope must also be structurally valid. `private` needs an owner or agent, `agent` needs an agent, and `project` needs a project.

## Errors

Records the caller cannot read are reported as `not_found`, so that scoped data is not revealed to exist. Attempts to write into a foreign scope return `forbidden`.

## Recall traces

A recall trace is not a scoped record: it belongs to the principal that ran the recall. `RecallTrace::readable_by` requires the same tenant, the same user and the same agent, and at least one of them to be recorded. Everyone else, including callers who can read every memory the trace names, gets `not_found` from reads and replays. See [replay and purge](replay-and-purge.md#who-may-replay).

## Narrowing

`ScopeFilter` (project, conversation, agent) can only narrow results the caller is already authorized to see.
