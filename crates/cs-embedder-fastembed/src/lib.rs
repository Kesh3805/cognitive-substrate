//! Local ONNX embeddings via [FastEmbed](https://github.com/anush008/fastembed-rs).
//!
//! Optional: this crate is outside the workspace's default members because
//! it links ONNX Runtime and downloads model files on first use. Build it
//! explicitly (`cargo build -p cs-embedder-fastembed`) or via the server's
//! `fastembed` feature.
//!
//! Inference is CPU-bound and synchronous, so it runs on Tokio's blocking
//! pool; the model sits behind a mutex because FastEmbed's `embed` takes
//! `&mut self`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cs_engine::{EmbedError, Embedder, EmbeddingSpace, embedding::PREPROCESSING_VERSION};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// Models with a known, fixed geometry. The space revision pins the
/// FastEmbed major version, which pins the model files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalModel {
    BgeSmallEnV15,
    BgeBaseEnV15,
    AllMiniLmL6V2,
    NomicEmbedTextV15,
}

impl LocalModel {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "bge-small-en-v1.5" => Self::BgeSmallEnV15,
            "bge-base-en-v1.5" => Self::BgeBaseEnV15,
            "all-minilm-l6-v2" => Self::AllMiniLmL6V2,
            "nomic-embed-text-v1.5" => Self::NomicEmbedTextV15,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::BgeSmallEnV15 => "bge-small-en-v1.5",
            Self::BgeBaseEnV15 => "bge-base-en-v1.5",
            Self::AllMiniLmL6V2 => "all-minilm-l6-v2",
            Self::NomicEmbedTextV15 => "nomic-embed-text-v1.5",
        }
    }

    fn fastembed(self) -> EmbeddingModel {
        match self {
            Self::BgeSmallEnV15 => EmbeddingModel::BGESmallENV15,
            Self::BgeBaseEnV15 => EmbeddingModel::BGEBaseENV15,
            Self::AllMiniLmL6V2 => EmbeddingModel::AllMiniLML6V2,
            Self::NomicEmbedTextV15 => EmbeddingModel::NomicEmbedTextV15,
        }
    }

    fn dimensions(self) -> u32 {
        match self {
            Self::BgeSmallEnV15 | Self::AllMiniLmL6V2 => 384,
            Self::BgeBaseEnV15 | Self::NomicEmbedTextV15 => 768,
        }
    }

    /// Asymmetric models need query/document prefixes.
    fn prefixes(self) -> (&'static str, &'static str) {
        match self {
            Self::NomicEmbedTextV15 => ("search_document: ", "search_query: "),
            Self::BgeSmallEnV15 | Self::BgeBaseEnV15 => (
                "",
                "Represent this sentence for searching relevant passages: ",
            ),
            Self::AllMiniLmL6V2 => ("", ""),
        }
    }

    pub fn space(self) -> EmbeddingSpace {
        let (document_prefix, query_prefix) = self.prefixes();
        EmbeddingSpace {
            provider: "fastembed".into(),
            model: self.name().into(),
            revision: "fastembed-7".into(),
            dimensions: self.dimensions(),
            normalized: true,
            preprocessing: PREPROCESSING_VERSION,
            document_prefix: document_prefix.into(),
            query_prefix: query_prefix.into(),
        }
    }
}

#[derive(Clone)]
pub struct FastEmbedder {
    space: EmbeddingSpace,
    model: Arc<Mutex<TextEmbedding>>,
}

impl std::fmt::Debug for FastEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastEmbedder")
            .field("space", &self.space.id())
            .finish()
    }
}

impl FastEmbedder {
    /// Load (downloading on first use into `cache_dir`) a local model.
    pub fn new(model: LocalModel, cache_dir: Option<std::path::PathBuf>) -> Result<Self, String> {
        let mut options = TextInitOptions::new(model.fastembed());
        if let Some(dir) = cache_dir {
            options = options.with_cache_dir(dir);
        }
        let inner = TextEmbedding::try_new(options).map_err(|e| e.to_string())?;
        Ok(Self {
            space: model.space(),
            model: Arc::new(Mutex::new(inner)),
        })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let model = Arc::clone(&self.model);
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut m = model
                .lock()
                .map_err(|_| EmbedError::Retryable("embedding model lock poisoned".into()))?;
            m.embed(&texts, None)
                .map_err(|e| EmbedError::Permanent(e.to_string()))
        })
        .await
        .map_err(|e| EmbedError::Retryable(format!("embedding task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_are_distinct_and_carry_prefixes() {
        let nomic = LocalModel::NomicEmbedTextV15.space();
        assert_eq!(nomic.dimensions, 768);
        assert_eq!(nomic.query_prefix, "search_query: ");
        assert_ne!(nomic.id(), LocalModel::BgeBaseEnV15.space().id());
        assert_eq!(
            LocalModel::parse("bge-small-en-v1.5"),
            Some(LocalModel::BgeSmallEnV15)
        );
        assert!(LocalModel::parse("unknown").is_none());
    }

    /// Downloads a model; run with `CS_TEST_FASTEMBED=1`.
    #[tokio::test(flavor = "multi_thread")]
    async fn embeds_locally_when_enabled() {
        if std::env::var("CS_TEST_FASTEMBED").is_err() {
            return;
        }
        let e = FastEmbedder::new(LocalModel::AllMiniLmL6V2, None).unwrap();
        let out = cs_engine::embedding::embed_texts(
            &e,
            &["Postgres stores memories".to_owned()],
            cs_engine::EmbedKind::Document,
            time::OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
        assert_eq!(out[0].vector.len(), 384);
    }
}
