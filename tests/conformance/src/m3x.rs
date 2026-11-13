//! M3.x conformance: embedding spaces, backfill, fallback and migration.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use cs_engine::{
    CognitiveEngine, EmbedError, Embedder, EmbeddingSpace, HashingEmbedder, ProcessingConfig,
    RecallRequest, ReindexRequest, ReplayRequest, RuleExtractor, SpaceRole, StatusChange, Worker,
    WorkerConfig,
};
use cs_memory::{DenseStatus, JobKind, JobStatus, RecallPolicy};

use crate::Harness;

/// Wraps the hashing embedder; can be told to fail documents and/or
/// queries (queries are recognised by the space's query prefix).
#[derive(Debug)]
pub struct Switchable {
    inner: HashingEmbedder,
    space: EmbeddingSpace,
    pub fail_documents: AtomicBool,
    pub fail_queries: AtomicBool,
}

impl Switchable {
    pub fn new(dims: usize) -> Arc<Self> {
        let inner = HashingEmbedder::new(dims);
        let mut space = inner.space().clone();
        space.provider = "test".into();
        space.model = "switchable".into();
        space.query_prefix = "q: ".into();
        Arc::new(Self {
            inner,
            space,
            fail_documents: AtomicBool::new(false),
            fail_queries: AtomicBool::new(false),
        })
    }

    pub fn outage(&self, docs: bool, queries: bool) {
        self.fail_documents.store(docs, Ordering::SeqCst);
        self.fail_queries.store(queries, Ordering::SeqCst);
    }
}

#[async_trait]
impl Embedder for Switchable {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        for t in texts {
            let query = t.starts_with("q: ");
            if (query && self.fail_queries.load(Ordering::SeqCst))
                || (!query && self.fail_documents.load(Ordering::SeqCst))
            {
                return Err(EmbedError::Retryable(
                    "embedding endpoint unavailable".into(),
                ));
            }
        }
        self.inner.embed_batch(texts).await
    }
}

fn engine(
    h: &Harness,
    active: Arc<dyn Embedder>,
    shadow: Option<Arc<dyn Embedder>>,
) -> CognitiveEngine {
    let mut b = CognitiveEngine::builder(h.engine.store().clone())
        .clock(h.clock.clone())
        .embedder(active)
        .processing(ProcessingConfig {
            auto_resolve: false,
            ..ProcessingConfig::default()
        });
    if let Some(s) = shadow {
        b = b.shadow_embedder(s);
    }
    b.build()
}

fn worker(e: &CognitiveEngine) -> Worker {
    Worker::new(e.clone(), Arc::new(RuleExtractor), WorkerConfig::default())
}

fn recall(q: &str) -> RecallRequest {
    RecallRequest {
        query: q.into(),
        policy: RecallPolicy::default(),
    }
}

async fn embed_jobs(e: &CognitiveEngine, h: &Harness, status: JobStatus) -> usize {
    e.list_jobs(&h.alice, Some(status), None)
        .await
        .unwrap()
        .iter()
        .filter(|j| j.kind == JobKind::Embed)
        .count()
}

fn coverage(s: &cs_engine::ProcessingStatus, role: SpaceRole) -> f64 {
    s.spaces
        .iter()
        .find(|c| c.role == role)
        .map(|c| c.ratio)
        .unwrap()
}

pub async fn writes_survive_embedder_outage(h: &Harness) {
    let emb = Switchable::new(64);
    let e = engine(h, emb.clone(), None);
    emb.outage(true, true);

    let m = e
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "The ledger service uses PostgreSQL"),
        )
        .await
        .unwrap();
    assert_eq!(
        embed_jobs(&e, h, JobStatus::Queued).await,
        1,
        "backfill committed with the write"
    );
    let status = e.processing_status(&h.alice).await.unwrap();
    assert_eq!(coverage(&status, SpaceRole::Active), 0.0);

    // Recall degrades explicitly to lexical + recency.
    h.tick(1);
    let r = e
        .recall(&h.alice, recall("ledger postgresql"))
        .await
        .unwrap();
    assert!(
        matches!(r.trace.dense, DenseStatus::Unavailable { .. }),
        "{:?}",
        r.trace.dense
    );
    assert_eq!(r.results[0].memory.id, m.id);

    // The backfill retries while the provider is down, then succeeds.
    let w = worker(&e);
    assert_eq!(w.run_once().await.unwrap().retried, 1);
    emb.outage(false, false);
    h.clock.advance(time::Duration::seconds(10));
    w.drain().await.unwrap();
    assert_eq!(embed_jobs(&e, h, JobStatus::Succeeded).await, 1);
    let status = e.processing_status(&h.alice).await.unwrap();
    assert_eq!(coverage(&status, SpaceRole::Active), 1.0);
    h.tick(1);
    let r = e
        .recall(&h.alice, recall("ledger postgresql"))
        .await
        .unwrap();
    assert!(r.trace.dense.is_used());
    assert_eq!(r.trace.dense_coverage, Some(1.0));
}

