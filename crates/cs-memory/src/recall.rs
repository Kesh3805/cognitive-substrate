//! Recall policy and trace contracts.
//!
//! The policy is an explicit value object: nothing about recall is decided by
//! defaults hidden inside a retrieval implementation. The scoring itself
//! lives in `cs-policy`.

use cs_core::{
    BeliefId, Error, MemoryId, MemoryRevisionId, RecallTraceId, Result, ScopeFilter, TenantId,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{MemoryDomain, unit_interval};

/// Current scoring function version (see `docs/specifications/recall-scoring.md`).
pub const SCORING_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecallPolicy {
    pub version: u32,
    pub allowed_domains: Vec<MemoryDomain>,
    /// Narrowing on top of the caller's authorized scope.
    pub scope_filter: Option<ScopeFilter>,
    /// Evaluate validity at this instant. Defaults to "now" at execution.
    #[serde(with = "time::serde::rfc3339::option")]
    pub as_of_valid_time: Option<OffsetDateTime>,
    /// See the substrate as it was at this instant. Defaults to "now".
    #[serde(with = "time::serde::rfc3339::option")]
    pub as_of_transaction_time: Option<OffsetDateTime>,
    pub max_candidates: u32,
    pub max_results: u32,
    pub max_context_tokens: Option<u32>,
    pub dense_weight: f64,
    pub lexical_weight: f64,
    pub recency_weight: f64,
    /// Age (days) at which the recency component halves.
    pub recency_half_life_days: f64,
    pub min_score: f64,
    /// Include revisions that were already superseded at the as-of time.
    pub include_superseded: bool,
    /// Include unvalidated (candidate) memories.
    pub include_candidates: bool,
    /// Persist the raw query text in the trace (enables exact replay).
    pub retain_query_text: bool,
    /// Recall beliefs alongside memories.
    pub include_beliefs: bool,
    /// Maximum beliefs returned.
    pub max_belief_results: u32,
    /// Include beliefs whose evidence is in conflict.
    pub include_disputed_beliefs: bool,
    /// Include retracted beliefs (history inspection).
    pub include_retracted_beliefs: bool,
    /// How the candidate set is generated. The default is the frozen v1
    /// control (the newest `max_candidates` revisions).
    pub candidates: CandidateGeneration,
    /// Record a miss as an information need (M5.4). Off by default, so
    /// recall never writes unless asked. Only present-time recalls record.
    pub record_information_needs: bool,
    /// Fixed (the full budget at once) or adaptive (a small first pass,
    /// expanded only when the lexical channel was saturated).
    pub budget: CandidateBudget,
}

/// How much of the candidate budget a recall uses (M5.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CandidateBudget {
    /// The full `max_candidates` budget at once.
    #[default]
    Fixed,
    /// A first pass with `initial` candidates and `initial_lexical` lexical
    /// slots. If the lexical channel is saturated (it filled every slot and
    /// the last admitted record shares as many query terms as the best, so
    /// equally good matches may lie beyond the cut), the recall runs again
    /// at the full budget. Requires lexical-only channels.
    Adaptive { initial: u32, initial_lexical: u32 },
}

/// What an adaptive budget did, recorded in the trace so that replay
/// re-derives the same decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetDecision {
    pub initial: u32,
    pub initial_lexical: u32,
    /// Lexical candidates the first pass found.
    pub lexical_found: u32,
    /// Shared query terms of the best and of the weakest admitted lexical
    /// candidate.
    pub best_overlap: u32,
    pub cut_overlap: u32,
    /// Every lexical slot was filled and the cut fell inside the
    /// best-matching tier (`cut_overlap == best_overlap`): equally good
    /// matches may lie beyond it, so the recall ran again at the full
    /// budget. It says the first pass was truncated, not that expanding
    /// helps.
    pub lexical_saturated: bool,
}

/// Candidate generation strategy. Every strategy fills the same budget
/// (`max_candidates` unique revisions) so strategies compare at equal cost.
///
/// The default, `channels { lexical: 100, dense: 0 }`, was adopted by the
/// preregistered experiment 006. `recency_v1` stays selectable as the
/// frozen control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum CandidateGeneration {
    /// v1, frozen: the newest revisions by transaction time.
    RecencyV1,
    /// Up to `lexical` revisions sharing the most query terms and up to
    /// `dense` nearest revisions by exact cosine similarity, deduplicated,
    /// then filled with the newest revisions up to `max_candidates`.
    /// Quotas are upper bounds: the set never exceeds `max_candidates`.
    Channels { lexical: u32, dense: u32 },
}

impl CandidateGeneration {
    /// The default adopted by experiment 006.
    pub const ADOPTED: Self = Self::Channels {
        lexical: 100,
        dense: 0,
    };
}

impl Default for CandidateGeneration {
    fn default() -> Self {
        Self::ADOPTED
    }
}

/// Which generation channel proposed a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateChannel {
    Recent,
    Lexical,
    Dense,
}

impl CandidateChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Lexical => "lexical",
            Self::Dense => "dense",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        [Self::Recent, Self::Lexical, Self::Dense]
            .into_iter()
            .find(|c| c.as_str() == v)
    }
}

impl Default for RecallPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            allowed_domains: MemoryDomain::ALL.to_vec(),
            scope_filter: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
            max_candidates: 200,
            max_results: 10,
            max_context_tokens: None,
            dense_weight: 0.55,
            lexical_weight: 0.30,
            recency_weight: 0.15,
            recency_half_life_days: 30.0,
            min_score: 0.0,
            include_superseded: false,
            include_candidates: false,
            retain_query_text: true,
            include_beliefs: true,
            max_belief_results: 5,
            include_disputed_beliefs: true,
            include_retracted_beliefs: false,
            candidates: CandidateGeneration::ADOPTED,
            record_information_needs: false,
            budget: CandidateBudget::Fixed,
        }
    }
}

