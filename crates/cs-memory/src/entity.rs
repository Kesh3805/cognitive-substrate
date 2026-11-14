//! Entities: the things memories are about.
//!
//! Entity identity is never inferred silently. Mentions resolve to an
//! entity only by exact normalized key within one partition; anything looser
//! is an explicit decision: a version-checked alias, or a merge proposal
//! bound to entity versions.

/// Version of [`normalize_entity_name`]. Bumping it changes which mentions
/// share a key, so stored keys are recomputed rather than compared across
/// versions.
pub const ENTITY_NORMALIZATION_VERSION: u32 = 1;

/// Only the definite article is dropped: "the API" is the API, but
/// "A record" (DNS) is not "record".
const ARTICLE: &str = "the";

/// Deterministic key for an entity mention: lowercase alphanumerics, with
/// a leading "the" removed. Separators and case boundaries carry no
/// identity, so `auth-service`, `auth_service`, `AuthService`,
/// `Auth Service` and `authservice` share the key `authservice`, and
/// `PostgreSQL` matches `postgresql`. Word boundaries (including camelCase)
/// are used only to recognise the leading article.
///
/// Symbols that change meaning are kept: `+` and `#` (`C++`, `C#` and `C`
/// stay distinct) and a `.` between digits (`v1.23` is not `v12.3`).
///
/// Returns `None` when nothing meaningful remains.
pub fn normalize_entity_name(name: &str) -> Option<String> {
    let chars: Vec<char> = name.trim().chars().collect();
    let mut spaced = String::with_capacity(chars.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        if c.is_uppercase()
            && let Some(p) = prev
        {
            // lower→Upper, or the last capital of an acronym before lower.
            let boundary = p.is_lowercase()
                || p.is_ascii_digit()
                || (p.is_uppercase() && next.is_some_and(char::is_lowercase));
            if boundary {
                spaced.push(' ');
            }
        }
        let digit = |o: Option<char>| o.is_some_and(|x| x.is_ascii_digit());
        if c == '.' && digit(prev) && digit(next) {
            spaced.push('.');
        } else if matches!(c, '+' | '#') {
            spaced.push(c);
        } else if matches!(c, '-' | '_' | '.' | '/' | ':' | '\\') {
            spaced.push(' ');
        } else if c.is_alphanumeric() || c.is_whitespace() {
            spaced.extend(c.to_lowercase());
        }
    }
    let mut words: Vec<&str> = spaced.split_whitespace().collect();
    if words.len() > 1 && words[0] == ARTICLE {
        words.remove(0);
    }
    let key = words.concat();
    (!key.is_empty()).then_some(key)
}

use cs_core::{EdgeId, EntityId, EntityMentionId, MemoryId, MemoryRevisionId, Scope, TimeInterval};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{MemoryContent, MemoryRecord};

/// Typed entity kinds. Extraction never guesses: projected entities start
/// as `Unknown`; a kind is set by an explicit decision.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    #[default]
    Unknown,
    Person,
    Organization,
    Team,
    Service,
    Component,
    Technology,
    Project,
    Document,
    Concept,
    Location,
}

