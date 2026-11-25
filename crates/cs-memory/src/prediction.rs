//! Bounded predictions (M6.4).
//!
//! A supported hypothesis, in one context of its scope, is a *rule*: "in
//! this environment and context, applying the treatment yields the
//! favourable outcome with probability in [lower, upper]". Rules are
//! computed on read and never stored. A **prediction** is the record that a
//! rule was relied on for one intervention, written at intent, before the
//! outcome is known, with the rule's numbers frozen in it. Predictions are
//! never evidence for a hypothesis: they are scored against outcomes, and a
//! sequential monitor over a rule's scored predictions demotes the rule when
//! it is confidently over-optimistic.
//!
//! Demotion is not stored either: it is a deterministic function of
//! append-only records (predictions and once-only outcomes), so the same
//! records always yield the same demotions, and a read as of any time
//! replays them.

use cs_core::{HypothesisId, InterventionId, PredictionId, Scope};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Version of the prediction rules (`docs/specifications/predictions.md`).
pub const PREDICTION_VERSION: u32 = 1;

/// One prediction, written at intent for one intervention under one
/// hypothesis. Immutable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub id: PredictionId,
    /// The intervention's scope (and the hypothesis's: they must match).
    pub scope: Scope,
    pub intervention_id: InterventionId,
    pub hypothesis_id: HypothesisId,
    /// The hypothesis version whose evaluation the rule came from.
    pub hypothesis_version: u64,
    pub context: String,
    /// The lineage (scope, environment, treatment, outcome, direction,
    /// context) that demotions attach to; shared by every hypothesis that
    /// states the same claim.
    pub lineage: String,
    /// The value of the hypothesis's outcome key that counts as favourable:
    /// `true` for `raises`, `false` for `lowers`.
    pub favourable: bool,
    /// Jeffreys estimate of P(favourable | treated, context).
    pub estimate: f64,
    /// 95% Wilson interval of the same proportion.
    pub lower: f64,
    pub upper: f64,
    /// The monitored null: the live favourable rate is at least this.
    /// The rule's lower bound, floored.
    pub null: f64,
    /// Treated outcomes the rule was computed from.
    pub treated_n: u32,
    /// The rule was re-earned from evidence recorded after a demotion.
    pub requalified: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// No strong-tier outcome by then: scored as unfavourable.
    #[serde(with = "time::serde::rfc3339")]
    pub deadline: OffsetDateTime,
}

/// How a prediction resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// A strong-tier outcome arrived by the deadline.
    Observed { favourable: bool },
    /// Nothing that counts arrived by the deadline: unfavourable.
    Missed,
}

impl Resolution {
    pub const fn favourable(self) -> bool {
        matches!(self, Self::Observed { favourable: true })
    }
}
