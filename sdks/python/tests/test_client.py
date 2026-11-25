"""Client construction, headers, error mapping and helpers."""

from __future__ import annotations

import asyncio
from datetime import datetime
from typing import Any, Callable
from uuid import UUID

import httpx
import pytest

import cognitive_substrate
from cognitive_substrate import ApiError, AsyncClient, Client

from .conftest import BASE, TENANT, USER, Recorder

AGENT = "0190a1b2-0000-7000-8000-0000000000c1"
P1 = "0190a1b2-0000-7000-8000-0000000000c2"
P2 = "0190a1b2-0000-7000-8000-0000000000c3"


# ---- headers & construction ---------------------------------------------------


def test_default_headers(client: Client, recorder: Recorder) -> None:
    client.debt()
    h = recorder.last.headers
    assert h["x-cs-tenant"] == TENANT
    assert h["x-cs-user"] == USER
    assert h["accept"] == "application/json"
    assert h["user-agent"] == f"cognitive-substrate-python/{cognitive_substrate.__version__}"
    assert "authorization" not in h
    assert "x-cs-agent" not in h
    assert "x-cs-projects" not in h


def test_api_key_agent_projects_and_extra_headers(
    make_client: Callable[..., Client], recorder: Recorder
) -> None:
    c = make_client(
        api_key="secret",
        agent=UUID(AGENT),
        projects=[P1, UUID(P2)],
        headers={"x-request-id": "r1"},
    )
    c.health()
    h = recorder.last.headers
    assert h["authorization"] == "Bearer secret"
    assert h["x-cs-agent"] == AGENT
    assert h["x-cs-projects"] == f"{P1},{P2}"
    assert h["x-request-id"] == "r1"


def test_user_is_optional(make_client: Callable[..., Client], recorder: Recorder) -> None:
    c = make_client(user=None)
    c.debt()
    assert "x-cs-user" not in recorder.last.headers
    with pytest.raises(ValueError, match="user="):
        c.private_scope()


def test_ids_are_normalized_and_validated() -> None:
    c = Client(BASE, tenant=UUID(TENANT), user=USER.upper())
    try:
        assert c.tenant == TENANT and c.user == USER
    finally:
        c.close()
    with pytest.raises(ValueError, match="tenant must be a UUID"):
        Client(BASE, tenant="acme")
    with pytest.raises(ValueError, match="user must be a UUID"):
        Client(BASE, tenant=TENANT, user="alice")
    with pytest.raises(ValueError, match="project must be a UUID"):
        Client(BASE, tenant=TENANT, projects=["p"])


def test_base_url_prefix_and_trailing_slash(recorder: Recorder) -> None:
    with Client(
        "http://gw.test/cs/", tenant=TENANT, transport=httpx.MockTransport(recorder)
    ) as c:
        assert c.base_url == "http://gw.test/cs"
        c.health()
    assert str(recorder.last.url) == "http://gw.test/cs/v1/health"


def test_timeout_is_applied() -> None:
    with Client(BASE, tenant=TENANT, timeout=2.5) as c:
        assert c._http.timeout == httpx.Timeout(2.5)
    with Client(BASE, tenant=TENANT) as c:
        assert c._http.timeout == httpx.Timeout(cognitive_substrate.DEFAULT_TIMEOUT)


def test_context_managers_close() -> None:
    with Client(BASE, tenant=TENANT) as c:
        pass
    assert c._http.is_closed

    async def run() -> AsyncClient:
        async with AsyncClient(BASE, tenant=TENANT) as a:
            pass
        return a

    assert asyncio.run(run())._http.is_closed


def test_repr_has_no_secrets() -> None:
    with Client(BASE, tenant=TENANT, api_key="secret") as c:
        assert "secret" not in repr(c)
        assert TENANT in repr(c)


# ---- error mapping ------------------------------------------------------------


@pytest.mark.parametrize(
    ("status", "code"),
    [
        (400, "invalid_request"),
        (401, "unauthorized"),
        (403, "forbidden"),
        (404, "not_found"),
        (409, "conflict"),
        (422, "validation_failed"),
        (503, "not_ready"),
    ],
)
def test_structured_errors(make_client: Callable[..., Client], recorder: Recorder, status: int, code: str) -> None:
    recorder.status = status
    recorder.payload = {"error": {"code": code, "message": f"{code} happened"}}
    c = make_client()
    with pytest.raises(ApiError) as info:
        c.get_memory("0190a1b2-0000-7000-8000-0000000000a1")
    err = info.value
    assert err.status == status
    assert err.code == code
    assert err.message == f"{code} happened"
    assert err.body == recorder.payload
    assert str(err) == f"{status} {code}: {code} happened"


