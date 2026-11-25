"""Typed shapes for request bodies.

These mirror the server's serde types (``cs-core``, ``cs-memory``,
``cs-engine``). They are ``TypedDict``s, so plain dicts work everywhere and
the annotations only help editors and type checkers. Responses are returned
as parsed JSON (``dict``/``list``) exactly as the server sent them.

UUIDs may be given as ``str`` or :class:`uuid.UUID`, and timestamps as
RFC 3339 strings or timezone-aware :class:`datetime.datetime` objects.
"""

from __future__ import annotations

import sys
from datetime import datetime
from typing import Any, Literal, Mapping, Union
from uuid import UUID

if sys.version_info >= (3, 11):
    from typing import NotRequired, Required, TypedDict
else:  # pragma: no cover - exercised on 3.10 only
    from typing_extensions import NotRequired, Required, TypedDict

__all__ = [
    "Arm",
    "Assignment",
    "BeliefEvidence",
    "BeliefPolarity",
    "BeliefStatus",
    "Confidence",
    "ConfidenceInterpretation",
    "DependencyInput",
    "DependencyRelation",
    "Direction",
    "EvidenceDirection",
    "ExtractionInfo",
    "HypothesisStatus",
    "Id",
    "JobStatus",
    "MemoryContent",
    "MemoryDomain",
    "ProposalStatus",
    "ProvenanceInput",
    "RecallPolicy",
    "Scope",
    "ScopeFilter",
    "SourceRef",
    "TimeInterval",
    "Timestamp",
    "Visibility",
]

Id = Union[str, UUID]
"""A UUID, as a string or :class:`uuid.UUID`."""

Timestamp = Union[str, datetime]
"""An RFC 3339 string or a timezone-aware datetime."""

Visibility = Literal["private", "agent", "project", "tenant"]
MemoryDomain = Literal["episodic", "semantic", "procedural", "relational", "temporal", "working"]
ConfidenceInterpretation = Literal[
    "calibrated_probability", "heuristic_score", "human_assessment", "model_estimate"
]
EvidenceDirection = Literal["supports", "refutes", "contextual"]
BeliefPolarity = Literal["supports", "refutes"]
BeliefStatus = Literal["active", "disputed", "retracted"]
DependencyRelation = Literal[
    "supports", "requires", "derived_from", "contradicts", "generalizes", "specializes"
]
ProposalStatus = Literal["pending", "accepted", "rejected", "stale"]
JobStatus = Literal["queued", "leased", "succeeded", "dead_lettered"]
Direction = Literal["raises", "lowers"]
Arm = Literal["treated", "control"]
Assignment = Literal["randomized", "passive"]
OracleTier = Literal["deterministic", "independent", "self_assessed"]
HypothesisStatus = Literal["proposed", "supported", "refuted", "inconclusive", "retired"]


class Scope(TypedDict, total=False):
    """Where a record belongs (``cs_core::Scope``)."""

    tenant_id: Required[Id]
    visibility: Required[Visibility]
    owner_user_id: Id
    agent_id: Id
    project_id: Id
    conversation_id: Id


class SourceRef(TypedDict, total=False):
    """Where information came from (``cs_core::SourceRef``), tagged by ``type``.

    ===================== ===========================================
    ``type``              other keys
    ===================== ===========================================
    ``user_input``        ``message_id`` (optional)
    ``agent_output``      ``run_id``
    ``tool_output``       ``tool``, ``execution_id``
    ``external_document`` ``uri``, ``content_hash``
    ``human_annotation``  ``annotation_id``
    ``system_derived``    ``derivation``
    ``observation``       ``observation_id``
    ===================== ===========================================
    """

    type: Required[
        Literal[
            "user_input",
            "agent_output",
            "tool_output",
            "external_document",
            "human_annotation",
            "system_derived",
            "observation",
        ]
    ]
    message_id: str
    run_id: str
    tool: str
    execution_id: str
    uri: str
    content_hash: str
    annotation_id: str
    derivation: str
    observation_id: Id


class ExtractionInfo(TypedDict):
    extractor_version: str
    model: NotRequired[str]
    model_version: NotRequired[str]
    prompt_version: NotRequired[str]


class ProvenanceInput(TypedDict):
    source: SourceRef
    observed_at: NotRequired[Timestamp]
    extraction: NotRequired[ExtractionInfo]
    parent_evidence: NotRequired[list[Id]]


class TimeInterval(TypedDict):
    """A half-open interval ``[start, end)``; no ``end`` means open-ended."""

    start: Timestamp
    end: NotRequired[Timestamp]


class MemoryContent(TypedDict):
    """``{"domain": ..., "data": {...}}``. The ``data`` keys depend on the domain,
    e.g. semantic: ``proposition`` plus optional ``subject``/``predicate``/``object``."""

    domain: MemoryDomain
    data: Mapping[str, Any]


class Confidence(TypedDict):
    value: float
    interpretation: ConfidenceInterpretation
    method: str


class BeliefEvidence(TypedDict):
    evidence_id: Id
    direction: EvidenceDirection
    strength: NotRequired[float]


class DependencyInput(TypedDict):
    belief_id: Id
    relation: DependencyRelation
    strength: NotRequired[float]


class ScopeFilter(TypedDict, total=False):
    project_id: Id
    conversation_id: Id
    agent_id: Id


class RecallPolicy(TypedDict, total=False):
    """Recall policy (``cs_memory::RecallPolicy``). Every field is optional;
    the server fills in its defaults. See ``docs/specifications/recall-scoring.md``."""

    version: int
    allowed_domains: list[MemoryDomain]
    scope_filter: ScopeFilter
    as_of_valid_time: Timestamp
    as_of_transaction_time: Timestamp
    max_candidates: int
    max_results: int
    max_context_tokens: int
    dense_weight: float
    lexical_weight: float
    recency_weight: float
    recency_half_life_days: float
    min_score: float
    include_superseded: bool
    include_candidates: bool
    retain_query_text: bool
    include_beliefs: bool
    max_belief_results: int
    include_disputed_beliefs: bool
    include_retracted_beliefs: bool
    candidates: Mapping[str, Any]
    record_information_needs: bool
    budget: Mapping[str, Any]
    metacognition: Mapping[str, Any]
