//! Causal hypotheses and interventions (M6.2).
//!
//! A hypothesis is a caller's claim that applying a treatment raises or
//! lowers a binary outcome in a declared environment. It is not a belief:
//! its status is decided only by evaluating recorded interventions, and
//! "supported" is always a claim about the contexts that were sampled, never
//! a universal law. Nothing here is generated automatically, and no model
//! output is evidence.

use std::collections::BTreeMap;

use cs_core::{Error, ExecutionId, HypothesisId, InterventionId, Result, Scope};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{capability::OracleTier, non_empty};

/// Version of the evaluation rules (`docs/specifications/causal-evidence.md`).
pub const CAUSAL_EVALUATION_VERSION: u32 = 1;

/// Maximum length of keys (environment, treatment, outcome, context).
const KEY_MAX: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Raises,
    Lowers,
}

impl Direction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raises => "raises",
            Self::Lowers => "lowers",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Raises, Self::Lowers]
            .into_iter()
            .find(|d| d.as_str() == v)
    }

    /// The effect in the hypothesis's own direction: positive supports it.
    pub fn signed(self, difference: f64) -> f64 {
        match self {
            Self::Raises => difference,
            Self::Lowers => -difference,
        }
    }
}

/// How the arm of an intervention was chosen. Declared at intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assignment {
    /// By a random draw independent of the state, made at intent. Requires
    /// the draw's record (`assignment_detail`, e.g. the seed) and the
    /// probability of treatment.
    Randomized,
    /// Chosen by an agent or a policy that may depend on the state:
    /// observational, never causal evidence.
    Passive,
}

impl Assignment {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Randomized => "randomized",
            Self::Passive => "passive",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Randomized, Self::Passive]
            .into_iter()
            .find(|a| a.as_str() == v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Treated,
    Control,
}

impl Arm {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Treated => "treated",
            Self::Control => "control",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Treated, Self::Control]
            .into_iter()
            .find(|a| a.as_str() == v)
    }
}

/// One outcome observed after an intervention. Recorded once per key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeObservation {
    pub value: bool,
    /// Who established it. Self-assessed outcomes are kept but never count.
    pub tier: OracleTier,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
}

/// A treatment applied (or deliberately withheld) once, in one context.
/// Written at intent, before its outcome is known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intervention {
    pub id: InterventionId,
    pub scope: Scope,
    /// The environment and its version, e.g. `deploy-pipeline@3`.
    pub environment: String,
    /// What is applied, e.g. a capability fingerprint or an action name.
    pub treatment: String,
    pub arm: Arm,
    pub assignment: Assignment,
    /// How the assignment was made (for example, the seed of the draw).
    #[serde(default)]
    pub assignment_detail: Option<String>,
    /// Probability of the treated arm under the draw (randomized only).
    #[serde(default)]
    pub propensity: Option<f64>,
    /// The context partition (for example, a capability input class).
    pub context: String,
    /// Relevant initial-state features, frozen at intent.
    #[serde(default)]
    pub state: BTreeMap<String, String>,
    #[serde(default)]
    pub execution_id: Option<ExecutionId>,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// Outcome key → observation.
    #[serde(default)]
    pub outcomes: BTreeMap<String, OutcomeObservation>,
}

impl Intervention {
    pub fn validate(&self) -> Result<()> {
        non_empty("environment", &self.environment, KEY_MAX)?;
        non_empty("treatment", &self.treatment, KEY_MAX)?;
        non_empty("context", &self.context, KEY_MAX)?;
        if self.state.len() > 32 {
            return Err(Error::validation("at most 32 state features"));
        }
        for (k, v) in &self.state {
            non_empty("state key", k, 100)?;
            if v.chars().count() > KEY_MAX {
                return Err(Error::validation("state value exceeds 200 characters"));
            }
        }
        if let Some(d) = &self.assignment_detail {
            non_empty("assignment_detail", d, 500)?;
        }
        match (self.assignment, self.propensity) {
            (Assignment::Randomized, Some(p)) if p.is_finite() && p > 0.0 && p < 1.0 => {
                if self.assignment_detail.is_none() {
                    return Err(Error::validation(
                        "a randomized assignment records its draw (assignment_detail)",
                    ));
                }
            }
            (Assignment::Randomized, _) => {
                return Err(Error::validation(
                    "a randomized assignment needs a propensity in (0, 1)",
                ));
            }
            (Assignment::Passive, Some(_)) => {
                return Err(Error::validation("a passive assignment has no propensity"));
            }
            (Assignment::Passive, None) => {}
        }
        Ok(())
    }