impl EntityKind {
    pub const ALL: [Self; 11] = [
        Self::Unknown,
        Self::Person,
        Self::Organization,
        Self::Team,
        Self::Service,
        Self::Component,
        Self::Technology,
        Self::Project,
        Self::Document,
        Self::Concept,
        Self::Location,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Person => "person",
            Self::Organization => "organization",
            Self::Team => "team",
            Self::Service => "service",
            Self::Component => "component",
            Self::Technology => "technology",
            Self::Project => "project",
            Self::Document => "document",
            Self::Concept => "concept",
            Self::Location => "location",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EntityStatus {
    Active,
    /// Identity merged into another entity at transaction time `at`.
    /// Mentions, aliases and edges stay attached to this entity; reads
    /// redirect to the survivor only as of `at` or later, so earlier
    /// as-of reads still see the graph as it was.
    Merged {
        into: EntityId,
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
    },
}

impl EntityStatus {
    /// The survivor this entity redirects to as of transaction time `t`.
    pub fn redirect_as_of(&self, t: OffsetDateTime) -> Option<EntityId> {
        match *self {
            Self::Merged { into, at } if at <= t => Some(into),
            _ => None,
        }
    }
}

/// The identity partition: entities, aliases and edges are only shared by
/// records that everyone able to read one could read the other (tenant,
/// project, owner, agent, visibility). This is what stops scope leakage
/// through a shared entity.
pub fn partition_key(scope: &Scope) -> String {
    let id = |o: Option<uuid::Uuid>| o.map_or_else(|| "-".to_owned(), |u| u.to_string());
    format!(
        "{}|p:{}|u:{}|a:{}|v:{}",
        scope.tenant_id,
        id(scope.project_id.map(|p| p.0)),
        id(scope.owner_user_id.map(|u| u.0)),
        id(scope.agent_id.map(|a| a.0)),
        scope.visibility.as_str()
    )
}

/// The scope an entity carries: its partition, without a conversation.
pub fn partition_scope(scope: &Scope) -> Scope {
    Scope {
        conversation_id: None,
        ..scope.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub scope: Scope,
    pub partition: String,
    pub kind: EntityKind,
    pub canonical_name: String,
    pub key: String,
    pub status: EntityStatus,
    /// Incremented on every identity change; proposals bind to it.
    pub version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasSource {
    /// The first surface form the entity was created from.
    Canonical,
    /// Added by an explicit, authorized decision.
    Explicit,
    /// Carried over from a merged entity.
    Merge,
}

impl AliasSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Explicit => "explicit",
            Self::Merge => "merge",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "canonical" => Self::Canonical,
            "explicit" => Self::Explicit,
            "merge" => Self::Merge,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityAlias {
    pub entity_id: EntityId,
    pub surface: String,
    pub key: String,
    pub normalization_version: u32,
    pub source: AliasSource,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionRole {
    Subject,
    Object,
    Participant,
}

impl MentionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Object => "object",
            Self::Participant => "participant",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "subject" => Self::Subject,
            "object" => Self::Object,
            "participant" => Self::Participant,
            _ => return None,
        })
    }
}

/// One occurrence of a name in one memory revision. A projection: deleted
/// with its revision, rebuilt deterministically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityMention {
    pub id: EntityMentionId,
    pub entity_id: EntityId,
    pub surface: String,
    pub key: String,
    pub role: MentionRole,
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
}

/// A typed edge between entities, derived from one memory revision. It is
/// visible exactly when its source revision is (bitemporally and by scope).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub id: EdgeId,
    pub scope: Scope,
    pub from: EntityId,
    pub relation: String,
    pub to: EntityId,
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
    pub valid_time: TimeInterval,
}

/// Canonical relation type: lowercase words joined by `_` (`depends on` →
/// `depends_on`), at most 64 characters.
pub fn normalize_relation(predicate: &str) -> Option<String> {
    let words: Vec<String> = predicate
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let r = words.join("_");
    (!r.is_empty() && r.len() <= 64).then_some(r)
}

/// A name found in a revision, before identity resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedMention {
    pub surface: String,
    pub key: String,
    pub role: MentionRole,
}

/// An edge found in a revision, by entity keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedEdge {
    pub from: ProjectedMention,
    pub relation: String,
    pub to: ProjectedMention,
}

/// Everything one revision contributes to the entity graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub memory_id: MemoryId,
    pub revision_id: MemoryRevisionId,
    pub scope: Scope,
    pub partition: String,
    pub valid_time: TimeInterval,
    pub mentions: Vec<ProjectedMention>,
    pub edges: Vec<ProjectedEdge>,
}

/// Namespace for deterministic entity ids: an entity created from a name in
/// a partition always gets `uuid5(ns, partition + key)`, so concurrent
/// projections of the same name converge on one entity.
pub const ENTITY_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x6a1c_6f3e_2d4b_4c8a_9e57_3b0f_c5d2_e811);

pub fn entity_id_for(partition: &str, key: &str) -> EntityId {
    EntityId(uuid::Uuid::new_v5(
        &ENTITY_NAMESPACE,
        format!("{partition}\u{1f}{key}").as_bytes(),
    ))
}

