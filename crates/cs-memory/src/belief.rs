//! Beliefs: separately versioned propositions with a structured evidence base.

use cs_core::{
    ActorRef, BeliefId, Bitemporal, Error, EvidenceId, ExtractionInfo, Result, Scope, SourceRef,
    TenantId,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{non_empty, unit_interval};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Belief {
    /// Identity of this revision.
    pub id: BeliefId,
    /// Identity of the first revision; stable across the lineage.
    pub lineage_id: BeliefId,
    pub scope: Scope,
    pub proposition: String,
    pub polarity: BeliefPolarity,
    pub confidence: Confidence,
    pub revision_number: u32,
    #[serde(default)]
    pub supersedes: Option<BeliefId>,
    #[serde(default)]
    pub superseded_by: Option<BeliefId>,
    pub revision_reason: String,
    pub status: BeliefStatus,
    /// Revalidate after this many days; `None` means no staleness policy.
    #[serde(default)]
    pub revalidation_interval_days: Option<u32>,
    pub temporal: Bitemporal,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Belief {
    pub fn is_current(&self) -> bool {
        self.temporal.transaction_time.is_open()
    }

    /// Normalized proposition used for contradiction detection.
    pub fn normalized_proposition(&self) -> String {
        normalize(&self.proposition)
    }

    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        non_empty("proposition", &self.proposition, 4096)?;
        non_empty("revision_reason", &self.revision_reason, 2048)?;
        self.confidence.validate()?;
        self.temporal.valid_time.validate()?;
        if self.revision_number == 0 {
            return Err(Error::validation("revision_number starts at 1"));
        }
        if self.revalidation_interval_days == Some(0) {
            return Err(Error::validation(
                "revalidation_interval_days must be positive",
            ));
        }
        Ok(())
    }
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeliefPolarity {
    /// The proposition is believed to hold.
    Supports,
    /// The proposition is believed not to hold.
    Refutes,
}

impl BeliefPolarity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Refutes => "refutes",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "supports" => Some(Self::Supports),
            "refutes" => Some(Self::Refutes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeliefStatus {
    Active,
    /// Conflicting evidence is unresolved. Never silently averaged away.
    Disputed,
    /// Withdrawn. Remains queryable historically.
    Retracted,
}

impl BeliefStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disputed => "disputed",
            Self::Retracted => "retracted",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "active" => Some(Self::Active),
            "disputed" => Some(Self::Disputed),
            "retracted" => Some(Self::Retracted),
            _ => None,
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Active | Self::Disputed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    /// Constrained to `[0, 1]`.
    pub value: f64,
    pub interpretation: ConfidenceInterpretation,
    /// How the value was produced (free text, e.g. `"analyst-judgement"`).
    pub method: String,
}

impl Confidence {
    pub fn validate(&self) -> Result<()> {
        unit_interval("confidence.value", self.value)?;
        non_empty("confidence.method", &self.method, 256)
    }
}

/// What a confidence number *means*. A heuristic score must never be
/// presented as a calibrated probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceInterpretation {
    CalibratedProbability,
    HeuristicScore,
    HumanAssessment,
    ModelEstimate,
}

impl ConfidenceInterpretation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CalibratedProbability => "calibrated_probability",
            Self::HeuristicScore => "heuristic_score",
            Self::HumanAssessment => "human_assessment",
            Self::ModelEstimate => "model_estimate",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "calibrated_probability" => Self::CalibratedProbability,
            "heuristic_score" => Self::HeuristicScore,
            "human_assessment" => Self::HumanAssessment,
            "model_estimate" => Self::ModelEstimate,
            _ => return None,
        })
    }
}

/// A normalized piece of evidence that beliefs can cite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub tenant_id: TenantId,
    pub source: SourceRef,
    pub actor: ActorRef,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    #[serde(default)]
    pub extraction: Option<ExtractionInfo>,
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Short human-readable excerpt or description.
    #[serde(default)]
    pub summary: Option<String>,
}

impl Evidence {
    pub fn validate(&self) -> Result<()> {
        if let Some(s) = &self.summary {
            non_empty("evidence.summary", s, 4096)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDirection {
    Supports,
    Refutes,
    Contextual,
}

impl EvidenceDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Refutes => "refutes",
            Self::Contextual => "contextual",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "supports" => Self::Supports,
            "refutes" => Self::Refutes,
            "contextual" => Self::Contextual,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeliefEvidence {
    pub evidence_id: EvidenceId,
    pub direction: EvidenceDirection,
    #[serde(default)]
    pub strength: Option<f64>,
}

impl BeliefEvidence {
    pub fn validate(&self) -> Result<()> {
        if let Some(s) = self.strength {
            unit_interval("evidence.strength", s)?;
        }
        Ok(())
    }
}

/// Directed, typed edge. An edge `from -> to` means `to` depends on `from`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeliefDependency {
    pub from: BeliefId,
    pub to: BeliefId,
    pub relation: DependencyRelation,
    #[serde(default)]
    pub strength: Option<f64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl BeliefDependency {
    pub fn validate(&self) -> Result<()> {
        if self.from == self.to {
            return Err(Error::validation("a belief cannot depend on itself"));
        }
        if let Some(s) = self.strength {
            unit_interval("dependency.strength", s)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyRelation {
    Supports,
    Requires,
    DerivedFrom,
    Contradicts,
    Generalizes,
    Specializes,
}

impl DependencyRelation {
    pub const ALL: [Self; 6] = [
        Self::Supports,
        Self::Requires,
        Self::DerivedFrom,
        Self::Contradicts,
        Self::Generalizes,
        Self::Specializes,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Requires => "requires",
            Self::DerivedFrom => "derived_from",
            Self::Contradicts => "contradicts",
            Self::Generalizes => "generalizes",
            Self::Specializes => "specializes",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == v)
    }

    /// Does `to` rely on `from` being true?
    pub fn is_reliance(self) -> bool {
        matches!(
            self,
            Self::Supports | Self::Requires | Self::DerivedFrom | Self::Specializes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_ignores_case_and_punctuation() {
        assert_eq!(normalize("  The API, supports X! "), "the api supports x");
    }

    #[test]
    fn confidence_must_be_in_unit_interval() {
        let c = Confidence {
            value: 1.5,
            interpretation: ConfidenceInterpretation::HeuristicScore,
            method: "test".into(),
        };
        assert!(c.validate().is_err());
        let nan = Confidence {
            value: f64::NAN,
            ..c
        };
        assert!(nan.validate().is_err());
    }
}
