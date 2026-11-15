//! Information needs and knowledge gaps (M5.4).
//!
//! A weak recall is not a knowledge gap. Three states are kept apart:
//!
//! * **Retrieval failure**: one recall found no qualifying evidence. The
//!   cause is unknown; it may be the phrasing.
//! * **Unresolved need**: the same need recurred in distinct sessions, or
//!   was asked explicitly. This is demand, not proof of absence.
//! * **Gap**: an explicit validation searched the caller's whole eligible
//!   substrate *completely* and found nothing. Until experiment 008 says
//!   otherwise, that produces a *candidate* gap for a person to confirm.
//!
//! Demand (how often a need recurs) and availability (whether evidence
//! exists) are separate fields, and only availability decides the status.

use cs_core::{Error, InformationNeedId, MemoryId, MemoryRevisionId, RecallTraceId, Result, Scope};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::lexical;

/// Version of the lexical intent signature.
pub const SIGNATURE_VERSION: u32 = 1;
/// Version of the detector deciding what counts as evidence.
pub const DETECTOR_VERSION: &str = "idf-coverage/1";
/// Share of the query's IDF weight a candidate's matched terms must carry.
pub const MIN_COVERAGE: f64 = 0.5;
/// Matched terms a candidate needs when the query has two or more terms,
/// so that one common word cannot qualify.
pub const MIN_MATCHED_TERMS: usize = 2;
pub const MAX_QUESTION: usize = 2_000;

/// A **lexical** intent signature: the query's distinct terms under
/// tokenizer v1. It is deliberately not called semantic identity: it does
/// not distinguish "who owns X" from "what does X own", and it drops
/// negation and temporal qualifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub version: u32,
    pub terms: Vec<String>,
}

impl Signature {
    /// `None` when the text has no terms.
    pub fn of(text: &str) -> Option<Self> {
        let terms = lexical::terms(text);
        (!terms.is_empty()).then_some(Self {
            version: SIGNATURE_VERSION,
            terms,
        })
    }

    /// The stored key: `v1:term term …`.
    pub fn key(&self) -> String {
        format!("v{}:{}", self.version, self.terms.join(" "))
    }
}

/// Term statistics of the caller's eligible population, used to weight
/// query terms: rare terms (entities) count for more than framing words
/// ("status", "current"), and a term that appears nowhere counts most.
#[derive(Debug, Clone, PartialEq)]
pub struct TermStats {
    /// Eligible revisions.
    pub documents: u64,
    /// Document frequency of each signature term, in signature order.
    pub frequencies: Vec<u64>,
}

impl TermStats {
    /// Smoothed IDF, `ln((N + 1) / (df + 1))`, per signature term: a term
    /// in every eligible revision weighs nothing, an absent term the most.
    /// If every term is ubiquitous (all weights zero), every term weighs
    /// one, so coverage falls back to plain term coverage.
    pub fn weights(&self) -> Vec<f64> {
        let n = self.documents as f64;
        let w: Vec<f64> = self
            .frequencies
            .iter()
            .map(|df| ((n + 1.0) / (*df as f64 + 1.0)).ln().max(0.0))
            .collect();
        if w.iter().all(|x| *x == 0.0) {
            vec![1.0; w.len()]
        } else {
            w
        }
    }
}