pub async fn backfill_never_changes_a_replay(h: &Harness) {
    let emb = Switchable::new(64);
    let e = engine(h, emb.clone(), None);
    e.create_memory(
        &h.alice,
        h.fact(h.alice_scope(), "Billing runs on Kubernetes"),
    )
    .await
    .unwrap();
    emb.outage(true, false); // documents fail, queries work
    e.create_memory(
        &h.alice,
        h.fact(h.alice_scope(), "Billing exports nightly to S3"),
    )
    .await
    .unwrap();
    h.tick(1);

    let first = e.recall(&h.alice, recall("billing")).await.unwrap();
    assert!(first.trace.dense.is_used());
    assert_eq!(
        first.trace.dense_coverage,
        Some(0.5),
        "one candidate awaits its vector"
    );
    let trace = e.get_trace(&h.alice, first.trace_id).await.unwrap();
    assert!(
        trace.candidates.iter().any(|c| c.dense_score.is_none()),
        "imputed, not zeroed"
    );
    assert!(trace.query_embedding.is_some());

    emb.outage(false, false);
    h.tick(1);
    worker(&e).drain().await.unwrap();

    // The vector now exists, but it did not at the recorded as-of time.
    let replay = e
        .replay_recall(&h.alice, first.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(replay.identical, "{replay:?}");
    assert!(
        replay
            .replay
            .candidates
            .iter()
            .any(|c| c.dense_score.is_none())
    );
    // A fresh recall sees full coverage.
    h.tick(1);
    let now = e.recall(&h.alice, recall("billing")).await.unwrap();
    assert_eq!(now.trace.dense_coverage, Some(1.0));
}

pub async fn fallback_recalls_replay_as_fallback(h: &Harness) {
    let emb = Switchable::new(64);
    let e = engine(h, emb.clone(), None);
    e.create_memory(
        &h.alice,
        h.fact(h.alice_scope(), "Search uses BM25 and vectors"),
    )
    .await
    .unwrap();
    h.tick(1);
    emb.outage(false, true);
    let r = e.recall(&h.alice, recall("search vectors")).await.unwrap();
    assert!(matches!(r.trace.dense, DenseStatus::Unavailable { .. }));
    emb.outage(false, false);
    let replay = e
        .replay_recall(&h.alice, r.trace_id, ReplayRequest::default())
        .await
        .unwrap();
    assert!(replay.identical);
    assert!(
        matches!(replay.replay.dense, DenseStatus::Unavailable { .. }),
        "replayed the same way"
    );

    // Without retained query text, the query vector is not kept either.
    let mut req = recall("search vectors");
    req.policy.retain_query_text = false;
    let r = e.recall(&h.alice, req).await.unwrap();
    let t = e.get_trace(&h.alice, r.trace_id).await.unwrap();
    assert!(t.query_text.is_none() && t.query_embedding.is_none());
}

pub async fn shadow_space_migration(h: &Harness) {
    let old: Arc<dyn Embedder> = Arc::new(HashingEmbedder::new(64));
    let new: Arc<dyn Embedder> = Arc::new(HashingEmbedder::new(96));
    let before = engine(h, old.clone(), None);
    let a = before
        .create_memory(&h.alice, h.fact(h.alice_scope(), "Payments use Stripe"))
        .await
        .unwrap();

    // Start the migration: dual-write new revisions into the shadow space.
    let during = engine(h, old.clone(), Some(new.clone()));
    let b = during
        .create_memory(&h.alice, h.fact(h.alice_scope(), "Payouts use Adyen"))
        .await
        .unwrap();
    let status = during.processing_status(&h.alice).await.unwrap();
    assert_eq!(coverage(&status, SpaceRole::Active), 1.0);
    assert_eq!(coverage(&status, SpaceRole::Shadow), 0.0);

    // Reindex backfills what predates the migration; rerunning is idempotent.
    let report = during
        .request_reembedding(&h.alice, ReindexRequest::default())
        .await
        .unwrap();
    assert_eq!(report.space, new.space().id());
    assert_eq!(report.enqueued, 2, "{report:?}");
    let again = during
        .request_reembedding(&h.alice, ReindexRequest::default())
        .await
        .unwrap();
    assert_eq!(
        again.enqueued, 2,
        "active-only dedupe returns the queued jobs"
    );
    worker(&during).drain().await.unwrap();
    let status = during.processing_status(&h.alice).await.unwrap();
    assert_eq!(coverage(&status, SpaceRole::Shadow), 1.0);
    let done = during
        .request_reembedding(&h.alice, ReindexRequest::default())
        .await
        .unwrap();
    assert_eq!((done.enqueued, done.already_embedded), (0, 2));

    // Cut over: the new space is fully covered and serves recall; the old
    // vectors are kept, so old traces still replay.
    let after = engine(h, new.clone(), None);
    let status = after.processing_status(&h.alice).await.unwrap();
    assert_eq!(coverage(&status, SpaceRole::Active), 1.0);
    h.tick(1);
    let r = after
        .recall(&h.alice, recall("payments stripe"))
        .await
        .unwrap();
    assert_eq!(
        r.trace.dense,
        DenseStatus::Used {
            space: new.space().id()
        }
    );
    assert_eq!(r.results[0].memory.id, a.id);
    let _ = b;
}

pub async fn purged_revisions_are_never_re_embedded(h: &Harness) {
    let emb = Switchable::new(64);
    let e = engine(h, emb.clone(), None);
    emb.outage(true, false);
    let m = e
        .create_memory(
            &h.alice,
            h.fact(h.alice_scope(), "Secret project codename Falcon"),
        )
        .await
        .unwrap();
    e.purge_memory(
        &h.alice,
        m.id,
        StatusChange {
            reason: "user request".into(),
        },
    )
    .await
    .unwrap();
    emb.outage(false, false);
    worker(&e).drain().await.unwrap();
    let jobs = e
        .list_jobs(&h.alice, Some(JobStatus::Succeeded), None)
        .await
        .unwrap();
    let job = jobs.iter().find(|j| j.kind == JobKind::Embed).unwrap();
    assert_eq!(job.result.as_ref().unwrap()["skipped"], "revision_purged");
    assert!(e.get_memory(&h.alice, m.id).await.is_err());
}