    /// The outcome under `key`, if established by a tier that may count.
    pub fn eligible_outcome(&self, key: &str) -> Option<bool> {
        self.outcomes
            .get(key)
            .filter(|o| o.tier.is_strong())
            .map(|o| o.value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    /// Not enough randomized evidence yet.
    Proposed,
    /// Randomized evidence supports an effect of at least `min_effect`
    /// within the sampled contexts.
    Supported,
    /// Randomized evidence establishes that the effect is below
    /// `min_effect` (or opposite) within the sampled contexts.
    Refuted,
    /// Interventional evidence is insufficient, heterogeneous or
    /// contradictory.
    Inconclusive,
    /// Withdrawn by a person. Terminal.
    Retired,
}

impl HypothesisStatus {
    pub const ALL: [Self; 5] = [
        Self::Proposed,
        Self::Supported,
        Self::Refuted,
        Self::Inconclusive,
        Self::Retired,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Supported => "supported",
            Self::Refuted => "refuted",
            Self::Inconclusive => "inconclusive",
            Self::Retired => "retired",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == v)
    }
}

/// Arm counts within one context: outcomes counted, and how many were
/// `true`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmCounts {
    pub n: u32,
    pub positive: u32,
}

/// The effect within one context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEffect {
    pub context: String,
    pub treated: ArmCounts,
    pub control: ArmCounts,
    /// Treated minus control proportion, signed by the hypothesis's
    /// direction (positive supports it), with a 95% interval. `None` when
    /// an arm is empty.
    pub effect: Option<f64>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    /// Both arms reach the minimum sample size.
    pub eligible: bool,
}

/// A pooled effect: signed by the hypothesis's direction (positive
/// supports it), with a 95% interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PooledEffect {
    pub estimate: f64,
    pub lower: f64,
    pub upper: f64,
    /// Sum of the contexts' Mantel–Haenszel weights.
    pub weight: f64,
}

/// One evaluation of a hypothesis, recorded with the status it set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HypothesisEvaluation {
    pub version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// Randomized interventions whose outcome counted, sorted.
    pub interventions: Vec<InterventionId>,
    pub contexts: Vec<ContextEffect>,
    pub eligible_contexts: u32,
    /// Stratified Newcombe–Wilson over eligible randomized contexts.
    pub effect: Option<PooledEffect>,
    /// An eligible context's own interval lies entirely in the opposite
    /// direction: support is blocked.
    pub opposite_context: bool,
    /// The contexts a `supported` or `refuted` status speaks for.
    pub scope_contexts: Vec<String>,
    /// Observational lift from passive interventions, the same estimator.
    /// Reported to prioritise what to randomize next; never decides the
    /// status.
    pub observational: Option<PooledEffect>,
    pub status: HypothesisStatus,
}

/// A versioned causal hypothesis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: HypothesisId,
    pub version: u64,
    pub scope: Scope,
    pub environment: String,
    pub treatment: String,
    pub outcome: String,
    pub direction: Direction,
    /// The smallest effect worth supporting, in proportion points (0..1).
    pub min_effect: f64,
    /// The declared target contexts. Empty: whichever contexts are sampled.
    #[serde(default)]
    pub contexts: Vec<String>,
    #[serde(default)]
    pub statement: Option<String>,
    pub status: HypothesisStatus,
    #[serde(default)]
    pub evaluation: Option<HypothesisEvaluation>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Hypothesis {
    pub fn validate(&self) -> Result<()> {
        non_empty("environment", &self.environment, KEY_MAX)?;
        non_empty("treatment", &self.treatment, KEY_MAX)?;
        non_empty("outcome", &self.outcome, KEY_MAX)?;
        if !(self.min_effect.is_finite() && (0.0..1.0).contains(&self.min_effect)) {
            return Err(Error::validation("min_effect must be in 0..1"));
        }
        if self.contexts.len() > 1000 {
            return Err(Error::validation("at most 1000 declared contexts"));
        }
        for c in &self.contexts {
            non_empty("context", c, KEY_MAX)?;
        }
        if let Some(s) = &self.statement {
            non_empty("statement", s, 2000)?;
        }
        Ok(())
    }

    /// Does an intervention bear on this hypothesis?
    pub fn concerns(&self, i: &Intervention) -> bool {
        i.environment == self.environment
            && i.treatment == self.treatment
            && (self.contexts.is_empty() || self.contexts.contains(&i.context))
    }
}
