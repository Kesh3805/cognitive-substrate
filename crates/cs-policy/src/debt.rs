//! Cognitive debt: the accumulated risk carried by knowledge that is
//! unsupported, contradictory, stale, built on invalidated assumptions, or
//! never verified.
//!
//! Per item:
//!
//! `D_i = I_i * C_i * (1 + log2(1 + A_i / H_i)) * E_i`, with
//! `E_i = min(3, 1 + ln(1 + N_i))`
//!
//! * `I` impact (1–5), `C` confidence that the issue is real (0–1),
//! * `A` age in days, `H` category half-life in days,
//! * `N` weighted count of live beliefs that rely on the affected belief.
//!
//! Aggregate: `CDI = 1000 * sum(D_i) / max(1, active_assets)`.
//!
//! This is a heuristic risk score for prioritising remediation, **not** a
//! probability of failure. See `docs/specifications/cognitive-debt.md`.

use std::collections::{BTreeMap, BTreeSet};

use cs_core::BeliefId;
use cs_memory::{
    Belief, BeliefDependency, BeliefEvidence, BeliefStatus, CapabilityStage, DependencyRelation,
    EvidenceDirection, MemoryContent, MemoryRecord, MemoryStatus,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Version of the debt formula and detection rules.
pub const DEBT_MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtCategory {
    UnsupportedBelief,
    UnresolvedContradiction,
    StaleBelief,
    InvalidatedDependency,
    UnverifiedProcedure,
}

impl DebtCategory {
    pub const ALL: [Self; 5] = [
        Self::UnsupportedBelief,
        Self::UnresolvedContradiction,
        Self::StaleBelief,
        Self::InvalidatedDependency,
        Self::UnverifiedProcedure,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CategoryParams {
    /// Consequence if this issue affects a decision, 1–5.
    pub impact: f64,
    /// Age (days) at which the age multiplier reaches 2.
    pub half_life_days: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebtConfig {
    pub unsupported_belief: CategoryParams,
    pub unresolved_contradiction: CategoryParams,
    pub stale_belief: CategoryParams,
    pub invalidated_dependency: CategoryParams,
    pub unverified_procedure: CategoryParams,
    /// Revalidation interval for beliefs that don't declare their own.
    pub default_revalidation_days: Option<u32>,
}

impl Default for DebtConfig {
    fn default() -> Self {
        Self {
            unsupported_belief: CategoryParams {
                impact: 3.0,
                half_life_days: 7.0,
            },
            unresolved_contradiction: CategoryParams {
                impact: 4.0,
                half_life_days: 3.0,
            },
            stale_belief: CategoryParams {
                impact: 2.0,
                half_life_days: 30.0,
            },
            invalidated_dependency: CategoryParams {
                impact: 4.0,
                half_life_days: 3.0,
            },
            unverified_procedure: CategoryParams {
                impact: 3.0,
                half_life_days: 14.0,
            },
            default_revalidation_days: None,
        }
    }
}

impl DebtConfig {
    pub fn params(&self, category: DebtCategory) -> CategoryParams {
        match category {
            DebtCategory::UnsupportedBelief => self.unsupported_belief,
            DebtCategory::UnresolvedContradiction => self.unresolved_contradiction,
            DebtCategory::StaleBelief => self.stale_belief,
            DebtCategory::InvalidatedDependency => self.invalidated_dependency,
            DebtCategory::UnverifiedProcedure => self.unverified_procedure,
        }
    }
}

/// Everything the evaluator needs, already restricted to the caller's scope.
#[derive(Debug, Clone, Copy)]
pub struct DebtInputs<'a> {
    pub now: OffsetDateTime,
    /// All belief revisions (current and historical) in scope.
    pub beliefs: &'a [Belief],
    /// Evidence links keyed by belief revision id.
    pub evidence: &'a BTreeMap<BeliefId, Vec<BeliefEvidence>>,
    pub dependencies: &'a [BeliefDependency],
    /// Current procedural memories in scope.
    pub procedures: &'a [MemoryRecord],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebtItem {
    pub category: DebtCategory,
    /// Beliefs or memories the item concerns (as UUID strings).
    pub subject_ids: Vec<String>,
    pub reason: String,
    pub impact: f64,
    pub issue_confidence: f64,
    pub age_days: f64,
    pub half_life_days: f64,
    pub exposure_count: f64,
    pub points: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebtCategoryScore {
    pub category: DebtCategory,
    pub item_count: u64,
    pub total_points: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitiveDebtReport {
    pub model_version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub evaluated_at: OffsetDateTime,
    pub active_assets: u64,
    pub debt_items: u64,
    pub total_debt_points: f64,
    /// Cognitive Debt Index: debt points per 1,000 active cognitive assets.
    pub debt_index_per_1000: f64,
    pub categories: Vec<DebtCategoryScore>,
    pub items: Vec<DebtItem>,
}

pub fn item_points(impact: f64, confidence: f64, age_days: f64, half_life: f64, n: f64) -> f64 {
    let age_multiplier = 1.0 + (1.0 + age_days.max(0.0) / half_life).log2();
    let exposure = (1.0 + (1.0 + n.max(0.0)).ln()).min(3.0);
    impact * confidence * age_multiplier * exposure
}

fn days_between(from: OffsetDateTime, to: OffsetDateTime) -> f64 {
    ((to - from).as_seconds_f64() / 86_400.0).max(0.0)
}

/// Deterministically evaluate cognitive debt.
pub fn evaluate_debt(config: &DebtConfig, input: DebtInputs<'_>) -> CognitiveDebtReport {
    let by_id: BTreeMap<BeliefId, &Belief> = input.beliefs.iter().map(|b| (b.id, b)).collect();
    let current_by_lineage: BTreeMap<BeliefId, &Belief> = input
        .beliefs
        .iter()
        .filter(|b| b.is_current())
        .map(|b| (b.lineage_id, b))
        .collect();
    let live: Vec<&Belief> = {
        let mut v: Vec<&Belief> = input
            .beliefs
            .iter()
            .filter(|b| b.is_current() && b.status.is_live())
            .collect();
        v.sort_by_key(|b| b.id);
        v
    };
    let live_ids: BTreeSet<BeliefId> = live.iter().map(|b| b.id).collect();
    let active_procedures: Vec<&MemoryRecord> = input
        .procedures
        .iter()
        .filter(|m| m.is_current() && m.status == MemoryStatus::Active)
        .collect();

    let exposure = |id: BeliefId| -> f64 {
        input
            .dependencies
            .iter()
            .filter(|d| d.from == id && d.relation.is_reliance() && live_ids.contains(&d.to))
            .map(|d| d.strength.unwrap_or(1.0))
            .sum()
    };

    let mut items = Vec::new();
    let mut push = |category: DebtCategory,
                    subjects: Vec<String>,
                    reason: String,
                    confidence: f64,
                    age_days: f64,
                    n: f64| {
        let p = config.params(category);
        items.push(DebtItem {
            category,
            subject_ids: subjects,
            reason,
            impact: p.impact,
            issue_confidence: confidence,
            age_days,
            half_life_days: p.half_life_days,
            exposure_count: n,
            points: item_points(p.impact, confidence, age_days, p.half_life_days, n),
        });
    };

    // 1. Unsupported beliefs.
    for b in &live {
        let supported = input.evidence.get(&b.id).is_some_and(|links| {
            links
                .iter()
                .any(|l| l.direction == EvidenceDirection::Supports)
        });
        if !supported {
            push(
                DebtCategory::UnsupportedBelief,
                vec![b.id.0.to_string()],
                "live belief has no supporting evidence".into(),
                1.0,
                days_between(b.temporal.transaction_time.start, input.now),
                exposure(b.id),
            );
        }
    }

    // 2. Unresolved contradictions (opposite polarity, or explicit edge).
    let mut seen_pairs = BTreeSet::new();
    for (i, a) in live.iter().enumerate() {
        for b in &live[i + 1..] {
            let same_scope =
                a.scope.tenant_id == b.scope.tenant_id && a.scope.project_id == b.scope.project_id;
            if same_scope
                && a.polarity != b.polarity
                && a.temporal.valid_time.overlaps(&b.temporal.valid_time)
                && a.normalized_proposition() == b.normalized_proposition()
                && seen_pairs.insert((a.id, b.id))
            {
                push(
                    DebtCategory::UnresolvedContradiction,
                    vec![a.id.0.to_string(), b.id.0.to_string()],
                    "beliefs assert and deny the same proposition over overlapping valid time"
                        .into(),
                    0.9,
                    days_between(
                        a.temporal
                            .transaction_time
                            .start
                            .max(b.temporal.transaction_time.start),
                        input.now,
                    ),
                    exposure(a.id) + exposure(b.id),
                );
            }
        }
    }
    for d in input.dependencies {
        if d.relation != DependencyRelation::Contradicts
            || !live_ids.contains(&d.from)
            || !live_ids.contains(&d.to)
        {
            continue;
        }
        let pair = if d.from < d.to {
            (d.from, d.to)
        } else {
            (d.to, d.from)
        };
        if !seen_pairs.insert(pair) {
            continue;
        }
        let (Some(a), Some(b)) = (by_id.get(&pair.0), by_id.get(&pair.1)) else {
            continue;
        };
        push(
            DebtCategory::UnresolvedContradiction,
            vec![a.id.0.to_string(), b.id.0.to_string()],
            "explicit contradiction edge between live beliefs".into(),
            1.0,
            days_between(d.created_at, input.now),
            exposure(a.id) + exposure(b.id),
        );
    }

    // 3. Stale beliefs.
    for b in &live {
        let Some(interval) = b
            .revalidation_interval_days
            .or(config.default_revalidation_days)
        else {
            continue;
        };
        let due = b.temporal.transaction_time.start + time::Duration::days(i64::from(interval));
        if input.now > due {
            push(
                DebtCategory::StaleBelief,
                vec![b.id.0.to_string()],
                format!("not revalidated within {interval} days"),
                1.0,
                days_between(due, input.now),
                exposure(b.id),
            );
        }
    }

    // 4. Invalidated dependencies.
    let mut seen_deps = BTreeSet::new();
    let mut deps: Vec<&BeliefDependency> = input
        .dependencies
        .iter()
        .filter(|d| d.relation.is_reliance())
        .collect();
    deps.sort_by_key(|d| (d.to, d.from));
    for d in deps {
        if !live_ids.contains(&d.to) {
            continue;
        }
        let Some(source) = by_id.get(&d.from) else {
            continue;
        };
        let Some(current) = current_by_lineage.get(&source.lineage_id) else {
            continue;
        };
        if !seen_deps.insert((d.to, source.lineage_id)) {
            continue;
        }
        let (reason, confidence, since) = if current.status == BeliefStatus::Retracted {
            (
                "depends on a retracted belief",
                1.0,
                current.temporal.transaction_time.start,
            )
        } else if current.id != source.id
            && !input
                .dependencies
                .iter()
                .any(|e| e.from == current.id && e.to == d.to)
        {
            (
                "depends on a superseded belief revision; re-evaluation required",
                0.8,
                source
                    .temporal
                    .transaction_time
                    .end
                    .unwrap_or(current.temporal.transaction_time.start),
            )
        } else {
            continue;
        };
        push(
            DebtCategory::InvalidatedDependency,
            vec![d.to.0.to_string(), source.id.0.to_string()],
            reason.into(),
            confidence,
            days_between(since, input.now),
            exposure(d.to),
        );
    }

    // 5. Unverified procedures.
    let mut procs = active_procedures.clone();
    procs.sort_by_key(|m| m.id);
    for m in procs {
        if let MemoryContent::Procedural(p) = &m.content
            && !p.stage.is_verified()
            && p.stage != CapabilityStage::Deprecated
        {
            push(
                DebtCategory::UnverifiedProcedure,
                vec![m.id.0.to_string()],
                format!("procedure '{}' has not passed evaluation", p.name),
                1.0,
                days_between(m.created_at, input.now),
                0.0,
            );
        }
    }

    items.sort_by(|a, b| {
        b.points
            .partial_cmp(&a.points)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.subject_ids.cmp(&b.subject_ids))
    });

    let categories = DebtCategory::ALL
        .into_iter()
        .map(|category| {
            let of_cat = items.iter().filter(|i| i.category == category);
            DebtCategoryScore {
                category,
                item_count: of_cat.clone().count() as u64,
                total_points: of_cat.map(|i| i.points).sum(),
            }
        })
        .collect();

    let active_assets = (live.len() + active_procedures.len()) as u64;
    let total: f64 = items.iter().map(|i| i.points).sum();
    CognitiveDebtReport {
        model_version: DEBT_MODEL_VERSION,
        evaluated_at: input.now,
        active_assets,
        debt_items: items.len() as u64,
        total_debt_points: total,
        debt_index_per_1000: 1000.0 * total / (active_assets.max(1) as f64),
        categories,
        items,
    }
}

#[cfg(test)]
mod tests {
    use cs_core::{Bitemporal, Scope, TenantId, TimeInterval};
    use cs_memory::{BeliefPolarity, Confidence, ConfidenceInterpretation};
    use time::{Duration, macros::datetime};
    use uuid::Uuid;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-11-13 0:00 UTC);

    fn belief(n: u128, prop: &str, polarity: BeliefPolarity) -> Belief {
        let id = BeliefId(Uuid::from_u128(n));
        Belief {
            id,
            lineage_id: id,
            scope: Scope::tenant(TenantId(Uuid::from_u128(1))),
            proposition: prop.into(),
            polarity,
            confidence: Confidence {
                value: 0.8,
                interpretation: ConfidenceInterpretation::HeuristicScore,
                method: "test".into(),
            },
            revision_number: 1,
            supersedes: None,
            superseded_by: None,
            revision_reason: "initial".into(),
            status: BeliefStatus::Active,
            revalidation_interval_days: None,
            temporal: Bitemporal {
                valid_time: TimeInterval::from(NOW - Duration::days(100)),
                transaction_time: TimeInterval::from(NOW - Duration::days(14)),
            },
            created_at: NOW - Duration::days(14),
        }
    }

    fn supported(ids: &[&Belief]) -> BTreeMap<BeliefId, Vec<BeliefEvidence>> {
        ids.iter()
            .map(|b| {
                (
                    b.id,
                    vec![BeliefEvidence {
                        evidence_id: cs_core::EvidenceId(Uuid::from_u128(999)),
                        direction: EvidenceDirection::Supports,
                        strength: None,
                    }],
                )
            })
            .collect()
    }

    fn run(
        beliefs: &[Belief],
        ev: &BTreeMap<BeliefId, Vec<BeliefEvidence>>,
        deps: &[BeliefDependency],
    ) -> CognitiveDebtReport {
        evaluate_debt(
            &DebtConfig::default(),
            DebtInputs {
                now: NOW,
                beliefs,
                evidence: ev,
                dependencies: deps,
                procedures: &[],
            },
        )
    }

    #[test]
    fn worked_example_matches_specification() {
        // Impact 5, confidence 0.9, 14 days old, half-life 7, three dependants.
        let p = item_points(5.0, 0.9, 14.0, 7.0, 3.0);
        assert!((p - 27.75).abs() < 0.05, "{p}");
    }

    #[test]
    fn exposure_multiplier_is_capped_at_three() {
        let p = item_points(1.0, 1.0, 0.0, 1.0, 1e9);
        assert!((p - 3.0).abs() < 1e-9);
    }

    #[test]
    fn healthy_knowledge_has_no_debt() {
        let a = belief(10, "rust is memory safe", BeliefPolarity::Supports);
        let r = run(std::slice::from_ref(&a), &supported(&[&a]), &[]);
        assert_eq!(r.debt_items, 0);
        assert_eq!(r.active_assets, 1);
        assert_eq!(r.debt_index_per_1000, 0.0);
    }

    #[test]
    fn detects_unsupported_belief() {
        let a = belief(10, "x", BeliefPolarity::Supports);
        let r = run(&[a], &BTreeMap::new(), &[]);
        assert_eq!(r.items[0].category, DebtCategory::UnsupportedBelief);
    }

    #[test]
    fn detects_polarity_contradiction() {
        let a = belief(10, "The API supports streaming", BeliefPolarity::Supports);
        let b = belief(11, "the api supports streaming.", BeliefPolarity::Refutes);
        let r = run(&[a.clone(), b.clone()], &supported(&[&a, &b]), &[]);
        assert_eq!(r.debt_items, 1);
        assert_eq!(r.items[0].category, DebtCategory::UnresolvedContradiction);
    }

    #[test]
    fn superseded_dependency_requires_reevaluation() {
        let mut old = belief(10, "library supports op", BeliefPolarity::Supports);
        old.temporal.transaction_time.end = Some(NOW - Duration::days(2));
        let mut new = belief(12, "library supports op", BeliefPolarity::Refutes);
        new.lineage_id = old.id;
        new.revision_number = 2;
        new.temporal.transaction_time = TimeInterval::from(NOW - Duration::days(2));
        let plan = belief(11, "plan uses op", BeliefPolarity::Supports);
        let edge = BeliefDependency {
            from: old.id,
            to: plan.id,
            relation: DependencyRelation::Requires,
            strength: None,
            created_at: NOW - Duration::days(14),
        };
        let ev = supported(&[&new, &plan]);
        let r = run(
            &[old.clone(), new.clone(), plan.clone()],
            &ev,
            std::slice::from_ref(&edge),
        );
        assert_eq!(r.debt_items, 1);
        assert_eq!(r.items[0].category, DebtCategory::InvalidatedDependency);

        // Re-affirming the dependency on the current revision clears it.
        let reaffirm = BeliefDependency {
            from: new.id,
            ..edge.clone()
        };
        let r = run(&[old, new, plan], &ev, &[edge, reaffirm]);
        assert_eq!(r.debt_items, 0);
    }

    #[test]
    fn stale_belief_detected() {
        let mut a = belief(10, "x", BeliefPolarity::Supports);
        a.revalidation_interval_days = Some(7);
        let r = run(std::slice::from_ref(&a), &supported(&[&a]), &[]);
        assert_eq!(r.items[0].category, DebtCategory::StaleBelief);
        assert!((r.items[0].age_days - 7.0).abs() < 1e-9);
    }
}
