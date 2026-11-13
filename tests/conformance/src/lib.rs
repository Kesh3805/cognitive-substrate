//! Behavioural conformance suite.
//!
//! Every [`CognitiveStore`] adapter must pass every scenario. Scenarios use
//! fresh tenants, so they can share one database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    missing_debug_implementations
)]

pub mod m3;

use std::sync::Arc;

use cs_core::{
    AccessContext, CognitiveEventType, Error, EventQuery, EventStreamId, ExtractionInfo,
    FixedClock, ProjectId, Scope, SourceRef, TenantId, UserId, Visibility,
};
use cs_engine::{
    CognitiveEngine, DependencyInput, NewBelief, NewDependency, NewEvidence, NewMemory,
    ProvenanceInput, RecallRequest, ReplayRequest, ReviseBelief, ReviseMemory, StatusChange,
};
use cs_memory::{
    BeliefEvidence, BeliefPolarity, BeliefStatus, Confidence, ConfidenceInterpretation,
    DependencyRelation, EvidenceDirection, MemoryContent, MemoryDomain, MemoryStatus, RecallPolicy,
    SemanticMemory, WorkingMemory,
};
use cs_policy::DebtCategory;
use cs_storage::CognitiveStore;
use time::{Duration, OffsetDateTime, macros::datetime};

pub struct Harness {
    pub engine: CognitiveEngine,
    pub clock: Arc<FixedClock>,
    pub tenant: TenantId,
    pub alice: AccessContext,
    pub bob: AccessContext,
}

impl Harness {
    pub fn new(store: Arc<dyn CognitiveStore>) -> Self {
        let clock = Arc::new(FixedClock::new(datetime!(2026-06-01 9:00 UTC)));
        let engine = CognitiveEngine::builder(store).clock(clock.clone()).build();
        let tenant = TenantId::new_v7();
        Self {
            engine,
            clock,
            tenant,
            alice: AccessContext::user(tenant, UserId::new_v7()),
            bob: AccessContext::user(tenant, UserId::new_v7()),
        }
    }

    pub fn tick(&self, minutes: i64) {
        self.clock.advance(Duration::minutes(minutes));
    }

    pub fn alice_scope(&self) -> Scope {
        Scope::private(self.tenant, self.alice.user_id.unwrap())
    }

    pub fn fact(&self, scope: Scope, text: &str) -> NewMemory {
        NewMemory {
            scope,
            content: MemoryContent::Semantic(SemanticMemory {
                proposition: text.into(),
                subject: None,
                predicate: None,
                object: None,
            }),
            provenance: ProvenanceInput {
                source: SourceRef::UserInput { message_id: None },
                observed_at: None,
                extraction: None,
                parent_evidence: vec![],
            },
            valid_time: None,
            expires_at: None,
            idempotency_key: None,
        }
    }

    pub fn belief(&self, proposition: &str, evidence: Vec<BeliefEvidence>) -> NewBelief {
        NewBelief {
            scope: self.alice_scope(),
            proposition: proposition.into(),
            polarity: BeliefPolarity::Supports,
            confidence: Confidence {
                value: 0.8,
                interpretation: ConfidenceInterpretation::HumanAssessment,
                method: "conformance".into(),
            },
            evidence,
            depends_on: vec![],
            valid_time: None,
            revalidation_interval_days: None,
            reason: None,
            idempotency_key: None,
        }
    }

    pub async fn evidence(&self, direction: EvidenceDirection) -> BeliefEvidence {
        let e = self
            .engine
            .record_evidence(
                &self.alice,
                NewEvidence {
                    source: SourceRef::HumanAnnotation {
                        annotation_id: "note-1".into(),
                    },
                    observed_at: None,
                    extraction: None,
                    content_hash: None,
                    summary: Some("observed in production".into()),
                },
            )
            .await
            .unwrap();
        BeliefEvidence {
            evidence_id: e.id,
            direction,
            strength: Some(0.9),
        }
    }
}

