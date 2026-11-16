//! Metacognitive policy v1: answer, investigate or abstain (M6.1).
//!
//! The decision is a pure function of signals the recall already has. It
//! claims nothing about the probability of being right: a score margin is
//! a signal, not a confidence.

use cs_core::{Error, MemoryId, Result};
use serde::{Deserialize, Serialize};

pub const METACOGNITION_VERSION: u32 = 1;

/// What a caller should do with a recall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The selected evidence covers the question, is validated and agrees.
    Answer,
    /// Evidence exists but cannot be relied on as it stands.
    Investigate,
    /// No selected evidence covers the question.
    Abstain,
}

impl Decision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Investigate => "investigate",
            Self::Abstain => "abstain",
        }
    }
}

/// Why the policy did not simply answer, in the order the rules apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Nothing was selected.
    NoEvidence,
    /// Nothing selected covers the question (detector `idf-coverage/1`).
    NotCovered,
    /// Every covering memory is an unvalidated candidate.
    OnlyUnvalidated,
    /// Covering, validated memories state different values for the same
    /// subject and predicate, and are valid at the same time.
    Contradiction,
    /// The best covering memory does not lead the next by `min_margin`.
    NarrowMargin,
}

/// The policy's parameters. Recorded in the trace with the decision.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetacognitivePolicy {
    /// Investigate when the best covering memory leads the next selected
    /// memory by less than this score. `None` disables the rule.
    #[serde(default)]
    pub min_margin: Option<f64>,
}

impl MetacognitivePolicy {
    pub fn validate(&self) -> Result<()> {
        match self.min_margin {
            Some(m) if !(m.is_finite() && (0.0..=1.0).contains(&m)) => Err(Error::validation(
                "metacognition.min_margin must be in 0..=1",
            )),
            _ => Ok(()),
        }
    }
}

/// What the policy decides from. Computed by the engine over the selected
/// memories of one recall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    /// Memories selected by the recall.
    pub selected: u32,
    /// Selected memories that cover the question, active or candidate.
    pub covering_active: u32,
    pub covering_candidate: u32,
    /// Pairs of covering, active memories that contradict each other.
    pub contradictions: Vec<(MemoryId, MemoryId)>,
    /// Score of the best covering memory, and its lead over the best other
    /// selected memory (the full score when it is the only one).
    pub top_score: Option<f64>,
    pub margin: Option<f64>,
}

/// A recorded decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetacognitiveDecision {
    pub version: u32,
    pub policy: MetacognitivePolicy,
    pub decision: Decision,
    /// Every rule that fired; empty when answering.
    pub reasons: Vec<Reason>,
    pub signals: Signals,
}

/// Rules, in order:
///
/// 1. nothing selected → abstain (`no_evidence`);
/// 2. nothing covering → abstain (`not_covered`);
/// 3. only candidates cover → investigate (`only_unvalidated`);
/// 4. covering active memories contradict → investigate (`contradiction`);
/// 5. margin below `min_margin` → investigate (`narrow_margin`);
/// 6. otherwise answer.
///
/// Every rule that applies is listed; the first decides.
pub fn decide(policy: MetacognitivePolicy, signals: Signals) -> MetacognitiveDecision {
    let mut reasons = Vec::new();
    if signals.selected == 0 {
        reasons.push(Reason::NoEvidence);
    }
    let covering = signals.covering_active + signals.covering_candidate;
    if signals.selected > 0 && covering == 0 {
        reasons.push(Reason::NotCovered);
    }
    if covering > 0 && signals.covering_active == 0 {
        reasons.push(Reason::OnlyUnvalidated);
    }
    if !signals.contradictions.is_empty() {
        reasons.push(Reason::Contradiction);
    }
    if let (Some(min), Some(margin)) = (policy.min_margin, signals.margin)
        && covering > 0
        && margin < min
    {
        reasons.push(Reason::NarrowMargin);
    }
    let decision = match reasons.first() {
        None => Decision::Answer,
        Some(Reason::NoEvidence | Reason::NotCovered) => Decision::Abstain,
        Some(_) => Decision::Investigate,
    };
    MetacognitiveDecision {
        version: METACOGNITION_VERSION,
        policy,
        decision,
        reasons,
        signals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(selected: u32, active: u32, candidate: u32) -> Signals {
        Signals {
            selected,
            covering_active: active,
            covering_candidate: candidate,
            contradictions: Vec::new(),
            top_score: Some(0.8),
            margin: Some(0.3),
        }
    }

    #[test]
    fn rules_apply_in_order() {
        let p = MetacognitivePolicy { min_margin: None };
        let d = decide(p, signals(0, 0, 0));
        assert_eq!(
            (d.decision, d.reasons.clone()),
            (Decision::Abstain, vec![Reason::NoEvidence])
        );
        assert_eq!(decide(p, signals(3, 0, 0)).decision, Decision::Abstain);
        let d = decide(p, signals(3, 0, 2));
        assert_eq!(
            (d.decision, d.reasons),
            (Decision::Investigate, vec![Reason::OnlyUnvalidated])
        );
        assert_eq!(decide(p, signals(3, 1, 2)).decision, Decision::Answer);

        let mut s = signals(3, 2, 0);
        s.contradictions = vec![(MemoryId::new_v7(), MemoryId::new_v7())];
        let d = decide(p, s);
        assert_eq!(
            (d.decision, d.reasons),
            (Decision::Investigate, vec![Reason::Contradiction])
        );
    }

    #[test]
    fn margin_rule_is_opt_in() {
        let mut s = signals(3, 1, 0);
        s.margin = Some(0.01);
        assert_eq!(
            decide(MetacognitivePolicy { min_margin: None }, s.clone()).decision,
            Decision::Answer
        );
        let d = decide(
            MetacognitivePolicy {
                min_margin: Some(0.05),
            },
            s,
        );
        assert_eq!(
            (d.decision, d.reasons),
            (Decision::Investigate, vec![Reason::NarrowMargin])
        );
        assert!(
            MetacognitivePolicy {
                min_margin: Some(1.5)
            }
            .validate()
            .is_err()
        );
        assert!(
            MetacognitivePolicy {
                min_margin: Some(f64::NAN)
            }
            .validate()
            .is_err()
        );
    }
}