impl RecallPolicy {
    pub const MAX_CANDIDATES: u32 = 10_000;

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::validation("unsupported recall policy version"));
        }
        if self.allowed_domains.is_empty() {
            return Err(Error::validation("allowed_domains must not be empty"));
        }
        if self.max_candidates == 0 || self.max_candidates > Self::MAX_CANDIDATES {
            return Err(Error::validation(format!(
                "max_candidates must be in 1..={}",
                Self::MAX_CANDIDATES
            )));
        }
        if let CandidateBudget::Adaptive {
            initial,
            initial_lexical,
        } = self.budget
        {
            if !matches!(
                self.candidates,
                CandidateGeneration::Channels { lexical, dense: 0 } if lexical > 0
            ) {
                return Err(Error::validation(
                    "an adaptive budget needs lexical-only candidate channels",
                ));
            }
            if initial == 0
                || initial > self.max_candidates
                || initial_lexical == 0
                || initial_lexical > initial
                || self.max_results > initial
            {
                return Err(Error::validation(
                    "adaptive budget needs 0 < initial_lexical <= initial <= max_candidates                      and max_results <= initial",
                ));
            }
        }
        if self.max_results == 0 || self.max_results > self.max_candidates {
            return Err(Error::validation(
                "max_results must be in 1..=max_candidates",
            ));
        }
        for (name, w) in [
            ("dense_weight", self.dense_weight),
            ("lexical_weight", self.lexical_weight),
            ("recency_weight", self.recency_weight),
            ("min_score", self.min_score),
        ] {
            unit_interval(name, w)?;
        }
        let sum = self.dense_weight + self.lexical_weight + self.recency_weight;
        if (sum - 1.0).abs() > 1e-6 {
            return Err(Error::validation("weights must sum to 1"));
        }
        if !(self.recency_half_life_days.is_finite() && self.recency_half_life_days > 0.0) {
            return Err(Error::validation("recency_half_life_days must be positive"));
        }
        if self.include_beliefs
            && (self.max_belief_results == 0 || self.max_belief_results > self.max_candidates)
        {
            return Err(Error::validation(
                "max_belief_results must be in 1..=max_candidates",
            ));
        }
        Ok(())
    }
}

/// Whether the dense channel took part in a recall, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DenseStatus {
    /// Dense scores came from this embedding space.
    Used { space: String },
    /// The query could not be embedded; scoring fell back to lexical and
    /// recency with weights renormalized to sum to 1.
    Unavailable { space: String, reason: String },
    /// No embedder is configured or the policy's dense weight is zero.
    #[default]
    Disabled,
}

impl DenseStatus {
    pub fn is_used(&self) -> bool {
        matches!(self, Self::Used { .. })
    }
}

/// A scored belief revision in a recall trace. Relevance (`score`) is kept
/// separate from the belief's own confidence; they are different signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallBeliefCandidate {
    pub belief_id: BeliefId,
    pub lineage_id: BeliefId,
    pub rank: u32,
    pub score: f64,
    pub dense_score: Option<f64>,
    pub lexical_score: Option<f64>,
    pub recency_score: Option<f64>,
    pub selected: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallCandidate {
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
    pub rank: u32,
    pub score: f64,
    pub dense_score: Option<f64>,
    pub lexical_score: Option<f64>,
    pub recency_score: Option<f64>,
    pub selected: bool,
    pub reason: String,
    /// Channels that proposed this candidate (empty in traces recorded
    /// before channels existed).
    #[serde(default)]
    pub channels: Vec<CandidateChannel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallTrace {
    pub id: RecallTraceId,
    pub tenant_id: TenantId,
    /// Fully resolved policy (as-of times filled in).
    pub policy: RecallPolicy,
    pub scoring_version: u32,
    pub query_hash: String,
    #[serde(default)]
    pub query_text: Option<String>,
    #[serde(default)]
    pub embedding_model: Option<String>,
    #[serde(default)]
    pub embedding_version: Option<String>,
    pub candidates: Vec<RecallCandidate>,
    #[serde(default)]
    pub belief_candidates: Vec<RecallBeliefCandidate>,
    /// Dense channel status for this recall.
    #[serde(default)]
    pub dense: DenseStatus,
    /// The query vector actually used (dropped with the query text when
    /// `retain_query_text` is false: it encodes the query).
    #[serde(default)]
    pub query_embedding: Option<Vec<f32>>,
    /// Identifies the data the recall saw: transaction as-of time, scoring
    /// version and embedding model. Re-running against the same snapshot
    /// must yield the same selection.
    pub data_snapshot: String,
    #[serde(with = "time::serde::rfc3339")]
    pub executed_at: OffsetDateTime,
    /// Set when the policy's budget was adaptive.
    #[serde(default)]
    pub budget: Option<BudgetDecision>,
}

impl RecallTrace {
    pub fn selected(&self) -> impl Iterator<Item = &RecallCandidate> {
        self.candidates.iter().filter(|c| c.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_valid() {
        RecallPolicy::default().validate().unwrap();
    }

    #[test]
    fn weights_must_sum_to_one() {
        let p = RecallPolicy {
            dense_weight: 0.9,
            ..RecallPolicy::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn partial_policy_json_uses_defaults() {
        let p: RecallPolicy = serde_json::from_str(r#"{"max_results": 3}"#).unwrap();
        assert_eq!(p.max_results, 3);
        assert_eq!(p.allowed_domains.len(), 6);
    }
}
