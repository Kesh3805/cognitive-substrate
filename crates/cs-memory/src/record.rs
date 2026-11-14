//! One record envelope, six typed domain payloads.

use cs_core::{
    Bitemporal, CapabilityId, Error, EventId, MemoryId, MemoryRevisionId, Provenance, Result, Scope,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::non_empty;

const MAX_TEXT: usize = 16_384;
const MAX_LABEL: usize = 512;
const MAX_ITEMS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDomain {
    /// What happened.
    Episodic,
    /// What is known.
    Semantic,
    /// How it is done.
    Procedural,
    /// How things connect.
    Relational,
    /// How knowledge changed.
    Temporal,
    /// What matters right now. Always ephemeral.
    Working,
}

impl MemoryDomain {
    pub const ALL: [Self; 6] = [
        Self::Episodic,
        Self::Semantic,
        Self::Procedural,
        Self::Relational,
        Self::Temporal,
        Self::Working,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
            Self::Relational => "relational",
            Self::Temporal => "temporal",
            Self::Working => "working",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == value)
    }
}

/// Lifecycle status of a revision.
///
/// "Superseded" is not a status: a revision is superseded when its
/// transaction-time interval is closed. The status of a closed revision is
/// therefore exactly what it was while it was current, which keeps
/// as-of-transaction-time queries truthful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    /// Proposed (e.g. model-extracted) and not yet validated.
    Candidate,
    Active,
    Rejected,
    Tombstoned,
}

