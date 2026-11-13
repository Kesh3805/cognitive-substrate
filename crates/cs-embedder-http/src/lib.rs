//! Embeddings over HTTP.
//!
//! Two wire protocols:
//! * [`Protocol::OpenAi`]: `POST {base}/embeddings` with `{model, input: [...]}`.
//!   Works with OpenAI, vLLM, TEI, LiteLLM and Ollama's `/v1` endpoint.
//! * [`Protocol::OllamaNative`]: `POST {base}/api/embed` with
//!   `{model, input: [...], truncate: false}`, so context overflow surfaces
//!   as an error instead of a silently truncated vector.
//!
//! Hardening that commonly matters in production:
//! * **Two-dimensional batching**: a batch is flushed at `max_batch_items`
//!   *or* when its total UTF-8 size would exceed `max_batch_bytes`, since
//!   servers limit total tokens per request, not only item count.
//! * **Poison isolation**: when a multi-item batch is rejected with a
//!   client error (400/413/422), it is bisected so only the offending item
//!   fails and the rest still embed.
//! * **Index ordering**: OpenAI-style responses are reordered by `index`,
//!   and the indices must be exactly `0..n`.
//! * Status classification: transport errors, 408, 429 and 5xx are
//!   retryable; other errors are permanent.
//!
//! Prefixing, length capping, hashing, dimension checks and L2
//! normalization are done by the engine (`cs_engine::embedding`), so this
//! adapter only maps strings to vectors.

use std::time::Duration;

use async_trait::async_trait;
use cs_engine::{EmbedError, Embedder, EmbeddingSpace};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAi,
    OllamaNative,
}

#[derive(Debug, Clone)]
pub struct HttpEmbedderConfig {
    pub protocol: Protocol,
    /// e.g. `https://api.openai.com/v1`, `http://localhost:11434/v1`
    /// (OpenAI protocol) or `http://localhost:11434` (native).
    pub base_url: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub max_batch_items: usize,
    /// Conservative proxy for a server's per-request token budget
    /// (about 3 bytes per token).
    pub max_batch_bytes: usize,
    /// The space this endpoint produces vectors in. `space.model` is sent
    /// as the request's `model`.
    pub space: EmbeddingSpace,
}

#[derive(Debug, Clone)]
pub struct HttpEmbedder {
    config: HttpEmbedderConfig,
    client: reqwest::Client,
}

impl HttpEmbedder {
    pub fn new(config: HttpEmbedderConfig) -> Result<Self, String> {
        if config.max_batch_items == 0 || config.max_batch_bytes == 0 {
            return Err("batch limits must be positive".into());
        }
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { config, client })
    }

    fn url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        match self.config.protocol {
            Protocol::OpenAi => format!("{base}/embeddings"),
            Protocol::OllamaNative => format!("{base}/api/embed"),
        }
    }

    /// Split inputs into batches bounded by item count and total bytes.
    /// A single oversize item still forms its own batch (the server
    /// decides whether it fits).
    pub fn plan_batches(&self, texts: &[String]) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let (mut start, mut bytes) = (0, 0);
        for (i, t) in texts.iter().enumerate() {
            let len = t.len();
            let full = i - start >= self.config.max_batch_items
                || (i > start && bytes + len > self.config.max_batch_bytes);
            if full {
                out.push(start..i);
                start = i;
                bytes = 0;
            }
            bytes += len;
        }
        if start < texts.len() {
            out.push(start..texts.len());
        }
        out
    }

    async fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Failure> {
        let body = match self.config.protocol {
            Protocol::OpenAi => json!({ "model": self.config.space.model, "input": texts }),
            Protocol::OllamaNative => {
                json!({ "model": self.config.space.model, "input": texts, "truncate": false })
            }
        };
        let mut req = self.client.post(self.url()).json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Failure::Retryable(format!("request failed: {e}")))?;
        let status = resp.status();
        if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
            return Err(Failure::Retryable(format!("endpoint returned {status}")));
        }
        if matches!(status.as_u16(), 400 | 413 | 422) {
            return Err(Failure::Rejected(format!("endpoint rejected input: {status}")));
        }
        if !status.is_success() {
            return Err(Failure::Permanent(format!("endpoint returned {status}")));
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Failure::Permanent(format!("unreadable response: {e}")))?;
        decode(self.config.protocol, &value, texts.len()).map_err(Failure::Permanent)
    }

    /// Embed one batch, bisecting on input rejection so a single poison
    /// item cannot fail its neighbours.
    fn embed_range<'a>(
        &'a self,
        texts: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Result<Vec<f32>, EmbedError>>> + Send + 'a>>
    {
        Box::pin(async move {
            match self.call(texts).await {
                Ok(vectors) => vectors.into_iter().map(Ok).collect(),
                Err(Failure::Rejected(_)) if texts.len() > 1 => {
                    let mid = texts.len() / 2;
                    let mut left = self.embed_range(&texts[..mid]).await;
                    left.extend(self.embed_range(&texts[mid..]).await);
                    left
                }
                Err(f) => {
                    let e = f.into_error();
                    texts.iter().map(|_| Err(e.clone())).collect()
                }
            }
        })
    }

    /// Per-item results, in input order. Used by callers that want partial
    /// success; [`Embedder::embed_batch`] fails if any item fails.
    pub async fn embed_each(&self, texts: &[String]) -> Vec<Result<Vec<f32>, EmbedError>> {
        let mut out = Vec::with_capacity(texts.len());
        for range in self.plan_batches(texts) {
            out.extend(self.embed_range(&texts[range]).await);
        }
        out
    }
}