fn recall(query: &str) -> RecallRequest {
    RecallRequest {
        query: query.into(),
        policy: RecallPolicy {
            min_score: 0.05,
            ..RecallPolicy::default()
        },
    }
}

pub async fn memory_lifecycle(h: &Harness) {
    let m = h
        .engine
        .create_memory(&h.alice, h.fact(h.alice_scope(), "We deploy on Fridays"))
        .await
        .unwrap();
    assert_eq!(m.status, MemoryStatus::Active);
    assert_eq!(m.revision_number, 1);
    assert_eq!(h.engine.get_memory(&h.alice, m.id).await.unwrap(), m);

    h.tick(5);
    let revised = h
        .engine
        .revise_memory(
            &h.alice,
            m.id,
            ReviseMemory {
                content: Some(MemoryContent::Semantic(SemanticMemory {
                    proposition: "We never deploy on Fridays".into(),
                    subject: None,
                    predicate: None,
                    object: None,
                })),
                valid_time: None,
                expires_at: None,
                provenance: None,
                reason: "policy changed".into(),
                expected_revision: Some(m.revision_id),
            },
        )
        .await
        .unwrap();
    assert_eq!(revised.revision_number, 2);
    assert_eq!(revised.supersedes, Some(m.revision_id));

    let history = h
        .engine
        .memory_history(&h.alice, m.id)
        .await
        .unwrap()
        .revisions;
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].temporal.transaction_time.end,
        Some(revised.temporal.transaction_time.start)
    );
    assert_eq!(history[0].superseded_by, Some(revised.revision_id));
    assert!(history[1].is_current());

    // Optimistic concurrency.
    let stale = h
        .engine
        .revise_memory(
            &h.alice,
            m.id,
            ReviseMemory {
                content: None,
                valid_time: None,
                expires_at: None,
                provenance: None,
                reason: "stale writer".into(),
                expected_revision: Some(m.revision_id),
            },
        )
        .await;
    assert!(matches!(stale, Err(Error::Conflict(_))));

    // Domain cannot change across revisions.
    let wrong = h
        .engine
        .revise_memory(
            &h.alice,
            m.id,
            ReviseMemory {
                content: Some(MemoryContent::Working(WorkingMemory {
                    objective: Some("x".into()),
                    active_context: vec![],
                    unresolved_questions: vec![],
                    priority: 1,
                })),
                valid_time: None,
                expires_at: None,
                provenance: None,
                reason: "bad".into(),
                expected_revision: None,
            },
        )
        .await;
    assert!(matches!(wrong, Err(Error::Validation(_))));
}

pub async fn bitemporal_recall_is_reproducible(h: &Harness) {
    let m = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "The primary database is MySQL"),
        )
        .await
        .unwrap();
    h.tick(10);
    let first = h
        .engine
        .recall(&h.alice, recall("which database is primary"))
        .await
        .unwrap();
    assert_eq!(first.results.len(), 1);
    assert_eq!(first.results[0].memory.revision_id, m.revision_id);

    h.tick(10);
    let mut req = h.fact(h.alice_scope(), "The primary database is PostgreSQL");
    let rev = h
        .engine
        .revise_memory(
            &h.alice,
            m.id,
            ReviseMemory {
                content: Some(req.content.clone()),
                valid_time: None,
                expires_at: None,
                provenance: Some(std::mem::replace(
                    &mut req.provenance,
                    ProvenanceInput {
                        source: SourceRef::UserInput { message_id: None },
                        observed_at: None,
                        extraction: None,
                        parent_evidence: vec![],
                    },
                )),
                reason: "migrated".into(),
                expected_revision: None,
            },
        )
        .await
        .unwrap();
    h.tick(10);
    let now = h
        .engine
        .recall(&h.alice, recall("which database is primary"))
        .await
        .unwrap();
    assert_eq!(now.results.len(), 1);
    assert_eq!(now.results[0].memory.revision_id, rev.revision_id);

    // Exact replay sees the substrate as it was: the MySQL revision.
    let replay = h
        .engine
        .replay_recall(&h.alice, first.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(replay.identical, "{replay:?}");
    assert!(!replay.counterfactual);
    assert_eq!(replay.replayed_selected[0].1, m.revision_id);

    // Trace is persisted with full candidate detail.
    let trace = h.engine.get_trace(&h.alice, first.trace_id).await.unwrap();
    assert_eq!(trace.selected().count(), 1);
    assert!(trace.data_snapshot.contains("scoring=v1"));

    // Counterfactual replay: raise min_score above anything achievable.
    let cf = h
        .engine
        .replay_recall(
            &h.alice,
            first.trace_id,
            ReplayRequest {
                policy_override: Some(RecallPolicy {
                    min_score: 1.0,
                    ..trace.policy.clone()
                }),
            },
        )
        .await
        .unwrap();
    assert!(cf.counterfactual);
    assert!(!cf.identical);
    assert_eq!(cf.removed, vec![m.id]);

    // Traces are tenant-isolated.
    let other = AccessContext::user(TenantId::new_v7(), UserId::new_v7());
    assert!(matches!(
        h.engine.get_trace(&other, first.trace_id).await,
        Err(Error::NotFound { .. })
    ));
}

