"""Every client method: HTTP method, path, query and JSON body, for both the
sync and the async client. Bodies mirror crates/cs-server/tests/http.rs."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Optional
from uuid import UUID

import httpx
import pytest

from cognitive_substrate import AsyncClient, Client

from .conftest import BASE, TENANT, USER, Recorder

MEM = "0190a1b2-0000-7000-8000-0000000000a1"
REV = "0190a1b2-0000-7000-8000-0000000000a2"
TRACE = "0190a1b2-0000-7000-8000-0000000000a3"
EV = "0190a1b2-0000-7000-8000-0000000000a4"
BEL = "0190a1b2-0000-7000-8000-0000000000a5"
BEL2 = "0190a1b2-0000-7000-8000-0000000000a6"
OBS = "0190a1b2-0000-7000-8000-0000000000a7"
PROP = "0190a1b2-0000-7000-8000-0000000000a8"
JOB = "0190a1b2-0000-7000-8000-0000000000a9"
HYP = "0190a1b2-0000-7000-8000-0000000000b1"
INT = "0190a1b2-0000-7000-8000-0000000000b2"

SCOPE = {"tenant_id": TENANT, "owner_user_id": USER, "visibility": "private"}
CONTENT = {
    "domain": "semantic",
    "data": {"proposition": "The project uses PostgreSQL as its authoritative store."},
}
PROV = {"source": {"type": "user_input"}}
CONF = {"value": 0.7, "interpretation": "heuristic_score", "method": "bench"}
NO_BODY = object()


@dataclass
class Case:
    name: str
    args: tuple[Any, ...]
    method: str
    path: str
    kwargs: dict[str, Any] = field(default_factory=dict)
    params: Optional[dict[str, str]] = None
    body: Any = NO_BODY

    def __str__(self) -> str:
        return self.name


CASES = [
    Case("health", (), "GET", "/v1/health"),
    Case("ready", (), "GET", "/v1/ready"),
    # memories
    Case(
        "create_memory",
        (SCOPE, CONTENT, PROV),
        "POST",
        "/v1/memories",
        body={"scope": SCOPE, "content": CONTENT, "provenance": PROV},
    ),
    Case(
        "create_memory",
        (SCOPE, CONTENT, PROV),
        "POST",
        "/v1/memories",
        kwargs={
            "valid_time": {"start": datetime(2026, 5, 1, 12, tzinfo=timezone.utc)},
            "expires_at": "2027-01-01T00:00:00Z",
            "idempotency_key": "k1",
        },
        body={
            "scope": SCOPE,
            "content": CONTENT,
            "provenance": PROV,
            "valid_time": {"start": "2026-05-01T12:00:00+00:00"},
            "expires_at": "2027-01-01T00:00:00Z",
            "idempotency_key": "k1",
        },
    ),
    Case("get_memory", (MEM,), "GET", f"/v1/memories/{MEM}"),
    Case("memory_history", (MEM,), "GET", f"/v1/memories/{MEM}/history"),
    Case(
        "revise_memory",
        (MEM,),
        "POST",
        f"/v1/memories/{MEM}/revisions",
        kwargs={"reason": "clarified", "expected_revision": UUID(REV)},
        body={"reason": "clarified", "expected_revision": REV},
    ),
    Case(
        "revise_memory",
        (MEM,),
        "POST",
        f"/v1/memories/{MEM}/revisions",
        kwargs={"reason": "fix", "content": CONTENT, "provenance": PROV},
        body={"reason": "fix", "content": CONTENT, "provenance": PROV},
    ),
    Case("promote_memory", (MEM, "ok"), "POST", f"/v1/memories/{MEM}/promote", body={"reason": "ok"}),
    Case("reject_memory", (MEM, "no"), "POST", f"/v1/memories/{MEM}/reject", body={"reason": "no"}),
    Case(
        "tombstone_memory",
        (MEM, "gone"),
        "POST",
        f"/v1/memories/{MEM}/tombstone",
        body={"reason": "gone"},
    ),
    Case("purge_memory", (MEM, "gdpr"), "POST", f"/v1/memories/{MEM}/purge", body={"reason": "gdpr"}),
    # recall
    Case(
        "recall",
        ("Why did we choose PostgreSQL?",),
        "POST",
        "/v1/recall",
        kwargs={"policy": {"max_results": 5}, "session": "s1"},
        body={"query": "Why did we choose PostgreSQL?", "policy": {"max_results": 5}, "session": "s1"},
    ),
    Case("recall", ("q",), "POST", "/v1/recall", body={"query": "q"}),
    Case("get_trace", (TRACE,), "GET", f"/v1/recall-traces/{TRACE}"),
    Case("replay", (TRACE,), "POST", f"/v1/recall-traces/{TRACE}/replay"),
    Case(
        "replay",
        (TRACE,),
        "POST",
        f"/v1/recall-traces/{TRACE}/replay",
        kwargs={"policy_override": {"min_score": 0.5}},
        body={"policy_override": {"min_score": 0.5}},
    ),
    # evidence & beliefs
    Case(
        "record_evidence",
        ({"type": "tool_output", "tool": "bench", "execution_id": "run-7"},),
        "POST",
        "/v1/evidence",
        kwargs={"summary": "p95 latency 41ms"},
        body={
            "source": {"type": "tool_output", "tool": "bench", "execution_id": "run-7"},
            "summary": "p95 latency 41ms",
        },
    ),
    Case("purge_evidence", (EV, "withdrawn"), "POST", f"/v1/evidence/{EV}/purge", body={"reason": "withdrawn"}),
    Case(
        "create_belief",
        (SCOPE, "p95 latency is under 50ms", CONF),
        "POST",
        "/v1/beliefs",
        kwargs={"evidence": [{"evidence_id": UUID(EV), "direction": "supports"}]},
        body={
            "scope": SCOPE,
            "proposition": "p95 latency is under 50ms",
            "confidence": CONF,
            "evidence": [{"evidence_id": EV, "direction": "supports"}],
        },
    ),
    Case(
        "create_belief",
        (SCOPE, "x", CONF),
        "POST",
        "/v1/beliefs",
        kwargs={
            "polarity": "refutes",
            "depends_on": ({"belief_id": BEL, "relation": "requires"},),
            "revalidation_interval_days": 30,
            "reason": "r",
            "idempotency_key": "k",
        },
        body={
            "scope": SCOPE,
            "proposition": "x",
            "polarity": "refutes",
            "confidence": CONF,
            "depends_on": [{"belief_id": BEL, "relation": "requires"}],
            "revalidation_interval_days": 30,
            "reason": "r",
            "idempotency_key": "k",
        },
    ),
    Case("get_belief", (BEL,), "GET", f"/v1/beliefs/{BEL}"),
    Case("belief_history", (BEL,), "GET", f"/v1/beliefs/{BEL}/history"),
    Case(
        "revise_belief",
        (BEL,),
        "POST",
        f"/v1/beliefs/{BEL}/revisions",
        kwargs={"reason": "re-measured", "confidence": CONF, "status": "disputed"},
        body={"reason": "re-measured", "confidence": CONF, "status": "disputed"},
    ),
    Case(
        "revise_belief",
        (BEL,),
        "POST",
        f"/v1/beliefs/{BEL}/revisions",
        kwargs={
            "reason": "more",
            "add_evidence": [{"evidence_id": EV, "direction": "refutes", "strength": 0.4}],
            "expected_revision": BEL,
        },
        body={
            "reason": "more",
            "add_evidence": [{"evidence_id": EV, "direction": "refutes", "strength": 0.4}],
            "expected_revision": BEL,
        },
    ),
    Case("retract_belief", (BEL, "wrong"), "POST", f"/v1/beliefs/{BEL}/retract", body={"reason": "wrong"}),
    Case(
        "add_dependency",
        (BEL, BEL2, "supports"),
        "POST",
        "/v1/dependencies",
        body={"from": BEL, "to": BEL2, "relation": "supports"},
    ),
    Case(
        "add_dependency",
        (BEL, BEL2, "requires"),
        "POST",
        "/v1/dependencies",
        kwargs={"strength": 0.5},
        body={"from": BEL, "to": BEL2, "relation": "requires", "strength": 0.5},
    ),
    # inspection
    Case("events", (), "GET", "/v1/events"),
    Case(
        "events",
        (),
        "GET",
        "/v1/events",
        kwargs={"stream_id": MEM, "event_type": "memory_revised", "after_sequence": 3, "limit": 10},
        params={"stream_id": MEM, "event_type": "memory_revised", "after_sequence": "3", "limit": "10"},
    ),
    Case("debt", (), "GET", "/v1/debt"),
    # observations, jobs, proposals
    Case(
        "submit_observation",
        (SCOPE, "The primary database is PostgreSQL.", {"type": "user_input"}),
        "POST",
        "/v1/observations",
        kwargs={"media_type": "text/plain", "idempotency_key": "o1"},
        body={
            "scope": SCOPE,
            "content": "The primary database is PostgreSQL.",
            "media_type": "text/plain",
            "source": {"type": "user_input"},
            "idempotency_key": "o1",
        },
    ),
    Case("get_observation", (OBS,), "GET", f"/v1/observations/{OBS}"),
    Case(
        "purge_observation",
        (OBS, "request"),
        "POST",
        f"/v1/observations/{OBS}/purge",
        body={"reason": "request"},
    ),
    Case("list_jobs", (), "GET", "/v1/jobs", kwargs={"status": "succeeded"}, params={"status": "succeeded"}),
    Case("get_job", (JOB,), "GET", f"/v1/jobs/{JOB}"),
    Case(
        "list_proposals",
        (),
        "GET",
        "/v1/proposals",
        kwargs={"status": "pending", "limit": 5},
        params={"status": "pending", "limit": "5"},
    ),
    Case("get_proposal", (PROP,), "GET", f"/v1/proposals/{PROP}"),
    Case("accept_proposal", (PROP, "same"), "POST", f"/v1/proposals/{PROP}/accept", body={"reason": "same"}),
    Case("reject_proposal", (PROP, "late"), "POST", f"/v1/proposals/{PROP}/reject", body={"reason": "late"}),
    # hypotheses & interventions
    Case(
        "create_hypothesis",
        (SCOPE, "deploy@1", "warm-cache", "fast", "raises"),
        "POST",
        "/v1/hypotheses",
        kwargs={"min_effect": 0.1, "contexts": ("c0", "c1"), "statement": "s"},
        body={
            "scope": SCOPE,
            "environment": "deploy@1",
            "treatment": "warm-cache",
            "outcome": "fast",
            "direction": "raises",
            "min_effect": 0.1,
            "contexts": ["c0", "c1"],
            "statement": "s",
        },
    ),
    Case(
        "list_hypotheses",
        (),
        "GET",
        "/v1/hypotheses",
        kwargs={"status": "supported", "limit": 3},
        params={"status": "supported", "limit": "3"},
    ),
    Case("get_hypothesis", (HYP,), "GET", f"/v1/hypotheses/{HYP}"),
    Case("hypothesis_history", (HYP,), "GET", f"/v1/hypotheses/{HYP}/history"),
    Case(
        "evaluate_hypothesis",
        (HYP, 1),
        "POST",
        f"/v1/hypotheses/{HYP}/evaluate",
        body={"expected_version": 1},
    ),
    Case(
        "retire_hypothesis",
        (HYP, 2),
        "POST",
        f"/v1/hypotheses/{HYP}/retire",
        body={"expected_version": 2},
    ),
    Case(
        "record_intervention",
        (SCOPE, "deploy@1", "warm-cache", "treated", "randomized", "c0"),
        "POST",
        "/v1/interventions",
        kwargs={"assignment_detail": "seed 3", "propensity": 0.5, "state": {"region": "eu"}},
        body={
            "scope": SCOPE,
            "environment": "deploy@1",
            "treatment": "warm-cache",
            "arm": "treated",
            "assignment": "randomized",
            "assignment_detail": "seed 3",
            "propensity": 0.5,
            "context": "c0",
            "state": {"region": "eu"},
        },
    ),
    Case("get_intervention", (INT,), "GET", f"/v1/interventions/{INT}"),
    Case(
        "observe_outcome",
        (INT, "fast", True, "deterministic"),
        "POST",
        f"/v1/interventions/{INT}/outcomes",
        body={"outcome": "fast", "value": True, "tier": "deterministic"},
    ),
    Case(
        "counterfactual",
        (INT, HYP),
        "GET",
        f"/v1/interventions/{INT}/counterfactual",
        params={"hypothesis": HYP},
    ),
    # predictions
    Case(
        "predict",
        (INT, UUID(HYP)),
        "POST",
        f"/v1/interventions/{INT}/predictions",
        body={"hypothesis_id": HYP},
    ),
    Case(
        "prediction_rule",
        (HYP, "c0"),
        "GET",
        f"/v1/hypotheses/{HYP}/rule",
        kwargs={"threshold": 0.8},
        params={"context": "c0", "threshold": "0.8"},
    ),
    Case(
        "prediction_rule",
        (HYP, "c0"),
        "GET",
        f"/v1/hypotheses/{HYP}/rule",
        kwargs={"threshold": 0, "min_effect": 0.5},
        params={"context": "c0", "threshold": "0", "min_effect": "0.5"},
    ),
    Case("prediction_rule", (HYP, "c 0/x"), "GET", f"/v1/hypotheses/{HYP}/rule", params={"context": "c 0/x"}),
]


def check(case: Case, rec: Recorder, result: Any) -> None:
    assert result == rec.payload
    req = rec.last
    assert req.method == case.method
    assert req.url.path == case.path
    assert dict(req.url.params) == (case.params or {})
    assert req.headers["x-cs-tenant"] == TENANT
    assert req.headers["x-cs-user"] == USER
    if case.body is NO_BODY:
        assert req.content == b""
    else:
        assert req.headers["content-type"] == "application/json"
        assert rec.last_body() == case.body


@pytest.mark.parametrize("case", CASES, ids=str)
def test_sync(case: Case) -> None:
    rec = Recorder(payload={"echo": case.name})
    with Client(BASE, tenant=TENANT, user=USER, transport=httpx.MockTransport(rec)) as c:
        result = getattr(c, case.name)(*case.args, **case.kwargs)
    check(case, rec, result)


@pytest.mark.parametrize("case", CASES, ids=str)
def test_async(case: Case) -> None:
    rec = Recorder(payload={"echo": case.name})

    async def run() -> Any:
        async with AsyncClient(
            BASE, tenant=TENANT, user=USER, transport=httpx.MockTransport(rec)
        ) as c:
            return await getattr(c, case.name)(*case.args, **case.kwargs)

    check(case, rec, asyncio.run(run()))


def test_every_public_method_is_covered() -> None:
    helpers = {"request", "private_scope", "scope", "close", "aclose"}
    public = {
        n for n in dir(Client) if not n.startswith("_") and callable(getattr(Client, n))
    } - helpers
    covered = {c.name for c in CASES}
    assert public == covered, f"missing cases: {sorted(public - covered)}"
    async_public = {
        n for n in dir(AsyncClient) if not n.startswith("_") and callable(getattr(AsyncClient, n))
    } - helpers
    assert async_public == public
