//! M3 conformance: governed cognitive processing.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cs_core::{
    AccessContext, CognitiveEventType, Error, EventQuery, EventStreamId, MemoryId, SourceRef,
    TimeInterval,
};
use cs_engine::{
    CognitiveEngine, Decision, ExtractError, Extractor, NewMemory, NewObservation,
    ProcessingConfig, RecallRequest, ReviseMemory, RuleExtractor, Worker, WorkerConfig,
};
use cs_memory::{
    BeliefPolarity, BeliefStatus, EvidenceDirection, ExtractedCandidate, JobKind, JobStatus,
    MemoryContent, MemoryStatus, Observation, ProposalStatus, RecallPolicy, ResolutionKind,
    ResolutionOperation, SemanticMemory, job::RetryPolicy,
};
use cs_policy::DebtCategory;
use cs_storage::JobClaim;
use time::Duration;

use crate::Harness;

fn worker(h: &Harness, extractor: Arc<dyn Extractor>) -> Worker {
    Worker::new(h.engine.clone(), extractor, WorkerConfig::default())
}

fn rules(h: &Harness) -> Worker {
    worker(h, Arc::new(RuleExtractor))
}

fn observe(h: &Harness, text: &str) -> NewObservation {
    NewObservation {
        scope: h.alice_scope(),
        content: text.into(),
        media_type: Some("text/plain".into()),
        source: SourceRef::UserInput { message_id: None },
        observed_at: None,
        idempotency_key: None,
    }
}

fn triple(h: &Harness, s: &str, p: &str, o: &str, valid_from_days: i64) -> NewMemory {
    let mut req = h.fact(h.alice_scope(), &format!("{s} {p} {o}"));
    req.content = MemoryContent::Semantic(SemanticMemory {
        proposition: format!("{s} {p} {o}"),
        subject: Some(s.into()),
        predicate: Some(p.into()),
        object: Some(o.into()),
    });
    // A fixed origin, so facts created minutes apart can share a start.
    let base = time::macros::datetime!(2026-06-01 9:00 UTC);
    req.valid_time = Some(TimeInterval::from(base + Duration::days(valid_from_days)));
    req
}

fn reason(r: &str) -> Decision {
    Decision { reason: r.into() }
}

pub async fn perception_creates_candidates_idempotently(h: &Harness) {
    let text = "The primary database is PostgreSQL. The API depends on the auth service. \
                Maybe the cache is Redis. It is fine.";
    let view = h
        .engine
        .submit_observation(&h.alice, observe(h, text))
        .await
        .unwrap();
    let id = view.observation.id;
    assert_eq!(view.job.as_ref().unwrap().kind, JobKind::Perceive);

    let report = rules(h).drain().await.unwrap();
    // One perception, then one entity projection per extracted candidate.
    assert_eq!(report.succeeded, 3, "{report:?}");
    let view = h.engine.get_observation(&h.alice, id).await.unwrap();
    assert_eq!(view.job.unwrap().status, JobStatus::Succeeded);
    assert_eq!(view.extraction_runs.len(), 1);
    let ids = view.extraction_runs[0].candidate_ids.clone();
    assert_eq!(ids.len(), 2, "hedged and pronoun sentences must be skipped");
    for mid in &ids {
        let m = h.engine.get_memory(&h.alice, *mid).await.unwrap();
        assert_eq!(m.status, MemoryStatus::Candidate);
        assert_eq!(m.provenance.observation(), Some(id));
        assert!(m.provenance.extraction.is_some());
    }

    // Re-running perception for the same observation and extractor version
    // is a no-op.
    let again = h.engine.new_perceive_job(&h.alice, id, "rerun");
    h.engine.store().enqueue_job(&again).await.unwrap();
    rules(h).drain().await.unwrap();
    let job = h.engine.get_job(&h.alice, again.id).await.unwrap();
    assert_eq!(job.result.unwrap()["skipped"], "already_extracted");
    assert_eq!(
        h.engine
            .get_observation(&h.alice, id)
            .await
            .unwrap()
            .extraction_runs
            .len(),
        1
    );

    // Idempotent submission.
    let mut req = observe(h, "The build system is Bazel.");
    req.idempotency_key = Some("obs-1".into());
    let a = h
        .engine
        .submit_observation(&h.alice, req.clone())
        .await
        .unwrap();
    let b = h.engine.submit_observation(&h.alice, req).await.unwrap();
    assert_eq!(a.observation.id, b.observation.id);

    // Observations are scoped like everything else.
    assert!(h.engine.get_observation(&h.bob, id).await.is_err());
}

