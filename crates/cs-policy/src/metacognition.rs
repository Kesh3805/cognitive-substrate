//! Signals for the metacognitive policy (M6.1) that need the records
//! themselves.

use std::collections::BTreeSet;

use cs_core::MemoryId;
use cs_memory::{MemoryContent, MemoryRecord, lexical};

use crate::resolve::{single_valued, triple};

/// Pairs of records that state different values for the same subject and
/// predicate (single-valued domains only) while valid at the same time:
/// the evidence disagrees, and nothing has resolved it yet. Each pair is
/// ordered by id, and pairs are sorted.
pub fn contradictions(records: &[&MemoryRecord]) -> Vec<(MemoryId, MemoryId)> {
    let mut out = Vec::new();
    for (i, a) in records.iter().enumerate() {
        if !single_valued(a.domain()) {
            continue;
        }
        let Some(ta) = triple(&a.content) else {
            continue;
        };
        for b in &records[i + 1..] {
            if a.id == b.id || a.domain() != b.domain() {
                continue;
            }
            let Some(tb) = triple(&b.content) else {
                continue;
            };
            if ta.0 == tb.0
                && ta.1 == tb.1
                && ta.2 != tb.2
                && a.temporal.valid_time.overlaps(&b.temporal.valid_time)
            {
                out.push(if a.id < b.id {
                    (a.id, b.id)
                } else {
                    (b.id, a.id)
                });
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// How one memory relates to a question (detector `accounted-terms/1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Every question term is accounted for. `specificity` is the number
    /// of subject terms (0 for unstructured memories): more specific
    /// subjects are preferred as the evidence to answer from.
    Covers { specificity: usize },
    /// The memory is about something the question names (all its subject
    /// terms are in the question) but leaves question terms unaccounted.
    Anchored { unaccounted: usize },
    /// Neither.
    Unrelated,
}

/// A term compared up to one trailing `s` (`stores` = `store`,
/// `depends` = `depend`), never for `ss` endings or terms shorter than 4.
pub fn match_form(term: &str) -> &str {
    if term.len() > 3 && term.ends_with('s') && !term.ends_with("ss") {
        &term[..term.len() - 1]
    } else {
        term
    }
}

/// Conversational framing that asks nothing (version 1). Ignored when
/// accounting for a question. Negations and temporal qualifiers (`not`,
/// `never`, `only`, `previously`, `before`, …) are deliberately absent:
/// they change what is asked, so they must be accounted for.
pub const FRAMING_TERMS: &[&str] = &[
    "can",
    "could",
    "currently",
    "kindly",
    "know",
    "let",
    "me",
    "now",
    "please",
    "show",
    "tell",
    "would",
    "you",
];

fn forms(text: &str) -> BTreeSet<String> {
    lexical::terms(text)
        .iter()
        .map(|t| match_form(t).to_owned())
        .collect()
}

/// A question's terms for accounting: its forms without framing terms, and
/// the framing terms it contained.
pub fn question_terms(question: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    forms(question)
        .into_iter()
        .partition(|t| !FRAMING_TERMS.contains(&t.as_str()))
}

/// `(subject, predicate, object)` as written, for structured memories.
fn fields(c: &MemoryContent) -> Option<(&str, &str, &str)> {
    match c {
        MemoryContent::Semantic(m) => match (&m.subject, &m.predicate, &m.object) {
            (Some(s), Some(p), Some(o)) => Some((s, p, o)),
            _ => None,
        },
        MemoryContent::Relational(m) => Some((&m.subject, &m.predicate, &m.object)),
        MemoryContent::Temporal(m) => Some((&m.subject, &m.predicate, &m.value)),
        _ => None,
    }
}

/// Detector `accounted-terms/1`. Question terms exclude
/// [`FRAMING_TERMS`].
///
/// * A **structured** memory covers the question when all of its subject
///   terms appear in the question and every question term appears in its
///   subject, predicate or object. It is anchored when its subject terms
///   all appear in the question but some question term is unaccounted.
/// * An **unstructured** memory covers the question when it contains every
///   question term. It is never anchored.
///
/// Terms follow the shared lexical contract, compared by [`match_form`].
/// A question without terms is covered by nothing.
pub fn coverage(question: &str, record: &MemoryRecord) -> Coverage {
    let (q, _) = question_terms(question);
    coverage_of(&q, record)
}

/// Question terms a memory leaves unaccounted, sorted.
pub fn unaccounted_terms(q: &BTreeSet<String>, record: &MemoryRecord) -> Vec<String> {
    let accounted: BTreeSet<String> = match fields(&record.content) {
        Some((s, p, o)) => {
            let mut a = forms(s);
            a.extend(forms(p));
            a.extend(forms(o));
            a
        }
        None => forms(&record.content.index_text()),
    };
    q.difference(&accounted).cloned().collect()
}

/// [`coverage`] for already computed question terms.
pub fn coverage_of(q: &BTreeSet<String>, record: &MemoryRecord) -> Coverage {
    if q.is_empty() {
        return Coverage::Unrelated;
    }
    match fields(&record.content) {
        Some((subject, predicate, object)) => {
            let s = forms(subject);
            if s.is_empty() || !s.is_subset(q) {
                return Coverage::Unrelated;
            }
            let mut accounted = s.clone();
            accounted.extend(forms(predicate));
            accounted.extend(forms(object));
            let unaccounted = q.difference(&accounted).count();
            if unaccounted == 0 {
                Coverage::Covers {
                    specificity: s.len(),
                }
            } else {
                Coverage::Anchored { unaccounted }
            }
        }
        None => {
            if q.is_subset(&forms(&record.content.index_text())) {
                Coverage::Covers { specificity: 0 }
            } else {
                Coverage::Unrelated
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use cs_core::{
        ActorRef, Bitemporal, MemoryId, MemoryRevisionId, Provenance, Scope, SourceRef, TenantId,
        TimeInterval,
    };
    use cs_memory::{EpisodicMemory, MemoryStatus, RelationalMemory, SemanticMemory};
    use time::macros::datetime;

    use super::*;

    fn record(content: MemoryContent) -> MemoryRecord {
        let t = datetime!(2026-01-01 0:00 UTC);
        MemoryRecord {
            id: MemoryId::new_v7(),
            revision_id: MemoryRevisionId::new_v7(),
            revision_number: 1,
            scope: Scope::tenant(TenantId::new_v7()),
            content,
            provenance: Provenance {
                source: SourceRef::UserInput { message_id: None },
                actor: ActorRef::System {
                    component: "test".into(),
                },
                observed_at: t,
                recorded_at: t,
                extraction: None,
                parent_evidence: vec![],
                corroborations: vec![],
            },
            temporal: Bitemporal {
                valid_time: TimeInterval::from(t),
                transaction_time: TimeInterval::from(t),
            },
            status: MemoryStatus::Active,
            status_reason: None,
            supersedes: None,
            superseded_by: None,
            created_at: t,
            expires_at: None,
        }
    }

    fn fact(s: &str, p: &str, o: &str) -> MemoryRecord {
        record(MemoryContent::Semantic(SemanticMemory {
            proposition: format!("{s} {p} {o}"),
            subject: Some(s.into()),
            predicate: Some(p.into()),
            object: Some(o.into()),
        }))
    }

    #[test]
    fn subject_and_aspect_must_both_be_accounted() {
        let owner = fact("cobalt forge", "is owned by the team", "payments");
        // A different subject that shares a word: unrelated, not covering.
        assert_eq!(
            coverage("polar forge is owned by which team", &owner),
            Coverage::Unrelated
        );
        assert_eq!(
            coverage("cobalt forge is owned by which team", &owner),
            Coverage::Covers { specificity: 2 }
        );
        // Right subject, unaccounted aspect: anchored.
        assert_eq!(
            coverage("who maintains cobalt forge", &owner),
            Coverage::Anchored { unaccounted: 1 }
        );
        // One trailing s is tolerated.
        let dep = record(MemoryContent::Relational(RelationalMemory {
            subject: "nova prism".into(),
            predicate: "depends on".into(),
            object: "orion vault".into(),
            directed: true,
        }));
        assert_eq!(
            coverage("what does nova prism depend on", &dep),
            Coverage::Covers { specificity: 2 }
        );
        // A twin's longer subject is not in the question.
        let twin = fact("cobalt forge replica", "is owned by the team", "search");
        assert_eq!(
            coverage("cobalt forge is owned by which team", &twin),
            Coverage::Unrelated
        );
        // Asked about the twin, the base fact leaves "replica" unaccounted.
        assert_eq!(
            coverage("cobalt forge replica is owned by which team", &owner),
            Coverage::Anchored { unaccounted: 1 }
        );
        assert_eq!(
            coverage("cobalt forge replica is owned by which team", &twin),
            Coverage::Covers { specificity: 3 }
        );
        assert_eq!(match_form("class"), "class");
        assert_eq!(match_form("maintains"), "maintain");
        assert_eq!(match_form("its"), "its");
        assert_eq!(match_form("bus"), "bus");
    }

    #[test]
    fn framing_is_ignored_but_qualifiers_are_not() {
        let owner = fact("cobalt forge", "is owned by the team", "payments");
        assert_eq!(
            coverage(
                "could you please tell me which team cobalt forge is currently owned by",
                &owner
            ),
            Coverage::Covers { specificity: 2 }
        );
        for q in [
            "which team previously owned cobalt forge",
            "cobalt forge is not owned by which team",
            "cobalt forge was never owned by which team",
            "which team owned cobalt forge before",
            "cobalt forge is only owned by which team",
            "cobalt forge is possibly owned by which team",
        ] {
            assert!(
                matches!(coverage(q, &owner), Coverage::Anchored { .. }),
                "{q}"
            );
        }
        let (terms, ignored) = question_terms("please tell me who owns cobalt forge");
        assert!(ignored.contains("please") && ignored.contains("tell"));
        assert!(terms.contains("cobalt") && !terms.contains("please"));
    }

    #[test]
    fn subjects_never_collide() {
        let a = fact("cobalt forge", "is owned by the team", "payments");
        // Swapped subject and object: the edge is about payments, which the
        // question does not name, so it is not evidence here (reverse
        // edges are a known limit of the detector).
        let b = record(MemoryContent::Relational(RelationalMemory {
            subject: "payments".into(),
            predicate: "owns".into(),
            object: "cobalt forge".into(),
            directed: true,
        }));
        assert_eq!(coverage("who owns cobalt forge", &b), Coverage::Unrelated);
        assert_eq!(
            coverage("what does payments own", &b),
            Coverage::Covers { specificity: 1 },
        );
        // A subject sharing one token is unrelated, whatever the predicate.
        assert_eq!(
            coverage("forge is owned by which team", &a),
            Coverage::Unrelated
        );
        // Same subject, another predicate: anchored, never covering.
        let region = fact("cobalt forge", "runs in region", "eu-west-1");
        assert!(matches!(
            coverage("cobalt forge is owned by which team", &region),
            Coverage::Anchored { .. }
        ));
    }

    #[test]
    fn unstructured_memories_need_every_term() {
        let note = record(MemoryContent::Episodic(EpisodicMemory {
            description: "standup note 4: discussed the migration for the polar forge".into(),
            event_id: None,
            participants: Vec::new(),
            subject: None,
            predicate: None,
            object: None,
        }));
        assert_eq!(
            coverage("polar forge is owned by which team", &note),
            Coverage::Unrelated
        );
        assert_eq!(
            coverage("polar forge migration", &note),
            Coverage::Covers { specificity: 0 }
        );
        assert_eq!(coverage("the", &note), Coverage::Unrelated);
    }
}
