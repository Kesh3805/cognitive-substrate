from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Callable

import httpx
import pytest

TENANT = "0190a1b2-0000-7000-8000-000000000001"
USER = "0190a1b2-0000-7000-8000-000000000002"
BASE = "http://cs.test"


@dataclass
class Recorder:
    """Records requests and answers each with a canned response."""

    status: int = 200
    payload: Any = field(default_factory=lambda: {"ok": True})
    raw: bytes | None = None
    requests: list[httpx.Request] = field(default_factory=list)

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        if self.raw is not None:
            return httpx.Response(self.status, content=self.raw)
        return httpx.Response(self.status, json=self.payload)

    @property
    def last(self) -> httpx.Request:
        assert self.requests, "no request was sent"
        return self.requests[-1]

    def last_body(self) -> Any:
        content = self.last.content
        return None if not content else json.loads(content)


@pytest.fixture
def recorder() -> Recorder:
    return Recorder()


@pytest.fixture
def make_client(recorder: Recorder) -> Callable[..., Any]:
    from cognitive_substrate import Client

    clients: list[Client] = []

    def make(**kwargs: Any) -> Client:
        kwargs.setdefault("tenant", TENANT)
        kwargs.setdefault("user", USER)
        c = Client(BASE, transport=httpx.MockTransport(recorder), **kwargs)
        clients.append(c)
        return c

    yield make
    for c in clients:
        c.close()


@pytest.fixture
def client(make_client: Callable[..., Any]) -> Any:
    return make_client()