pub async fn leases_are_fenced(h: &Harness) {
    let view = h
        .engine
        .submit_observation(&h.alice, observe(h, "The queue is Postgres."))
        .await
        .unwrap();
    let job_id = view.job.unwrap().id;
    let store = h.engine.store();
    let now = h.clock_now();
    let claim = |owner: &str, now| JobClaim {
        kinds: vec![JobKind::Perceive],
        owner: owner.into(),
        now,
        lease_until: now + Duration::seconds(30),
        limit: 10,
    };
    let first = store.claim_jobs(&claim("w1", now)).await.unwrap();
    let first = first.into_iter().find(|c| c.job.id == job_id).unwrap();
    // Not claimable again while the lease is live.
    assert!(
        store
            .claim_jobs(&claim("w2", now))
            .await
            .unwrap()
            .iter()
            .all(|c| c.job.id != job_id)
    );

    // The lease expires; another worker reclaims with a new generation.
    h.tick(1);
    let later = h.clock_now();
    let second = store.claim_jobs(&claim("w2", later)).await.unwrap();
    let second = second.into_iter().find(|c| c.job.id == job_id).unwrap();
    assert_eq!(second.lease.generation, first.lease.generation + 1);
    assert_eq!(second.job.attempts, 2);

    // The stale worker cannot heartbeat, finish or release.
    let ok = cs_memory::JobOutcome::Succeeded {
        result: serde_json_value(),
    };
    assert!(matches!(
        store.heartbeat_job(first.lease, later).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        store
            .finish_job(first.lease, later, ok.clone(), Default::default())
            .await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        store.release_job(first.lease, later).await,
        Err(Error::Conflict(_))
    ));
    // The current holder can.
    store
        .heartbeat_job(second.lease, later + Duration::seconds(60))
        .await
        .unwrap();
    let done = store
        .finish_job(second.lease, later, ok, Default::default())
        .await
        .unwrap();
    assert_eq!(done.status, JobStatus::Succeeded);
}

fn serde_json_value() -> serde_json::Value {
    serde_json::json!({ "ok": true })
}

#[derive(Debug)]
struct Flaky;

#[async_trait]
impl Extractor for Flaky {
    fn name(&self) -> &str {
        "flaky"
    }
    fn version(&self) -> &str {
        "1"
    }
    async fn extract(&self, _: &Observation) -> Result<Vec<ExtractedCandidate>, ExtractError> {
        Err(ExtractError::Retryable("model endpoint unavailable".into()))
    }
}

