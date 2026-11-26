//! Bounded predictions (M6.4).
//!
//! A rule is computed on read from a supported hypothesis and one of its
//! contexts; a prediction records, at an intervention's intent, that the
//! rule was relied on, with its numbers frozen. A sequential monitor over a
//! lineage's resolved predictions demotes the lineage when the rule is
//! confidently over-optimistic. Demotions are derived from append-only
//! records, so they are recomputed (identically) on every read and never
//! stored. Predictions are never evidence for a hypothesis.

use std::collections::HashMap;

use cs_core::{
    AccessContext, CognitiveEventType, Error, HypothesisId, InterventionId, PredictionId, Result,
};
use cs_memory::{
    causal::{Arm, Hypothesis, Intervention},
    prediction::{PREDICTION_VERSION, Prediction},
};
use cs_policy::prediction::{
    self as policy, GateDecision, Monitor, OUTCOME_DEADLINE_SECONDS, Rule, RuleRefusal,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Duration, OffsetDateTime};

use crate::{CognitiveEngine, causal::MAX_EVALUATED};

/// A rule with its lineage's monitor, computed on read. Always labelled
/// inferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleView {
    /// Always `"inferred"`.
    pub kind: String,
    pub version: u32,
    pub hypothesis_id: HypothesisId,
    pub hypothesis_version: u64,
    pub context: String,
    #[serde(flatten)]
    pub result: RuleResult,
    pub monitor: Monitor,
    /// Present when a threshold was asked for.
    #[serde(default)]
    pub gate: Option<GateDecision>,
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RuleResult {
    Rule { rule: Rule },
    Refused { reason: RuleRefusal },
}

impl RuleResult {
    fn from(r: Result<Rule, RuleRefusal>) -> Self {
        match r {
            Ok(rule) => Self::Rule { rule },
            Err(reason) => Self::Refused { reason },
        }
    }
}

/// What asking for a prediction did: recorded one, or refused (and wrote
/// nothing).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PredictionResult {
    Recorded { prediction: Box<Prediction> },
    Refused { reason: RuleRefusal },
}

struct Lineage {
    interventions: Vec<Intervention>,
    monitor: Monitor,
}

impl CognitiveEngine {
    /// The interventions bearing on `h` in `context` (its own scope) and the
    /// monitor of the lineage `h` × `context`, as of `now`.
    ///
    /// Rules, requalification blocks and the lineage's predictions only
    /// concern `h`'s exact scope and `context` (a prediction's intervention
    /// is in both), so only those interventions are read, the same for
    /// every caller who can read `h`. The read is never truncated: a
    /// lineage over [`MAX_EVALUATED`] interventions is an error, since a
    /// dropped intervention would score its prediction as a miss.
    async fn lineage(
        &self,
        ctx: &AccessContext,
        h: &Hypothesis,
        context: &str,
        now: OffsetDateTime,
    ) -> Result<Lineage> {
        let interventions: Vec<Intervention> = self
            .store
            .interventions_in_context(
                ctx,
                &h.scope,
                &h.environment,
                &h.treatment,
                context,
                MAX_EVALUATED + 1,
            )
            .await?;
        if interventions.len() > MAX_EVALUATED as usize {
            return Err(Error::validation(format!(
                "the lineage has more than {MAX_EVALUATED} interventions; \
                 a rule is never computed from a truncated read"
            )));
        }
        let predictions = self
            .store
            .predictions_for(&policy::lineage(h, context))
            .await?;
        let by_id: HashMap<InterventionId, &Intervention> =
            interventions.iter().map(|i| (i.id, i)).collect();
        let resolved: Vec<_> = predictions
            .iter()
            .map(|p| {
                let i = by_id.get(&p.intervention_id).copied();
                (p, policy::resolve(p, &h.outcome, i, now))
            })
            .collect();
        let monitor = policy::monitor(&resolved);
        Ok(Lineage {
            interventions,
            monitor,
        })
    }

