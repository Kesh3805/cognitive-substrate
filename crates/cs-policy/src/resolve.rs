//! Deterministic resolution detection.
//!
//! Detectors *propose*; they never mutate. Each finding names the exact
//! revisions it was computed against and the signals that matched.
//!
//! Similarity is not identity: an embedding match alone can only produce a
//! duplicate *candidate*, never a contradiction, and never applies itself.

use std::collections::BTreeMap;

use cs_core::Scope;
use cs_memory::{
    Belief, MemoryContent, MemoryDomain, MemoryRecord, MemoryStatus, RecordVersion, ResolutionKind,
    ResolutionOperation, ResolutionSignal, belief::normalize,
};
use serde::{Deserialize, Serialize};

use crate::cosine_similarity;

pub const DETECTOR_VERSION: &str = "resolve-v1";

/// The built-in hashing embedder's space at its default 384 dimensions.
pub const HASHING_SPACE_384: &str = "cs/hashing@1+d384+l2+pp1";

/// Duplicate thresholds validated for one embedding space, with the
/// provenance needed to trust them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibratedSpace {
    /// Detector version the calibration was run against; thresholds are
    /// ignored under any other detector version.
    pub detector_version: String,
    /// Calibration dataset and version (e.g. a suite name and commit).
    pub calibration: String,
    /// When the calibration was validated (free-form date).
    pub validated_at: String,
    /// Highest cosine observed among hard negatives that pass the
    /// structural pre-veto.
    #[serde(default)]
    pub max_hard_negative: Option<f64>,
    pub thresholds: BTreeMap<MemoryDomain, f64>,
}

/// Dense duplicate detection is enabled only for embedding spaces listed
/// here, under the current detector version. Unknown or uncalibrated spaces
/// never produce dense duplicate proposals; they remain usable for recall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionConfig {
    pub schema_version: u32,
    pub spaces: BTreeMap<String, CalibratedSpace>,
}

impl Default for ResolutionConfig {
    fn default() -> Self {
        // Calibrated against crates/cs-engine/tests/resolution_calibration.rs:
        // the highest non-vetoed hard negative scores 0.815, so thresholds
        // keep a margin of at least 0.065 while catching close paraphrases.
        Self {
            schema_version: 1,
            spaces: BTreeMap::from([(
                HASHING_SPACE_384.to_owned(),
                CalibratedSpace {
                    detector_version: DETECTOR_VERSION.to_owned(),
                    calibration: "resolution_calibration@m3".to_owned(),
                    validated_at: "2026-11-13".to_owned(),
                    max_hard_negative: Some(0.815),
                    thresholds: BTreeMap::from([
                        (MemoryDomain::Episodic, 0.88),
                        (MemoryDomain::Semantic, 0.88),
                        (MemoryDomain::Procedural, 0.90),
                    ]),
                },
            )]),
        }
    }
}

impl ResolutionConfig {
    /// Threshold for a space and domain, if that space is calibrated for
    /// the running detector version.
    pub fn threshold(&self, space: Option<&str>, domain: MemoryDomain) -> Option<f64> {
        let s = self.spaces.get(space?)?;
        if s.detector_version != DETECTOR_VERSION {
            return None;
        }
        s.thresholds.get(&domain).copied()
    }

