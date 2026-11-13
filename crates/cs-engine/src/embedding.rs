//! Embedding provider abstraction.
//!
//! Embeddings are **versioned, derived data**: never the source of truth,
//! and never compared across embedding spaces. Embedders are async,
//! batch-capable and allowed to fail. A write whose embedding fails is still
//! stored, together with an `embed` job that backfills the vector (ADR-0016).
//!
//! The engine, not the adapter, owns the exact bytes sent to a model:
//! it caps the canonical projection ([`embedding_text`]), applies the
//! space's query or document prefix, and hashes the result. Adapters only
//! turn strings into vectors; [`embed_texts`] then enforces the contract
//! (count, dimensions, finite, non-zero) and L2-normalizes in Rust when the
//! space is normalized, so every adapter behaves identically.

use async_trait::async_trait;
pub use cs_memory::embedding::{
    EmbeddingSpace, PREPROCESSING_VERSION, embedding_text, l2_normalize,
};
use cs_memory::EmbeddingRecord;
use time::OffsetDateTime;

/// Why embedding failed, which decides whether a backfill job retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedError {
    /// Transient (network, rate limit, 5xx, model loading): retry.
    Retryable(String),
    /// Will not succeed (bad input, wrong dimensions, 4xx): dead-letter.
    Permanent(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(m) => write!(f, "retryable: {m}"),
            Self::Permanent(m) => write!(f, "permanent: {m}"),
        }
    }
}

/// What is being embedded; selects the space's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedKind {
    Query,
    Document,
}

#[async_trait]
pub trait Embedder: Send + Sync + std::fmt::Debug {
    /// The immutable space this embedder produces vectors in.
    fn space(&self) -> &EmbeddingSpace;

    /// Embed already-prepared texts (capped and prefixed by the engine).
    /// Must return exactly one vector per input, in input order.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// The exact string sent to the model for a canonical projection.
pub fn prepare(space: &EmbeddingSpace, projection: &str, kind: EmbedKind) -> String {
    let prefix = match kind {
        EmbedKind::Query => &space.query_prefix,
        EmbedKind::Document => &space.document_prefix,
    };
    format!("{prefix}{}", embedding_text(projection))
}

/// SHA-256 of preprocessing version and the exact prepared text.
pub fn content_hash(space: &EmbeddingSpace, prepared: &str) -> String {
    cs_policy::query_hash(&format!("pp{}\u{1f}{prepared}", space.preprocessing))
}

/// Validate one returned vector and normalize it if the space requires.
pub fn check_vector(space: &EmbeddingSpace, mut v: Vec<f32>) -> Result<Vec<f32>, EmbedError> {
    if v.len() != space.dimensions as usize {
        return Err(EmbedError::Permanent(format!(
            "{} returned {} dimensions, expected {}",
            space.id(),
            v.len(),
            space.dimensions
        )));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(EmbedError::Permanent("embedding contains non-finite values".into()));
    }
    let norm = v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    if norm <= 1e-6 {
        return Err(EmbedError::Permanent("embedding has zero norm".into()));
    }
    if space.normalized {
        l2_normalize(&mut v);
    }
    Ok(v)
}

/// Embed canonical projections into the embedder's space, enforcing the
/// adapter contract. Empty projections are rejected before any call.
pub async fn embed_texts(
    embedder: &dyn Embedder,
    projections: &[String],
    kind: EmbedKind,
    now: OffsetDateTime,
) -> Result<Vec<EmbeddingRecord>, EmbedError> {
    if projections.iter().any(|p| p.trim().is_empty()) {
        return Err(EmbedError::Permanent("cannot embed empty text".into()));
    }
    let space = embedder.space();
    let prepared: Vec<String> = projections.iter().map(|p| prepare(space, p, kind)).collect();
    let vectors = embedder.embed_batch(&prepared).await?;
    if vectors.len() != prepared.len() {
        return Err(EmbedError::Permanent(format!(
            "expected {} vectors, got {}",
            prepared.len(),
            vectors.len()
        )));
    }
    vectors
        .into_iter()
        .zip(&prepared)
        .map(|(v, text)| {
            Ok(EmbeddingRecord {
                model_name: space.model_name(),
                model_version: space.model_version(),
                vector: check_vector(space, v)?,
                content_hash: Some(content_hash(space, text)),
                embedded_at: Some(now),
            })
        })
        .collect()
}

/// Embed one projection.
pub async fn embed_one(
    embedder: &dyn Embedder,
    projection: &str,
    kind: EmbedKind,
    now: OffsetDateTime,
) -> Result<EmbeddingRecord, EmbedError> {
    let mut v = embed_texts(embedder, &[projection.to_owned()], kind, now).await?;
    v.pop().ok_or_else(|| EmbedError::Permanent("no vector returned".into()))
}

/// Deterministic, dependency-free baseline: signed feature hashing of
/// unigrams and bigrams.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    space: EmbeddingSpace,
}

