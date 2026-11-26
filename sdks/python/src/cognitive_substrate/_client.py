"""Synchronous and asynchronous clients for the ``/v1`` HTTP API.

Every endpoint is defined once on :class:`_Api`, which only describes the
request. :class:`Client` executes it with ``httpx.Client`` and returns the
parsed JSON; :class:`AsyncClient` executes it with ``httpx.AsyncClient`` and
returns an awaitable of the parsed JSON. The two therefore cannot drift.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Awaitable, Generic, Iterable, Mapping, Optional, TypeVar, Union
from urllib.parse import quote
from uuid import UUID

import httpx

from ._errors import ApiError
from ._version import __version__
from .types import (
    Arm,
    Assignment,
    BeliefEvidence,
    BeliefPolarity,
    BeliefStatus,
    Confidence,
    DependencyInput,
    DependencyRelation,
    Direction,
    ExtractionInfo,
    HypothesisStatus,
    Id,
    JobStatus,
    MemoryContent,
    OracleTier,
    ProposalStatus,
    ProvenanceInput,
    RecallPolicy,
    Scope,
    SourceRef,
    TimeInterval,
    Timestamp,
    Visibility,
)

__all__ = ["AsyncClient", "Client", "DEFAULT_BASE_URL", "DEFAULT_TIMEOUT"]

DEFAULT_BASE_URL = "http://localhost:8080"
DEFAULT_TIMEOUT = 30.0

_R = TypeVar("_R")

TimeoutTypes = Union[float, httpx.Timeout, None]


# ---- request description ------------------------------------------------------


class _NoBody:
    """Marker: send the request without a body (distinct from JSON ``null``)."""


_NO_BODY = _NoBody()


@dataclass(frozen=True)
class _Call:
    method: str
    path: str
    params: Optional[dict[str, str]] = None
    body: Any = field(default=_NO_BODY)


def _uuid(value: Id, what: str) -> str:
    try:
        return str(value if isinstance(value, UUID) else UUID(str(value)))
    except ValueError:
        raise ValueError(f"{what} must be a UUID, got {value!r}") from None


def _seg(value: Id) -> str:
    """One path segment. IDs are passed through as given (the server answers
    400 for a malformed UUID) but always percent-encoded."""
    return quote(str(value), safe="")


def _jsonable(value: Any) -> Any:
    if isinstance(value, UUID):
        return str(value)
    if isinstance(value, datetime):
        if value.tzinfo is None or value.utcoffset() is None:
            raise ValueError("datetimes must be timezone-aware (RFC 3339 needs an offset)")
        return value.isoformat()
    if isinstance(value, Mapping):
        return {str(k): _jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(v) for v in value]
    return value


def _body(**fields: Any) -> dict[str, Any]:
    """A JSON object without the fields left as ``None`` (so the server's
    defaults apply), with UUIDs and datetimes converted."""
    return {k: _jsonable(v) for k, v in fields.items() if v is not None}


def _query(**params: Any) -> Optional[dict[str, str]]:
    out: dict[str, str] = {}
    for k, v in params.items():
        if v is None:
            continue
        if isinstance(v, bool):
            out[k] = "true" if v else "false"
        else:
            out[k] = str(v)
    return out or None


def _raise_for_status(response: httpx.Response) -> None:
    if response.is_success:
        return
    try:
        payload = response.json()
    except ValueError:
        payload = None
    err = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(err, dict) and isinstance(err.get("code"), str):
        raise ApiError(response.status_code, err["code"], str(err.get("message", "")), payload)
    raise ApiError(
        response.status_code,
        "unexpected_response",
        response.text or response.reason_phrase,
        payload,
    )


def _decode(response: httpx.Response) -> Any:
    _raise_for_status(response)
    if not response.content:
        return None
    return response.json()


def _request_kwargs(call: _Call) -> dict[str, Any]:
    kwargs: dict[str, Any] = {}
    if call.params:
        kwargs["params"] = call.params
    if call.body is not _NO_BODY:
        kwargs["json"] = call.body
    return kwargs


# ---- endpoints -----------------------------------------------------------------


class _Api(Generic[_R]):
    """Endpoint definitions shared by :class:`Client` and :class:`AsyncClient`.

    Methods return the parsed JSON response body (``dict`` or ``list``) as the
    server sent it, or raise :class:`~cognitive_substrate.ApiError`.
    """

    tenant: str
    user: Optional[str]

    def _call(self, call: _Call) -> _R:  # pragma: no cover - overridden
        raise NotImplementedError

    # -- helpers -----------------------------------------------------------------

    def private_scope(self) -> Scope:
        """A scope owned by this client's user, visible only to them."""
        if self.user is None:
            raise ValueError("private_scope() needs a client created with user=")
        return {"tenant_id": self.tenant, "owner_user_id": self.user, "visibility": "private"}

    def scope(self, visibility: Visibility = "tenant", **extra: Id) -> Scope:
        """A scope in this client's tenant. ``extra`` may set ``owner_user_id``,
        ``agent_id``, ``project_id`` or ``conversation_id``."""
        out: dict[str, Any] = {"tenant_id": self.tenant, "visibility": visibility}
        out.update({k: _jsonable(v) for k, v in extra.items()})
        return out  # type: ignore[return-value]

    def request(
        self,
        method: str,
        path: str,
        *,
        json: Any = _NO_BODY,
        params: Optional[Mapping[str, Any]] = None,
    ) -> _R:
        """Call any route (e.g. ``"/v1/entities"``) with the client's headers,
        for endpoints this SDK has no helper for."""
        body = json if json is _NO_BODY else _jsonable(json)
        return self._call(_Call(method.upper(), path, _query(**(params or {})), body))

    # -- service -----------------------------------------------------------------

    def health(self) -> _R:
        """``GET /v1/health``: liveness and server version."""
        return self._call(_Call("GET", "/v1/health"))

    def ready(self) -> _R:
        """``GET /v1/ready``: storage readiness (503 ``not_ready`` otherwise)."""
        return self._call(_Call("GET", "/v1/ready"))

    # -- memories ----------------------------------------------------------------

    def create_memory(
        self,
        scope: Scope,
        content: MemoryContent,
        provenance: ProvenanceInput,
        *,
        valid_time: Optional[TimeInterval] = None,
        expires_at: Optional[Timestamp] = None,
        idempotency_key: Optional[str] = None,
    ) -> _R:
        """``POST /v1/memories`` (201). Returns the memory record."""
        return self._call(
            _Call(
                "POST",
                "/v1/memories",
                body=_body(
                    scope=scope,
                    content=content,
                    provenance=provenance,
                    valid_time=valid_time,
                    expires_at=expires_at,
                    idempotency_key=idempotency_key,
                ),
            )
        )

    def get_memory(self, memory_id: Id) -> _R:
        """``GET /v1/memories/{id}``: the current revision."""
        return self._call(_Call("GET", f"/v1/memories/{_seg(memory_id)}"))

    def memory_history(self, memory_id: Id) -> _R:
        """``GET /v1/memories/{id}/history``: ``{"memory_id", "revisions"}``."""
        return self._call(_Call("GET", f"/v1/memories/{_seg(memory_id)}/history"))

    def revise_memory(
        self,
        memory_id: Id,
        *,
        reason: str,
        content: Optional[MemoryContent] = None,
        valid_time: Optional[TimeInterval] = None,
        expires_at: Optional[Timestamp] = None,
        provenance: Optional[ProvenanceInput] = None,
        expected_revision: Optional[Id] = None,
    ) -> _R:
        """``POST /v1/memories/{id}/revisions`` (201). ``expected_revision`` makes
        the revision fail with ``conflict`` unless it is still current."""
        return self._call(
            _Call(
                "POST",
                f"/v1/memories/{_seg(memory_id)}/revisions",
                body=_body(
                    content=content,
                    valid_time=valid_time,
                    expires_at=expires_at,
                    provenance=provenance,
                    reason=reason,
                    expected_revision=expected_revision,
                ),
            )
        )

    def _status_change(self, kind: str, memory_id: Id, reason: str) -> _R:
        return self._call(
            _Call("POST", f"/v1/memories/{_seg(memory_id)}/{kind}", body={"reason": reason})
        )

    def promote_memory(self, memory_id: Id, reason: str) -> _R:
        """``POST /v1/memories/{id}/promote``: validate a candidate."""
        return self._status_change("promote", memory_id, reason)

    def reject_memory(self, memory_id: Id, reason: str) -> _R:
        """``POST /v1/memories/{id}/reject``: reject a candidate."""
        return self._status_change("reject", memory_id, reason)

    def tombstone_memory(self, memory_id: Id, reason: str) -> _R:
        """``POST /v1/memories/{id}/tombstone``: logical delete."""
        return self._status_change("tombstone", memory_id, reason)

    def purge_memory(self, memory_id: Id, reason: str) -> _R:
        """``POST /v1/memories/{id}/purge``: governance hard delete. Returns the
        recorded cognitive event."""
        return self._status_change("purge", memory_id, reason)

    # -- recall ------------------------------------------------------------------

    def recall(
        self,
        query: str,
        *,
        policy: Optional[RecallPolicy] = None,
        session: Optional[str] = None,
    ) -> _R:
        """``POST /v1/recall``. Returns ``trace_id``, ``results``, ``beliefs`` and
        the ``trace`` summary (plus ``information_need``/``metacognition`` when
        the policy asks for them)."""
        return self._call(
            _Call("POST", "/v1/recall", body=_body(query=query, policy=policy, session=session))
        )

    def get_trace(self, trace_id: Id) -> _R:
        """``GET /v1/recall-traces/{id}``: the full recall decision."""
        return self._call(_Call("GET", f"/v1/recall-traces/{_seg(trace_id)}"))

    def replay(self, trace_id: Id, *, policy_override: Optional[RecallPolicy] = None) -> _R:
        """``POST /v1/recall-traces/{id}/replay``.

        Without ``policy_override`` this is an exact replay (empty body); with
        it, a counterfactual replay under the substituted policy. Returns a
        report with ``identical``, ``added``, ``removed`` and the replay trace.
        """
        body: Any = _NO_BODY if policy_override is None else _body(policy_override=policy_override)
        return self._call(_Call("POST", f"/v1/recall-traces/{_seg(trace_id)}/replay", body=body))

    # -- evidence & beliefs ------------------------------------------------------

    def record_evidence(
        self,
        source: SourceRef,
        *,
        observed_at: Optional[Timestamp] = None,
        extraction: Optional[ExtractionInfo] = None,
        content_hash: Optional[str] = None,
        summary: Optional[str] = None,
    ) -> _R:
        """``POST /v1/evidence`` (201)."""
        return self._call(
            _Call(
                "POST",
                "/v1/evidence",
                body=_body(
                    source=source,
                    observed_at=observed_at,
                    extraction=extraction,
                    content_hash=content_hash,
                    summary=summary,
                ),
            )
        )

    def purge_evidence(self, evidence_id: Id, reason: str) -> _R:
        """``POST /v1/evidence/{id}/purge``: idempotent governed purge. The report
        lists ``beliefs_needing_reevaluation``."""
        return self._call(
            _Call("POST", f"/v1/evidence/{_seg(evidence_id)}/purge", body={"reason": reason})
        )

    def create_belief(
        self,
        scope: Scope,
        proposition: str,
        confidence: Confidence,
        *,
        polarity: Optional[BeliefPolarity] = None,
        evidence: Optional[Iterable[BeliefEvidence]] = None,
        depends_on: Optional[Iterable[DependencyInput]] = None,
        valid_time: Optional[TimeInterval] = None,
        revalidation_interval_days: Optional[int] = None,
        reason: Optional[str] = None,
        idempotency_key: Optional[str] = None,
    ) -> _R:
        """``POST /v1/beliefs`` (201). ``polarity`` defaults to ``supports``."""
        return self._call(
            _Call(
                "POST",
                "/v1/beliefs",
                body=_body(
                    scope=scope,
                    proposition=proposition,
                    polarity=polarity,
                    confidence=confidence,
                    evidence=None if evidence is None else list(evidence),
                    depends_on=None if depends_on is None else list(depends_on),
                    valid_time=valid_time,
                    revalidation_interval_days=revalidation_interval_days,
                    reason=reason,
                    idempotency_key=idempotency_key,
                ),
            )
        )

    def get_belief(self, belief_id: Id) -> _R:
        """``GET /v1/beliefs/{id}``: the belief with its evidence, dependencies,
        dependents and ``current_revision_id``."""
        return self._call(_Call("GET", f"/v1/beliefs/{_seg(belief_id)}"))

    def belief_history(self, belief_id: Id) -> _R:
        """``GET /v1/beliefs/{id}/history``: every revision (a list)."""
        return self._call(_Call("GET", f"/v1/beliefs/{_seg(belief_id)}/history"))

    def revise_belief(
        self,
        belief_id: Id,
        *,
        reason: str,
        proposition: Optional[str] = None,
        polarity: Optional[BeliefPolarity] = None,
        confidence: Optional[Confidence] = None,
        add_evidence: Optional[Iterable[BeliefEvidence]] = None,
        valid_time: Optional[TimeInterval] = None,
        status: Optional[BeliefStatus] = None,
        revalidation_interval_days: Optional[int] = None,
        expected_revision: Optional[Id] = None,
    ) -> _R:
        """``POST /v1/beliefs/{id}/revisions`` (201). Returns ``{"belief",
        "needs_reevaluation"}``: dependents flagged, never auto-retracted."""
        return self._call(
            _Call(
                "POST",
                f"/v1/beliefs/{_seg(belief_id)}/revisions",
                body=_body(
                    proposition=proposition,
                    polarity=polarity,
                    confidence=confidence,
                    add_evidence=None if add_evidence is None else list(add_evidence),
                    valid_time=valid_time,
                    status=status,
                    revalidation_interval_days=revalidation_interval_days,
                    reason=reason,
                    expected_revision=expected_revision,
                ),
            )
        )

    def retract_belief(self, belief_id: Id, reason: str) -> _R:
        """``POST /v1/beliefs/{id}/retract``. Returns ``{"belief", "needs_reevaluation"}``."""
        return self._call(
            _Call("POST", f"/v1/beliefs/{_seg(belief_id)}/retract", body={"reason": reason})
        )

    def add_dependency(
        self,
        from_belief: Id,
        to_belief: Id,
        relation: DependencyRelation,
        *,
        strength: Optional[float] = None,
    ) -> _R:
        """``POST /v1/dependencies`` (201): ``from_belief`` relates to ``to_belief``."""
        return self._call(
            _Call(
                "POST",
                "/v1/dependencies",
                # "from" is a Python keyword, hence the mapping.
                body=_body(
                    **{"from": from_belief, "to": to_belief},
                    relation=relation,
                    strength=strength,
                ),
            )
        )

    # -- inspection --------------------------------------------------------------

    def events(
        self,
        *,
        stream_id: Optional[Id] = None,
        event_type: Optional[str] = None,
        after_sequence: Optional[int] = None,
        limit: Optional[int] = None,
    ) -> _R:
        """``GET /v1/events``: the scope-filtered cognitive event log (a list)."""
        return self._call(
            _Call(
                "GET",
                "/v1/events",
                _query(
                    stream_id=stream_id,
                    event_type=event_type,
                    after_sequence=after_sequence,
                    limit=limit,
                ),
            )
        )

    def debt(self) -> _R:
        """``GET /v1/debt``: the Cognitive Debt Index report."""
        return self._call(_Call("GET", "/v1/debt"))

    # -- observations, jobs & proposals -------------------------------------------

    def submit_observation(
        self,
        scope: Scope,
        content: str,
        source: SourceRef,
        *,
        media_type: Optional[str] = None,
        observed_at: Optional[Timestamp] = None,
        idempotency_key: Optional[str] = None,
    ) -> _R:
        """``POST /v1/observations`` (202). Perception runs in the background;
        returns ``{"observation", "job", "extraction_runs"}``."""
        return self._call(
            _Call(
                "POST",
                "/v1/observations",
                body=_body(
                    scope=scope,
                    content=content,
                    media_type=media_type,
                    source=source,
                    observed_at=observed_at,
                    idempotency_key=idempotency_key,
                ),
            )
        )

    def get_observation(self, observation_id: Id) -> _R:
        """``GET /v1/observations/{id}``: observation, job and extraction runs."""
        return self._call(_Call("GET", f"/v1/observations/{_seg(observation_id)}"))

    def purge_observation(self, observation_id: Id, reason: str) -> _R:
        """``POST /v1/observations/{id}/purge``: idempotent governed purge."""
        return self._call(
            _Call("POST", f"/v1/observations/{_seg(observation_id)}/purge", body={"reason": reason})
        )

    def list_jobs(self, *, status: Optional[JobStatus] = None, limit: Optional[int] = None) -> _R:
        """``GET /v1/jobs``: background jobs (a list)."""
        return self._call(_Call("GET", "/v1/jobs", _query(status=status, limit=limit)))

    def get_job(self, job_id: Id) -> _R:
        """``GET /v1/jobs/{id}``."""
        return self._call(_Call("GET", f"/v1/jobs/{_seg(job_id)}"))

    def list_proposals(
        self, *, status: Optional[ProposalStatus] = None, limit: Optional[int] = None
    ) -> _R:
        """``GET /v1/proposals``: resolution proposals (a list)."""
        return self._call(_Call("GET", "/v1/proposals", _query(status=status, limit=limit)))

    def get_proposal(self, proposal_id: Id) -> _R:
        """``GET /v1/proposals/{id}``."""
        return self._call(_Call("GET", f"/v1/proposals/{_seg(proposal_id)}"))

    def accept_proposal(self, proposal_id: Id, reason: str) -> _R:
        """``POST /v1/proposals/{id}/accept`` (409 ``conflict`` once decided)."""
        return self._call(
            _Call("POST", f"/v1/proposals/{_seg(proposal_id)}/accept", body={"reason": reason})
        )

    def reject_proposal(self, proposal_id: Id, reason: str) -> _R:
        """``POST /v1/proposals/{id}/reject`` (409 ``conflict`` once decided)."""
        return self._call(
            _Call("POST", f"/v1/proposals/{_seg(proposal_id)}/reject", body={"reason": reason})
        )

    # -- causal hypotheses & interventions ---------------------------------------

    def create_hypothesis(
        self,
        scope: Scope,
        environment: str,
        treatment: str,
        outcome: str,
        direction: Direction,
        *,
        min_effect: Optional[float] = None,
        contexts: Optional[Iterable[str]] = None,
        statement: Optional[str] = None,
    ) -> _R:
        """``POST /v1/hypotheses`` (201): "doing ``treatment`` in ``environment``
        ``direction`` ``outcome``". Starts at version 1, status ``proposed``."""
        return self._call(
            _Call(
                "POST",
                "/v1/hypotheses",
                body=_body(
                    scope=scope,
                    environment=environment,
                    treatment=treatment,
                    outcome=outcome,
                    direction=direction,
                    min_effect=min_effect,
                    contexts=None if contexts is None else list(contexts),
                    statement=statement,
                ),
            )
        )

    def list_hypotheses(
        self, *, status: Optional[HypothesisStatus] = None, limit: Optional[int] = None
    ) -> _R:
        """``GET /v1/hypotheses`` (a list; the server's default limit is 100)."""
        return self._call(_Call("GET", "/v1/hypotheses", _query(status=status, limit=limit)))

    def get_hypothesis(self, hypothesis_id: Id) -> _R:
        """``GET /v1/hypotheses/{id}``: the current version."""
        return self._call(_Call("GET", f"/v1/hypotheses/{_seg(hypothesis_id)}"))

    def hypothesis_history(self, hypothesis_id: Id) -> _R:
        """``GET /v1/hypotheses/{id}/history``: every version (a list)."""
        return self._call(_Call("GET", f"/v1/hypotheses/{_seg(hypothesis_id)}/history"))

    def evaluate_hypothesis(self, hypothesis_id: Id, expected_version: int) -> _R:
        """``POST /v1/hypotheses/{id}/evaluate``: version-checked evaluation."""
        return self._call(
            _Call(
                "POST",
                f"/v1/hypotheses/{_seg(hypothesis_id)}/evaluate",
                body={"expected_version": expected_version},
            )
        )

    def retire_hypothesis(self, hypothesis_id: Id, expected_version: int) -> _R:
        """``POST /v1/hypotheses/{id}/retire``: version-checked retirement."""
        return self._call(
            _Call(
                "POST",
                f"/v1/hypotheses/{_seg(hypothesis_id)}/retire",
                body={"expected_version": expected_version},
            )
        )

    def record_intervention(
        self,
        scope: Scope,
        environment: str,
        treatment: str,
        arm: Arm,
        assignment: Assignment,
        context: str,
        *,
        assignment_detail: Optional[str] = None,
        propensity: Optional[float] = None,
        state: Optional[Mapping[str, str]] = None,
        execution_id: Optional[Id] = None,
    ) -> _R:
        """``POST /v1/interventions`` (201), recorded at intent. A ``randomized``
        assignment needs ``assignment_detail`` (e.g. the seed) and ``propensity``."""
        return self._call(
            _Call(
                "POST",
                "/v1/interventions",
                body=_body(
                    scope=scope,
                    environment=environment,
                    treatment=treatment,
                    arm=arm,
                    assignment=assignment,
                    assignment_detail=assignment_detail,
                    propensity=propensity,
                    context=context,
                    state=None if state is None else dict(state),
                    execution_id=execution_id,
                ),
            )
        )

    def get_intervention(self, intervention_id: Id) -> _R:
        """``GET /v1/interventions/{id}``: the intervention with its outcomes."""
        return self._call(_Call("GET", f"/v1/interventions/{_seg(intervention_id)}"))

    def observe_outcome(
        self, intervention_id: Id, outcome: str, value: bool, tier: OracleTier
    ) -> _R:
        """``POST /v1/interventions/{id}/outcomes``: observe an outcome once."""
        return self._call(
            _Call(
                "POST",
                f"/v1/interventions/{_seg(intervention_id)}/outcomes",
                body={"outcome": outcome, "value": value, "tier": tier},
            )
        )

    def counterfactual(self, intervention_id: Id, hypothesis_id: Id) -> _R:
        """``GET /v1/interventions/{id}/counterfactual?hypothesis=``: read-time
        bounds (experimental). ``result`` is ``bounds`` or ``refused``."""
        return self._call(
            _Call(
                "GET",
                f"/v1/interventions/{_seg(intervention_id)}/counterfactual",
                _query(hypothesis=hypothesis_id),
            )
        )

    # -- bounded predictions -----------------------------------------------------

    def predict(self, intervention_id: Id, hypothesis_id: Id) -> _R:
        """``POST /v1/interventions/{id}/predictions``: record a prediction at
        intent. ``result`` is ``recorded`` (HTTP 201, with ``prediction``) or
        ``refused`` (HTTP 200, with ``reason``; nothing written). A second
        prediction for the same intervention raises a 409 ``ApiError``."""
        return self._call(
            _Call(
                "POST",
                f"/v1/interventions/{_seg(intervention_id)}/predictions",
                body=_body(hypothesis_id=hypothesis_id),
            )
        )

    def prediction_rule(
        self,
        hypothesis_id: Id,
        context: str,
        *,
        threshold: Optional[float] = None,
        min_effect: Optional[float] = None,
    ) -> _R:
        """``GET /v1/hypotheses/{id}/rule``: the rule for one context, its
        lineage ``monitor`` and, when ``threshold`` is given, the ``gate``
        decision (``gate.execute``). ``min_effect`` requires ``threshold``."""
        return self._call(
            _Call(
                "GET",
                f"/v1/hypotheses/{_seg(hypothesis_id)}/rule",
                _query(context=context, threshold=threshold, min_effect=min_effect),
            )
        )