#[derive(Debug)]
enum Failure {
    Retryable(String),
    /// The server rejected the input itself (bisectable).
    Rejected(String),
    Permanent(String),
}

impl Failure {
    fn into_error(self) -> EmbedError {
        match self {
            Self::Retryable(m) => EmbedError::Retryable(m),
            Self::Rejected(m) | Self::Permanent(m) => EmbedError::Permanent(m),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiItem {
    index: usize,
    embedding: Vec<f32>,
}

/// Decode a response body for `expected` inputs. Public for testing.
pub fn decode(protocol: Protocol, value: &serde_json::Value, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    match protocol {
        Protocol::OpenAi => {
            let mut items: Vec<OpenAiItem> = serde_json::from_value(value["data"].clone())
                .map_err(|e| format!("unexpected response shape: {e}"))?;
            items.sort_by_key(|i| i.index);
            if items.len() != expected || items.iter().enumerate().any(|(n, i)| i.index != n) {
                return Err(format!(
                    "response indices do not cover 0..{expected} exactly ({} items)",
                    items.len()
                ));
            }
            Ok(items.into_iter().map(|i| i.embedding).collect())
        }
        Protocol::OllamaNative => {
            let vectors: Vec<Vec<f32>> = serde_json::from_value(value["embeddings"].clone())
                .map_err(|e| format!("unexpected response shape: {e}"))?;
            if vectors.len() != expected {
                return Err(format!("expected {expected} embeddings, got {}", vectors.len()));
            }
            Ok(vectors)
        }
    }
}

#[async_trait]
impl Embedder for HttpEmbedder {
    fn space(&self) -> &EmbeddingSpace {
        &self.config.space
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_each(texts).await.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedder(max_items: usize, max_bytes: usize) -> HttpEmbedder {
        HttpEmbedder::new(HttpEmbedderConfig {
            protocol: Protocol::OpenAi,
            base_url: "http://127.0.0.1:1".into(),
            api_key: None,
            timeout: Duration::from_secs(1),
            max_batch_items: max_items,
            max_batch_bytes: max_bytes,
            space: EmbeddingSpace {
                provider: "test".into(),
                model: "m".into(),
                revision: "1".into(),
                dimensions: 2,
                normalized: true,
                preprocessing: 1,
                document_prefix: String::new(),
                query_prefix: String::new(),
            },
        })
        .unwrap()
    }

    #[test]
    fn batches_respect_items_and_bytes() {
        let t = |n: usize| "x".repeat(n);
        let e = embedder(3, 10);
        assert_eq!(e.plan_batches(&[t(1), t(1), t(1), t(1)]), vec![0..3, 3..4]);
        assert_eq!(e.plan_batches(&[t(6), t(6), t(1)]), vec![0..1, 1..3]);
        // An oversize item stands alone.
        assert_eq!(e.plan_batches(&[t(50), t(1)]), vec![0..1, 1..2]);
        assert!(e.plan_batches(&[]).is_empty());
    }

    #[test]
    fn openai_responses_are_reordered_and_checked() {
        let v = json!({ "data": [
            { "index": 1, "embedding": [0.0, 1.0] },
            { "index": 0, "embedding": [1.0, 0.0] }
        ]});
        assert_eq!(decode(Protocol::OpenAi, &v, 2).unwrap(), vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        let gap = json!({ "data": [ { "index": 0, "embedding": [1.0] }, { "index": 2, "embedding": [1.0] } ] });
        assert!(decode(Protocol::OpenAi, &gap, 2).is_err());
        assert!(decode(Protocol::OpenAi, &v, 3).is_err());
    }

    #[test]
    fn ollama_responses_are_checked() {
        let v = json!({ "embeddings": [[1.0, 0.0]] });
        assert_eq!(decode(Protocol::OllamaNative, &v, 1).unwrap().len(), 1);
        assert!(decode(Protocol::OllamaNative, &v, 2).is_err());
    }
}
