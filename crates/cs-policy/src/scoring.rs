//! Recall scoring, version 1.
//!
//! `S(d, q) = w_d * D(d, q) + w_l * L(d, q) + w_r * R(d)`
//!
//! * `D` — cosine similarity mapped from `[-1, 1]` to `[0, 1]`; 0 without an
//!   embedding from the active model.
//! * `L` — BM25 over the filtered candidate set, normalized by its maximum.
//! * `R` — `0.5 ^ (age_days / half_life_days)`, age measured from when the
//!   information was observed to the as-of transaction time.
//!
//! Ranking is `(score DESC, memory_id ASC, revision_id ASC)`. Scoring is exact
//! over the candidate set; no approximate nearest-neighbour search is used,
//! so the result is fully deterministic. This is a baseline, not a claim of
//! optimality.

use std::cmp::Ordering;

use cs_core::{BeliefId, MemoryId, MemoryRevisionId};
use cs_memory::{RecallBeliefCandidate, RecallCandidate, RecallPolicy};
use uuid::Uuid;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::lexical::bm25_normalized;

#[derive(Debug, Clone)]
pub struct ScoringInput<'a> {
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
    pub text: &'a str,
    pub embedding: Option<&'a [f32]>,
    pub observed_at: OffsetDateTime,
}

pub fn query_hash(query: &str) -> String {
    let digest = Sha256::digest(query.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some((dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0))
}

/// Rough, deterministic token estimate used for context budgets.
pub fn estimate_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX)
}

/// One rankable item: a stable two-part key (identity, revision), the text
/// to score, an optional embedding and when it was observed.
#[derive(Debug, Clone)]
pub struct RankItem<'a> {
    pub key: (Uuid, Uuid),
    pub text: &'a str,
    pub embedding: Option<&'a [f32]>,
    pub observed_at: OffsetDateTime,
}

/// A ranked item with its component scores and selection decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub key: (Uuid, Uuid),
    pub rank: u32,
    pub score: f64,
    pub dense: Option<f64>,
    pub lexical: f64,
    pub recency: f64,
    pub selected: bool,
    pub reason: &'static str,
}

/// Selection limits applied while walking the ranking.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub min_score: f64,
    pub max_results: u32,
    pub max_context_tokens: Option<u32>,
}

