//! Model-backed extraction over any OpenAI-compatible chat-completions
//! endpoint: OpenAI, vLLM, llama.cpp server, LM Studio, or Ollama
//! (`http://localhost:11434/v1`).
//!
//! The model's output is untrusted. It must be a JSON object of the form
//!
//! ```json
//! {"memories": [{"domain": "semantic", "data": {...}, "confidence": 0.7, "rationale": "..."}]}
//! ```
//!
//! Every item is decoded into a typed memory payload and validated; one
//! invalid item rejects the whole extraction (no partial writes). The engine
//! stores accepted items only as *candidate* memories.
//!
//! Failure classification: timeouts, connection errors, 408, 429 and 5xx are
//! retryable; other 4xx and malformed output are permanent.

use std::time::Duration;

use async_trait::async_trait;
use cs_engine::{ExtractError, Extractor};
use cs_memory::{ExtractedCandidate, MemoryContent, MemoryDomain, Observation};
use serde::Deserialize;
use serde_json::json;

/// Bumped whenever the prompt or decoding changes.
pub const PROMPT_VERSION: &str = "1";
const MAX_CANDIDATES: usize = 50;

const SYSTEM_PROMPT: &str = "You extract durable knowledge from text for a memory system. \
Return ONLY a JSON object: {\"memories\": [...]}. Each item is {\"domain\", \"data\", \
\"confidence\", \"rationale\"} where domain is one of semantic, episodic, procedural, \
relational, temporal. Shapes: semantic {proposition, subject?, predicate?, object?}; \
episodic {description, participants?}; procedural {name, steps[], preconditions?, \
expected_outcome?}; relational {subject, predicate, object}; temporal {subject, predicate, \
value, change_kind: created|updated|invalidated|superseded}. Extract only explicit, \
assertive statements. Skip questions, hedges, opinions, hypotheticals, negations and \
small talk. confidence is your heuristic estimate in [0,1]. Return {\"memories\": []} \
when nothing qualifies.";

#[derive(Debug, Clone)]
pub struct HttpExtractorConfig {
    /// Base URL ending before `/chat/completions`, e.g.
    /// `https://api.openai.com/v1` or `http://localhost:11434/v1`.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct HttpExtractor {
    config: HttpExtractorConfig,
    client: reqwest::Client,
    version: String,
}

impl HttpExtractor {
    pub fn new(config: HttpExtractorConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            version: format!("p{PROMPT_VERSION}"),
            config,
            client,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Extraction {
    memories: Vec<Item>,
}

#[derive(Debug, Deserialize)]
struct Item {
    domain: MemoryDomain,
    data: serde_json::Value,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    rationale: Option<String>,
}

fn permanent(m: impl Into<String>) -> ExtractError {
    ExtractError::Permanent(m.into())
}

/// Decode and validate model output. Public for testing.
pub fn decode(raw: &str) -> Result<Vec<ExtractedCandidate>, ExtractError> {
    // Tolerate a fenced JSON block, nothing else.
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed);
    let parsed: Extraction = serde_json::from_str(body)
        .map_err(|e| permanent(format!("malformed model output: {e}")))?;
    if parsed.memories.len() > MAX_CANDIDATES {
        return Err(permanent(format!("more than {MAX_CANDIDATES} candidates")));
    }
    parsed
        .memories
        .into_iter()
        .map(|item| {
            if item.domain == MemoryDomain::Working {
                return Err(permanent("working memory is not extractable"));
            }
            let content: MemoryContent =
                serde_json::from_value(json!({ "domain": item.domain, "data": item.data }))
                    .map_err(|e| {
                        permanent(format!("invalid {} payload: {e}", item.domain.as_str()))
                    })?;
            content.validate().map_err(|e| permanent(e.to_string()))?;
            if let Some(c) = item.confidence
                && !(c.is_finite() && (0.0..=1.0).contains(&c))
            {
                return Err(permanent("confidence must be in [0, 1]"));
            }
            Ok(ExtractedCandidate {
                content,
                confidence: item.confidence,
                rationale: item.rationale.map(|r| r.chars().take(500).collect()),
            })
        })
        .collect()
}

#[async_trait]
impl Extractor for HttpExtractor {
    fn name(&self) -> &str {
        "openai-compatible"
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn model(&self) -> Option<&str> {
        Some(&self.config.model)
    }

    async fn extract(
        &self,
        observation: &Observation,
    ) -> Result<Vec<ExtractedCandidate>, ExtractError> {
        let text = observation
            .content
            .as_deref()
            .ok_or_else(|| permanent("observation content was purged"))?;
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = json!({
            "model": self.config.model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": text },
            ],
        });
        let mut req = self.client.post(url).json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ExtractError::Retryable(format!("request failed: {e}")))?;
        let status = resp.status();
        if status.is_server_error() || status.as_u16() == 429 || status.as_u16() == 408 {
            return Err(ExtractError::Retryable(format!(
                "endpoint returned {status}"
            )));
        }
        if !status.is_success() {
            return Err(permanent(format!("endpoint returned {status}")));
        }
        let chat: ChatResponse = resp
            .json()
            .await
            .map_err(|e| permanent(format!("unexpected response shape: {e}")))?;
        let content = chat
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| permanent("response had no message content"))?;
        decode(&content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_typed_memories() {
        let out = decode(
            r#"{"memories":[{"domain":"relational","data":{"subject":"api","predicate":"uses","object":"postgres"},"confidence":0.8}]}"#,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.domain(), MemoryDomain::Relational);
    }

    #[test]
    fn tolerates_fenced_json() {
        assert!(
            decode("```json\n{\"memories\": []}\n```")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_invalid_output_wholesale() {
        for bad in [
            "not json",
            r#"{"items": []}"#,
            r#"{"memories":[{"domain":"semantic","data":{"proposition":""}}]}"#,
            r#"{"memories":[{"domain":"working","data":{"objective":"x"}}]}"#,
            r#"{"memories":[{"domain":"semantic","data":{"proposition":"ok"},"confidence":3}]}"#,
            r#"{"memories":[{"domain":"semantic","data":{"proposition":"ok"}},{"domain":"procedural","data":{"name":"x","steps":[]}}]}"#,
        ] {
            assert!(
                matches!(decode(bad), Err(ExtractError::Permanent(_))),
                "{bad}"
            );
        }
    }
}