pub async fn retries_back_off_then_dead_letter(h: &Harness) {
    let engine = CognitiveEngine::builder(h.engine.store().clone())
        .clock(h.clock.clone())
        .processing(ProcessingConfig {
            retry: RetryPolicy {
                max_attempts: 2,
                jitter_fraction: 0.0,
                ..RetryPolicy::default()
            },
            ..ProcessingConfig::default()
        })
        .build();
    let view = engine
        .submit_observation(&h.alice, observe(h, "The API uses gRPC."))
        .await
        .unwrap();
    let job_id = view.job.unwrap().id;
    let w = Worker::new(engine.clone(), Arc::new(Flaky), WorkerConfig::default());

    let r = w.run_once().await.unwrap();
    assert_eq!(r.retried, 1);
    let job = engine.get_job(&h.alice, job_id).await.unwrap();
    assert_eq!(job.status, JobStatus::Queued);
    assert!(
        job.available_at > h.clock_now(),
        "backoff must delay the retry"
    );
    assert!(!job.last_error.as_ref().unwrap().permanent);
    // Not due yet.
    assert_eq!(w.run_once().await.unwrap().claimed, 0);

    h.clock.advance(Duration::seconds(5));
    let r = w.run_once().await.unwrap();
    assert_eq!(r.dead_lettered, 1);
    let job = engine.get_job(&h.alice, job_id).await.unwrap();
    assert_eq!(job.status, JobStatus::DeadLettered);
    assert_eq!(job.attempts, 2);
    let events = engine
        .events(
            &h.alice,
            EventQuery {
                stream_id: Some(EventStreamId(job_id.0)),
                ..EventQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events[0].event_type, CognitiveEventType::JobDeadLettered);

    // Dead-lettered work can be resubmitted under the same dedupe key.
    let again = engine.new_perceive_job(
        &h.alice,
        view.observation.id,
        &format!("observation:{}", view.observation.id),
    );
    let enq = engine.store().enqueue_job(&again).await.unwrap();
    assert_eq!(
        enq.id, again.id,
        "terminal jobs must not block resubmission"
    );
}

/// Purges the observation it is extracting from, mid-flight.
#[derive(Debug)]
struct Purging {
    engine: Mutex<Option<CognitiveEngine>>,
    ctx: AccessContext,
}

#[async_trait]
impl Extractor for Purging {
    fn name(&self) -> &str {
        "purging"
    }
    fn version(&self) -> &str {
        "1"
    }
    async fn extract(&self, o: &Observation) -> Result<Vec<ExtractedCandidate>, ExtractError> {
        let out = cs_engine::extract::extract_rules(o.content.as_deref().unwrap_or_default());
        let engine = self.engine.lock().unwrap().clone().unwrap();
        engine
            .purge_observation(&self.ctx, o.id, reason("user request"))
            .await
            .unwrap();
        Ok(out)
    }
}

pub async fn purge_during_extraction_discards_effects(h: &Harness) {
    let view = h
        .engine
        .submit_observation(&h.alice, observe(h, "The primary database is PostgreSQL."))
        .await
        .unwrap();
    let ex = Purging {
        engine: Mutex::new(Some(h.engine.clone())),
        ctx: h.alice.clone(),
    };
    worker(h, Arc::new(ex)).drain().await.unwrap();
    let job = h
        .engine
        .get_job(&h.alice, view.job.unwrap().id)
        .await
        .unwrap();
    assert_eq!(job.status, JobStatus::Succeeded);
    assert_eq!(job.result.unwrap()["discarded"], "observation_purged");
    let obs = h
        .engine
        .get_observation(&h.alice, view.observation.id)
        .await
        .unwrap();
    assert!(obs.observation.content.is_none());
    assert!(
        obs.extraction_runs.is_empty(),
        "no effects may land after the purge"
    );
    let hits = h
        .engine
        .recall(
            &h.alice,
            RecallRequest {
                query: "primary database".into(),
                policy: RecallPolicy {
                    include_candidates: true,
                    ..RecallPolicy::default()
                },
            },
        )
        .await
        .unwrap();
    assert!(hits.results.is_empty());
}

fn auto(h: &Harness) -> CognitiveEngine {
    CognitiveEngine::builder(h.engine.store().clone())
        .clock(h.clock.clone())
        .processing(ProcessingConfig {
            auto_resolve: true,
            ..ProcessingConfig::default()
        })
        .build()
}

pub async fn duplicate_merge_unions_provenance(h: &Harness) {
    let engine = auto(h);
    let w = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let a = engine
        .create_memory(&h.alice, triple(h, "primary db", "is", "postgres", 0))
        .await
        .unwrap();
    h.tick(1);
    let b = engine
        .create_memory(&h.alice, triple(h, "Primary DB", "is", "Postgres", 0))
        .await
        .unwrap();
    w.drain().await.unwrap();

    let pending = engine
        .list_proposals(&h.alice, Some(ProposalStatus::Pending), None)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    let p = &pending[0];
    assert_eq!(p.kind, ResolutionKind::Duplicate);
    assert_eq!(
        p.operation,
        ResolutionOperation::Merge {
            canonical: a.id,
            duplicate: b.id
        }
    );
    // Proposals never mutate on creation.
    assert_eq!(
        engine.get_memory(&h.alice, b.id).await.unwrap().revision_id,
        b.revision_id
    );
    // Others cannot see it.
    assert!(engine.get_proposal(&h.bob, p.id).await.is_err());

    h.tick(1);
    let accepted = engine
        .accept_proposal(&h.alice, p.id, reason("same fact"))
        .await
        .unwrap();
    assert_eq!(accepted.status, ProposalStatus::Accepted);
    assert_eq!(
        engine.get_memory(&h.alice, b.id).await.unwrap().status,
        MemoryStatus::Tombstoned
    );
    let canonical = engine.get_memory(&h.alice, a.id).await.unwrap();
    assert_eq!(canonical.revision_number, 2);
    assert_eq!(
        canonical.provenance.observed_at, b.provenance.observed_at,
        "recency refreshed"
    );
    // Accepting again is idempotent; rejecting now conflicts.
    assert_eq!(
        engine
            .accept_proposal(&h.alice, p.id, reason("again"))
            .await
            .unwrap()
            .status,
        ProposalStatus::Accepted
    );
    assert!(
        engine
            .reject_proposal(&h.alice, p.id, reason("too late"))
            .await
            .is_err()
    );
}

pub async fn stale_proposals_are_never_applied(h: &Harness) {
    let engine = auto(h);
    let w = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let old = engine
        .create_memory(&h.alice, triple(h, "primary db", "is", "mysql", 0))
        .await
        .unwrap();
    h.tick(1);
    let new = engine
        .create_memory(&h.alice, triple(h, "primary db", "is", "postgres", 30))
        .await
        .unwrap();
    w.drain().await.unwrap();
    let p = engine
        .list_proposals(&h.alice, Some(ProposalStatus::Pending), None)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.kind == ResolutionKind::Supersession)
        .unwrap();
    assert_eq!(
        p.operation,
        ResolutionOperation::Supersede {
            older: old.id,
            newer: new.id
        }
    );

    // A target changes after detection.
    h.tick(1);
    engine
        .revise_memory(
            &h.alice,
            old.id,
            ReviseMemory {
                content: None,
                valid_time: None,
                expires_at: None,
                provenance: None,
                reason: "annotated".into(),
                expected_revision: None,
            },
        )
        .await
        .unwrap();
    let err = engine
        .accept_proposal(&h.alice, p.id, reason("apply"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
    assert_eq!(
        engine.get_proposal(&h.alice, p.id).await.unwrap().status,
        ProposalStatus::Stale
    );

    // The revision re-triggered resolution, producing a fresh proposal.
    w.drain().await.unwrap();
    let fresh = engine
        .list_proposals(&h.alice, Some(ProposalStatus::Pending), None)
        .await
        .unwrap()
        .into_iter()
        .find(|q| q.kind == ResolutionKind::Supersession)
        .unwrap();
    assert_ne!(fresh.fingerprint, p.fingerprint);
    h.tick(1);
    engine
        .accept_proposal(&h.alice, fresh.id, reason("migrated"))
        .await
        .unwrap();
    let closed = engine.get_memory(&h.alice, old.id).await.unwrap();
    assert_eq!(
        closed.temporal.valid_time.end,
        Some(new.temporal.valid_time.start)
    );
    let history = engine
        .memory_history(&h.alice, old.id)
        .await
        .unwrap()
        .revisions;
    assert!(
        history[0].temporal.valid_time.end.is_none(),
        "history is preserved"
    );
}

pub async fn rejected_findings_are_not_reproposed(h: &Harness) {
    let engine = auto(h);
    let w = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let a = engine
        .create_memory(&h.alice, triple(h, "region", "is", "eu-west-1", 0))
        .await
        .unwrap();
    h.tick(1);
    let b = engine
        .create_memory(&h.alice, triple(h, "region", "is", "us-east-1", 0))
        .await
        .unwrap();
    w.drain().await.unwrap();
    let p = engine.list_proposals(&h.alice, None, None).await.unwrap();
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].kind, ResolutionKind::Contradiction);
    engine
        .reject_proposal(&h.alice, p[0].id, reason("both regions are used"))
        .await
        .unwrap();
    // Targets untouched.
    assert_eq!(
        engine.get_memory(&h.alice, a.id).await.unwrap().revision_id,
        a.revision_id
    );
    assert_eq!(
        engine.get_memory(&h.alice, b.id).await.unwrap().status,
        MemoryStatus::Active
    );
    // Detection runs again over the same revisions: nothing new.
    engine
        .request_memory_resolution(&h.alice, b.id)
        .await
        .unwrap();
    engine
        .request_memory_resolution(&h.alice, a.id)
        .await
        .unwrap();
    w.drain().await.unwrap();
    assert_eq!(
        engine
            .list_proposals(&h.alice, None, None)
            .await
            .unwrap()
            .len(),
        1
    );
}