    pub fn is_calibrated(&self, space: &str) -> bool {
        self.spaces
            .get(space)
            .is_some_and(|s| s.detector_version == DETECTOR_VERSION && !s.thresholds.is_empty())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported schema_version {}",
                self.schema_version
            ));
        }
        for (id, s) in &self.spaces {
            if s.calibration.trim().is_empty() {
                return Err(format!("{id}: calibration provenance is required"));
            }
            for (d, t) in &s.thresholds {
                if !(t.is_finite() && *t > 0.0 && *t <= 1.0) {
                    return Err(format!("{id}: {} threshold must be in (0, 1]", d.as_str()));
                }
                if let Some(h) = s.max_hard_negative
                    && *t <= h
                {
                    return Err(format!(
                        "{id}: {} threshold {t} does not exceed the hard-negative ceiling {h}",
                        d.as_str()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Parse and validate a JSON config file's contents.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let c: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        c.validate()?;
        Ok(c)
    }
}
/// A finding before it becomes a stored proposal.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalDraft {
    pub kind: ResolutionKind,
    pub operation: ResolutionOperation,
    pub explanation: String,
    pub signals: Vec<ResolutionSignal>,
    pub score: Option<f64>,
    pub based_on: Vec<RecordVersion>,
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub record: &'a MemoryRecord,
    pub embedding: Option<&'a [f32]>,
}

/// Records are only compared when anyone able to read one can read the
/// other, so a proposal can never reveal a record to someone outside its
/// scope. (The read rule does not depend on the conversation.)
fn compatible(a: &Scope, b: &Scope) -> bool {
    a.tenant_id == b.tenant_id
        && a.project_id == b.project_id
        && a.owner_user_id == b.owner_user_id
        && a.agent_id == b.agent_id
        && a.visibility == b.visibility
}

fn eligible(m: &MemoryRecord) -> bool {
    m.is_current() && matches!(m.status, MemoryStatus::Active | MemoryStatus::Candidate)
}

/// `(subject, predicate, object)` for domains with structured propositions.
fn triple(c: &MemoryContent) -> Option<(String, String, String)> {
    match c {
        MemoryContent::Semantic(m) => match (&m.subject, &m.predicate, &m.object) {
            (Some(s), Some(p), Some(o)) => Some((normalize(s), normalize(p), normalize(o))),
            _ => None,
        },
        MemoryContent::Relational(m) => Some((
            normalize(&m.subject),
            normalize(&m.predicate),
            normalize(&m.object),
        )),
        MemoryContent::Temporal(m) => Some((
            normalize(&m.subject),
            normalize(&m.predicate),
            normalize(&m.value),
        )),
        _ => None,
    }
}

/// Single-valued predicates can conflict; relational edges are multi-valued
/// ("uses", "depends on") and never conflict on object alone.
fn single_valued(domain: MemoryDomain) -> bool {
    matches!(domain, MemoryDomain::Semantic | MemoryDomain::Temporal)
}

fn version(m: &MemoryRecord) -> RecordVersion {
    RecordVersion::Memory {
        id: m.id,
        revision_id: m.revision_id,
    }
}

fn sorted(mut v: Vec<RecordVersion>) -> Vec<RecordVersion> {
    v.sort();
    v
}

/// Older first: by creation time, then id.
fn order<'a>(a: &'a MemoryRecord, b: &'a MemoryRecord) -> (&'a MemoryRecord, &'a MemoryRecord) {
    if (a.created_at, a.id) <= (b.created_at, b.id) {
        (a, b)
    } else {
        (b, a)
    }
}

const NEGATIONS: &[&str] = &[
    "not", "no", "never", "none", "without", "disabled", "cannot",
];

/// Tokens that must agree, *in order*, for two texts to be duplicates:
/// anything with a digit (numbers, versions, units), negations, and
/// identifier-like tokens (snake_case, kebab-case, paths, camelCase,
/// PascalCase, acronyms). Order matters so that swapped arguments
/// (`a calls b` / `b calls a`) are never merged.
pub fn hard_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '"' | '\'' | '`'))
        .map(|t| t.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?')))
        .filter(|t| !t.is_empty())
        .filter(|t| {
            let lower = t.to_lowercase();
            let inner_upper =
                t.chars().skip(1).any(char::is_uppercase) && t.chars().any(char::is_lowercase);
            let acronym = t.chars().filter(|c| c.is_uppercase()).count() >= 2
                && !t.chars().any(char::is_lowercase);
            t.chars().any(|c| c.is_ascii_digit())
                || NEGATIONS.contains(&lower.as_str())
                || lower.ends_with("n't")
                || t.contains(['_', '-', '/']) && t.len() > 2
                || t.contains("::")
                || inner_upper
                || acronym
        })
        .map(str::to_lowercase)
        .collect()
}
/// Compare `target` against every other candidate.
pub fn detect_memory(
    config: &ResolutionConfig,
    model: Option<&str>,
    target: Candidate<'_>,
    others: &[Candidate<'_>],
) -> Vec<ProposalDraft> {
    let t = target.record;
    if !eligible(t) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut ordered: Vec<&Candidate<'_>> = others.iter().collect();
    ordered.sort_by_key(|c| (c.record.id, c.record.revision_id));

    for other in ordered {
        let o = other.record;
        if o.id == t.id
            || !eligible(o)
            || o.domain() != t.domain()
            || !compatible(&t.scope, &o.scope)
        {
            continue;
        }
        let overlap = t.temporal.valid_time.overlaps(&o.temporal.valid_time);
        let based_on = sorted(vec![version(t), version(o)]);
        let (older, newer) = order(t, o);
        let merge = ResolutionOperation::Merge {
            canonical: older.id,
            duplicate: newer.id,
        };

        // 1. Exact normalized text.
        if overlap && normalize(&t.content.index_text()) == normalize(&o.content.index_text()) {
            out.push(ProposalDraft {
                kind: ResolutionKind::Duplicate,
                operation: merge,
                explanation: "identical normalized content in overlapping valid time".into(),
                signals: vec![
                    ResolutionSignal::NormalizedTextExact,
                    ResolutionSignal::ScopeCompatible,
                    ResolutionSignal::TemporalOverlap,
                ],
                score: Some(1.0),
                based_on,
            });
            continue;
        }

        let (tt, ot) = (triple(&t.content), triple(&o.content));
        if let (Some(a), Some(b)) = (&tt, &ot) {
            // 2. Structured equality.
            if overlap && a == b {
                out.push(ProposalDraft {
                    kind: ResolutionKind::Duplicate,
                    operation: merge,
                    explanation: "identical subject, predicate and object".into(),
                    signals: vec![
                        ResolutionSignal::StructuredFieldsExact,
                        ResolutionSignal::ScopeCompatible,
                        ResolutionSignal::TemporalOverlap,
                    ],
                    score: Some(1.0),
                    based_on,
                });
                continue;
            }
            // 3. Same subject+predicate, different value.
            if single_valued(t.domain()) && a.0 == b.0 && a.1 == b.1 && a.2 != b.2 {
                let conflict = ResolutionSignal::PredicateConflict {
                    field: "object".into(),
                    left: a.2.clone(),
                    right: b.2.clone(),
                };
                let (vo, vn) = if o.temporal.valid_time.start <= t.temporal.valid_time.start {
                    (o, t)
                } else {
                    (t, o)
                };
                if vo.temporal.valid_time.start < vn.temporal.valid_time.start {
                    out.push(ProposalDraft {
                        kind: ResolutionKind::Supersession,
                        operation: ResolutionOperation::Supersede {
                            older: vo.id,
                            newer: vn.id,
                        },
                        explanation: format!(
                            "'{} {}' changed value; the newer fact starts later in valid time",
                            a.0, a.1
                        ),
                        signals: vec![
                            conflict,
                            ResolutionSignal::ValidTimeOrdered,
                            ResolutionSignal::ScopeCompatible,
                        ],
                        score: None,
                        based_on,
                    });
                } else if overlap {
                    out.push(ProposalDraft {
                        kind: ResolutionKind::Contradiction,
                        operation: ResolutionOperation::Quarantine {
                            memories: {
                                let mut v = vec![t.id, o.id];
                                v.sort();
                                v
                            },
                        },
                        explanation: format!(
                            "'{} {}' has incompatible values over the same valid time",
                            a.0, a.1
                        ),
                        signals: vec![
                            conflict,
                            ResolutionSignal::TemporalOverlap,
                            ResolutionSignal::ScopeCompatible,
                        ],
                        score: None,
                        based_on,
                    });
                }
                continue;
            }
        }

        // 4. Dense similarity: duplicate candidate only.
        if let (Some(threshold), Some(te), Some(oe)) =
            (config.threshold(model, t.domain()), target.embedding, other.embedding)
            && overlap
            // Structural pre-veto: never merge records that disagree on
            // numbers, versions, negation or identifiers, however close
            // their embeddings are.
            && hard_tokens(&t.content.index_text()) == hard_tokens(&o.content.index_text())
            && let Some(cos) = cosine_similarity(te, oe)
            && cos >= threshold
        {
            out.push(ProposalDraft {
                kind: ResolutionKind::Duplicate,
                operation: merge,
                explanation: format!("embedding similarity {cos:.3} >= {threshold:.3}"),
                signals: vec![
                    ResolutionSignal::DenseSimilarity {
                        cosine: cos,
                        threshold,
                    },
                    ResolutionSignal::ScopeCompatible,
                    ResolutionSignal::TemporalOverlap,
                ],
                score: Some(cos),
                based_on,
            });
        }
    }
    out
}

/// Beliefs asserting and denying the same proposition.
pub fn detect_beliefs(target: &Belief, others: &[Belief]) -> Vec<ProposalDraft> {
    if !(target.is_current() && target.status.is_live()) {
        return Vec::new();
    }
    let prop = target.normalized_proposition();
    let mut ordered: Vec<&Belief> = others.iter().collect();
    ordered.sort_by_key(|b| b.id);
    ordered
        .into_iter()
        .filter(|o| {
            o.lineage_id != target.lineage_id
                && o.is_current()
                && o.status.is_live()
                && compatible(&o.scope, &target.scope)
                && o.polarity != target.polarity
                && o.temporal.valid_time.overlaps(&target.temporal.valid_time)
                && o.normalized_proposition() == prop
        })
        .map(|o| {
            let mut lineages = vec![target.lineage_id, o.lineage_id];
            lineages.sort();
            ProposalDraft {
                kind: ResolutionKind::Contradiction,
                operation: ResolutionOperation::Dispute { beliefs: lineages },
                explanation: "the same proposition is both asserted and denied".into(),
                signals: vec![
                    ResolutionSignal::OppositePolarity,
                    ResolutionSignal::NormalizedTextExact,
                    ResolutionSignal::TemporalOverlap,
                ],
                score: None,
                based_on: sorted(vec![
                    RecordVersion::Belief {
                        lineage_id: target.lineage_id,
                        revision_id: target.id,
                    },
                    RecordVersion::Belief {
                        lineage_id: o.lineage_id,
                        revision_id: o.id,
                    },
                ]),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use cs_core::{
        ActorRef, Bitemporal, MemoryId, MemoryRevisionId, Provenance, SourceRef, TenantId,
        TimeInterval, UserId,
    };
    use cs_memory::{RelationalMemory, SemanticMemory};
    use time::{Duration, OffsetDateTime, macros::datetime};
    use uuid::Uuid;

    use super::*;

    const T0: OffsetDateTime = datetime!(2026-01-01 0:00 UTC);

    fn mem(n: u128, content: MemoryContent, valid_from_days: i64) -> MemoryRecord {
        let tenant = TenantId(Uuid::from_u128(1));
        MemoryRecord {
            id: MemoryId(Uuid::from_u128(n)),
            revision_id: MemoryRevisionId(Uuid::from_u128(n + 1000)),
            revision_number: 1,
            scope: Scope::private(tenant, UserId(Uuid::from_u128(2))),
            content,
            provenance: Provenance {
                source: SourceRef::UserInput { message_id: None },
                actor: ActorRef::System {
                    component: "test".into(),
                },
                observed_at: T0,
                recorded_at: T0,
                extraction: None,
                parent_evidence: vec![],
                corroborations: vec![],
            },
            temporal: Bitemporal {
                valid_time: TimeInterval::from(T0 + Duration::days(valid_from_days)),
                transaction_time: TimeInterval::from(T0 + Duration::days(n as i64)),
            },
            status: MemoryStatus::Active,
            status_reason: None,
            supersedes: None,
            superseded_by: None,
            created_at: T0 + Duration::days(n as i64),
            expires_at: None,
        }
    }

    fn fact(s: &str, p: &str, o: &str) -> MemoryContent {
        MemoryContent::Semantic(SemanticMemory {
            proposition: format!("{s} {p} {o}"),
            subject: Some(s.into()),
            predicate: Some(p.into()),
            object: Some(o.into()),
        })
    }

    fn run(t: &MemoryRecord, others: &[MemoryRecord]) -> Vec<ProposalDraft> {
        let c: Vec<Candidate<'_>> = others
            .iter()
            .map(|r| Candidate {
                record: r,
                embedding: None,
            })
            .collect();
        detect_memory(
            &ResolutionConfig::default(),
            Some(HASHING_SPACE_384),
            Candidate {
                record: t,
                embedding: None,
            },
            &c,
        )
    }

    #[test]
    fn exact_text_duplicate_merges_into_older() {
        let a = mem(1, fact("db", "is", "postgres"), 0);
        let b = mem(2, fact("DB", "is", "Postgres."), 0);
        let found = run(&b, std::slice::from_ref(&a));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ResolutionKind::Duplicate);
        assert_eq!(
            found[0].operation,
            ResolutionOperation::Merge {
                canonical: a.id,
                duplicate: b.id
            }
        );
    }

    #[test]
    fn later_value_supersedes() {
        let a = mem(1, fact("primary db", "is", "mysql"), 0);
        let b = mem(2, fact("primary db", "is", "postgres"), 30);
        let found = run(&b, std::slice::from_ref(&a));
        assert_eq!(found[0].kind, ResolutionKind::Supersession);
        assert_eq!(
            found[0].operation,
            ResolutionOperation::Supersede {
                older: a.id,
                newer: b.id
            }
        );
    }

    #[test]
    fn simultaneous_values_contradict() {
        let a = mem(1, fact("primary db", "is", "mysql"), 0);
        let b = mem(2, fact("primary db", "is", "postgres"), 0);
        assert_eq!(run(&b, &[a])[0].kind, ResolutionKind::Contradiction);
    }

    #[test]
    fn relational_edges_are_multi_valued() {
        let rel = |o: &str| {
            MemoryContent::Relational(RelationalMemory {
                subject: "api".into(),
                predicate: "uses".into(),
                object: o.into(),
                directed: true,
            })
        };
        assert!(run(&mem(2, rel("redis"), 0), &[mem(1, rel("postgres"), 0)]).is_empty());
    }

    #[test]
    fn similarity_requires_threshold() {
        let a = mem(1, fact("x", "is", "y"), 0);
        let b = mem(
            2,
            MemoryContent::Semantic(SemanticMemory {
                proposition: "something else".into(),
                subject: None,
                predicate: None,
                object: None,
            }),
            0,
        );
        let e1 = [1.0_f32, 0.0];
        let e2 = [0.99_f32, 0.141];
        let found = detect_memory(
            &ResolutionConfig::default(),
            Some(HASHING_SPACE_384),
            Candidate {
                record: &b,
                embedding: Some(&e2),
            },
            &[Candidate {
                record: &a,
                embedding: Some(&e1),
            }],
        );
        assert_eq!(found.len(), 1);
        assert!(matches!(
            found[0].signals[0],
            ResolutionSignal::DenseSimilarity { .. }
        ));
        let far = [0.0_f32, 1.0];
        assert!(
            detect_memory(
                &ResolutionConfig::default(),
                Some(HASHING_SPACE_384),
                Candidate {
                    record: &b,
                    embedding: Some(&far)
                },
                &[Candidate {
                    record: &a,
                    embedding: Some(&e1)
                }],
            )
            .is_empty()
        );
    }

    #[test]
    fn hard_tokens_catch_numbers_negation_and_identifiers() {
        assert_ne!(
            hard_tokens("retry backoff max is 30s"),
            hard_tokens("retry backoff max is 60s")
        );
        assert_ne!(
            hard_tokens("exact scoring is enabled"),
            hard_tokens("exact scoring is not enabled")
        );
        assert_ne!(
            hard_tokens("cs-core defines ScopeId"),
            hard_tokens("cs-memory defines ScopeId")
        );
        assert_eq!(
            hard_tokens("The API uses Postgres."),
            hard_tokens("the API uses postgres")
        );
    }

    #[test]
    fn dense_match_is_vetoed_by_differing_numbers() {
        let sem = |p: &str| {
            MemoryContent::Semantic(SemanticMemory {
                proposition: p.into(),
                subject: None,
                predicate: None,
                object: None,
            })
        };
        let a = mem(1, sem("request timeout is 5s"), 0);
        let b = mem(2, sem("request timeout is 50s"), 0);
        let e = [1.0_f32, 0.0];
        let found = detect_memory(
            &ResolutionConfig::default(),
            Some(HASHING_SPACE_384),
            Candidate {
                record: &b,
                embedding: Some(&e),
            },
            &[Candidate {
                record: &a,
                embedding: Some(&e),
            }],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn different_visibility_is_not_compared() {
        let a = mem(1, fact("db", "is", "postgres"), 0);
        let mut b = mem(2, fact("db", "is", "postgres"), 0);
        b.scope.visibility = cs_core::Visibility::Tenant;
        assert!(run(&b, &[a]).is_empty());
    }

    #[test]
    fn uncalibrated_or_mismatched_spaces_are_disabled() {
        let c = ResolutionConfig::default();
        assert!(
            c.threshold(Some(HASHING_SPACE_384), MemoryDomain::Semantic)
                .is_some()
        );
        assert!(
            c.threshold(Some("ollama/nomic@v1+d768+l2+pp1"), MemoryDomain::Semantic)
                .is_none()
        );
        assert!(c.threshold(None, MemoryDomain::Semantic).is_none());
        let mut stale = c.clone();
        stale
            .spaces
            .get_mut(HASHING_SPACE_384)
            .unwrap()
            .detector_version = "resolve-v0".into();
        assert!(
            stale
                .threshold(Some(HASHING_SPACE_384), MemoryDomain::Semantic)
                .is_none()
        );
    }

    #[test]
    fn config_files_are_validated() {
        let json = serde_json::to_string(&ResolutionConfig::default()).unwrap();
        assert!(ResolutionConfig::from_json(&json).is_ok());
        let mut bad = ResolutionConfig::default();
        bad.spaces
            .get_mut(HASHING_SPACE_384)
            .unwrap()
            .thresholds
            .insert(MemoryDomain::Semantic, 0.5);
        assert!(
            bad.validate().is_err(),
            "threshold below the hard-negative ceiling"
        );
    }

    #[test]
    fn different_projects_are_not_compared() {
        let a = mem(1, fact("db", "is", "postgres"), 0);
        let mut b = mem(2, fact("db", "is", "postgres"), 0);
        b.scope.project_id = Some(cs_core::ProjectId(Uuid::from_u128(9)));
        assert!(run(&b, &[a]).is_empty());
    }
}
