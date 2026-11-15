//! Information needs and knowledge gaps (M5.4).
//!
//! See `cs_memory::need` for the model. Here: recording misses from recalls
//! that opted in, explicit questions, validation (a complete search that
//! establishes absence or finds evidence), and acquisition priority.

use std::collections::BTreeMap;

use cs_core::{
    AccessContext, CognitiveEventType, Error, InformationNeedId, MemoryRevisionId, Result, Scope,
    Visibility,
};
use cs_memory::{
    CandidateGeneration, DenseStatus, MemoryDomain, MemoryRecord, MemoryStatus, RecallTrace,
    entity::partition_key,
    need::{
        AcquisitionPriority, DETECTOR_VERSION, EvidenceRef, GapValidation, GapValidationOutcome,
        InformationNeed, NeedOrigin, NeedStatus, Signature, TermStats, acquisition_priority,
        qualifies,
    },
};
use cs_storage::{CandidateOrder, MemoryQuery};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use crate::CognitiveEngine;

/// Lexical candidates examined by validation. If more eligible revisions
/// share a term with the need, the validation is incomplete.
pub const VALIDATION_BUDGET: u32 = 5_000;
/// Evidence references kept on a need.
const MAX_EVIDENCE: usize = 10;

/// What a recall that opted in did to an information need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedOutcome {
    pub need_id: InformationNeedId,
    pub status: NeedStatus,
    /// `true` when the recall missed; `false` when it satisfied the need.
    pub missed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedView {
    pub need: InformationNeed,
    pub priority: AcquisitionPriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskQuestion {
    pub question: String,
    #[serde(default = "one")]
    pub impact: u8,
    #[serde(default = "one")]
    pub cost: u8,
}

const fn one() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedChange {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedWeights {
    pub impact: u8,
    pub cost: u8,
    pub expected_version: u32,
}

/// The caller's own scope: needs belong to the principal that had them,
/// never to a shared scope where their terms would be visible to others.
fn own_scope(ctx: &AccessContext) -> Option<Scope> {
    match (ctx.user_id, ctx.agent_id) {
        (Some(u), _) => Some(Scope::private(ctx.tenant_id, u)),
        (None, Some(a)) => Some(
            Scope::tenant(ctx.tenant_id)
                .with_agent(a)
                .with_visibility(Visibility::Agent),
        ),
        (None, None) => None,
    }
}

fn evidence(records: &[&MemoryRecord]) -> Vec<EvidenceRef> {
    records
        .iter()
        .take(MAX_EVIDENCE)
        .map(|m| EvidenceRef {
            memory_id: m.id,
            revision_id: m.revision_id,
        })
        .collect()
}

impl CognitiveEngine {
    fn need_query(
        &self,
        ctx: &AccessContext,
        at: OffsetDateTime,
        statuses: Vec<MemoryStatus>,
    ) -> MemoryQuery {
        MemoryQuery {
            access: ctx.clone(),
            domains: MemoryDomain::ALL.to_vec(),
            statuses,
            scope_filter: None,
            as_of_valid_time: at,
            as_of_transaction_time: at,
            include_superseded: false,
            ignore_valid_time: false,
            embedding_model: None,
            limit: u32::MAX,
            order: CandidateOrder::Recent,
        }
    }

    async fn stats(
        &self,
        ctx: &AccessContext,
        sig: &Signature,
        at: OffsetDateTime,
    ) -> Result<TermStats> {
        let q = self.need_query(ctx, at, vec![MemoryStatus::Active]);
        self.store.term_stats(&q, &sig.terms).await
    }

    /// After a present-time recall that opted in: record a miss, or
    /// satisfy the open need its signature names.
    pub(crate) async fn observe_recall(
        &self,
        ctx: &AccessContext,
        query: &str,
        session: Option<&str>,
        trace: &RecallTrace,
        records: &BTreeMap<MemoryRevisionId, MemoryRecord>,
    ) -> Result<Option<NeedOutcome>> {
        let Some(scope) = own_scope(ctx) else {
            return Ok(None);
        };
        let Some(sig) = Signature::of(query) else {
            return Ok(None);
        };
        // A miss counts only when the search could have found the evidence:
        // the lexical channel ran, and the dense channel, if asked for,
        // was not unavailable.
        let lexical = matches!(
            trace.policy.candidates,
            CandidateGeneration::Channels { lexical, .. } if lexical > 0
        );
        if !lexical || matches!(trace.dense, DenseStatus::Unavailable { .. }) {
            return Ok(None);
        }
        let at = trace.executed_at;
        let stats = self.stats(ctx, &sig, at).await?;
        let qualifying: Vec<&MemoryRecord> = trace
            .candidates
            .iter()
            .filter_map(|c| records.get(&c.revision_id))
            .filter(|m| qualifies(&sig, &stats, &m.content.index_terms()))
            .collect();
        let partition = partition_key(&scope);
        if qualifying.is_empty() {
            let template = InformationNeed {
                id: InformationNeedId::new_v7(),
                scope: scope.clone(),
                partition,
                signature: sig,
                query_hash: cs_policy::query_hash(query),
                question: None,
                origin: NeedOrigin::Observed,
                status: NeedStatus::RetrievalFailure,
                sessions: 1,
                occurrences: 1,
                first_seen: at,
                last_seen: at,
                last_trace: Some(trace.id),
                validation: None,
                resolved_by: Vec::new(),
                impact: 1,
                cost: 1,
                version: 1,
                created_at: at,
                updated_at: at,
            };
            // Repeats within one session count once; without a session
            // key, one UTC day is one session.
            let session =
                session.map_or_else(|| format!("day:{}", at.date()), |s| format!("session:{s}"));
            // The store sets the stream and payload (ids and counts only).
            let event = self.draft(
                ctx,
                template.id.0,
                &scope,
                CognitiveEventType::InformationNeedObserved,
                json!({}),
                at,
            );
            let need = self
                .store
                .observe_need(&template, &session, trace.id, at, event)
                .await?;
            return Ok(Some(NeedOutcome {
                need_id: need.id,
                status: need.status,
                missed: true,
            }));
        }
        // Evidence was found: an open need with this signature is satisfied.
        let Some(mut need) = self.store.find_need(&partition, &sig.key()).await? else {
            return Ok(None);
        };
        if !need.status.is_open() {
            return Ok(Some(NeedOutcome {
                need_id: need.id,
                status: need.status,
                missed: false,
            }));
        }
        let expected = need.version;
        need.status = NeedStatus::Satisfied;
        need.resolved_by = evidence(&qualifying);
        need.last_trace = Some(trace.id);
        need.version += 1;
        need.updated_at = at;
        let event = self.need_event(ctx, &need, "satisfied by recall", at);
        let need = self.store.write_need(&need, Some(expected), event).await?;
        Ok(Some(NeedOutcome {
            need_id: need.id,
            status: need.status,
            missed: false,
        }))
    }

    fn need_event(
        &self,
        ctx: &AccessContext,
        n: &InformationNeed,
        change: &str,
        at: OffsetDateTime,
    ) -> cs_core::EventDraft {
        self.draft(
            ctx,
            n.id.0,
            &n.scope,
            CognitiveEventType::InformationNeedChanged,
            json!({
                "need_id": n.id,
                "status": n.status,
                "change": change,
                "version": n.version,
            }),
            at,
        )
    }

    async fn readable_need(
        &self,
        ctx: &AccessContext,
        id: InformationNeedId,
    ) -> Result<InformationNeed> {
        self.store
            .get_need(id)
            .await?
            .filter(|n| ctx.can_read(&n.scope))
            .ok_or_else(|| Error::not_found("information need", id))
    }

    pub async fn get_need(&self, ctx: &AccessContext, id: InformationNeedId) -> Result<NeedView> {
        let need = self.readable_need(ctx, id).await?;
        Ok(NeedView {
            priority: acquisition_priority(&need, self.now()),
            need,
        })
    }

    /// Needs the caller can read, by acquisition priority (descending,
    /// then id), evaluated now.
    pub async fn list_needs(
        &self,
        ctx: &AccessContext,
        status: Option<NeedStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<NeedView>> {
        let now = self.now();
        let mut v: Vec<NeedView> = self
            .store
            .list_needs(ctx, status, 10_000)
            .await?
            .into_iter()
            .map(|need| NeedView {
                priority: acquisition_priority(&need, now),
                need,
            })
            .collect();
        v.sort_by(|a, b| {
            b.priority
                .priority
                .total_cmp(&a.priority.priority)
                .then_with(|| a.need.id.cmp(&b.need.id))
        });
        v.truncate(limit.unwrap_or(100).min(1_000) as usize);
        Ok(v)
    }

    /// Record an explicit question as an unresolved need. Asking
    /// establishes demand, never absence.
    pub async fn ask(&self, ctx: &AccessContext, req: AskQuestion) -> Result<NeedView> {
        let scope =
            own_scope(ctx).ok_or_else(|| Error::validation("needs belong to a user or agent"))?;
        let sig = Signature::of(&req.question)
            .ok_or_else(|| Error::validation("question has no terms"))?;
        let partition = partition_key(&scope);
        let now = self.now();
        if let Some(existing) = self.store.find_need(&partition, &sig.key()).await? {
            return Ok(NeedView {
                priority: acquisition_priority(&existing, now),
                need: existing,
            });
        }
        let need = InformationNeed {
            id: InformationNeedId::new_v7(),
            scope,
            partition,
            signature: sig,
            query_hash: cs_policy::query_hash(&req.question),
            question: Some(req.question.trim().to_owned()),
            origin: NeedOrigin::Explicit,
            status: NeedStatus::Unresolved,
            sessions: 1,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
            last_trace: None,
            validation: None,
            resolved_by: Vec::new(),
            impact: req.impact,
            cost: req.cost,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        need.validate_inputs()?;
        let event = self.need_event(ctx, &need, "asked", now);
        let need = self.store.write_need(&need, None, event).await?;
        Ok(NeedView {
            priority: acquisition_priority(&need, now),
            need,
        })
    }

    /// Search the caller's whole eligible substrate, as of now, for
    /// evidence. Complete-and-empty proposes a candidate gap (or, if the
    /// engine is configured to, validates it). Evidence satisfies the need,
    /// or marks it pending promotion if all of it is unvalidated.
    /// An incomplete search changes nothing but the recorded validation.
    pub async fn validate_need(
        &self,
        ctx: &AccessContext,
        id: InformationNeedId,
    ) -> Result<NeedView> {
        let mut need = self.readable_need(ctx, id).await?;
        ctx.ensure_write(&need.scope)?;
        if !need.status.is_open() {
            return Err(Error::conflict(format!("need is {}", need.status.as_str())));
        }
        let now = self.now();
        let stats = self.stats(ctx, &need.signature, now).await?;
        // Every revision that could qualify shares a term with the need,
        // so the lexical order over all of them is a complete search.
        let found = self
            .store
            .query_memories(&MemoryQuery {
                limit: VALIDATION_BUDGET + 1,
                order: CandidateOrder::Lexical(need.signature.terms.clone()),
                ..self.need_query(
                    ctx,
                    now,
                    vec![MemoryStatus::Active, MemoryStatus::Candidate],
                )
            })
            .await?;
        let examined = u32::try_from(found.len()).unwrap_or(u32::MAX);
        let outcome = if found.len() > VALIDATION_BUDGET as usize {
            GapValidationOutcome::Incomplete {
                reason: format!(
                    "more than {VALIDATION_BUDGET} eligible revisions share a term with the need"
                ),
            }
        } else {
            let qualifying: Vec<&MemoryRecord> = found
                .iter()
                .map(|s| &s.record)
                .filter(|m| qualifies(&need.signature, &stats, &m.content.index_terms()))
                .collect();
            if qualifying.is_empty() {
                GapValidationOutcome::CompleteNoEvidence
            } else {
                let active: Vec<&MemoryRecord> = qualifying
                    .iter()
                    .copied()
                    .filter(|m| m.status == MemoryStatus::Active)
                    .collect();
                let unvalidated_only = active.is_empty();
                GapValidationOutcome::CompleteEvidenceFound {
                    evidence: evidence(if unvalidated_only {
                        &qualifying
                    } else {
                        &active
                    }),
                    unvalidated_only,
                }
            }
        };
        let expected = need.version;
        match &outcome {
            GapValidationOutcome::CompleteNoEvidence => {
                need.status = if self.processing.auto_validate_gaps {
                    NeedStatus::ValidatedGap
                } else {
                    NeedStatus::CandidateGap
                };
            }
            GapValidationOutcome::CompleteEvidenceFound {
                evidence,
                unvalidated_only,
            } => {
                need.status = if *unvalidated_only {
                    NeedStatus::PendingPromotion
                } else {
                    NeedStatus::Satisfied
                };
                need.resolved_by = evidence.clone();
            }
            GapValidationOutcome::Incomplete { .. } => {}
        }
        need.validation = Some(GapValidation {
            outcome,
            detector: DETECTOR_VERSION.to_owned(),
            as_of: now,
            candidates_examined: examined,
        });
        need.version += 1;
        need.updated_at = now;
        let event = self.draft(
            ctx,
            need.id.0,
            &need.scope,
            CognitiveEventType::InformationNeedValidated,
            json!({
                "need_id": need.id,
                "status": need.status,
                "detector": DETECTOR_VERSION,
                "candidates_examined": examined,
                "version": need.version,
            }),
            now,
        );
        let need = self.store.write_need(&need, Some(expected), event).await?;
        Ok(NeedView {
            priority: acquisition_priority(&need, now),
            need,
        })
    }

    async fn change_need(
        &self,
        ctx: &AccessContext,
        id: InformationNeedId,
        change: &str,
        apply: impl FnOnce(&mut InformationNeed) -> Result<()>,
    ) -> Result<NeedView> {
        let mut need = self.readable_need(ctx, id).await?;
        ctx.ensure_write(&need.scope)?;
        let expected = need.version;
        apply(&mut need)?;
        need.validate_inputs()?;
        let now = self.now();
        need.version += 1;
        need.updated_at = now;
        let event = self.need_event(ctx, &need, change, now);
        let need = self.store.write_need(&need, Some(expected), event).await?;
        Ok(NeedView {
            priority: acquisition_priority(&need, now),
            need,
        })
    }

    /// A person confirms a candidate gap.
    pub async fn confirm_gap(
        &self,
        ctx: &AccessContext,
        id: InformationNeedId,
        req: NeedChange,
    ) -> Result<NeedView> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        self.change_need(ctx, id, "gap confirmed", |n| {
            if n.status != NeedStatus::CandidateGap {
                return Err(Error::conflict("only a candidate gap can be confirmed"));
            }
            n.status = NeedStatus::ValidatedGap;
            Ok(())
        })
        .await
    }

    pub async fn dismiss_need(
        &self,
        ctx: &AccessContext,
        id: InformationNeedId,
        req: NeedChange,
    ) -> Result<NeedView> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("reason must not be empty"));
        }
        self.change_need(ctx, id, "dismissed", |n| {
            n.status = NeedStatus::Dismissed;
            Ok(())
        })
        .await
    }

    pub async fn weigh_need(
        &self,
        ctx: &AccessContext,
        id: InformationNeedId,
        req: NeedWeights,
    ) -> Result<NeedView> {
        self.change_need(ctx, id, "weights changed", |n| {
            if n.version != req.expected_version {
                return Err(Error::conflict("information need changed; reload it"));
            }
            n.impact = req.impact;
            n.cost = req.cost;
            Ok(())
        })
        .await
    }

    /// Erase a need (its signature may contain sensitive terms).
    pub async fn purge_need(&self, ctx: &AccessContext, id: InformationNeedId) -> Result<()> {
        let need = self.readable_need(ctx, id).await?;
        ctx.ensure_write(&need.scope)?;
        let event = self.draft(
            ctx,
            need.id.0,
            &need.scope,
            CognitiveEventType::InformationNeedChanged,
            json!({ "need_id": need.id, "change": "purged" }),
            self.now(),
        );
        self.store.purge_need(id, event).await
    }
}