/// Does a candidate with these (sorted, distinct) terms count as evidence
/// for `signature` under detector `idf-coverage/1`? Its matched terms must
/// carry at least half the query's IDF weight, and it must match at least
/// two terms when the query has two or more.
pub fn qualifies(signature: &Signature, stats: &TermStats, candidate_terms: &[String]) -> bool {
    let weights = stats.weights();
    let total: f64 = weights.iter().sum();
    let mut matched = 0usize;
    let mut covered = 0.0;
    for (term, w) in signature.terms.iter().zip(&weights) {
        if candidate_terms.binary_search(term).is_ok() {
            matched += 1;
            covered += w;
        }
    }
    let needed = signature.terms.len().min(MIN_MATCHED_TERMS);
    matched >= needed.max(1) && total > 0.0 && covered >= MIN_COVERAGE * total
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedStatus {
    RetrievalFailure,
    Unresolved,
    /// Evidence exists, but only in unvalidated (candidate) memories that
    /// recall does not use: promote or reject them.
    PendingPromotion,
    /// Validation was complete and found nothing; awaiting confirmation.
    CandidateGap,
    /// Confirmed by a person (or, if experiment 008 permits it, by
    /// automatic validation).
    ValidatedGap,
    Satisfied,
    Dismissed,
}

impl NeedStatus {
    pub const ALL: [Self; 7] = [
        Self::RetrievalFailure,
        Self::Unresolved,
        Self::PendingPromotion,
        Self::CandidateGap,
        Self::ValidatedGap,
        Self::Satisfied,
        Self::Dismissed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrievalFailure => "retrieval_failure",
            Self::Unresolved => "unresolved",
            Self::PendingPromotion => "pending_promotion",
            Self::CandidateGap => "candidate_gap",
            Self::ValidatedGap => "validated_gap",
            Self::Satisfied => "satisfied",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == v)
    }

    /// Still worth acquiring knowledge for.
    pub const fn is_open(self) -> bool {
        !matches!(self, Self::Satisfied | Self::Dismissed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedOrigin {
    /// Recorded from recalls that opted in.
    Observed,
    /// Asked explicitly.
    Explicit,
}

/// One piece of evidence: an exact revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GapValidationOutcome {
    /// The whole eligible substrate was searched and nothing qualified.
    CompleteNoEvidence,
    /// Qualifying evidence exists. `unvalidated_only` when every piece of
    /// it is a candidate (unvalidated) memory.
    CompleteEvidenceFound {
        evidence: Vec<EvidenceRef>,
        unvalidated_only: bool,
    },
    /// The search could not establish absence (budget reached, dense
    /// channel unavailable, …). Never produces a gap.
    Incomplete { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapValidation {
    pub outcome: GapValidationOutcome,
    pub detector: String,
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// Eligible revisions sharing at least one term with the signature.
    pub candidates_examined: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InformationNeed {
    pub id: InformationNeedId,
    /// The identity partition's scope (no conversation).
    pub scope: Scope,
    pub partition: String,
    pub signature: Signature,
    /// SHA-256 of the first query or question (never the text itself).
    pub query_hash: String,
    /// Only for explicit needs.
    #[serde(default)]
    pub question: Option<String>,
    pub origin: NeedOrigin,
    pub status: NeedStatus,
    // ---- demand -----------------------------------------------------------
    /// Distinct sessions (conversation, else recall trace) that hit it.
    pub sessions: u32,
    pub occurrences: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
    #[serde(default)]
    pub last_trace: Option<RecallTraceId>,
    // ---- availability -----------------------------------------------------
    #[serde(default)]
    pub validation: Option<GapValidation>,
    #[serde(default)]
    pub resolved_by: Vec<EvidenceRef>,
    // ---- acquisition inputs (1..=5) ---------------------------------------
    pub impact: u8,
    pub cost: u8,
    pub version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl InformationNeed {
    /// Apply one more miss. Demand grows; a need recurring in a second
    /// distinct session becomes unresolved. Returns whether the status
    /// changed.
    pub fn observe(&mut self, new_session: bool, trace: RecallTraceId, at: OffsetDateTime) -> bool {
        self.occurrences += 1;
        if new_session {
            self.sessions += 1;
        }
        self.last_seen = self.last_seen.max(at);
        self.last_trace = Some(trace);
        self.updated_at = at;
        self.version += 1;
        // A satisfied need that is missed again is open again: the
        // evidence that satisfied it may be gone.
        let next = match self.status {
            NeedStatus::RetrievalFailure | NeedStatus::Satisfied if self.sessions >= 2 => {
                NeedStatus::Unresolved
            }
            NeedStatus::Satisfied => NeedStatus::RetrievalFailure,
            s => s,
        };
        let changed = next != self.status;
        self.status = next;
        changed
    }

    pub fn validate_inputs(&self) -> Result<()> {
        if !(1..=5).contains(&self.impact) || !(1..=5).contains(&self.cost) {
            return Err(Error::validation("impact and cost must be in 1..=5"));
        }
        if let Some(q) = &self.question
            && (q.trim().is_empty() || q.chars().count() > MAX_QUESTION)
        {
            return Err(Error::validation(format!(
                "question must be 1..={MAX_QUESTION} characters"
            )));
        }
        Ok(())
    }
}

/// Acquisition priority v1: a ranking heuristic, **not** information gain.
///
/// `P = ln(1 + sessions) × 0.5^(age_days / 14) × status weight × impact
/// ÷ cost`, evaluated at an explicit reference time. Status weights:
/// validated gap 1.0, candidate gap 0.75, unresolved 0.5, pending
/// promotion 0.25, retrieval failure 0.25, closed 0. Impact and cost are in
/// 1..=5. Every component is returned so the ranking can be inspected.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AcquisitionPriority {
    pub version: u32,
    pub sessions: f64,
    pub recency: f64,
    pub status_weight: f64,
    pub impact: f64,
    pub cost: f64,
    pub priority: f64,
}

pub const PRIORITY_HALF_LIFE_DAYS: f64 = 14.0;

pub fn acquisition_priority(n: &InformationNeed, now: OffsetDateTime) -> AcquisitionPriority {
    let sessions = (1.0 + f64::from(n.sessions)).ln();
    let age_days = ((now - n.last_seen).as_seconds_f64() / 86_400.0).max(0.0);
    let recency = 0.5f64.powf(age_days / PRIORITY_HALF_LIFE_DAYS);
    let status_weight = match n.status {
        NeedStatus::ValidatedGap => 1.0,
        NeedStatus::CandidateGap => 0.75,
        NeedStatus::Unresolved => 0.5,
        NeedStatus::RetrievalFailure | NeedStatus::PendingPromotion => 0.25,
        NeedStatus::Satisfied | NeedStatus::Dismissed => 0.0,
    };
    let (impact, cost) = (f64::from(n.impact), f64::from(n.cost));
    AcquisitionPriority {
        version: 1,
        sessions,
        recency,
        status_weight,
        impact,
        cost,
        priority: sessions * recency * status_weight * impact / cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_core::TenantId;
    use time::macros::datetime;

    fn need(status: NeedStatus, sessions: u32) -> InformationNeed {
        let t = datetime!(2026-01-01 0:00 UTC);
        InformationNeed {
            id: InformationNeedId::new_v7(),
            scope: Scope::tenant(TenantId::new_v7()),
            partition: "p".into(),
            signature: Signature::of("who owns billing").unwrap(),
            query_hash: "h".into(),
            question: None,
            origin: NeedOrigin::Observed,
            status,
            sessions,
            occurrences: sessions,
            first_seen: t,
            last_seen: t,
            last_trace: None,
            validation: None,
            resolved_by: Vec::new(),
            impact: 1,
            cost: 1,
            version: 1,
            created_at: t,
            updated_at: t,
        }
    }

    #[test]
    fn signatures_are_lexical_and_versioned() {
        let a = Signature::of("Who owns the billing service?").unwrap();
        let b = Signature::of("billing service: who owns").unwrap();
        assert_eq!(a.key(), b.key());
        assert_eq!(a.key(), "v1:billing owns service");
        assert!(Signature::of("the a of").is_none());
    }

    #[test]
    fn rare_terms_dominate_coverage() {
        let t = |x: &str| crate::lexical::terms(x);
        // "current deployment status of helios": helios appears nowhere.
        let s = Signature::of("current deployment status helios").unwrap();
        // Signature order: current, deployment, helios, status.
        let stats = TermStats {
            documents: 1_000,
            frequencies: vec![300, 200, 0, 400],
        };
        assert!(
            !qualifies(&s, &stats, &t("current deployment status is green")),
            "framing words alone must not hide a gap"
        );
        assert!(qualifies(&s, &stats, &t("helios deployment paused")));
        // A verbose query is still answered by a short fact naming the
        // rare terms.
        let v = Signature::of("please tell me which datastore the orion ledger uses now").unwrap();
        let stats = TermStats {
            documents: 1_000,
            frequencies: v
                .terms
                .iter()
                .map(|w| {
                    if w == "orion" || w == "ledger" {
                        3
                    } else {
                        500
                    }
                })
                .collect(),
        };
        assert!(qualifies(
            &v,
            &stats,
            &t("orion ledger stores data in postgres")
        ));
        let single = Signature::of("billing").unwrap();
        let one = TermStats {
            documents: 10,
            frequencies: vec![2],
        };
        assert!(qualifies(&single, &one, &t("billing exports")));
        // A term in every document still matches when nothing is rarer.
        let all = TermStats {
            documents: 10,
            frequencies: vec![10],
        };
        assert!(qualifies(&single, &all, &t("billing exports")));
    }

    #[test]
    fn small_scopes_do_not_let_framing_words_tie_with_entities() {
        let t = |x: &str| crate::lexical::terms(x);
        // Seven documents; six say "current deployment status".
        let s = Signature::of("current deployment status helios").unwrap();
        let stats = TermStats {
            documents: 7,
            frequencies: vec![6, 6, 0, 6],
        };
        assert!(!qualifies(
            &s,
            &stats,
            &t("the current deployment status is green")
        ));
    }

    #[test]
    fn recurrence_in_a_second_session_makes_a_need_unresolved() {
        let mut n = need(NeedStatus::RetrievalFailure, 1);
        let at = datetime!(2026-01-02 0:00 UTC);
        assert!(!n.observe(false, RecallTraceId::new_v7(), at));
        assert_eq!(
            (n.status, n.sessions, n.occurrences),
            (NeedStatus::RetrievalFailure, 1, 2)
        );
        assert!(n.observe(true, RecallTraceId::new_v7(), at));
        assert_eq!(n.status, NeedStatus::Unresolved);
        // Demand never upgrades a need to a gap.
        for _ in 0..10 {
            n.observe(true, RecallTraceId::new_v7(), at);
        }
        assert_eq!(n.status, NeedStatus::Unresolved);
    }

    #[test]
    fn priority_saturates_demand_and_decays() {
        let now = datetime!(2026-01-15 0:00 UTC);
        let n = need(NeedStatus::Unresolved, 1_000);
        let p = acquisition_priority(&n, now);
        assert!((p.sessions - 1001f64.ln()).abs() < 1e-9, "log-saturated");
        assert!((p.recency - 0.5).abs() < 1e-9, "14 days old");
        assert!((p.priority - 1001f64.ln() * 0.5 * 0.5).abs() < 1e-9);
        assert_eq!(
            acquisition_priority(&need(NeedStatus::Satisfied, 5), now).priority,
            0.0
        );
    }
}