impl HashingEmbedder {
    pub fn new(dimensions: usize) -> Self {
        Self {
            space: EmbeddingSpace {
                provider: "cs".into(),
                model: "hashing".into(),
                revision: "1".into(),
                dimensions: u32::try_from(dimensions.max(8)).unwrap_or(384),
                normalized: true,
                preprocessing: PREPROCESSING_VERSION,
                document_prefix: String::new(),
                query_prefix: String::new(),
            },
        }
    }

    /// Synchronous embedding (the hashing embedder never fails).
    pub fn embed_text(&self, text: &str) -> Vec<f32> {
        let dims = self.space.dimensions as usize;
        let tokens = cs_policy::lexical::tokenize(text);
        let mut v = vec![0.0_f32; dims];
        let mut add = |feature: &str, weight: f32| {
            let h = cs_memory::embedding::fnv1a(feature.as_bytes());
            let idx = (h % dims as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            v[idx] += sign * weight;
        };
        for t in &tokens {
            add(t, 1.0);
        }
        for pair in tokens.windows(2) {
            add(&format!("{} {}", pair[0], pair[1]), 0.5);
        }
        l2_normalize(&mut v);
        v
    }
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self::new(384)
    }
}

#[async_trait]
impl Embedder for HashingEmbedder {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|t| {
                let v = self.embed_text(t);
                // Text with no tokens hashes to the zero vector; give it a
                // fixed, valid direction so the contract holds.
                if v.iter().all(|x| *x == 0.0) {
                    let mut z = vec![0.0; v.len()];
                    z[0] = 1.0;
                    z
                } else {
                    v
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use cs_policy::cosine_similarity;

    use super::*;

    #[test]
    fn deterministic_and_normalized() {
        let e = HashingEmbedder::default();
        let a = e.embed_text("postgres stores memories");
        assert_eq!(a, e.embed_text("postgres stores memories"));
        let norm: f32 = a.iter().map(|x| x * x).sum();
        assert!((norm - 1.0).abs() < 1e-5);
        assert_eq!(e.space().id(), "cs/hashing@1+d384+l2+pp1");
    }

    #[test]
    fn related_text_is_closer() {
        let e = HashingEmbedder::default();
        let q = e.embed_text("postgres vector storage");
        let near = cosine_similarity(&q, &e.embed_text("vector storage in postgres")).unwrap();
        let far = cosine_similarity(&q, &e.embed_text("the cat sat on a mat")).unwrap();
        assert!(near > far);
    }

    #[test]
    fn contract_checks() {
        let s = HashingEmbedder::new(8).space().clone();
        assert!(check_vector(&s, vec![1.0; 8]).is_ok());
        assert!(matches!(check_vector(&s, vec![1.0; 4]), Err(EmbedError::Permanent(_))));
        assert!(check_vector(&s, vec![f32::NAN; 8]).is_err());
        assert!(check_vector(&s, vec![0.0; 8]).is_err(), "zero norm");
        let v = check_vector(&s, vec![3.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        assert!((v[0] - 0.6).abs() < 1e-6, "renormalized in Rust");
    }

    #[test]
    fn prefixes_are_part_of_what_is_hashed() {
        let mut s = HashingEmbedder::default().space().clone();
        let plain = content_hash(&s, &prepare(&s, "x", EmbedKind::Document));
        s.document_prefix = "search_document: ".into();
        let prefixed = content_hash(&s, &prepare(&s, "x", EmbedKind::Document));
        assert_ne!(plain, prefixed);
        assert!(prepare(&s, "x", EmbedKind::Document).starts_with("search_document: "));
        assert_eq!(prepare(&s, "x", EmbedKind::Query), "x");
    }
}
