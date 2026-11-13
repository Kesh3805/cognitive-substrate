//! Where knowledge came from and who put it there.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{AgentId, EvidenceId, UserId};

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
}

impl Provenance {
    /// Model-derived content must pass an explicit validation path before it
    /// becomes trusted knowledge.
    pub fn is_model_derived(&self) -> bool {
        self.extraction.as_ref().is_some_and(|e| e.model.is_some())
            || matches!(self.source, SourceRef::AgentOutput { .. })
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
}

impl SourceRef {
    pub const fn kind(&self) -> &'static str {
        match self {
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
