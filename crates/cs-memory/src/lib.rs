//! Domain model: what the substrate retains and what it believes.
//!
//! Three things are deliberately kept distinct:
//!
//! * a [`MemoryRecord`] — something retained by the system;
//! * a [`Belief`] — a proposition accepted to some degree, based on evidence;
//! * a `CognitiveEvent` (in `cs-core`) — an immutable record of a change.

pub mod belief;
pub mod embedding;
pub mod entity;
pub mod job;
pub mod observation;
pub mod proposal;
pub mod recall;
pub mod record;

pub use belief::{
    Belief, BeliefDependency, BeliefEvidence, BeliefPolarity, BeliefStatus, Confidence,
    ConfidenceInterpretation, DependencyRelation, Evidence, EvidenceDirection,
};
pub use embedding::{EmbeddingRecord, EmbeddingSpace};
pub use job::{ClaimedJob, Job, JobError, JobKind, JobOutcome, JobStatus, Lease, RetryPolicy};
pub use observation::{ExtractedCandidate, ExtractionRun, Observation};
pub use proposal::{
    DetectionMetadata, ProposalDecision, ProposalStatus, RecordVersion, ResolutionKind,
    ResolutionOperation, ResolutionProposal, ResolutionRationale, ResolutionSignal,
};
pub use recall::{DenseStatus, RecallBeliefCandidate, RecallCandidate, RecallPolicy, RecallTrace};
pub use record::{
    CapabilityStage, EpisodicMemory, MemoryContent, MemoryDomain, MemoryRecord, MemoryStatus,
    ProceduralMemory, RelationalMemory, SemanticMemory, TemporalChangeKind, TemporalMemory,
    WorkingMemory,
};

pub(crate) fn non_empty(field: &str, value: &str, max: usize) -> cs_core::Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(cs_core::Error::validation(format!(
            "{field} must not be empty"
        )));
    }
    if value.chars().count() > max {
        return Err(cs_core::Error::validation(format!(
            "{field} exceeds {max} characters"
        )));
    }
    Ok(())
}

pub(crate) fn unit_interval(field: &str, value: f64) -> cs_core::Result<()> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(cs_core::Error::validation(format!(
            "{field} must be a finite value in [0, 1]"
        )))
    }
}