impl MemoryStatus {
    pub const ALL: [Self; 4] = [
        Self::Candidate,
        Self::Active,
        Self::Rejected,
        Self::Tombstoned,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Rejected => "rejected",
            Self::Tombstoned => "tombstoned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub revision_id: MemoryRevisionId,
    /// 1 for the first revision, incremented on each revision.
    pub revision_number: u32,
    pub scope: Scope,
    pub content: MemoryContent,
    pub provenance: Provenance,
    pub temporal: Bitemporal,
    pub status: MemoryStatus,
    #[serde(default)]
    pub status_reason: Option<String>,
    #[serde(default)]
    pub supersedes: Option<MemoryRevisionId>,
    #[serde(default)]
    pub superseded_by: Option<MemoryRevisionId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

impl MemoryRecord {
    pub fn domain(&self) -> MemoryDomain {
        self.content.domain()
    }

    pub fn is_current(&self) -> bool {
        self.temporal.transaction_time.is_open()
    }

    pub fn is_expired(&self, at: OffsetDateTime) -> bool {
        self.expires_at.is_some_and(|e| e <= at)
    }

    /// Envelope invariants independent of storage.
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        self.content.validate()?;
        self.temporal.valid_time.validate()?;
        if self.domain() == MemoryDomain::Working && self.expires_at.is_none() {
            return Err(Error::validation("working memory requires expires_at"));
        }
        if let Some(expires) = self.expires_at
            && expires <= self.created_at
        {
            return Err(Error::validation("expires_at must be after created_at"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "domain", content = "data", rename_all = "snake_case")]
pub enum MemoryContent {
    Episodic(EpisodicMemory),
    Semantic(SemanticMemory),
    Procedural(ProceduralMemory),
    Relational(RelationalMemory),
    Temporal(TemporalMemory),
    Working(WorkingMemory),
}

impl MemoryContent {
    pub fn domain(&self) -> MemoryDomain {
        match self {
            Self::Episodic(_) => MemoryDomain::Episodic,
            Self::Semantic(_) => MemoryDomain::Semantic,
            Self::Procedural(_) => MemoryDomain::Procedural,
            Self::Relational(_) => MemoryDomain::Relational,
            Self::Temporal(_) => MemoryDomain::Temporal,
            Self::Working(_) => MemoryDomain::Working,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Episodic(m) => {
                non_empty("episodic.description", &m.description, MAX_TEXT)?;
                let parts = [
                    ("episodic.subject", &m.subject),
                    ("episodic.predicate", &m.predicate),
                    ("episodic.object", &m.object),
                ];
                let present = parts.iter().filter(|(_, v)| v.is_some()).count();
                if present != 0 && present != 3 {
                    return Err(Error::validation(
                        "episodic subject, predicate and object go together",
                    ));
                }
                for (field, value) in parts {
                    if let Some(v) = value {
                        non_empty(field, v, MAX_LABEL)?;
                    }
                }
                bounded_list("episodic.participants", &m.participants)
            }
            Self::Semantic(m) => {
                non_empty("semantic.proposition", &m.proposition, MAX_TEXT)?;
                for (field, value) in [
                    ("semantic.subject", &m.subject),
                    ("semantic.predicate", &m.predicate),
                    ("semantic.object", &m.object),
                ] {
                    if let Some(v) = value {
                        non_empty(field, v, MAX_LABEL)?;
                    }
                }
                Ok(())
            }
            Self::Procedural(m) => {
                non_empty("procedural.name", &m.name, MAX_LABEL)?;
                if m.steps.is_empty() {
                    return Err(Error::validation("procedural.steps must not be empty"));
                }
                bounded_list("procedural.steps", &m.steps)?;
                bounded_list("procedural.preconditions", &m.preconditions)
            }
            Self::Relational(m) => {
                non_empty("relational.subject", &m.subject, MAX_LABEL)?;
                non_empty("relational.predicate", &m.predicate, MAX_LABEL)?;
                non_empty("relational.object", &m.object, MAX_LABEL)
            }
            Self::Temporal(m) => {
                non_empty("temporal.subject", &m.subject, MAX_LABEL)?;
                non_empty("temporal.predicate", &m.predicate, MAX_LABEL)?;
                non_empty("temporal.value", &m.value, MAX_TEXT)
            }
            Self::Working(m) => {
                if let Some(o) = &m.objective {
                    non_empty("working.objective", o, MAX_TEXT)?;
                }
                if m.priority > 10 {
                    return Err(Error::validation("working.priority must be in 0..=10"));
                }
                bounded_list("working.active_context", &m.active_context)?;
                bounded_list("working.unresolved_questions", &m.unresolved_questions)
            }
        }
    }

    /// Canonical text used for lexical indexing and embedding. Deterministic.
    pub fn index_text(&self) -> String {
        match self {
            Self::Episodic(m) => {
                let mut s = m.description.clone();
                if !m.participants.is_empty() {
                    s.push_str(&format!(" (participants: {})", m.participants.join(", ")));
                }
                if let Some((sub, pred, obj)) = m.triple() {
                    s.push_str(&format!(" [{sub} {pred} {obj}]"));
                }
                s
            }
            Self::Semantic(m) => {
                let mut s = m.proposition.clone();
                if let (Some(sub), Some(pred), Some(obj)) = (&m.subject, &m.predicate, &m.object) {
                    s.push_str(&format!(" [{sub} {pred} {obj}]"));
                }
                s
            }
            Self::Procedural(m) => {
                let mut s = format!("{}: {}", m.name, m.steps.join("; "));
                if !m.preconditions.is_empty() {
                    s.push_str(&format!(" (when: {})", m.preconditions.join("; ")));
                }
                if let Some(o) = &m.expected_outcome {
                    s.push_str(&format!(" -> {o}"));
                }
                s
            }
            Self::Relational(m) => format!("{} {} {}", m.subject, m.predicate, m.object),
            Self::Temporal(m) => format!("{} {} {}", m.subject, m.predicate, m.value),
            Self::Working(m) => {
                let mut parts = Vec::new();
                if let Some(o) = &m.objective {
                    parts.push(o.clone());
                }
                parts.extend(m.active_context.iter().cloned());
                parts.extend(m.unresolved_questions.iter().cloned());
                parts.join(" | ")
            }
        }
    }
}

fn bounded_list(field: &str, items: &[String]) -> Result<()> {
    if items.len() > MAX_ITEMS {
        return Err(Error::validation(format!(
            "{field} exceeds {MAX_ITEMS} items"
        )));
    }
    for item in items {
        non_empty(field, item, MAX_TEXT)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodicMemory {
    pub description: String,
    #[serde(default)]
    pub event_id: Option<EventId>,
    #[serde(default)]
    pub participants: Vec<String>,
    /// Optional structured reading of what happened (`API` `called`
    /// `auth`). All three or none. Recurring episode triples from
    /// independent sources can be compiled into semantic knowledge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

impl EpisodicMemory {
    /// The structured triple, when all three parts are present.
    pub fn triple(&self) -> Option<(&str, &str, &str)> {
        match (&self.subject, &self.predicate, &self.object) {
            (Some(s), Some(p), Some(o)) => Some((s, p, o)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticMemory {
    pub proposition: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub predicate: Option<String>,
    #[serde(default)]
    pub object: Option<String>,
}

/// Capability lifecycle: a successful workflow is not automatically trusted.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStage {
    #[default]
    Observed,
    Candidate,
    Evaluated,
    Trusted,
    /// Retired on purpose. Terminal.
    Deprecated,
    /// Suspended because recent evidence says it is failing. Distinct from
    /// deprecation: a quarantined capability can be reinstated as a
    /// candidate and must re-earn its evaluation.
    Quarantined,
}

impl CapabilityStage {
    pub const ALL: [Self; 6] = [
        Self::Observed,
        Self::Candidate,
        Self::Evaluated,
        Self::Trusted,
        Self::Deprecated,
        Self::Quarantined,
    ];

    /// Has this capability passed its required evaluation?
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Evaluated | Self::Trusted)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Candidate => "candidate",
            Self::Evaluated => "evaluated",
            Self::Trusted => "trusted",
            Self::Deprecated => "deprecated",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == v)
    }
}

impl ProceduralMemory {
    /// Identity of the procedure itself (name, preconditions, steps,
    /// expected outcome), independent of its lifecycle stage. Evidence
    /// carries across revisions that only change the stage, and resets
    /// when the procedure changes.
    pub fn fingerprint(&self) -> uuid::Uuid {
        const NS: uuid::Uuid = uuid::Uuid::from_u128(0x8d0b_52e1_44c7_4f3a_b6e9_0a17_c3d5_92f4);
        let canon = serde_json::json!([
            self.name,
            self.preconditions,
            self.steps,
            self.expected_outcome
        ]);
        uuid::Uuid::new_v5(&NS, canon.to_string().as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProceduralMemory {
    #[serde(default)]
    pub capability_id: Option<CapabilityId>,
    pub name: String,
    #[serde(default)]
    pub preconditions: Vec<String>,
    pub steps: Vec<String>,
    #[serde(default)]
    pub expected_outcome: Option<String>,
    #[serde(default)]
    pub stage: CapabilityStage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationalMemory {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default = "default_true")]
    pub directed: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalChangeKind {
    Created,
    Updated,
    Invalidated,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalMemory {
    pub subject: String,
    pub predicate: String,
    pub value: String,
    pub change_kind: TemporalChangeKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingMemory {
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub active_context: Vec<String>,
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
    #[serde(default)]
    pub priority: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_serializes_with_domain_tag() {
        let c = MemoryContent::Semantic(SemanticMemory {
            proposition: "Postgres is authoritative".into(),
            subject: None,
            predicate: None,
            object: None,
        });
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["domain"], "semantic");
        assert_eq!(v["data"]["proposition"], "Postgres is authoritative");
        let back: MemoryContent = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn empty_procedure_is_rejected() {
        let c = MemoryContent::Procedural(ProceduralMemory {
            capability_id: None,
            name: "deploy".into(),
            preconditions: vec![],
            steps: vec![],
            expected_outcome: None,
            stage: CapabilityStage::Observed,
        });
        assert!(c.validate().is_err());
    }

    #[test]
    fn stage_ordering_tracks_lifecycle() {
        assert!(CapabilityStage::Observed < CapabilityStage::Trusted);
        assert!(!CapabilityStage::Candidate.is_verified());
        assert!(CapabilityStage::Evaluated.is_verified());
    }
}
