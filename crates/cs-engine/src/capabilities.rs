//! Procedural learning (M4.4): executions, verification, evaluation suites
//! and the capability lifecycle.
//!
//! `observed → candidate → evaluated → trusted`, with `quarantined`
//! (suspended by evidence, reinstatable) and `deprecated` (retired,
//! terminal). Promotions past `candidate` require the evaluation to
//! support them; quarantine happens automatically when recent evidence
//! says the capability is failing.

use cs_core::{
    AccessContext, CognitiveEventType, Error, EvaluationCaseId, ExecutionId, MemoryId,
    MemoryRevisionId, Result, TimeInterval, VerificationId,
};
use cs_memory::{
    CapabilityStage, MemoryContent, MemoryRecord, ProceduralMemory,
    capability::{
        AbortReason, CaseKind, CaseOrigin, EvaluationCase, Execution, ExecutionStatus, MAX_DETAIL,
        OracleTier, Verdict, Verification, validate_input_class,
    },
};
use cs_policy::capability::{CapabilityEvaluation, evaluate};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{CognitiveEngine, actor};

/// Executions considered per evaluation.
const MAX_EXECUTIONS: u32 = 10_000;

// ---- contracts --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartExecution {
    /// Opaque class of the input (a structural signature or hash), never
    /// the raw input.
    pub input_class: String,
    #[serde(default)]
    pub case_id: Option<EvaluationCaseId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishExecution {
    pub status: ExecutionStatus,
    #[serde(default)]
    pub abort_reason: Option<AbortReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyExecution {
    pub tier: OracleTier,
    pub verdict: Verdict,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCase {
    pub kind: CaseKind,
    pub input_class: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeStage {
    pub stage: CapabilityStage,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityView {
    pub memory: MemoryRecord,
    pub cases: Vec<EvaluationCase>,
    pub evaluation: CapabilityEvaluation,
}

fn procedure(m: &MemoryRecord) -> Result<&ProceduralMemory> {
    match &m.content {
        MemoryContent::Procedural(p) => Ok(p),
        _ => Err(Error::validation("memory is not a procedural capability")),
    }
}

impl CognitiveEngine {
    async fn capability(&self, ctx: &AccessContext, id: MemoryId) -> Result<MemoryRecord> {
        let m = self
            .store
            .current_memory(id)
            .await?
            .filter(|m| ctx.can_read(&m.scope))
            .ok_or_else(|| Error::not_found("capability", id))?;
        procedure(&m)?;
        Ok(m)
    }

    async fn readable_execution(&self, ctx: &AccessContext, id: ExecutionId) -> Result<Execution> {
        self.store
            .get_execution(id)
            .await?
            .filter(|e| ctx.can_read(&e.scope))
            .ok_or_else(|| Error::not_found("execution", id))
    }

    async fn evaluation_of(&self, m: &MemoryRecord) -> Result<CapabilityEvaluation> {
        let p = procedure(m)?;
        let executions = self.store.executions_for(m.id, MAX_EXECUTIONS).await?;
        let cases = self.store.cases_for(m.id).await?;
        Ok(evaluate(
            p.stage,
            p.fingerprint(),
            &executions,
            &cases,
            &self.processing.capability,
            self.now(),
        ))
    }

    /// A capability with its suite and current evaluation.
    pub async fn get_capability(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
    ) -> Result<CapabilityView> {
        let memory = self.capability(ctx, id).await?;
        Ok(CapabilityView {
            cases: self.store.cases_for(id).await?,
            evaluation: self.evaluation_of(&memory).await?,
            memory,
        })
    }

    /// Record an execution at intent. Evidence is counted from here, so
    /// runs that never report back still count (as failures, after the
    /// timeout).
    pub async fn start_execution(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: StartExecution,
    ) -> Result<Execution> {
        let m = self.capability(ctx, id).await?;
        ctx.ensure_write(&m.scope)?;
        let p = procedure(&m)?;
        if p.stage == CapabilityStage::Deprecated {
            return Err(Error::conflict("capability is deprecated"));
        }
        validate_input_class(&req.input_class)?;
        if let Some(case) = req.case_id
            && !self.store.cases_for(id).await?.iter().any(|c| c.id == case)
        {
            return Err(Error::validation("case does not belong to this capability"));
        }
        let now = self.now();
        let e = Execution {
            id: ExecutionId::new_v7(),
            capability_id: m.id,
            capability_revision: m.revision_id,
            procedure_fingerprint: p.fingerprint(),
            scope: m.scope.clone(),
            case_id: req.case_id,
            input_class: req.input_class,
            actor: actor(ctx),
            status: ExecutionStatus::Started,
            abort_reason: None,
            started_at: now,
            finished_at: None,
            verifications: Vec::new(),
        };
        let event = self.draft(
            ctx,
            m.id.0,
            &m.scope,
            CognitiveEventType::ExecutionStarted,
            json!({ "execution_id": e.id, "capability_id": m.id, "case_id": e.case_id }),
            now,
        );
        self.store.insert_execution(&e, event).await
    }

    pub async fn finish_execution(
        &self,
        ctx: &AccessContext,
        id: ExecutionId,
        req: FinishExecution,
    ) -> Result<Execution> {
        let e = self.readable_execution(ctx, id).await?;
        ctx.ensure_write(&e.scope)?;
        match (req.status, req.abort_reason) {
            (ExecutionStatus::Completed, None) | (ExecutionStatus::Aborted, Some(_)) => {}
            (ExecutionStatus::Started, _) => {
                return Err(Error::validation("finish as completed or aborted"));
            }
            _ => {
                return Err(Error::validation(
                    "an abort needs a reason, and only an abort has one",
                ));
            }
        }
        let now = self.now();
        let event = self.draft(
            ctx,
            e.capability_id.0,
            &e.scope,
            CognitiveEventType::ExecutionFinished,
            json!({
                "execution_id": id,
                "status": req.status,
                "abort_reason": req.abort_reason,
            }),
            now,
        );
        let out = self
            .store
            .finish_execution(id, req.status, req.abort_reason, now, event)
            .await?;
        self.quarantine_if_failing(ctx, e.capability_id).await?;
        Ok(out)
    }

    /// Record a verifier's verdict. A completed field run that fails a
    /// strong verification becomes a locked regression case (the failure
    /// ratchet), atomically.
    pub async fn verify_execution(
        &self,
        ctx: &AccessContext,
        id: ExecutionId,
        req: VerifyExecution,
    ) -> Result<Execution> {
        let e = self.readable_execution(ctx, id).await?;
        ctx.ensure_write(&e.scope)?;
        if e.status == ExecutionStatus::Started {
            return Err(Error::conflict("execution has not finished"));
        }
        if let Some(d) = &req.detail
            && d.chars().count() > MAX_DETAIL
        {
            return Err(Error::validation(format!(
                "detail exceeds {MAX_DETAIL} characters"
            )));
        }
        let now = self.now();
        let v = Verification {
            id: VerificationId::new_v7(),
            execution_id: id,
            tier: req.tier,
            verdict: req.verdict,
            verifier: actor(ctx),
            detail: req.detail,
            recorded_at: now,
        };
        let event = self.draft(
            ctx,
            e.capability_id.0,
            &e.scope,
            CognitiveEventType::ExecutionVerified,
            json!({
                "execution_id": id,
                "verification_id": v.id,
                "tier": v.tier,
                "verdict": v.verdict,
            }),
            now,
        );
        let ratchet = (e.status == ExecutionStatus::Completed
            && e.case_id.is_none()
            && v.tier.is_strong()
            && v.verdict == Verdict::Failed)
            .then(|| {
                let case = EvaluationCase {
                    id: EvaluationCaseId::new_v7(),
                    capability_id: e.capability_id,
                    scope: e.scope.clone(),
                    kind: CaseKind::Regression,
                    input_class: e.input_class.clone(),
                    description: Some(format!("failed verification of execution {id}")),
                    locked: true,
                    origin: CaseOrigin::Ratchet { execution_id: id },
                    created_at: now,
                };
                let event = self.case_event(ctx, &case);
                (case, event)
            });
        let out = self
            .store
            .add_verification(&v, ratchet.as_ref().map(|(c, ev)| (c, ev.clone())), event)
            .await?;
        self.quarantine_if_failing(ctx, e.capability_id).await?;
        Ok(out)
    }

    fn case_event(&self, ctx: &AccessContext, c: &EvaluationCase) -> cs_core::EventDraft {
        self.draft(
            ctx,
            c.capability_id.0,
            &c.scope,
            CognitiveEventType::EvaluationCaseAdded,
            json!({
                "case_id": c.id,
                "capability_id": c.capability_id,
                "kind": c.kind,
                "locked": c.locked,
                "origin": c.origin,
            }),
            c.created_at,
        )
    }

    pub async fn get_execution(&self, ctx: &AccessContext, id: ExecutionId) -> Result<Execution> {
        self.readable_execution(ctx, id).await
    }

    /// Executions of a capability, newest first.
    pub async fn list_executions(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        limit: Option<u32>,
    ) -> Result<Vec<Execution>> {
        self.capability(ctx, id).await?;
        let mut v = self.store.executions_for(id, MAX_EXECUTIONS).await?;
        v.reverse();
        v.truncate(limit.unwrap_or(100).min(1000) as usize);
        Ok(v)
    }

    /// Add a case to a capability's suite. Regression cases are locked.
    pub async fn add_case(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: NewCase,
    ) -> Result<EvaluationCase> {
        let m = self.capability(ctx, id).await?;
        ctx.ensure_write(&m.scope)?;
        validate_input_class(&req.input_class)?;
        if let Some(d) = &req.description
            && d.chars().count() > MAX_DETAIL
        {
            return Err(Error::validation(format!(
                "description exceeds {MAX_DETAIL} characters"
            )));
        }
        let case = EvaluationCase {
            id: EvaluationCaseId::new_v7(),
            capability_id: m.id,
            scope: m.scope.clone(),
            kind: req.kind,
            input_class: req.input_class,
            description: req.description,
            locked: req.kind == CaseKind::Regression,
            origin: CaseOrigin::Authored,
            created_at: self.now(),
        };
        let event = self.case_event(ctx, &case);
        self.store.insert_case(&case, event).await
    }

    /// Move a capability through its lifecycle. Promotion to `evaluated`
    /// or `trusted` requires the current evaluation to support it.
    pub async fn change_capability_stage(
        &self,
        ctx: &AccessContext,
        id: MemoryId,
        req: ChangeStage,
    ) -> Result<MemoryRecord> {
        use CapabilityStage as S;
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        let m = self.capability(ctx, id).await?;
        ctx.ensure_write(&m.scope)?;
        let from = procedure(&m)?.stage;
        let allowed = match (from, req.stage) {
            (a, b) if a == b => return Ok(m),
            (S::Deprecated, _) | (_, S::Observed) => false,
            (_, S::Deprecated) => true,
            (S::Observed | S::Quarantined, S::Candidate) => true,
            (S::Candidate | S::Evaluated | S::Trusted, S::Quarantined) => true,
            (S::Candidate, S::Evaluated) | (S::Evaluated, S::Trusted) => {
                let e = self.evaluation_of(&m).await?;
                if e.eligible < Some(req.stage) {
                    return Err(Error::validation(format!(
                        "evidence does not support {}: {}",
                        req.stage.as_str(),
                        e.blockers.join("; ")
                    )));
                }
                true
            }
            // Manual demotion.
            (S::Trusted, S::Evaluated) | (S::Evaluated | S::Trusted, S::Candidate) => true,
            _ => false,
        };
        if !allowed {
            return Err(Error::validation(format!(
                "cannot move a capability from {} to {}",
                from.as_str(),
                req.stage.as_str()
            )));
        }
        self.set_stage(ctx, &m, req.stage, &req.reason).await
    }

    async fn set_stage(
        &self,
        ctx: &AccessContext,
        m: &MemoryRecord,
        stage: CapabilityStage,
        reason: &str,
    ) -> Result<MemoryRecord> {
        let mut p = procedure(m)?.clone();
        let from = p.stage;
        p.stage = stage;
        let closed_at = self.next_tx(m.temporal.transaction_time.start);
        let mut next = m.clone();
        next.revision_id = MemoryRevisionId::new_v7();
        next.revision_number = m.revision_number + 1;
        next.supersedes = Some(m.revision_id);
        next.superseded_by = None;
        next.content = MemoryContent::Procedural(p);
        next.status_reason = Some(reason.to_owned());
        next.temporal.transaction_time = TimeInterval::from(closed_at);
        next.created_at = closed_at;
        next.validate()?;
        let event = self.draft(
            ctx,
            m.id.0,
            &m.scope,
            CognitiveEventType::CapabilityStageChanged,
            json!({
                "memory_id": m.id,
                "revision_id": next.revision_id,
                "supersedes": m.revision_id,
                "from": from,
                "to": stage,
                "reason": reason,
            }),
            closed_at,
        );
        let (embedding, follow_up) = self.memory_embedding_plan(ctx, &next).await;
        self.store
            .supersede_memory(
                m.revision_id,
                closed_at,
                &next,
                embedding.as_ref(),
                &follow_up,
                event,
            )
            .await?;
        Ok(next)
    }

    /// Quarantine automatically when recent field evidence says the
    /// capability is failing. Only stages that claim usefulness are
    /// suspended; observed, quarantined and deprecated ones are left alone.
    async fn quarantine_if_failing(&self, ctx: &AccessContext, id: MemoryId) -> Result<()> {
        let Some(m) = self.store.current_memory(id).await? else {
            return Ok(());
        };
        let stage = procedure(&m)?.stage;
        if !matches!(
            stage,
            CapabilityStage::Candidate | CapabilityStage::Evaluated | CapabilityStage::Trusted
        ) {
            return Ok(());
        }
        if let Some(reason) = self.evaluation_of(&m).await?.quarantine {
            self.set_stage(
                ctx,
                &m,
                CapabilityStage::Quarantined,
                &format!("automatic quarantine: {reason}"),
            )
            .await?;
        }
        Ok(())
    }
}