# ---- transports ----------------------------------------------------------------


def _headers(
    tenant: str,
    user: Optional[str],
    agent: Optional[Id],
    projects: Optional[Iterable[Id]],
    api_key: Optional[str],
    extra: Optional[Mapping[str, str]],
) -> dict[str, str]:
    headers = {
        "x-cs-tenant": tenant,
        "accept": "application/json",
        "user-agent": f"cognitive-substrate-python/{__version__}",
    }
    if user is not None:
        headers["x-cs-user"] = user
    if agent is not None:
        headers["x-cs-agent"] = _uuid(agent, "agent")
    if projects is not None:
        ids = [_uuid(p, "project") for p in projects]
        if ids:
            headers["x-cs-projects"] = ",".join(ids)
    if api_key is not None:
        headers["authorization"] = f"Bearer {api_key}"
    if extra:
        headers.update(extra)
    return headers


class _Base:
    def _init_identity(self, tenant: Id, user: Optional[Id]) -> None:
        self.tenant = _uuid(tenant, "tenant")
        self.user = None if user is None else _uuid(user, "user")


class Client(_Base, _Api[Any]):
    """Synchronous client.

    >>> with Client(tenant=TENANT, user=ME) as cs:
    ...     cs.health()

    ``tenant`` (required) and ``user``, ``agent``, ``projects`` become the
    ``x-cs-*`` principal headers; ``api_key`` is sent as
    ``Authorization: Bearer``. ``transport`` is passed to ``httpx.Client``
    (useful for ``httpx.MockTransport`` in tests).
    """

    def __init__(
        self,
        base_url: str = DEFAULT_BASE_URL,
        *,
        tenant: Id,
        user: Optional[Id] = None,
        api_key: Optional[str] = None,
        timeout: TimeoutTypes = DEFAULT_TIMEOUT,
        agent: Optional[Id] = None,
        projects: Optional[Iterable[Id]] = None,
        headers: Optional[Mapping[str, str]] = None,
        transport: Optional[httpx.BaseTransport] = None,
    ) -> None:
        self._init_identity(tenant, user)
        self.base_url = base_url.rstrip("/")
        self._http = httpx.Client(
            base_url=self.base_url,
            headers=_headers(self.tenant, self.user, agent, projects, api_key, headers),
            timeout=timeout,
            transport=transport,
        )

    def _call(self, call: _Call) -> Any:
        response = self._http.request(call.method, call.path, **_request_kwargs(call))
        return _decode(response)

    def close(self) -> None:
        """Close the underlying connection pool."""
        self._http.close()

    def __enter__(self) -> Client:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def __repr__(self) -> str:
        return f"Client(base_url={self.base_url!r}, tenant={self.tenant!r}, user={self.user!r})"


