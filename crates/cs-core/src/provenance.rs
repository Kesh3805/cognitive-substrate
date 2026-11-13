//! Where knowledge came from and who put it there.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{AgentId, EvidenceId, ObservationId, UserId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: SourceRef,
    pub actor: ActorRef,
    /// When the source information was encountered.
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// When the substrate recorded it. Not a substitute for transaction time.
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    #[serde(default)]
    pub extraction: Option<ExtractionInfo>,
    #[serde(default)]
    pub parent_evidence: Vec<EvidenceId>,
    /// Independent sources that asserted the same content (recorded when
    /// duplicates are merged). A memory survives the purge of its primary
    /// source if a corroborating source remains.
    #[serde(default)]
    pub corroborations: Vec<SourceRef>,
}

impl Provenance {
    /// Extracted or agent-produced content must pass an explicit validation
    /// path before it becomes trusted knowledge. Any extraction counts, not
    /// only model extraction: a rule-based extractor is still an inference.
    pub fn requires_validation(&self) -> bool {
        self.extraction.is_some()
            || matches!(
                self.source,
                SourceRef::AgentOutput { .. } | SourceRef::Observation { .. }
            )
    }

    /// The observation this content was extracted from, if any.
    pub fn observation(&self) -> Option<ObservationId> {
        match self.source {
            SourceRef::Observation { observation_id } => Some(observation_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceRef {
    UserInput {
        #[serde(default)]
        message_id: Option<String>,
    },
    AgentOutput {
        run_id: String,
    },
    ToolOutput {
        tool: String,
        execution_id: String,
    },
    ExternalDocument {
        uri: String,
        content_hash: String,
    },
    HumanAnnotation {
        annotation_id: String,
    },
    SystemDerived {
        derivation: String,
    },
    /// Extracted from a stored raw observation.
    Observation {
        observation_id: ObservationId,
    },
}

impl SourceRef {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Observation { .. } => "observation",
            Self::UserInput { .. } => "user_input",
            Self::AgentOutput { .. } => "agent_output",
            Self::ToolOutput { .. } => "tool_output",
            Self::ExternalDocument { .. } => "external_document",
            Self::HumanAnnotation { .. } => "human_annotation",
            Self::SystemDerived { .. } => "system_derived",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActorRef {
    User { user_id: UserId },
    Agent { agent_id: AgentId },
    System { component: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionInfo {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub prompt_version: Option<String>,
    pub extractor_version: String,
}