    /// The rule for a hypothesis in one context, its lineage's monitor and,
    /// with `threshold`, the gate's decision; with `min_effect` too, an
    /// effect gate's. A read: nothing is written.
    pub async fn prediction_rule(
        &self,
        ctx: &AccessContext,
        hypothesis: HypothesisId,
        context: &str,
        threshold: Option<f64>,
        min_effect: Option<f64>,
    ) -> Result<RuleView> {
        crate::non_empty_key("context", context)?;
        if let Some(t) = threshold
            && !(t.is_finite() && (0.0..=1.0).contains(&t))
        {
            return Err(Error::validation("threshold must be in 0..=1"));
        }
        if let Some(m) = min_effect
            && !(m.is_finite() && (-1.0..=1.0).contains(&m))
        {
            return Err(Error::validation("min_effect must be in -1..=1"));
        }
        if min_effect.is_some() && threshold.is_none() {
            return Err(Error::validation(
                "min_effect needs a threshold (0 for none on the treated rate)",
            ));
        }
        let h = self.get_hypothesis(ctx, hypothesis).await?;
        let now = self.now();
        let lineage = self.lineage(ctx, &h, context, now).await?;
        let rule = policy::rule(
            &h,
            context,
            &lineage.interventions,
            lineage.monitor.demoted_at(),
            now,
        );
        Ok(RuleView {
            kind: "inferred".into(),
            version: PREDICTION_VERSION,
            hypothesis_id: h.id,
            hypothesis_version: h.version,
            context: context.to_owned(),
            gate: threshold.map(|t| policy::gate(&rule, t, min_effect)),
            result: RuleResult::from(rule),
            monitor: lineage.monitor,
            as_of: now,
        })
    }

    /// Record a prediction for an intervention under a hypothesis, at
    /// intent: before the intervention has an outcome for the hypothesis's
    /// key. Refusals are results, not errors, and write nothing.
    pub async fn predict(
        &self,
        ctx: &AccessContext,
        intervention: InterventionId,
        hypothesis: HypothesisId,
    ) -> Result<PredictionResult> {
        let i = self.get_intervention(ctx, intervention).await?;
        ctx.ensure_write(&i.scope)?;
        let h = self.get_hypothesis(ctx, hypothesis).await?;
        if let Err(reason) = policy::applicable(&h, &i) {
            return Ok(PredictionResult::Refused { reason });
        }
        if i.outcomes.contains_key(&h.outcome) {
            return Err(Error::conflict(
                "the outcome is already recorded; a prediction is made at intent",
            ));
        }
        let now = self.now();
        let lineage = self.lineage(ctx, &h, &i.context, now).await?;
        let rule = match policy::rule(
            &h,
            &i.context,
            &lineage.interventions,
            lineage.monitor.demoted_at(),
            now,
        ) {
            Ok(r) => r,
            Err(reason) => return Ok(PredictionResult::Refused { reason }),
        };
        // A treated intervention is predicted from the treated arm; a control
        // one (a holdout under an effect gate) from the control arm, and its
        // null is an upper bound.
        let (estimate, lower, upper, null, n) = match i.arm {
            Arm::Treated => (
                rule.estimate,
                rule.lower,
                rule.upper,
                rule.null,
                rule.treated_n,
            ),
            Arm::Control => match rule.control {
                Some(c) => (c.estimate, c.lower, c.upper, c.control_null(), c.n),
                None => {
                    return Ok(PredictionResult::Refused {
                        reason: RuleRefusal::Imprecise,
                    });
                }
            },
        };
        let p = Prediction {
            id: PredictionId::new_v7(),
            scope: i.scope.clone(),
            intervention_id: i.id,
            hypothesis_id: h.id,
            hypothesis_version: h.version,
            context: i.context.clone(),
            lineage: policy::lineage(&h, &i.context),
            arm: i.arm,
            favourable: rule.favourable,
            estimate,
            lower,
            upper,
            null,
            treated_n: n,
            requalified: rule.requalified,
            recorded_at: now,
            deadline: now + Duration::seconds(OUTCOME_DEADLINE_SECONDS),
        };
        let event = self.draft(
            ctx,
            p.id.0,
            &p.scope,
            CognitiveEventType::PredictionRecorded,
            json!({
                "prediction_id": p.id,
                "intervention_id": p.intervention_id,
                "hypothesis_id": p.hypothesis_id,
                "hypothesis_version": p.hypothesis_version,
                "requalified": p.requalified,
            }),
            now,
        );
        self.store.insert_prediction(&p, &h.outcome, event).await?;
        Ok(PredictionResult::Recorded {
            prediction: Box::new(p),
        })
    }
}