pub async fn contradiction_quarantines_memories(h: &Harness) {
    let engine = auto(h);
    let w = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let a = engine
        .create_memory(&h.alice, triple(h, "port", "is", "8080", 0))
        .await
        .unwrap();
    h.tick(1);
    let b = engine
        .create_memory(&h.alice, triple(h, "port", "is", "9090", 0))
        .await
        .unwrap();
    w.drain().await.unwrap();
    let p = engine
        .list_proposals(&h.alice, Some(ProposalStatus::Pending), None)
        .await
        .unwrap();
    h.tick(1);
    engine
        .accept_proposal(&h.alice, p[0].id, reason("unclear which is right"))
        .await
        .unwrap();
    for id in [a.id, b.id] {
        assert_eq!(
            engine.get_memory(&h.alice, id).await.unwrap().status,
            MemoryStatus::Candidate
        );
    }
    // Quarantined facts drop out of default recall but are preserved.
    h.tick(1);
    let hits = engine
        .recall(
            &h.alice,
            RecallRequest {
                query: "port".into(),
                policy: RecallPolicy::default(),
            },
        )
        .await
        .unwrap();
    assert!(hits.results.is_empty());
}

pub async fn belief_contradictions_are_disputed(h: &Harness) {
    let engine = auto(h);
    let w = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let e1 = h.evidence(EvidenceDirection::Supports).await;
    let e2 = h.evidence(EvidenceDirection::Supports).await;
    let yes = engine
        .create_belief(
            &h.alice,
            h.belief("The cache hit rate exceeds 90%", vec![e1]),
        )
        .await
        .unwrap();
    let mut no = h.belief("the cache hit rate exceeds 90%", vec![e2]);
    no.polarity = BeliefPolarity::Refutes;
    h.tick(1);
    let no = engine.create_belief(&h.alice, no).await.unwrap();
    w.drain().await.unwrap();
    let p = engine
        .list_proposals(&h.alice, Some(ProposalStatus::Pending), None)
        .await
        .unwrap();
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(matches!(
        p[0].operation,
        ResolutionOperation::Dispute { .. }
    ));
    h.tick(1);
    engine
        .accept_proposal(&h.alice, p[0].id, reason("needs measurement"))
        .await
        .unwrap();
    for id in [yes.id, no.id] {
        let view = engine.get_belief(&h.alice, id).await.unwrap();
        let current = engine
            .get_belief(&h.alice, view.current_revision_id.unwrap())
            .await
            .unwrap();
        assert_eq!(current.belief.status, BeliefStatus::Disputed);
        assert_eq!(current.evidence.len(), 1, "evidence carried forward");
    }
}