/// Score and rank items under the policy's weights; select under `limits`.
/// Ranking is `(score DESC, key ASC)`, which is total and deterministic.
pub fn rank_items(
    policy: &RecallPolicy,
    limits: Limits,
    query: &str,
    query_embedding: Option<&[f32]>,
    as_of: OffsetDateTime,
    items: &[RankItem<'_>],
) -> Vec<Ranked> {
    let texts: Vec<&str> = items.iter().map(|i| i.text).collect();
    let lexical = bm25_normalized(query, &texts);

    let mut scored: Vec<(Ranked, u32)> = items
        .iter()
        .zip(lexical)
        .map(|(item, lex)| {
            let dense = match (query_embedding, item.embedding) {
                (Some(q), Some(e)) => cosine_similarity(q, e).map(|c| (c + 1.0) / 2.0),
                _ => None,
            };
            let age_days = ((as_of - item.observed_at).as_seconds_f64() / 86_400.0).max(0.0);
            let recency = 0.5_f64.powf(age_days / policy.recency_half_life_days);
            let score = policy.dense_weight * dense.unwrap_or(0.0)
                + policy.lexical_weight * lex
                + policy.recency_weight * recency;
            (
                Ranked {
                    key: item.key,
                    rank: 0,
                    score,
                    dense,
                    lexical: lex,
                    recency,
                    selected: false,
                    reason: "",
                },
                estimate_tokens(item.text),
            )
        })
        .collect();

    scored.sort_by(|(a, _), (b, _)| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let mut selected_count = 0_u32;
    let mut tokens_used = 0_u32;
    scored
        .into_iter()
        .enumerate()
        .map(|(idx, (mut r, tokens))| {
            let (selected, reason) = if r.score < limits.min_score {
                (false, "below_min_score")
            } else if selected_count >= limits.max_results {
                (false, "max_results_reached")
            } else if limits
                .max_context_tokens
                .is_some_and(|budget| tokens_used.saturating_add(tokens) > budget)
            {
                (false, "context_budget_exceeded")
            } else {
                selected_count += 1;
                tokens_used = tokens_used.saturating_add(tokens);
                (true, "selected")
            };
            r.rank = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            r.selected = selected;
            r.reason = reason;
            r
        })
        .collect()
}

/// Score, rank and select memory candidates under `policy`.
///
/// `as_of` must be the resolved as-of transaction time (never "now" read
/// inside this function).
pub fn score_candidates(
    policy: &RecallPolicy,
    query: &str,
    query_embedding: Option<&[f32]>,
    as_of: OffsetDateTime,
    inputs: &[ScoringInput<'_>],
) -> Vec<RecallCandidate> {
    let items: Vec<RankItem<'_>> = inputs
        .iter()
        .map(|i| RankItem {
            key: (i.memory_id.0, i.revision_id.0),
            text: i.text,
            embedding: i.embedding,
            observed_at: i.observed_at,
        })
        .collect();
    let limits = Limits {
        min_score: policy.min_score,
        max_results: policy.max_results,
        max_context_tokens: policy.max_context_tokens,
    };
    rank_items(policy, limits, query, query_embedding, as_of, &items)
        .into_iter()
        .map(|r| RecallCandidate {
            memory_id: MemoryId(r.key.0),
            revision_id: MemoryRevisionId(r.key.1),
            rank: r.rank,
            score: r.score,
            dense_score: r.dense,
            lexical_score: Some(r.lexical),
            recency_score: Some(r.recency),
            selected: r.selected,
            reason: r.reason.to_owned(),
        })
        .collect()
}

/// A belief revision to score: `key` is `(lineage_id, revision_id)`.
#[derive(Debug, Clone)]
pub struct BeliefScoringInput<'a> {
    pub lineage_id: BeliefId,
    pub belief_id: BeliefId,
    pub text: &'a str,
    pub embedding: Option<&'a [f32]>,
    pub observed_at: OffsetDateTime,
}

/// Score beliefs with the same function as memories. Relevance only:
/// belief confidence is deliberately *not* blended in.
pub fn score_belief_candidates(
    policy: &RecallPolicy,
    query: &str,
    query_embedding: Option<&[f32]>,
    as_of: OffsetDateTime,
    inputs: &[BeliefScoringInput<'_>],
) -> Vec<RecallBeliefCandidate> {
    let items: Vec<RankItem<'_>> = inputs
        .iter()
        .map(|i| RankItem {
            key: (i.lineage_id.0, i.belief_id.0),
            text: i.text,
            embedding: i.embedding,
            observed_at: i.observed_at,
        })
        .collect();
    let limits = Limits {
        min_score: policy.min_score,
        max_results: policy.max_belief_results,
        max_context_tokens: None,
    };
    rank_items(policy, limits, query, query_embedding, as_of, &items)
        .into_iter()
        .map(|r| RecallBeliefCandidate {
            lineage_id: BeliefId(r.key.0),
            belief_id: BeliefId(r.key.1),
            rank: r.rank,
            score: r.score,
            dense_score: r.dense,
            lexical_score: Some(r.lexical),
            recency_score: Some(r.recency),
            selected: r.selected,
            reason: r.reason.to_owned(),
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use time::{Duration, macros::datetime};
    use uuid::Uuid;

    use super::*;

    fn input(n: u128, text: &str, age_days: i64) -> ScoringInput<'_> {
        ScoringInput {
            memory_id: MemoryId(Uuid::from_u128(n)),
            revision_id: MemoryRevisionId(Uuid::from_u128(n)),
            text,
            embedding: None,
            observed_at: datetime!(2026-11-01 0:00 UTC) - Duration::days(age_days),
        }
    }

    fn policy() -> RecallPolicy {
        RecallPolicy {
            dense_weight: 0.0,
            lexical_weight: 0.7,
            recency_weight: 0.3,
            ..RecallPolicy::default()
        }
    }

    #[test]
    fn lexical_match_outranks_noise() {
        let inputs = [
            input(1, "the weather is nice", 0),
            input(2, "postgres stores memories", 0),
        ];
        let out = score_candidates(
            &policy(),
            "postgres",
            None,
            datetime!(2026-11-01 0:00 UTC),
            &inputs,
        );
        assert_eq!(out[0].memory_id, MemoryId(Uuid::from_u128(2)));
        assert_eq!(out[0].rank, 1);
    }

    #[test]
    fn ties_break_on_memory_id() {
        let inputs = [input(9, "same text", 0), input(3, "same text", 0)];
        let out = score_candidates(
            &policy(),
            "same",
            None,
            datetime!(2026-11-01 0:00 UTC),
            &inputs,
        );
        assert_eq!(out[0].memory_id, MemoryId(Uuid::from_u128(3)));
    }

    #[test]
    fn deterministic_across_input_order() {
        let a = [
            input(1, "alpha beta", 3),
            input(2, "beta gamma", 1),
            input(3, "gamma", 9),
        ];
        let b = [a[2].clone(), a[0].clone(), a[1].clone()];
        let now = datetime!(2026-11-01 0:00 UTC);
        assert_eq!(
            score_candidates(&policy(), "beta", None, now, &a),
            score_candidates(&policy(), "beta", None, now, &b)
        );
    }

    #[test]
    fn recency_halves_at_half_life() {
        let p = policy();
        let inputs = [input(1, "x", 30)];
        let out = score_candidates(&p, "zzz", None, datetime!(2026-11-01 0:00 UTC), &inputs);
        assert!((out[0].recency_score.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn respects_max_results_and_min_score() {
        let p = RecallPolicy {
            max_results: 1,
            min_score: 0.5,
            ..policy()
        };
        let inputs = [
            input(1, "rust", 0),
            input(2, "rust rust", 0),
            input(3, "python", 1000),
        ];
        let out = score_candidates(&p, "rust", None, datetime!(2026-11-01 0:00 UTC), &inputs);
        assert_eq!(out.iter().filter(|c| c.selected).count(), 1);
        assert_eq!(out.last().unwrap().reason, "below_min_score");
    }

    #[test]
    fn context_budget_is_enforced() {
        let p = RecallPolicy {
            max_context_tokens: Some(3),
            ..policy()
        };
        let long = "rust ".repeat(20);
        let inputs = [input(1, &long, 0), input(2, "rust", 0)];
        let out = score_candidates(&p, "rust", None, datetime!(2026-11-01 0:00 UTC), &inputs);
        let long_c = out
            .iter()
            .find(|c| c.memory_id == MemoryId(Uuid::from_u128(1)))
            .unwrap();
        assert_eq!(long_c.reason, "context_budget_exceeded");
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-12);
        assert!(cosine_similarity(&[1.0], &[1.0, 2.0]).is_none());
    }
}
