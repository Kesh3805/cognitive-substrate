# cognitive-substrate (Python)

A small, typed Python client for the [Cognitive Substrate](../../README.md) HTTP API (`/v1`). It has a synchronous `Client` and an `AsyncClient` with the same methods. Both are built on [`httpx`](https://www.python-httpx.org/), which is the only runtime dependency.

- Python 3.10 or newer.
- Responses come back as parsed JSON (`dict` or `list`) exactly as the server sent them. The SDK adds no fields and wraps nothing in models.
- Request bodies are typed with `TypedDict`s in `cognitive_substrate.types`, so plain dicts work.
- Non-2xx responses raise `ApiError`, which carries `status`, `code` and `message`.

## Install

The package is not on PyPI yet. Install it from a checkout:

```bash
python -m pip install -e sdks/python          # from the repository root
python -m pip install -e "sdks/python[test]"  # with the test dependencies
```

## Quick start

Start the service and its worker against PostgreSQL:

```bash
docker compose -f deploy/compose.yaml up --build
```

The API listens on `http://localhost:8080`. Every call names a tenant, and usually a user, by UUID. If the server sets `CS_API_KEY`, pass the same value as `api_key=`.

```python
import uuid
from cognitive_substrate import Client

TENANT, ME = uuid.uuid4(), uuid.uuid4()

with Client("http://localhost:8080", tenant=TENANT, user=ME) as cs:
    print(cs.health())   # {'status': 'ok', 'version': '...'}
    cs.ready()           # raises ApiError(503, 'not_ready', ...) until storage is up
```

`AsyncClient` takes the same arguments: `async with AsyncClient(...) as cs: await cs.health()`.

### Remember, recall, replay

```python
with Client(tenant=TENANT, user=ME) as cs:
    memory = cs.create_memory(
        cs.private_scope(),  # {"tenant_id": TENANT, "owner_user_id": ME, "visibility": "private"}
        {"domain": "semantic", "data": {
            "proposition": "The project uses PostgreSQL as its authoritative store.",
            "subject": "cognitive-substrate", "predicate": "uses", "object": "PostgreSQL",
        }},
        {"source": {"type": "user_input"}},
    )

    # Optimistic concurrency: fails with ApiError(409, "conflict") if someone revised it first.
    cs.revise_memory(memory["id"], reason="clarified", expected_revision=memory["revision_id"])
    assert len(cs.memory_history(memory["id"])["revisions"]) == 2

    hits = cs.recall("Why did we choose PostgreSQL?", policy={"max_results": 5})
    for r in hits["results"]:
        print(r["rank"], round(r["score"], 3), r["memory"]["content"]["data"]["proposition"])

    trace = cs.get_trace(hits["trace_id"])            # why each candidate was or was not selected
    exact = cs.replay(hits["trace_id"])               # exact replay: same revisions, same order
    assert exact["identical"]
    what_if = cs.replay(hits["trace_id"], policy_override={"min_score": 0.9})
    print(what_if["added"], what_if["removed"])       # counterfactual replay
```

### Evidence and beliefs

```python
from cognitive_substrate import ApiError

with Client(tenant=TENANT, user=ME) as cs:
    ev = cs.record_evidence(
        {"type": "tool_output", "tool": "bench", "execution_id": "run-7"},
        summary="p95 latency 41ms",
    )
    latency = cs.create_belief(
        cs.private_scope(),
        "p95 latency is under 50ms",
        {"value": 0.7, "interpretation": "heuristic_score", "method": "bench"},
        evidence=[{"evidence_id": ev["id"], "direction": "supports"}],
    )
    caching = cs.create_belief(
        cs.private_scope(),
        "caching is unnecessary",
        {"value": 0.5, "interpretation": "human_assessment", "method": "review"},
    )
    cs.add_dependency(latency["id"], caching["id"], "supports")

    out = cs.revise_belief(
        latency["id"],
        reason="re-measured",
        confidence={"value": 0.9, "interpretation": "heuristic_score", "method": "bench-v2"},
    )
    print(out["needs_reevaluation"])  # [caching["id"]]: flagged for review, never auto-retracted

    view = cs.get_belief(latency["id"])  # belief + evidence + depends_on + dependents
    print(cs.debt()["items"])            # e.g. unsupported beliefs

    try:
        cs.get_belief(uuid.uuid4())
    except ApiError as e:
        print(e.status, e.code, e.message)  # 404 not_found ...
```

### The prediction gate

A supported causal hypothesis becomes a rule for one context only where that context's own randomized evidence supports it. The gate fails closed. See [docs/specifications/predictions.md](../../docs/specifications/predictions.md). These endpoints are **experimental**.

```python
with Client(tenant=TENANT, user=ME) as cs:
    scope = cs.private_scope()
    hyp = cs.create_hypothesis(scope, "deploy@1", "warm-cache", "fast", "raises", min_effect=0.1)

    # Randomized interventions are recorded at intent, then their outcomes observed once.
    for i in range(40):
        arm = "treated" if i % 2 == 0 else "control"
        iv = cs.record_intervention(
            scope, "deploy@1", "warm-cache", arm, "randomized", "c0",
            assignment_detail=f"seed {i}", propensity=0.5,
        )
        cs.observe_outcome(iv["id"], "fast", arm == "treated", "deterministic")

    hyp = cs.evaluate_hypothesis(hyp["id"], expected_version=hyp["version"])
    print(hyp["status"])  # "supported" once there is enough randomized evidence

    view = cs.prediction_rule(hyp["id"], "c0", threshold=0.8)
    if view["gate"]["execute"]:
        # Act, and record the prediction before the outcome is known.
        iv = cs.record_intervention(scope, "deploy@1", "warm-cache", "treated", "passive", "c0")
        p = cs.predict(iv["id"], hyp["id"])
        print(p["result"])  # "recorded" (HTTP 201) or "refused" (HTTP 200, nothing written)
    else:
        print(view["result"], view["gate"])  # e.g. "refused", {"execute": False, "refusal": "out_of_scope", ...}

    # Effect gate: act only if the treated-minus-control effect clears min_effect.
    cs.prediction_rule(hyp["id"], "c0", threshold=0.0, min_effect=0.5)
```

## Reference

| Area | Methods |
|---|---|
| Service | `health`, `ready` |
| Memories | `create_memory`, `get_memory`, `memory_history`, `revise_memory`, `promote_memory`, `reject_memory`, `tombstone_memory`, `purge_memory` |
| Recall | `recall`, `get_trace`, `replay` (exact, or counterfactual with `policy_override=`) |
| Evidence & beliefs | `record_evidence`, `purge_evidence`, `create_belief`, `get_belief`, `belief_history`, `revise_belief`, `retract_belief`, `add_dependency` |
| Inspection | `events`, `debt` |
| Observations & proposals | `submit_observation`, `get_observation`, `purge_observation`, `list_jobs`, `get_job`, `list_proposals`, `get_proposal`, `accept_proposal`, `reject_proposal` |
| Hypotheses & interventions | `create_hypothesis`, `list_hypotheses`, `get_hypothesis`, `hypothesis_history`, `evaluate_hypothesis`, `retire_hypothesis`, `record_intervention`, `get_intervention`, `observe_outcome`, `counterfactual` |
| Predictions | `predict`, `prediction_rule` |
| Helpers | `private_scope()`, `scope(visibility, **ids)`, `request(method, path, json=, params=)` |

Each method's docstring names its route. For routes without a helper (entities, derivations, capabilities, information needs, processing status), use `request`:

```python
cs.request("GET", "/v1/entities", params={"name": "auth"})
cs.request("POST", f"/v1/entities/{entity_id}/neighborhood", json={"direction": "in", "depth": 1})
```

### Client options

| Argument | Meaning |
|---|---|
| `base_url` | Defaults to `http://localhost:8080`. A path prefix such as `https://gw/cs` is kept. |
| `tenant` | Tenant UUID, sent as `x-cs-tenant` (required). |
| `user` | User UUID, sent as `x-cs-user`. |
| `agent`, `projects` | Sent as `x-cs-agent` and comma-separated `x-cs-projects`. |
| `api_key` | Sent as `Authorization: Bearer <key>`. |
| `timeout` | Seconds or an `httpx.Timeout`. Defaults to 30. |
| `headers`, `transport` | Extra headers. An `httpx` transport, for example `httpx.MockTransport` in tests. |

UUID arguments accept `str` or `uuid.UUID`. Timestamps accept RFC 3339 strings or timezone-aware `datetime`s. Arguments left as `None` are omitted, so the server's defaults apply.

### Errors

The server always answers errors as `{"error": {"code": "...", "message": "..."}}`. `ApiError` exposes them with the HTTP `status`, plus `body` for the raw payload. Common codes are `invalid_request` (400), `unauthorized` (401), `forbidden` (403), `not_found` (404), `conflict` (409), `validation_failed` (422) and `not_ready` (503). A body without that envelope, such as a proxy error page, yields `code == "unexpected_response"`. Connection failures and timeouts are not wrapped. They surface as `httpx` exceptions.

## Development

```bash
cd sdks/python
python -m venv .venv && .venv/bin/python -m pip install -e ".[test]"   # .venv\Scripts\python on Windows
.venv/bin/python -m pytest
```

The tests use `httpx.MockTransport` and never touch the network. They check every method's route, query, body and headers against the shapes in `crates/cs-server/tests/http.rs`, for both clients.

## License

Apache-2.0, like the rest of the repository.