pub async fn beliefs_are_recalled_with_evidence(h: &Harness) {
    let support = h.evidence(EvidenceDirection::Supports).await;
    let b = h
        .engine
        .create_belief(
            &h.alice,
            h.belief(
                "Postgres handles our write throughput",
                vec![support.clone()],
            ),
        )
        .await
        .unwrap();
    h.tick(1);
    let res = h
        .engine
        .recall(
            &h.alice,
            RecallRequest {
                query: "postgres write throughput".into(),
                policy: RecallPolicy::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(res.beliefs.len(), 1);
    let rb = &res.beliefs[0];
    assert_eq!(rb.belief.id, b.id);
    assert_eq!(rb.supporting_evidence, vec![support.evidence_id]);
    assert!(rb.relevance > 0.0);
    assert!(
        (rb.belief.confidence.value - 0.8).abs() < 1e-9,
        "confidence untouched by relevance"
    );
    let trace = h.engine.get_trace(&h.alice, res.trace_id).await.unwrap();
    assert_eq!(
        trace
            .belief_candidates
            .iter()
            .filter(|c| c.selected)
            .count(),
        1
    );
    let replay = h
        .engine
        .replay_recall(&h.alice, res.trace_id, Default::default())
        .await
        .unwrap();
    assert!(replay.identical);
    // Scope isolation matches memory recall.
    let bob = h
        .engine
        .recall(
            &h.bob,
            RecallRequest {
                query: "postgres write throughput".into(),
                policy: RecallPolicy::default(),
            },
        )
        .await
        .unwrap();
    assert!(bob.beliefs.is_empty());
    // Beliefs can be excluded.
    let off = h
        .engine
        .recall(
            &h.alice,
            RecallRequest {
                query: "postgres".into(),
                policy: RecallPolicy {
                    include_beliefs: false,
                    ..RecallPolicy::default()
                },
            },
        )
        .await
        .unwrap();
    assert!(off.beliefs.is_empty());
}

pub async fn evidence_purge_flags_beliefs(h: &Harness) {
    let support = h.evidence(EvidenceDirection::Supports).await;
    let b = h
        .engine
        .create_belief(
            &h.alice,
            h.belief("Batch size 500 is optimal", vec![support.clone()]),
        )
        .await
        .unwrap();
    assert_eq!(
        h.engine.cognitive_debt(&h.alice).await.unwrap().debt_items,
        0
    );

    let report = h
        .engine
        .purge_evidence(&h.alice, support.evidence_id, reason("source withdrawn"))
        .await
        .unwrap();
    assert!(!report.already_purged);
    assert_eq!(report.beliefs_needing_reevaluation, vec![b.id]);
    assert!(
        h.engine
            .get_belief(&h.alice, b.id)
            .await
            .unwrap()
            .evidence
            .is_empty()
    );
    let debt = h.engine.cognitive_debt(&h.alice).await.unwrap();
    assert!(
        debt.items
            .iter()
            .any(|i| i.category == DebtCategory::UnsupportedBelief)
    );
    // Idempotent.
    let again = h
        .engine
        .purge_evidence(&h.alice, support.evidence_id, reason("again"))
        .await
        .unwrap();
    assert!(again.already_purged);
    assert_eq!(again.purged_at, report.purged_at);
}

pub async fn observation_purge_keeps_corroborated_facts(h: &Harness) {
    let engine = auto(h);
    let w = Worker::new(
        engine.clone(),
        Arc::new(RuleExtractor),
        WorkerConfig::default(),
    );
    let o1 = engine
        .submit_observation(&h.alice, observe(h, "The primary database is PostgreSQL."))
        .await
        .unwrap()
        .observation
        .id;
    w.drain().await.unwrap();
    h.tick(1);
    let o2 = engine
        .submit_observation(&h.alice, observe(h, "The primary database is PostgreSQL."))
        .await
        .unwrap()
        .observation
        .id;
    w.drain().await.unwrap();
    let c1: MemoryId = engine
        .get_observation(&h.alice, o1)
        .await
        .unwrap()
        .extraction_runs[0]
        .candidate_ids[0];
    let c2: MemoryId = engine
        .get_observation(&h.alice, o2)
        .await
        .unwrap()
        .extraction_runs[0]
        .candidate_ids[0];

    let p = engine
        .list_proposals(&h.alice, Some(ProposalStatus::Pending), None)
        .await
        .unwrap();
    assert_eq!(p.len(), 1, "{p:?}");
    h.tick(1);
    engine
        .accept_proposal(&h.alice, p[0].id, reason("same statement"))
        .await
        .unwrap();
    assert!(
        engine
            .get_memory(&h.alice, c1)
            .await
            .unwrap()
            .provenance
            .corroborations
            .contains(&SourceRef::Observation { observation_id: o2 })
    );

    // Purging the first source keeps the corroborated fact, re-homed.
    h.tick(1);
    let r1 = engine
        .purge_observation(&h.alice, o1, reason("user request"))
        .await
        .unwrap();
    assert!(!r1.purged_memories.contains(&c1), "{r1:?}");
    let kept = engine.get_memory(&h.alice, c1).await.unwrap();
    assert_eq!(
        kept.provenance.source,
        SourceRef::Observation { observation_id: o2 }
    );

    // Purging the last source erases it, wherever it was first extracted.
    h.tick(1);
    let r2 = engine
        .purge_observation(&h.alice, o2, reason("user request"))
        .await
        .unwrap();
    assert!(
        r2.purged_memories.contains(&c1) && r2.purged_memories.contains(&c2),
        "{r2:?}"
    );
    assert!(engine.get_memory(&h.alice, c1).await.is_err());
    assert!(engine.memory_history(&h.alice, c1).await.is_err());
    let again = engine
        .purge_observation(&h.alice, o2, reason("again"))
        .await
        .unwrap();
    assert!(again.already_purged);
    // The log recorded the purge without content.
    let events = engine
        .events(
            &h.alice,
            EventQuery {
                stream_id: Some(EventStreamId(o2.0)),
                ..EventQuery::default()
            },
        )
        .await
        .unwrap();
    let purge = events
        .iter()
        .find(|e| e.event_type == CognitiveEventType::ObservationPurged)
        .unwrap();
    assert!(!purge.payload.to_string().contains("PostgreSQL"));
}
