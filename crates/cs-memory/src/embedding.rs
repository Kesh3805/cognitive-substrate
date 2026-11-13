//! Embeddings are versioned, derived data. They are never the source of
//! truth, and vectors from different embedding spaces are never compared.

use serde::{Deserialize, Serialize};

/// Version of the canonical text projection that gets embedded
/// (`MemoryContent::index_text`, belief propositions). Bump it when the
/// projection changes, since that changes the vector space.
pub const PREPROCESSING_VERSION: u32 = 1;

/// Immutable identity of a vector space. Two embedders produce comparable
/// vectors only if their spaces are equal. A provider silently updating a
/// model behind a stable name must be reflected in `revision`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbeddingSpace {
    /// e.g. `cs`, `openai`, `ollama`, `fastembed`.
    pub provider: String,
    pub model: String,
    /// Model revision or digest; `"unversioned"` if the provider has none.
    pub revision: String,
    pub dimensions: u32,
    /// Vectors are L2-normalized before storage.
    pub normalized: bool,
    pub preprocessing: u32,
    /// Prefix applied to stored documents (asymmetric models such as
    /// nomic, e5 or bge use e.g. `search_document: `). Part of the identity.
    #[serde(default)]
    pub document_prefix: String,
    /// Prefix applied to queries (e.g. `search_query: `).
    #[serde(default)]
    pub query_prefix: String,
}

impl EmbeddingSpace {
    /// Stored as `model_name` on every vector.
    pub fn model_name(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Stored as `model_version` on every vector.
    pub fn model_version(&self) -> String {
        let mut v = format!(
            "{}+d{}+{}+pp{}",
            self.revision,
            self.dimensions,
            if self.normalized { "l2" } else { "raw" },
            self.preprocessing
        );
        if !self.document_prefix.is_empty() || !self.query_prefix.is_empty() {
            // Prefixes change the geometry; a short stable fingerprint keeps
            // the id readable.
            let fp =
                fnv1a(format!("{}\u{1f}{}", self.document_prefix, self.query_prefix).as_bytes());
            v.push_str(&format!("+px{:08x}", fp as u32));
        }
        v
    }

    /// Stable space id: `provider/model@revision+dN+l2+ppN`.
    pub fn id(&self) -> String {
        format!("{}@{}", self.model_name(), self.model_version())
    }
}

/// FNV-1a 64-bit (stable across platforms and releases).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Longest canonical projection embedded, in characters. Longer texts keep
/// their head and tail, so the cap is deterministic and part of the
/// preprocessing version rather than an invisible server-side truncation.
pub const MAX_EMBED_CHARS: usize = 8_192;
const TAIL_CHARS: usize = 2_048;

/// The exact text embedded for a canonical projection (pp1).
pub fn embedding_text(projection: &str) -> String {
    let n = projection.chars().count();
    if n <= MAX_EMBED_CHARS {
        return projection.to_owned();
    }
    let head: String = projection
        .chars()
        .take(MAX_EMBED_CHARS - TAIL_CHARS - 3)
        .collect();
    let tail: String = projection.chars().skip(n - TAIL_CHARS).collect();
    format!("{head} … {tail}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    pub model_name: String,
    pub model_version: String,
    pub vector: Vec<f32>,
    /// SHA-256 of the preprocessing version and embedded text: identifies
    /// exactly what was embedded, and detects stale vectors.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// When the vector became available. Recall as of time T only uses
    /// vectors embedded at or before T, so a later backfill never changes
    /// the result of replaying an earlier recall.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub embedded_at: Option<time::OffsetDateTime>,
}

impl EmbeddingRecord {
    pub fn dimensions(&self) -> usize {
        self.vector.len()
    }

    pub fn same_model(&self, name: &str, version: &str) -> bool {
        self.model_name == name && self.model_version == version
    }

    pub fn space_id(&self) -> String {
        format!("{}@{}", self.model_name, self.model_version)
    }
}

/// L2-normalize in place; zero vectors are left unchanged.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_identity_covers_everything_that_changes_geometry() {
        let a = EmbeddingSpace {
            provider: "ollama".into(),
            model: "nomic-embed-text".into(),
            revision: "v1.5".into(),
            dimensions: 768,
            normalized: true,
            preprocessing: PREPROCESSING_VERSION,
            document_prefix: String::new(),
            query_prefix: String::new(),
        };
        assert_eq!(a.id(), "ollama/nomic-embed-text@v1.5+d768+l2+pp1");
        let b = EmbeddingSpace {
            revision: "v2".into(),
            ..a.clone()
        };
        let c = EmbeddingSpace {
            preprocessing: 2,
            ..a.clone()
        };
        let d = EmbeddingSpace {
            normalized: false,
            ..a.clone()
        };
        assert_ne!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        assert_ne!(a.id(), d.id());
        let e = EmbeddingSpace {
            query_prefix: "search_query: ".into(),
            ..a.clone()
        };
        assert_ne!(a.id(), e.id());
    }

    #[test]
    fn projection_cap_keeps_head_and_tail() {
        let long = format!("{}END", "x".repeat(20_000));
        let t = embedding_text(&long);
        assert!(t.chars().count() <= MAX_EMBED_CHARS);
        assert!(t.ends_with("END"));
        assert_eq!(embedding_text("short"), "short");
    }
}
