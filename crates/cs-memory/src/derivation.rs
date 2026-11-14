//! Knowledge compilation: derived memories and the support behind them.
//!
//! A compiled memory is never more trustworthy than the evidence it was
//! compiled from. Three rules keep compilation from laundering weak
//! evidence:
//!
//! * **Support is counted in independent units, not records.** Roots that
//!   share an actor, an origin and a valid-time bucket count once, however
//!   many records, re-extractions or retries produced them.
//! * **Derived memories never support anything.** Only validated (active)
//!   episodes whose source is not itself derived can be roots.
//! * **Counterexamples are looked for.** Episodes with the same subject and
//!   relation but a different object count against compilation.
//!
//! A [`Derivation`] records the operator, the thresholds it applied, the
//! support it found and the exact root revisions behind it, so that losing
//! support (a purge, a tombstone, a revision) can be detected and acted on.

use cs_core::{
    ActorRef, DerivationId, EntityId, Error, MemoryId, MemoryRevisionId, Result, Scope, SourceRef,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The first compilation operator: recurring episode triples become a
/// semantic candidate.
pub const RECURRING_EPISODE_OPERATOR: &str = "recurring-episode/1";

/// Namespace for opaque independence keys (no content is stored in them).
const KEY_NAMESPACE: Uuid = Uuid::from_u128(0x3f2e_91c4_7a0d_4b6e_8c15_d2a9_60e7_4b31);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompilationThresholds {
    /// Independent support units required.
    pub min_units: u32,
    /// Distinct (actor, origin) pairs required among those units.
    pub min_origins: u32,
    /// Distinct valid-time buckets required (temporal dispersion).
    pub min_buckets: u32,
    /// Width of a valid-time bucket.
    pub bucket_seconds: i64,
    /// Largest tolerated share of counterexample units.
    pub max_counter_ratio: f64,
}

impl Default for CompilationThresholds {
    fn default() -> Self {
        Self {
            min_units: 3,
            min_origins: 2,
            min_buckets: 2,
            bucket_seconds: 86_400,
            max_counter_ratio: 0.1,
        }
    }
}

impl CompilationThresholds {
    pub fn validate(&self) -> Result<()> {
        if self.min_units < 2 || self.min_origins < 1 || self.min_buckets < 1 {
            return Err(Error::validation(
                "compilation needs at least two units, one origin and one bucket",
            ));
        }
        if self.bucket_seconds < 1 {
            return Err(Error::validation("bucket_seconds must be positive"));
        }
        if !(0.0..1.0).contains(&self.max_counter_ratio) {
            return Err(Error::validation("max_counter_ratio must be in [0, 1)"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationStatus {
    /// Support meets the thresholds.
    Active,
    /// A root was purged; recompilation is pending.
    RevalidationRequired,
    /// Support no longer meets the thresholds. The compiled memory has
    /// been returned to `candidate`.
    Invalidated,
}

impl DerivationStatus {
    pub const ALL: [Self; 3] = [Self::Active, Self::RevalidationRequired, Self::Invalidated];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::RevalidationRequired => "revalidation_required",
            Self::Invalidated => "invalidated",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == v)
    }
}

/// One independent unit of support, represented by one root revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub root_memory_id: MemoryId,
    pub root_revision_id: MemoryRevisionId,
    /// Opaque `H(actor, origin, bucket)`.
    pub unit_key: Uuid,
    /// Opaque `H(actor, origin)`.
    pub origin_key: Uuid,
    pub bucket: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Support {
    pub units: u32,
    pub origins: u32,
    pub buckets: u32,
    pub counter_units: u32,
}

impl Support {
    /// Count distinct units, origins and buckets among `witnesses` (one
    /// witness per unit), plus counterexample units.
    pub fn of(witnesses: &[Witness], counter_units: u32) -> Self {
        let distinct = |f: &dyn Fn(&Witness) -> String| {
            let mut v: Vec<String> = witnesses.iter().map(f).collect();
            v.sort();
            v.dedup();
            u32::try_from(v.len()).unwrap_or(u32::MAX)
        };
        Self {
            units: distinct(&|w| w.unit_key.to_string()),
            origins: distinct(&|w| w.origin_key.to_string()),
            buckets: distinct(&|w| w.bucket.to_string()),
            counter_units,
        }
    }

    pub fn counter_ratio(&self) -> f64 {
        let total = self.units + self.counter_units;
        if total == 0 {
            0.0
        } else {
            f64::from(self.counter_units) / f64::from(total)
        }
    }

    pub fn meets(&self, t: &CompilationThresholds) -> bool {
        self.units >= t.min_units
            && self.origins >= t.min_origins
            && self.buckets >= t.min_buckets
            && self.counter_ratio() <= t.max_counter_ratio
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Derivation {
    pub id: DerivationId,
    /// The partition scope of the roots (no conversation).
    pub scope: Scope,
    pub partition: String,
    pub operator: String,
    /// `subject|relation|object` with canonical entity ids.
    pub signature: String,
    pub subject: EntityId,
    pub relation: String,
    pub object: EntityId,
    /// The compiled memory (created as `candidate`).
    pub derived_memory_id: MemoryId,
    pub thresholds: CompilationThresholds,
    pub support: Support,
    /// One representative root per unit, in deterministic order.
    pub witnesses: Vec<Witness>,
    /// Every supporting root memory, including non-representatives.
    pub roots: Vec<MemoryId>,
    pub status: DerivationStatus,
    #[serde(default)]
    pub status_reason: Option<String>,
    pub version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

pub fn signature(subject: EntityId, relation: &str, object: EntityId) -> String {
    format!("{subject}|{relation}|{object}")
}

/// Where a root's information came from, for independence. `None` means
/// the source is derived and can never be support.
pub fn origin(source: &SourceRef) -> Option<String> {
    Some(match source {
        SourceRef::UserInput { .. } => "user_input".to_owned(),
        SourceRef::AgentOutput { run_id } => format!("agent_run:{run_id}"),
        // One tool is one origin, however many times it ran.
        SourceRef::ToolOutput { tool, .. } => format!("tool:{tool}"),
        SourceRef::ExternalDocument { uri, .. } => format!("document:{uri}"),
        SourceRef::HumanAnnotation { .. } => "human_annotation".to_owned(),
        // Observations are resolved to their own source by the caller.
        SourceRef::Observation { observation_id } => format!("observation:{observation_id}"),
        SourceRef::SystemDerived { .. } => return None,
    })
}

fn actor_key(actor: &ActorRef) -> String {
    match actor {
        ActorRef::User { user_id } => format!("user:{user_id}"),
        ActorRef::Agent { agent_id } => format!("agent:{agent_id}"),
        ActorRef::System { component } => format!("system:{component}"),
    }
}

/// Opaque origin key: `H(actor, origin)`.
pub fn origin_key(actor: &ActorRef, origin: &str) -> Uuid {
    Uuid::new_v5(
        &KEY_NAMESPACE,
        format!("{}\u{1f}{origin}", actor_key(actor)).as_bytes(),
    )
}

/// Opaque unit key: `H(actor, origin, bucket)`.
pub fn unit_key(actor: &ActorRef, origin: &str, bucket: i64) -> Uuid {
    Uuid::new_v5(
        &KEY_NAMESPACE,
        format!("{}\u{1f}{origin}\u{1f}{bucket}", actor_key(actor)).as_bytes(),
    )
}

pub fn bucket_of(t: OffsetDateTime, bucket_seconds: i64) -> i64 {
    t.unix_timestamp().div_euclid(bucket_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_core::UserId;

    fn w(actor: &ActorRef, origin: &str, bucket: i64) -> Witness {
        Witness {
            root_memory_id: MemoryId::new_v7(),
            root_revision_id: MemoryRevisionId::new_v7(),
            unit_key: unit_key(actor, origin, bucket),
            origin_key: origin_key(actor, origin),
            bucket,
        }
    }

    #[test]
    fn one_source_on_one_day_is_one_unit() {
        let a = ActorRef::User {
            user_id: UserId::new_v7(),
        };
        let ws = [w(&a, "tool:monitor", 7), w(&a, "tool:monitor", 7)];
        let s = Support::of(&ws, 0);
        assert_eq!((s.units, s.origins, s.buckets), (1, 1, 1));
        assert!(!s.meets(&CompilationThresholds::default()));
    }

    #[test]
    fn thresholds_need_units_origins_dispersion_and_few_counterexamples() {
        let a = ActorRef::User {
            user_id: UserId::new_v7(),
        };
        let t = CompilationThresholds::default();
        let ws = [
            w(&a, "user_input", 1),
            w(&a, "tool:monitor", 1),
            w(&a, "tool:monitor", 2),
        ];
        assert!(Support::of(&ws, 0).meets(&t));
        assert!(!Support::of(&ws, 1).meets(&t), "one counterexample in four");
        let same_day = [
            w(&a, "user_input", 1),
            w(&a, "tool:monitor", 1),
            w(&a, "tool:pager", 1),
        ];
        assert!(!Support::of(&same_day, 0).meets(&t), "no dispersion");
    }

    #[test]
    fn derived_sources_never_count() {
        assert!(
            origin(&SourceRef::SystemDerived {
                derivation: "x".into()
            })
            .is_none()
        );
        assert_eq!(
            origin(&SourceRef::ToolOutput {
                tool: "monitor".into(),
                execution_id: "1".into()
            }),
            origin(&SourceRef::ToolOutput {
                tool: "monitor".into(),
                execution_id: "2".into()
            })
        );
    }
}
