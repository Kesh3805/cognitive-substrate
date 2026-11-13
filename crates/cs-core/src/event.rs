//! The append-only cognitive event log.
//!
//! Events are immutable history of authoritative mutations. They are *not*
//! the query model: current state lives in bitemporal tables. Event payloads
//! carry identifiers and metadata only, never memory content, so that
//! governance deletion of content never requires rewriting the log.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{ActorRef, EventId, EventStreamId, Scope, TenantId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitiveEventType {
    MemoryCreated,
    MemoryRevised,
    MemoryPromoted,
    MemoryRejected,
    MemoryTombstoned,
    MemoryPurged,
    BeliefCreated,
    BeliefRevised,
    BeliefDisputed,
    BeliefRetracted,
    BeliefReevaluationRequested,
    EvidenceRecorded,
    DependencyCreated,
    RecallExecuted,
    ObservationRecorded,
    ObservationPurged,
    ExtractionCompleted,
    EvidencePurged,
    ProposalCreated,
    ProposalAccepted,
    ProposalRejected,
    ProposalStale,
    JobDeadLettered,
}

impl CognitiveEventType {
    pub const ALL: [Self; 23] = [
        Self::MemoryCreated,
        Self::MemoryRevised,
        Self::MemoryPromoted,
        Self::MemoryRejected,
        Self::MemoryTombstoned,
        Self::MemoryPurged,
        Self::BeliefCreated,
        Self::BeliefRevised,
        Self::BeliefDisputed,
        Self::BeliefRetracted,
        Self::BeliefReevaluationRequested,
        Self::EvidenceRecorded,
        Self::DependencyCreated,
        Self::RecallExecuted,
        Self::ObservationRecorded,
        Self::ObservationPurged,
        Self::ExtractionCompleted,
        Self::EvidencePurged,
        Self::ProposalCreated,
        Self::ProposalAccepted,
        Self::ProposalRejected,
        Self::ProposalStale,
        Self::JobDeadLettered,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryCreated => "memory_created",
            Self::MemoryRevised => "memory_revised",
            Self::MemoryPromoted => "memory_promoted",
            Self::MemoryRejected => "memory_rejected",
            Self::MemoryTombstoned => "memory_tombstoned",
            Self::MemoryPurged => "memory_purged",
            Self::BeliefCreated => "belief_created",
            Self::BeliefRevised => "belief_revised",
            Self::BeliefDisputed => "belief_disputed",
            Self::BeliefRetracted => "belief_retracted",
            Self::BeliefReevaluationRequested => "belief_reevaluation_requested",
            Self::EvidenceRecorded => "evidence_recorded",
            Self::DependencyCreated => "dependency_created",
            Self::RecallExecuted => "recall_executed",
            Self::ObservationRecorded => "observation_recorded",
            Self::ObservationPurged => "observation_purged",
            Self::ExtractionCompleted => "extraction_completed",
            Self::EvidencePurged => "evidence_purged",
            Self::ProposalCreated => "proposal_created",
            Self::ProposalAccepted => "proposal_accepted",
            Self::ProposalRejected => "proposal_rejected",
            Self::ProposalStale => "proposal_stale",
            Self::JobDeadLettered => "job_dead_lettered",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value)
    }
}

/// Current event schema version written by this build.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitiveEvent {
    pub id: EventId,
    pub stream_id: EventStreamId,
    /// Monotonic per stream, starting at 1. Assigned by storage.
    pub sequence: u64,
    pub scope: Scope,
    pub actor: ActorRef,
    pub event_type: CognitiveEventType,
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    #[serde(default)]
    pub causation_id: Option<EventId>,
    #[serde(default)]
    pub correlation_id: Option<Uuid>,
    pub schema_version: u32,
}

/// An event before storage assigns its sequence and recording time.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    pub id: EventId,
    pub stream_id: EventStreamId,
    pub scope: Scope,
    pub actor: ActorRef,
    pub event_type: CognitiveEventType,
    pub payload: serde_json::Value,
    pub occurred_at: OffsetDateTime,
    pub causation_id: Option<EventId>,
    pub correlation_id: Option<Uuid>,
}

impl EventDraft {
    pub fn into_event(self, sequence: u64, recorded_at: OffsetDateTime) -> CognitiveEvent {
        CognitiveEvent {
            id: self.id,
            stream_id: self.stream_id,
            sequence,
            scope: self.scope,
            actor: self.actor,
            event_type: self.event_type,
            payload: self.payload,
            occurred_at: self.occurred_at,
            recorded_at,
            causation_id: self.causation_id,
            correlation_id: self.correlation_id,
            schema_version: EVENT_SCHEMA_VERSION,
        }
    }
}

/// Event inspection query. Results are always restricted to the caller's
/// tenant and readable scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventQuery {
    #[serde(skip)]
    pub tenant_id: Option<TenantId>,
    #[serde(default)]
    pub stream_id: Option<EventStreamId>,
    #[serde(default)]
    pub event_type: Option<CognitiveEventType>,
    /// Only events with `sequence > after_sequence` (requires `stream_id`).
    #[serde(default)]
    pub after_sequence: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_roundtrip() {
        for t in CognitiveEventType::ALL {
            assert_eq!(CognitiveEventType::parse(t.as_str()), Some(t));
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
        }
    }
}
