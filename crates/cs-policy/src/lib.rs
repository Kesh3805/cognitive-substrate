//! Pure, deterministic policy logic.
//!
//! Nothing in this crate performs I/O, reads a clock or calls a model. Given
//! the same inputs it always produces the same outputs, which is what makes
//! recall traces replayable and debt reports auditable.

pub mod debt;
pub mod lexical;
pub mod resolve;
pub mod scoring;

pub use debt::{
    CognitiveDebtReport, DebtCategory, DebtCategoryScore, DebtConfig, DebtInputs, DebtItem,
    evaluate_debt,
};
pub use resolve::{
    CalibratedSpace, ProposalDraft, ResolutionConfig, detect_beliefs, detect_memory,
};
pub use scoring::{
    BeliefScoringInput, ScoringInput, cosine_similarity, query_hash, score_belief_candidates,
    score_candidates,
};