def test_unstructured_error_body(make_client: Callable[..., Client], recorder: Recorder) -> None:
    recorder.status = 502
    recorder.raw = b"Bad Gateway from proxy"
    with pytest.raises(ApiError) as info:
        make_client().health()
    assert info.value.status == 502
    assert info.value.code == "unexpected_response"
    assert info.value.message == "Bad Gateway from proxy"
    assert info.value.body is None


def test_json_error_without_error_envelope(make_client: Callable[..., Client], recorder: Recorder) -> None:
    recorder.status = 500
    recorder.payload = {"detail": "nope"}
    with pytest.raises(ApiError) as info:
        make_client().health()
    assert info.value.code == "unexpected_response"
    assert info.value.body == {"detail": "nope"}


def test_empty_success_body_returns_none(make_client: Callable[..., Client], recorder: Recorder) -> None:
    recorder.status = 204
    recorder.raw = b""
    assert make_client().request("POST", "/v1/information-needs/x/purge") is None


def test_created_and_refused_predictions_both_return_body(
    make_client: Callable[..., Client], recorder: Recorder
) -> None:
    c = make_client()
    recorder.status = 201
    recorder.payload = {"result": "recorded", "prediction": {"arm": "control"}}
    assert c.predict(P1, P2)["result"] == "recorded"
    recorder.status = 200
    recorder.payload = {"result": "refused", "reason": "out_of_scope"}
    assert c.predict(P1, P2) == {"result": "refused", "reason": "out_of_scope"}


def test_async_errors_match_sync(recorder: Recorder) -> None:
    recorder.status = 409
    recorder.payload = {"error": {"code": "conflict", "message": "already decided"}}

    async def run() -> ApiError:
        async with AsyncClient(BASE, tenant=TENANT, transport=httpx.MockTransport(recorder)) as c:
            with pytest.raises(ApiError) as info:
                await c.accept_proposal(P1, "late")
            return info.value

    err = asyncio.run(run())
    assert (err.status, err.code, err.message) == (409, "conflict", "already decided")


def test_transport_errors_are_not_wrapped() -> None:
    def boom(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("refused", request=request)

    with Client(BASE, tenant=TENANT, transport=httpx.MockTransport(boom)) as c:
        with pytest.raises(httpx.ConnectError):
            c.health()


# ---- helpers ------------------------------------------------------------------


def test_scopes(client: Client) -> None:
    assert client.private_scope() == {
        "tenant_id": TENANT,
        "owner_user_id": USER,
        "visibility": "private",
    }
    assert client.scope() == {"tenant_id": TENANT, "visibility": "tenant"}
    assert client.scope("project", project_id=UUID(P1)) == {
        "tenant_id": TENANT,
        "visibility": "project",
        "project_id": P1,
    }


def test_request_escape_hatch(client: Client, recorder: Recorder) -> None:
    client.request("get", "/v1/entities", params={"name": "Auth Service", "limit": None})
    assert recorder.last.method == "GET"
    assert recorder.last.url.path == "/v1/entities"
    assert dict(recorder.last.url.params) == {"name": "Auth Service"}
    assert recorder.last.content == b""

    client.request("POST", "/v1/entities/x/neighborhood", json={"direction": "in", "depth": 1})
    assert recorder.last_body() == {"direction": "in", "depth": 1}

    client.request("POST", "/v1/embeddings/reindex", params={"flag": True})
    assert dict(recorder.last.url.params) == {"flag": "true"}


def test_path_ids_are_percent_encoded(client: Client, recorder: Recorder) -> None:
    client.get_memory("../debt")
    assert recorder.last.url.raw_path == b"/v1/memories/..%2Fdebt"


def test_naive_datetimes_are_rejected(client: Client, recorder: Recorder) -> None:
    with pytest.raises(ValueError, match="timezone-aware"):
        client.record_evidence({"type": "user_input"}, observed_at=datetime(2026, 1, 1))
    assert recorder.requests == []


def test_async_scope_helpers() -> None:
    async def run() -> Any:
        async with AsyncClient(BASE, tenant=TENANT, user=USER) as c:
            return c.private_scope()

    assert asyncio.run(run())["owner_user_id"] == USER