/// Version of [`project_memory`]: which fields become mentions and edges.
pub const PROJECTION_VERSION: u32 = 1;

fn mention(surface: &str, role: MentionRole) -> Option<ProjectedMention> {
    let key = normalize_entity_name(surface)?;
    let surface = surface.trim();
    (surface.chars().count() <= 256).then(|| ProjectedMention {
        surface: surface.to_owned(),
        key,
        role,
    })
}

/// Deterministically project a memory revision into mentions and edges.
/// Structured fields only: free text is never parsed for entities here.
pub fn project_memory(m: &MemoryRecord) -> Projection {
    let mut mentions = Vec::new();
    let mut edges = Vec::new();
    let mut triple = |s: &str, p: &str, o: &str| {
        if let (Some(from), Some(relation), Some(to)) = (
            mention(s, MentionRole::Subject),
            normalize_relation(p),
            mention(o, MentionRole::Object),
        ) {
            if from.key != to.key {
                mentions.push(from.clone());
                mentions.push(to.clone());
                edges.push(ProjectedEdge { from, relation, to });
            }
        }
    };
    match &m.content {
        MemoryContent::Relational(r) => triple(&r.subject, &r.predicate, &r.object),
        MemoryContent::Semantic(s) => {
            if let (Some(sub), Some(pred), Some(obj)) = (&s.subject, &s.predicate, &s.object) {
                triple(sub, pred, obj);
            }
        }
        MemoryContent::Temporal(t) => triple(&t.subject, &t.predicate, &t.value),
        MemoryContent::Episodic(e) => {
            mentions.extend(
                e.participants
                    .iter()
                    .filter_map(|p| mention(p, MentionRole::Participant)),
            );
        }
        MemoryContent::Procedural(_) | MemoryContent::Working(_) => {}
    }
    mentions.sort_by(|a, b| {
        (&a.key, a.role.as_str(), &a.surface).cmp(&(&b.key, b.role.as_str(), &b.surface))
    });
    mentions.dedup();
    Projection {
        memory_id: m.id,
        revision_id: m.revision_id,
        scope: m.scope.clone(),
        partition: partition_key(&m.scope),
        valid_time: m.temporal.valid_time,
        mentions,
        edges,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> String {
        normalize_entity_name(s).unwrap()
    }

    #[test]
    fn separators_and_case_fold_together() {
        for s in [
            "auth-service",
            "auth_service",
            "AuthService",
            "The Auth Service",
            "  auth   service. ",
            "authservice",
        ] {
            assert_eq!(n(s), "authservice", "{s}");
        }
    }

    #[test]
    fn acronyms_and_paths() {
        assert_eq!(n("HTTPServer"), "httpserver");
        assert_eq!(n("cs_core::Scope"), "cscorescope");
        assert_eq!(n("PostgreSQL"), n("postgresql"));
        assert_eq!(
            n("TheTradeDesk"),
            "tradedesk",
            "camelCase reveals the article"
        );
    }

    #[test]
    fn distinct_things_stay_distinct() {
        assert_ne!(n("auth service"), n("auth services"));
        assert_ne!(n("cs-core"), n("cs-memory"));
        assert!(normalize_entity_name(" -- ").is_none());
        assert_eq!(n("The"), "the", "a lone article is a name, not an article");
    }

    #[test]
    fn meaningful_symbols_survive() {
        let keys = [n("C"), n("C++"), n("C#"), n("A+"), n("A")];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_ne!(n("v1.23"), n("v12.3"));
        assert_eq!(n("Node.js"), n("nodejs"));
        assert_eq!(n("A record"), "arecord", "only \"the\" is an article");
    }

    #[test]
    fn relations_normalize() {
        assert_eq!(
            normalize_relation("Depends On").as_deref(),
            Some("depends_on")
        );
        assert_eq!(
            normalize_relation("is-part-of").as_deref(),
            Some("is_part_of")
        );
        assert!(normalize_relation(" ... ").is_none());
    }
}