class AsyncClient(_Base, _Api[Awaitable[Any]]):
    """Asynchronous client with the same methods as :class:`Client`; each
    returns an awaitable.

    >>> async with AsyncClient(tenant=TENANT, user=ME) as cs:
    ...     await cs.health()
    """

    def __init__(
        self,
        base_url: str = DEFAULT_BASE_URL,
        *,
        tenant: Id,
        user: Optional[Id] = None,
        api_key: Optional[str] = None,
        timeout: TimeoutTypes = DEFAULT_TIMEOUT,
        agent: Optional[Id] = None,
        projects: Optional[Iterable[Id]] = None,
        headers: Optional[Mapping[str, str]] = None,
        transport: Optional[httpx.AsyncBaseTransport] = None,
    ) -> None:
        self._init_identity(tenant, user)
        self.base_url = base_url.rstrip("/")
        self._http = httpx.AsyncClient(
            base_url=self.base_url,
            headers=_headers(self.tenant, self.user, agent, projects, api_key, headers),
            timeout=timeout,
            transport=transport,
        )

    def _call(self, call: _Call) -> Awaitable[Any]:
        return self._send(call)

    async def _send(self, call: _Call) -> Any:
        response = await self._http.request(call.method, call.path, **_request_kwargs(call))
        return _decode(response)

    async def aclose(self) -> None:
        """Close the underlying connection pool."""
        await self._http.aclose()

    async def __aenter__(self) -> AsyncClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()

    def __repr__(self) -> str:
        return f"AsyncClient(base_url={self.base_url!r}, tenant={self.tenant!r}, user={self.user!r})"