pub async fn scope_isolation(h: &Harness) {
    let private = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "alice secret launch codename"),
        )
        .await
        .unwrap();

    // Bob (same tenant) cannot read, recall or modify it.
    assert!(matches!(
        h.engine.get_memory(&h.bob, private.id).await,
        Err(Error::NotFound { .. })
    ));
    assert!(
        h.engine
            .recall(&h.bob, recall("launch codename"))
            .await
            .unwrap()
            .results
            .is_empty()
    );
    assert!(
        h.engine
            .tombstone_memory(&h.bob, private.id, StatusChange { reason: "x".into() })
            .await
            .is_err()
    );
    // Bob cannot write into Alice's scope.
    assert!(matches!(
        h.engine
            .create_memory(&h.bob, h.fact(h.alice_scope(), "forged"))
            .await,
        Err(Error::Forbidden(_))
    ));

    // Project visibility requires membership.
    let project = ProjectId::new_v7();
    let alice_p = h.alice.clone().with_projects([project]);
    let shared_scope = h
        .alice_scope()
        .with_project(project)
        .with_visibility(Visibility::Project);
    let shared = h
        .engine
        .create_memory(&alice_p, h.fact(shared_scope, "project roadmap milestone"))
        .await
        .unwrap();
    assert!(h.engine.get_memory(&h.bob, shared.id).await.is_err());
    let bob_p = h.bob.clone().with_projects([project]);
    assert_eq!(
        h.engine.get_memory(&bob_p, shared.id).await.unwrap().id,
        shared.id
    );
    let hits = h
        .engine
        .recall(&bob_p, recall("roadmap milestone"))
        .await
        .unwrap();
    assert_eq!(hits.results.len(), 1);
    assert_eq!(hits.results[0].memory.id, shared.id);

    // Tenant visibility never crosses tenants.
    let tenant_wide = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(Scope::tenant(h.tenant), "tenant wide glossary"),
        )
        .await
        .unwrap();
    assert!(h.engine.get_memory(&h.bob, tenant_wide.id).await.is_ok());
    let stranger = AccessContext::user(TenantId::new_v7(), UserId::new_v7());
    assert!(
        h.engine
            .get_memory(&stranger, tenant_wide.id)
            .await
            .is_err()
    );
    assert!(
        h.engine
            .recall(&stranger, recall("glossary"))
            .await
            .unwrap()
            .results
            .is_empty()
    );
}

