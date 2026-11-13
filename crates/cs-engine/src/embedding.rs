//! Embedding provider abstraction.
//!
//! Model-backed embedders are adapters that live outside the core. The
//! built-in [`HashingEmbedder`] is a dependency-free, fully deterministic
//! baseline (signed feature hashing of unigrams and bigrams). It is what the
//! retrieval experiments compare learned embeddings against.

use cs_memory::EmbeddingRecord;

pub trait Embedder: Send + Sync + std::fmt::Debug {
    fn model_name(&self) -> &str;
    fn model_version(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;

    fn record(&self, text: &str) -> EmbeddingRecord {
        EmbeddingRecord {
            model_name: self.model_name().to_owned(),
            model_version: self.model_version().to_owned(),
            vector: self.embed(text),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dimensions: usize,
}

impl HashingEmbedder {
    pub const MODEL_NAME: &'static str = "cs-hashing";
    pub const MODEL_VERSION: &'static str = "1";

    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions: dimensions.max(8),
        }
    }
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self::new(384)
    }
}

/// FNV-1a, 64-bit: stable across platforms and releases.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

impl Embedder for HashingEmbedder {
    fn model_name(&self) -> &str {
        Self::MODEL_NAME
    }

    fn model_version(&self) -> &str {
        Self::MODEL_VERSION
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let tokens = cs_policy::lexical::tokenize(text);
        let mut v = vec![0.0_f32; self.dimensions];
        let mut add = |feature: &str, weight: f32| {
            let h = fnv1a(feature.as_bytes());
            let idx = (h % self.dimensions as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            v[idx] += sign * weight;
        };
        for t in &tokens {
            add(t, 1.0);
        }
        for pair in tokens.windows(2) {
            add(&format!("{} {}", pair[0], pair[1]), 0.5);
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use cs_policy::cosine_similarity;

    use super::*;

    #[test]
    fn deterministic_and_normalized() {
        let e = HashingEmbedder::default();
        let a = e.embed("postgres stores memories");
        assert_eq!(a, e.embed("postgres stores memories"));
        let norm: f32 = a.iter().map(|x| x * x).sum();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn related_text_is_closer() {
        let e = HashingEmbedder::default();
        let q = e.embed("postgres vector storage");
        let near = cosine_similarity(&q, &e.embed("vector storage in postgres")).unwrap();
        let far = cosine_similarity(&q, &e.embed("the cat sat on a mat")).unwrap();
        assert!(near > far);
    }
}
