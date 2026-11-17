//! Causal hypotheses and interventions (M6.2).
//!
//! Callers state hypotheses and record interventions; the engine only
//! evaluates. A status changes only by evaluating recorded evidence (or by
//! a person retiring the hypothesis), and every evaluation is kept in the
//! hypothesis's version history.

use std::collections::BTreeMap;

use cs_core::{
    AccessContext, CognitiveEventType, Error, ExecutionId, HypothesisId, InterventionId, Result,
    Scope,
};
use cs_memory::{
    capability::OracleTier,
    causal::{
        Arm, Assignment, Direction, Hypothesis, HypothesisStatus, Intervention, OutcomeObservation,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::CognitiveEngine;

/// Interventions read for one evaluation, at most.
pub const MAX_EVALUATED: u32 = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewHypothesis {
    pub scope: Scope,
    pub environment: String,
    pub treatment: String,
    pub outcome: String,
    pub direction: Direction,
    #[serde(default)]
    pub min_effect: f64,
    #[serde(default)]
    pub contexts: Vec<String>,
    #[serde(default)]
    pub statement: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewIntervention {
    pub scope: Scope,
    pub environment: String,
    pub treatment: String,
    pub arm: Arm,
    pub assignment: Assignment,
    #[serde(default)]
    pub assignment_detail: Option<String>,
    #[serde(default)]
    pub propensity: Option<f64>,
    pub context: String,
    #[serde(default)]
    pub state: BTreeMap<String, String>,
    #[serde(default)]
    pub execution_id: Option<ExecutionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveOutcome {
    pub outcome: String,
    pub value: bool,
    pub tier: OracleTier,
}

impl CognitiveEngine {
    pub async fn create_hypothesis(
        &self,
        ctx: &AccessContext,
        req: NewHypothesis,
    ) -> Result<Hypothesis> {
        ctx.ensure_write(&req.scope)?;
        let now = self.now();
        let mut contexts = req.contexts;
        contexts.sort();
        contexts.dedup();
        let h = Hypothesis {
            id: HypothesisId::new_v7(),
            version: 1,
            scope: req.scope,
            environment: req.environment,
            treatment: req.treatment,
            outcome: req.outcome,
            direction: req.direction,
            min_effect: req.min_effect,
            contexts,
            statement: req.statement,
            status: HypothesisStatus::Proposed,
            evaluation: None,
            created_at: now,
            updated_at: now,
        };
        h.validate()?;
        let event = self.hypothesis_event(ctx, &h, "created");
        self.store.write_hypothesis(&h, None, event).await
    }

    pub async fn get_hypothesis(
        &self,
        ctx: &AccessContext,
        id: HypothesisId,
    ) -> Result<Hypothesis> {
        self.store
            .get_hypothesis(id)
            .await?
            .filter(|h| ctx.can_read(&h.scope))
            .ok_or_else(|| Error::not_found("hypothesis", id))
    }

    /// Every version, oldest first: each status change with the evaluation
    /// that made it.
    pub async fn hypothesis_history(
        &self,
        ctx: &AccessContext,
        id: HypothesisId,
    ) -> Result<Vec<Hypothesis>> {
        self.get_hypothesis(ctx, id).await?;
        self.store.hypothesis_history(id).await
    }

    pub async fn list_hypotheses(
        &self,
        ctx: &AccessContext,
        status: Option<HypothesisStatus>,
        limit: u32,
    ) -> Result<Vec<Hypothesis>> {
        self.store
            .list_hypotheses(ctx, status, limit.clamp(1, 500))
            .await
    }

    /// Record an intervention at intent, before its outcome is known.
    pub async fn record_intervention(
        &self,
        ctx: &AccessContext,
        req: NewIntervention,
    ) -> Result<Intervention> {
        ctx.ensure_write(&req.scope)?;
        if let Some(e) = req.execution_id {
            self.store
                .get_execution(e)
                .await?
                .filter(|x| ctx.can_read(&x.scope))
                .ok_or_else(|| Error::not_found("execution", e))?;
        }
        let i = Intervention {
            id: InterventionId::new_v7(),
            scope: req.scope,
            environment: req.environment,
            treatment: req.treatment,
            arm: req.arm,
            assignment: req.assignment,
            assignment_detail: req.assignment_detail,
            propensity: req.propensity,
            context: req.context,
            state: req.state,
            execution_id: req.execution_id,
            recorded_at: self.now(),
            outcomes: BTreeMap::new(),
        };
        i.validate()?;
        let event = self.draft(
            ctx,
            i.id.0,
            &i.scope,
            CognitiveEventType::InterventionRecorded,
            json!({
                "intervention_id": i.id,
                "arm": i.arm,
                "assignment": i.assignment,
            }),
            i.recorded_at,
        );
        self.store.insert_intervention(&i, event).await?;
        Ok(i)
    }

    pub async fn get_intervention(
        &self,
        ctx: &AccessContext,
        id: InterventionId,
    ) -> Result<Intervention> {
        self.store
            .get_intervention(id)
            .await?
            .filter(|i| ctx.can_read(&i.scope))
            .ok_or_else(|| Error::not_found("intervention", id))
    }

    /// Record one outcome of an intervention, once per outcome key.
    pub async fn observe_outcome(
        &self,
        ctx: &AccessContext,
        id: InterventionId,
        req: ObserveOutcome,
    ) -> Result<Intervention> {
        let i = self.get_intervention(ctx, id).await?;
        ctx.ensure_write(&i.scope)?;
        crate::non_empty_key("outcome", &req.outcome)?;
        let now = self.now();
        let observation = OutcomeObservation {
            value: req.value,
            tier: req.tier,
            observed_at: now,
        };
        let event = self.draft(
            ctx,
            i.id.0,
            &i.scope,
            CognitiveEventType::InterventionObserved,
            json!({
                "intervention_id": i.id,
                "tier": req.tier,
            }),
            now,
        );
        self.store
            .observe_intervention(id, &req.outcome, &observation, event)
            .await
    }

    /// Evaluate a hypothesis against the interventions recorded in its own
    /// scope, and write the resulting version. The status is whatever the
    /// evidence says now (it may go down as well as up); the evaluation and
    /// the interventions it counted are kept with it.
    pub async fn evaluate_hypothesis(
        &self,
        ctx: &AccessContext,
        id: HypothesisId,
        expected_version: u64,
    ) -> Result<Hypothesis> {
        let mut h = self.get_hypothesis(ctx, id).await?;
        ctx.ensure_write(&h.scope)?;
        if h.status == HypothesisStatus::Retired {
            return Err(Error::validation("a retired hypothesis is not evaluated"));
        }
        if h.version != expected_version {
            return Err(Error::conflict("hypothesis changed; reload it"));
        }
        let interventions: Vec<Intervention> = self
            .store
            .interventions_for(ctx, &h.environment, &h.treatment, MAX_EVALUATED)
            .await?
            .into_iter()
            .filter(|i| i.scope == h.scope)
            .collect();
        let now = self.now();
        let evaluation = cs_policy::causal::evaluate(&h, &interventions, now);
        h.status = evaluation.status;
        h.evaluation = Some(evaluation);
        h.version += 1;
        h.updated_at = now;
        let event = self.hypothesis_event(ctx, &h, "evaluated");
        self.store
            .write_hypothesis(&h, Some(expected_version), event)
            .await
    }

    /// Withdraw a hypothesis. Terminal.
    pub async fn retire_hypothesis(
        &self,
        ctx: &AccessContext,
        id: HypothesisId,
        expected_version: u64,
    ) -> Result<Hypothesis> {
        let mut h = self.get_hypothesis(ctx, id).await?;
        ctx.ensure_write(&h.scope)?;
        if h.status == HypothesisStatus::Retired {
            return Err(Error::validation("hypothesis is already retired"));
        }
        h.status = HypothesisStatus::Retired;
        h.version += 1;
        h.updated_at = self.now();
        let event = self.hypothesis_event(ctx, &h, "retired");
        self.store
            .write_hypothesis(&h, Some(expected_version), event)
            .await
    }

    fn hypothesis_event(
        &self,
        ctx: &AccessContext,
        h: &Hypothesis,
        change: &str,
    ) -> cs_core::EventDraft {
        let e = h.evaluation.as_ref();
        self.draft(
            ctx,
            h.id.0,
            &h.scope,
            CognitiveEventType::HypothesisChanged,
            json!({
                "hypothesis_id": h.id,
                "version": h.version,
                "status": h.status,
                "change": change,
                "eligible_contexts": e.map(|e| e.eligible_contexts),
                "interventions": e.map(|e| e.interventions.len()),
            }),
            h.updated_at,
        )
    }
}

/// A counterfactual, computed on request from one randomized intervention
/// and one supported hypothesis (M6.3). Always labelled inferred; never
/// stored, never evented, never evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterfactualView {
    /// Always `"inferred"`.
    pub kind: String,
    pub version: u32,
    pub hypothesis_id: HypothesisId,
    pub hypothesis_version: u64,
    pub intervention_id: InterventionId,
    #[serde(flatten)]
    pub result: CounterfactualResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CounterfactualResult {
    Bounds {
        bounds: cs_policy::counterfactual::Bounds,
    },
    Refused {
        reason: cs_policy::counterfactual::Refusal,
    },
}

impl CognitiveEngine {
    /// Compute a counterfactual for an intervention under a hypothesis. A
    /// read: nothing is written. Refusals are results, not errors.
    pub async fn counterfactual(
        &self,
        ctx: &AccessContext,
        intervention: InterventionId,
        hypothesis: HypothesisId,
    ) -> Result<CounterfactualView> {
        let i = self.get_intervention(ctx, intervention).await?;
        let h = self.get_hypothesis(ctx, hypothesis).await?;
        let result = match cs_policy::counterfactual::counterfactual(&h, &i) {
            Ok(bounds) => CounterfactualResult::Bounds { bounds },
            Err(reason) => CounterfactualResult::Refused { reason },
        };
        Ok(CounterfactualView {
            kind: "inferred".into(),
            version: cs_policy::counterfactual::COUNTERFACTUAL_VERSION,
            hypothesis_id: h.id,
            hypothesis_version: h.version,
            intervention_id: i.id,
            result,
        })
    }
}