pub async fn model_derived_memory_requires_validation(h: &Harness) {
    let mut req = h.fact(h.alice_scope(), "extracted: the user prefers rust");
    req.provenance.extraction = Some(ExtractionInfo {
        model: Some("local-model".into()),
        model_version: Some("1".into()),
        prompt_version: Some("p1".into()),
        extractor_version: "x1".into(),
    });
    let m = h.engine.create_memory(&h.alice, req).await.unwrap();
    assert_eq!(m.status, MemoryStatus::Candidate);
    h.tick(1);
    assert!(
        h.engine
            .recall(&h.alice, recall("prefers rust"))
            .await
            .unwrap()
            .results
            .is_empty()
    );

    let mut with_candidates = recall("prefers rust");
    with_candidates.policy.include_candidates = true;
    assert_eq!(
        h.engine
            .recall(&h.alice, with_candidates)
            .await
            .unwrap()
            .results
            .len(),
        1
    );

    let promoted = h
        .engine
        .promote_memory(
            &h.alice,
            m.id,
            StatusChange {
                reason: "user confirmed".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(promoted.status, MemoryStatus::Active);
    h.tick(1);
    assert_eq!(
        h.engine
            .recall(&h.alice, recall("prefers rust"))
            .await
            .unwrap()
            .results
            .len(),
        1
    );
    assert!(
        h.engine
            .promote_memory(
                &h.alice,
                m.id,
                StatusChange {
                    reason: "again".into()
                }
            )
            .await
            .is_err()
    );
}

pub async fn tombstone_and_purge(h: &Harness) {
    let m = h
        .engine
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "temporary staging password rotation"),
        )
        .await
        .unwrap();
    h.tick(1);
    h.engine
        .tombstone_memory(
            &h.alice,
            m.id,
            StatusChange {
                reason: "obsolete".into(),
            },
        )
        .await
        .unwrap();
    h.tick(1);
    assert!(
        h.engine
            .recall(&h.alice, recall("staging password rotation"))
            .await
            .unwrap()
            .results
            .is_empty()
    );
    assert_eq!(
        h.engine
            .memory_history(&h.alice, m.id)
            .await
            .unwrap()
            .revisions
            .len(),
        2
    );

    let purged = h
        .engine
        .purge_memory(
            &h.alice,
            m.id,
            StatusChange {
                reason: "user deletion request".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(purged.event_type, CognitiveEventType::MemoryPurged);
    assert!(h.engine.get_memory(&h.alice, m.id).await.is_err());
    assert!(h.engine.memory_history(&h.alice, m.id).await.is_err());

    // The log survives the purge and holds no content.
    let events = h
        .engine
        .events(
            &h.alice,
            EventQuery {
                stream_id: Some(EventStreamId(m.id.0)),
                ..EventQuery::default()
            },
        )
        .await
        .unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.event_type).collect();
    assert_eq!(
        kinds,
        vec![
            CognitiveEventType::MemoryCreated,
            CognitiveEventType::MemoryTombstoned,
            CognitiveEventType::MemoryPurged
        ]
    );
    assert!(
        events
            .iter()
            .all(|e| !e.payload.to_string().contains("password"))
    );
}

pub async fn event_log_is_sequenced(h: &Harness) {
    let m = h
        .engine
        .create_memory(&h.alice, h.fact(h.alice_scope(), "v1"))
        .await
        .unwrap();
    for i in 0..3 {
        h.tick(1);
        h.engine
            .revise_memory(
                &h.alice,
                m.id,
                ReviseMemory {
                    content: None,
                    valid_time: None,
                    expires_at: None,
                    provenance: None,
                    reason: format!("rev {i}"),
                    expected_revision: None,
                },
            )
            .await
            .unwrap();
    }
    let q = EventQuery {
        stream_id: Some(EventStreamId(m.id.0)),
        ..EventQuery::default()
    };
    let events = h.engine.events(&h.alice, q.clone()).await.unwrap();
    assert_eq!(
        events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    let after = h
        .engine
        .events(
            &h.alice,
            EventQuery {
                after_sequence: Some(2),
                ..q
            },
        )
        .await
        .unwrap();
    assert_eq!(after.len(), 2);
    // Bob cannot see Alice's private stream.
    let bob_view = h
        .engine
        .events(
            &h.bob,
            EventQuery {
                stream_id: Some(EventStreamId(m.id.0)),
                ..EventQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(bob_view.is_empty());
}

pub async fn idempotent_create(h: &Harness) {
    let mut req = h.fact(h.alice_scope(), "idempotent");
    req.idempotency_key = Some("req-42".into());
    let a = h.engine.create_memory(&h.alice, req.clone()).await.unwrap();
    let b = h.engine.create_memory(&h.alice, req).await.unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(
        h.engine
            .memory_history(&h.alice, a.id)
            .await
            .unwrap()
            .revisions
            .len(),
        1
    );
}

pub async fn working_memory_is_ephemeral(h: &Harness) {
    let mut req = h.fact(h.alice_scope(), "unused");
    req.content = MemoryContent::Working(WorkingMemory {
        objective: Some("finish the quarterly migration plan".into()),
        active_context: vec![],
        unresolved_questions: vec![],
        priority: 5,
    });
    assert!(matches!(
        h.engine.create_memory(&h.alice, req.clone()).await,
        Err(Error::Validation(_))
    ));
    req.expires_at = Some(h.clock_now() + Duration::hours(1));
    h.engine.create_memory(&h.alice, req).await.unwrap();
    h.tick(1);
    let mut r = recall("quarterly migration plan");
    r.policy.allowed_domains = vec![MemoryDomain::Working];
    assert_eq!(
        h.engine
            .recall(&h.alice, r.clone())
            .await
            .unwrap()
            .results
            .len(),
        1
    );
    h.tick(120);
    assert!(
        h.engine
            .recall(&h.alice, r)
            .await
            .unwrap()
            .results
            .is_empty()
    );
}

impl Harness {
    pub fn clock_now(&self) -> OffsetDateTime {
        use cs_core::Clock;
        self.clock.now()
    }
}

pub async fn belief_revision_and_cognitive_debt(h: &Harness) {
    let support = h.evidence(EvidenceDirection::Supports).await;
    let api = h
        .engine
        .create_belief(
            &h.alice,
            h.belief("Library X supports batch writes", vec![support.clone()]),
        )
        .await
        .unwrap();
    assert_eq!(api.status, BeliefStatus::Active);
    let mut plan_req = h.belief("Ingestion plan uses batch writes", vec![support.clone()]);
    plan_req.depends_on = vec![DependencyInput {
        belief_id: api.id,
        relation: DependencyRelation::Requires,
        strength: None,
    }];
    let plan = h.engine.create_belief(&h.alice, plan_req).await.unwrap();

    let debt = h.engine.cognitive_debt(&h.alice).await.unwrap();
    assert_eq!(debt.debt_items, 0, "{debt:?}");
    assert_eq!(debt.active_assets, 2);

    // A version change removes the capability.
    h.tick(60);
    let outcome = h
        .engine
        .revise_belief(
            &h.alice,
            api.id,
            ReviseBelief {
                polarity: Some(BeliefPolarity::Refutes),
                reason: "removed in v3".into(),
                ..ReviseBelief::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.needs_reevaluation, vec![plan.id]);
    assert_eq!(outcome.belief.revision_number, 2);

    h.tick(60 * 24);
    let debt = h.engine.cognitive_debt(&h.alice).await.unwrap();
    let invalidated: Vec<_> = debt
        .items
        .iter()
        .filter(|i| i.category == DebtCategory::InvalidatedDependency)
        .collect();
    assert_eq!(invalidated.len(), 1, "{debt:?}");
    assert!(debt.debt_index_per_1000 > 0.0);

    // Re-evaluation: the plan explicitly re-affirms against the new revision.
    h.engine
        .add_dependency(
            &h.alice,
            NewDependency {
                from: api.id,
                to: plan.id,
                relation: DependencyRelation::Requires,
                strength: None,
            },
        )
        .await
        .unwrap();
    let debt = h.engine.cognitive_debt(&h.alice).await.unwrap();
    assert_eq!(debt.debt_items, 0, "{debt:?}");

    // Retraction preserves history.
    h.tick(1);
    h.engine
        .retract_belief(
            &h.alice,
            api.id,
            StatusChange {
                reason: "library dropped".into(),
            },
        )
        .await
        .unwrap();
    let history = h.engine.belief_history(&h.alice, api.id).await.unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[2].status, BeliefStatus::Retracted);
    let view = h.engine.get_belief(&h.alice, api.id).await.unwrap();
    assert_eq!(view.current_revision_id, Some(history[2].id));
    assert_eq!(view.evidence.len(), 1);
    assert!(view.evidence[0].evidence.is_some());

    // Depending on a retracted belief is debt.
    let debt = h.engine.cognitive_debt(&h.alice).await.unwrap();
    assert!(
        debt.items
            .iter()
            .any(|i| i.category == DebtCategory::InvalidatedDependency)
    );

    // Bob sees none of it.
    assert!(h.engine.get_belief(&h.bob, api.id).await.is_err());
    assert_eq!(
        h.engine.cognitive_debt(&h.bob).await.unwrap().active_assets,
        0
    );
}

pub async fn refuting_evidence_disputes(h: &Harness) {
    let s = h.evidence(EvidenceDirection::Supports).await;
    let r = h.evidence(EvidenceDirection::Refutes).await;
    let b = h
        .engine
        .create_belief(&h.alice, h.belief("Cache hit rate exceeds 90%", vec![s, r]))
        .await
        .unwrap();
    assert_eq!(b.status, BeliefStatus::Disputed);

    // Contradicting beliefs show up as debt.
    let mut opposite = h.belief(
        "cache hit rate exceeds 90%",
        vec![h.evidence(EvidenceDirection::Supports).await],
    );
    opposite.polarity = BeliefPolarity::Refutes;
    h.engine.create_belief(&h.alice, opposite).await.unwrap();
    let debt = h.engine.cognitive_debt(&h.alice).await.unwrap();
    assert!(
        debt.items
            .iter()
            .any(|i| i.category == DebtCategory::UnresolvedContradiction)
    );

    // Evidence from another tenant cannot be cited.
    let stranger = Harness::new_like(h);
    let foreign = stranger.evidence(EvidenceDirection::Supports).await;
    assert!(
        h.engine
            .create_belief(&h.alice, h.belief("x", vec![foreign]))
            .await
            .is_err()
    );
}

impl Harness {
    /// Another tenant sharing the same engine/store.
    pub fn new_like(other: &Harness) -> Self {
        let tenant = TenantId::new_v7();
        Self {
            engine: other.engine.clone(),
            clock: other.clock.clone(),
            tenant,
            alice: AccessContext::user(tenant, UserId::new_v7()),
            bob: AccessContext::user(tenant, UserId::new_v7()),
        }
    }
}

/// Run every scenario against stores produced by `make`.
pub async fn run_all(make: impl Fn() -> Arc<dyn CognitiveStore>) {
    macro_rules! scenario {
        ($($f:ident),+ $(,)?) => {$(
            println!("conformance: {}", stringify!($f));
            $f(&Harness::new(make())).await;
        )+};
    }
    scenario!(
        memory_lifecycle,
        bitemporal_recall_is_reproducible,
        scope_isolation,
        model_derived_memory_requires_validation,
        tombstone_and_purge,
        event_log_is_sequenced,
        idempotent_create,
        working_memory_is_ephemeral,
        belief_revision_and_cognitive_debt,
        refuting_evidence_disputes,
    );
    use m3::*;
    scenario!(
        perception_creates_candidates_idempotently,
        leases_are_fenced,
        retries_back_off_then_dead_letter,
        purge_during_extraction_discards_effects,
        duplicate_merge_unions_provenance,
        stale_proposals_are_never_applied,
        rejected_findings_are_not_reproposed,
        contradiction_quarantines_memories,
        belief_contradictions_are_disputed,
        beliefs_are_recalled_with_evidence,
        evidence_purge_flags_beliefs,
        observation_purge_keeps_corroborated_facts,
    );
}
