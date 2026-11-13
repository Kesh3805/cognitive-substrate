//! Duplicate-detection calibration for the built-in hashing embedder.
//!
//! A false-positive merge loses information; a false negative only leaves
//! two records. So the invariant is: **no hard negative ever becomes a
//! duplicate proposal**, while true duplicates still do. Re-run this suite
//! (and re-derive thresholds) whenever the embedder or thresholds change.
#![allow(clippy::unwrap_used)]

use cs_core::{
    ActorRef, Bitemporal, MemoryId, MemoryRevisionId, Provenance, Scope, SourceRef, TenantId,
    TimeInterval, UserId,
};
use cs_engine::{Embedder, HashingEmbedder};
use cs_memory::{
    EpisodicMemory, MemoryContent, MemoryRecord, MemoryStatus, ResolutionKind, SemanticMemory,
};
use cs_policy::{ResolutionConfig, detect_memory, resolve::Candidate};
use time::macros::datetime;
use uuid::Uuid;

fn record(n: u128, content: MemoryContent) -> MemoryRecord {
    let t = datetime!(2026-01-01 0:00 UTC);
    MemoryRecord {
        id: MemoryId(Uuid::from_u128(n)),
        revision_id: MemoryRevisionId(Uuid::from_u128(n + 1_000)),
        revision_number: 1,
        scope: Scope::private(TenantId(Uuid::from_u128(1)), UserId(Uuid::from_u128(2))),
        content,
        provenance: Provenance {
            source: SourceRef::UserInput { message_id: None },
            actor: ActorRef::System {
                component: "calibration".into(),
            },
            observed_at: t,
            recorded_at: t,
            extraction: None,
            parent_evidence: vec![],
            corroborations: vec![],
        },
        temporal: Bitemporal {
            valid_time: TimeInterval::from(t),
            transaction_time: TimeInterval::from(t),
        },
        status: MemoryStatus::Active,
        status_reason: None,
        supersedes: None,
        superseded_by: None,
        created_at: t,
        expires_at: None,
    }
}

fn sem(p: &str) -> MemoryContent {
    MemoryContent::Semantic(SemanticMemory {
        proposition: p.into(),
        subject: None,
        predicate: None,
        object: None,
    })
}

fn epi(p: &str) -> MemoryContent {
    MemoryContent::Episodic(EpisodicMemory {
        description: p.into(),
        event_id: None,
        participants: vec![],
    })
}

fn duplicate(a: MemoryContent, b: MemoryContent) -> (bool, f64) {
    let e = HashingEmbedder::default();
    let (ra, rb) = (record(1, a), record(2, b));
    let (ea, eb) = (
        e.embed(&ra.content.index_text()),
        e.embed(&rb.content.index_text()),
    );
    let cos = cs_policy::cosine_similarity(&ea, &eb).unwrap();
    let found = detect_memory(
        &ResolutionConfig::default(),
        Some("cs-hashing@1"),
        Candidate {
            record: &rb,
            embedding: Some(&eb),
        },
        &[Candidate {
            record: &ra,
            embedding: Some(&ea),
        }],
    );
    (
        found.iter().any(|p| p.kind == ResolutionKind::Duplicate),
        cos,
    )
}

#[test]
fn hard_negatives_never_merge() {
    let pairs = [
        // Argument inversion.
        ("service-a calls service-b", "service-b calls service-a"),
        // Identifier swap.
        ("cs-core defines ScopeId", "cs-memory defines ScopeId"),
        // Numeric and version flips.
        ("retry backoff max is 30s", "retry backoff max is 60s"),
        (
            "the API is served on port 8080",
            "the API is served on port 9090",
        ),
        ("we upgraded sqlx to 0.8", "we upgraded sqlx to 0.9"),
        // Negation flip.
        ("exact scoring is enabled", "exact scoring is not enabled"),
        // Same topic, different fact (pass the structural veto).
        (
            "the recall trace stores the query hash",
            "the recall trace stores the embedding model",
        ),
        (
            "the team prefers small pull requests",
            "the team prefers big pull requests",
        ),
        (
            "alice owns the billing service",
            "bob owns the billing service",
        ),
    ];
    for (a, b) in pairs {
        for (ca, cb) in [(sem(a), sem(b)), (epi(a), epi(b))] {
            let (dup, cos) = duplicate(ca, cb);
            assert!(!dup, "hard negative merged (cos {cos:.3}): {a:?} vs {b:?}");
        }
    }
}

#[test]
fn true_duplicates_are_proposed() {
    let pairs = [
        (
            "The primary database is PostgreSQL.",
            "the primary database is postgresql",
        ),
        (
            "Deploys are frozen on Fridays",
            "deploys are frozen on fridays!",
        ),
    ];
    for (a, b) in pairs {
        let (dup, cos) = duplicate(sem(a), sem(b));
        assert!(dup, "duplicate missed (cos {cos:.3}): {a:?} vs {b:?}");
    }
    // Near-paraphrases above the semantic threshold also qualify.
    let (dup, cos) = duplicate(
        sem("the ingestion worker retries failed batches with backoff"),
        sem("the ingestion worker retries failed batches with exponential backoff"),
    );
    assert!(dup, "paraphrase missed (cos {cos:.3})");
}
