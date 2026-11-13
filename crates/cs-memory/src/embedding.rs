//! Embeddings are a derived retrieval index, never the source of truth.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    pub model_name: String,
    pub model_version: String,
    pub vector: Vec<f32>,
}

impl EmbeddingRecord {
    pub fn dimensions(&self) -> usize {
        self.vector.len()
    }

    pub fn same_model(&self, name: &str, version: &str) -> bool {
        self.model_name == name && self.model_version == version
    }
}
