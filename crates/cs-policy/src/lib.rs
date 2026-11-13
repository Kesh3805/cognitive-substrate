//! Pure, deterministic policy logic.
//!
//! Nothing in this crate performs I/O, reads a clock or calls a model. Given
//! the same inputs it always produces the same outputs, which is what makes
//! recall traces replayable.

pub mod lexical;
pub mod scoring;

pub use scoring::{ScoringInput, cosine_similarity, query_hash, score_candidates};
