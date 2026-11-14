//! Entities and the source-linked knowledge graph (M4.1 / M4.2).
//!
//! * **Mentions are not identity.** A projection job turns each memory
//!   revision's structured fields into mentions and edges; names resolve to
//!   entities by exact normalized key within a partition.
//! * **Identity is governed.** Aliases are added explicitly (evented,
//!   version-checked); merges happen only through accepted proposals.
//! * **Edges are evidence-bound.** An edge is visible exactly when the
//!   revision it came from is visible: same scope, current at the as-of
//!   transaction time, valid at the as-of valid time.
//! * **Traversal is bounded and deterministic.** Breadth-first, depth ≤ 3,
//!   with node, edge and path budgets; every reported path names the edges
//!   (and so the source revisions) it relies on.

use std::collections::{BTreeMap, HashMap, HashSet};

use cs_core::{
    AccessContext, CognitiveEventType, EdgeId, EntityId, Error, MemoryId, MemoryRevisionId,
    ProposalId, Result,
};
use cs_memory::{
    DetectionMetadata, Job, JobKind, MemoryRecord, MemoryStatus, ProposalStatus, RecordVersion,
    ResolutionKind, ResolutionOperation, ResolutionProposal, ResolutionRationale,
    entity::{
        AliasSource, ENTITY_NORMALIZATION_VERSION, Entity, EntityAlias, EntityKind, EntityMention,
        EntityStatus, KnowledgeEdge, normalize_entity_name, project_memory,
    },
    proposal::fingerprint,
};
use cs_storage::{EdgeDirection, EdgeQuery, JobEffects};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use crate::{
    CognitiveEngine,
    processing::{HandlerError, HandlerResult},
};

/// Version of the merge-candidate detector recorded on its proposals.
pub const ENTITY_MERGE_DETECTOR_VERSION: &str = "entity-stem/1";

/// Corporate and service suffixes ignored when comparing names for merge
/// candidates (`auth-service` ~ `auth`, `Acme Inc` ~ `Acme`).
const STEM_SUFFIXES: [&str; 9] = [
    "service", "svc", "inc", "corp", "corporation", "ltd", "llc", "app", "co",
];

/// Traversal limits: requests are clamped to these.
pub const MAX_DEPTH: u32 = 3;
pub const MAX_NODES: u32 = 1_000;
pub const MAX_EDGES: u32 = 5_000;
pub const MAX_PATHS: u32 = 1_000;

// ---- contracts --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityView {
    pub entity: Entity,
    pub aliases: Vec<EntityAlias>,
    pub mentions: Vec<EntityMention>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddAlias {
    pub name: String,
    pub expected_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetEntityKind {
    pub kind: EntityKind,
    pub expected_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeEntityMerge {
    pub survivor: EntityId,
    pub merged: EntityId,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NeighborhoodRequest {
    /// Hops from the root; default 2, at most 3.
    #[serde(default)]
    pub depth: Option<u32>,
    #[serde(default)]
    pub direction: EdgeDirection,
    /// Only these relation types (normalized); all if absent.
    #[serde(default)]
    pub relations: Option<Vec<String>>,
    /// Transaction-time as-of; default now.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub as_of: Option<OffsetDateTime>,
    /// Valid-time point; default now.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub valid_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub include_candidates: bool,
    #[serde(default)]
    pub max_nodes: Option<u32>,
    #[serde(default)]
    pub max_edges: Option<u32>,
    #[serde(default)]
    pub max_paths: Option<u32>,
}

/// One shortest path from the root, with the edges it relies on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphPath {
    pub entities: Vec<EntityId>,
    pub edges: Vec<EdgeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraversalLimit {
    MaxNodes,
    MaxEdges,
    MaxPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Neighborhood {
    pub root: EntityId,
    pub depth: u32,
    pub direction: EdgeDirection,
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub valid_at: OffsetDateTime,
    /// Reached entities including the root, ordered by id.
    pub nodes: Vec<Entity>,
    /// Edges in discovery order (depth, then relation, other entity, id).
    pub edges: Vec<KnowledgeEdge>,
    pub paths: Vec<GraphPath>,
    /// Budgets that cut the traversal short, if any.
    pub truncated_by: Vec<TraversalLimit>,
}

// ---- helpers ----------------------------------------------------------------

/// Name stem for merge-candidate detection.
pub fn identity_stem(key: &str) -> &str {
    for s in STEM_SUFFIXES {
        if let Some(stem) = key.strip_suffix(s) {
            if stem.chars().count() >= 3 {
                return stem;
            }
        }
    }
    key
}

fn bad_payload(e: serde_json::Error) -> HandlerError {
    HandlerError {
        permanent: true,
        message: format!("bad payload: {e}"),
    }
}

impl CognitiveEngine {
    /// The projection follow-up for a new revision, if it names anything.
    pub(crate) fn projection_job(&self, ctx: &AccessContext, m: &MemoryRecord) -> Option<Job> {
        if m.status == MemoryStatus::Tombstoned || project_memory(m).mentions.is_empty() {
            return None;
        }
        Some(self.new_job(
            ctx,
            JobKind::Project,
            format!("project:{}", m.revision_id),
            json!({ "memory_id": m.id, "revision_id": m.revision_id }),
        ))
    }

    /// `project` job handler: project one exact revision.
    pub(crate) async fn project(&self, job: &Job) -> HandlerResult {
        let memory_id: MemoryId =
            serde_json::from_value(job.payload["memory_id"].clone()).map_err(bad_payload)?;
        let revision_id: MemoryRevisionId =
            serde_json::from_value(job.payload["revision_id"].clone()).map_err(bad_payload)?;
        let mut effects = JobEffects::default();
        let history = self.store.memory_history(memory_id).await?;
        let Some(rev) = history.into_iter().find(|r| r.revision_id == revision_id) else {
            return Ok((effects, json!({ "skipped": "revision_purged" })));
        };
        let p = project_memory(&rev);
        let (mentions, edges) = (p.mentions.len(), p.edges.len());
        effects.events.push(self.draft(
            &job.principal,
            memory_id.0,
            &rev.scope,
            CognitiveEventType::ProjectionUpdated,
            json!({
                "memory_id": memory_id,
                "revision_id": revision_id,
                "mentions": mentions,
                "edges": edges,
                "projection_version": cs_memory::entity::PROJECTION_VERSION,
            }),
            self.now(),
        ));
        effects.projection = Some(p);
        Ok((effects, json!({ "mentions": mentions, "edges": edges })))
    }

    async fn readable_entity(&self, ctx: &AccessContext, id: EntityId) -> Result<Entity> {
        self.store
            .get_entity(id)
            .await?
            .filter(|e| ctx.can_read(&e.scope))
            .ok_or_else(|| Error::not_found("entity", id))
    }

    /// An entity with its aliases and most recent mentions. A merged
    /// entity is returned as-is; its status names the survivor.
    pub async fn get_entity(&self, ctx: &AccessContext, id: EntityId) -> Result<EntityView> {
        let entity = self.readable_entity(ctx, id).await?;
        Ok(EntityView {
            aliases: self.store.entity_aliases(id).await?,
            mentions: self.store.entity_mentions(id, 50).await?,
            entity,
        })
    }

    /// Entities the caller can read whose names normalize to `name`.
    pub async fn find_entities(&self, ctx: &AccessContext, name: &str) -> Result<Vec<Entity>> {
        let key = normalize_entity_name(name)
            .ok_or_else(|| Error::validation("name has no identifying characters"))?;
        self.store.entities_by_key(ctx, &key).await
    }

    pub async fn add_entity_alias(
        &self,
        ctx: &AccessContext,
        id: EntityId,
        req: AddAlias,
    ) -> Result<Entity> {
        let e = self.readable_entity(ctx, id).await?;
        ctx.ensure_write(&e.scope)?;
        let surface = req.name.trim();
        if surface.chars().count() > 256 {
            return Err(Error::validation("alias exceeds 256 characters"));
        }
        let key = normalize_entity_name(surface)
            .ok_or_else(|| Error::validation("name has no identifying characters"))?;
        let now = self.now();
        let event = self.draft(
            ctx,
            id.0,
            &e.scope,
            CognitiveEventType::EntityAliasAdded,
            json!({ "entity_id": id, "surface": surface, "key": key, "version": req.expected_version + 1 }),
            now,
        );
        let alias = EntityAlias {
            entity_id: id,
            surface: surface.to_owned(),
            key,
            normalization_version: ENTITY_NORMALIZATION_VERSION,
            source: AliasSource::Explicit,
            created_at: now,
        };
        self.store
            .add_entity_alias(id, req.expected_version, &alias, event)
            .await
    }

    pub async fn set_entity_kind(
        &self,
        ctx: &AccessContext,
        id: EntityId,
        req: SetEntityKind,
    ) -> Result<Entity> {
        let e = self.readable_entity(ctx, id).await?;
        ctx.ensure_write(&e.scope)?;
        if e.kind == req.kind && e.version == req.expected_version {
            return Ok(e); // idempotent
        }
        let now = self.now();
        let event = self.draft(
            ctx,
            id.0,
            &e.scope,
            CognitiveEventType::EntityKindChanged,
            json!({ "entity_id": id, "kind": req.kind, "previous": e.kind, "version": req.expected_version + 1 }),
            now,
        );
        self.store
            .set_entity_kind(id, req.expected_version, req.kind, now, event)
            .await
    }

    fn merge_proposal(
        &self,
        survivor: &Entity,
        merged: &Entity,
        rationale: ResolutionRationale,
        detector: &str,
        score: Option<f64>,
    ) -> ResolutionProposal {
        let now = self.now();
        let mut based_on = vec![
            RecordVersion::Entity {
                id: survivor.id,
                version: survivor.version,
            },
            RecordVersion::Entity {
                id: merged.id,
                version: merged.version,
            },
        ];
        based_on.sort();
        ResolutionProposal {
            id: ProposalId::new_v7(),
            scope: survivor.scope.clone(),
            kind: ResolutionKind::EntityMerge,
            status: ProposalStatus::Pending,
            operation: ResolutionOperation::MergeEntities {
                survivor: survivor.id,
                merged: merged.id,
            },
            rationale,
            detection: DetectionMetadata {
                detector_version: detector.to_owned(),
                score,
                detected_at: now,
            },
            fingerprint: fingerprint(ResolutionKind::EntityMerge, &based_on),
            based_on,
            created_at: now,
            decided_at: None,
            decided_by: None,
            decision_reason: None,
        }
    }

    async fn record_proposal(
        &self,
        ctx: &AccessContext,
        p: ResolutionProposal,
    ) -> Result<ResolutionProposal> {
        p.validate()?;
        let event = self.draft(
            ctx,
            p.id.0,
            &p.scope,
            CognitiveEventType::ProposalCreated,
            json!({ "proposal_id": p.id, "kind": p.kind, "operation": p.operation }),
            p.created_at,
        );
        self.store.insert_proposal(&p, event).await
    }

    async fn mergeable_pair(
        &self,
        ctx: &AccessContext,
        survivor: EntityId,
        merged: EntityId,
    ) -> Result<(Entity, Entity)> {
        if survivor == merged {
            return Err(Error::validation("an entity cannot be merged into itself"));
        }
        let s = self.readable_entity(ctx, survivor).await?;
        let m = self.readable_entity(ctx, merged).await?;
        ctx.ensure_write(&s.scope)?;
        ctx.ensure_write(&m.scope)?;
        if s.partition != m.partition {
            return Err(Error::validation(
                "entities in different partitions cannot be merged",
            ));
        }
        if s.status != EntityStatus::Active || m.status != EntityStatus::Active {
            return Err(Error::conflict("only active entities can be merged"));
        }
        Ok((s, m))
    }

    /// Propose merging two entities. Nothing changes until the proposal is
    /// accepted; acceptance re-checks both entity versions.
    pub async fn propose_entity_merge(
        &self,
        ctx: &AccessContext,
        req: ProposeEntityMerge,
    ) -> Result<ResolutionProposal> {
        if req.reason.trim().is_empty() {
            return Err(Error::validation("a merge proposal needs a reason"));
        }
        let (s, m) = self.mergeable_pair(ctx, req.survivor, req.merged).await?;
        let p = self.merge_proposal(
            &s,
            &m,
            ResolutionRationale {
                explanation: req.reason,
                signals: Vec::new(),
            },
            "explicit",
            None,
        );
        self.record_proposal(ctx, p).await
    }

    /// Propose merges for entities in `id`'s partition whose names share a
    /// stem with it (`auth` / `auth-service`). The shorter key survives;
    /// ties go to the lower id. Deterministic and idempotent.
    pub async fn detect_entity_merges(
        &self,
        ctx: &AccessContext,
        id: EntityId,
    ) -> Result<Vec<ResolutionProposal>> {
        let e = self.readable_entity(ctx, id).await?;
        ctx.ensure_write(&e.scope)?;
        if e.status != EntityStatus::Active {
            return Ok(Vec::new());
        }
        let stem = identity_stem(&e.key);
        let mut out = Vec::new();
        for other in self.store.partition_entities(&e.partition, 10_000).await? {
            if other.id == e.id
                || other.status != EntityStatus::Active
                || identity_stem(&other.key) != stem
                || !ctx.can_write(&other.scope)
            {
                continue;
            }
            let (s, m) = if (e.key.len(), e.id) <= (other.key.len(), other.id) {
                (&e, &other)
            } else {
                (&other, &e)
            };
            let rationale = ResolutionRationale {
                explanation: format!(
                    "\"{}\" and \"{}\" share the name stem \"{stem}\"",
                    s.canonical_name, m.canonical_name
                ),
                signals: Vec::new(),
            };
            let p = self.merge_proposal(s, m, rationale, ENTITY_MERGE_DETECTOR_VERSION, None);
            out.push(self.record_proposal(ctx, p).await?);
        }
        Ok(out)
    }

    /// Bounded breadth-first neighborhood of an entity.
    pub async fn entity_neighborhood(
        &self,
        ctx: &AccessContext,
        id: EntityId,
        req: NeighborhoodRequest,
    ) -> Result<Neighborhood> {
        let root = self.readable_entity(ctx, id).await?;
        let depth = req.depth.unwrap_or(2).clamp(1, MAX_DEPTH);
        let max_nodes = req.max_nodes.unwrap_or(200).clamp(1, MAX_NODES) as usize;
        let max_edges = req.max_edges.unwrap_or(1_000).clamp(1, MAX_EDGES) as usize;
        let max_paths = req.max_paths.unwrap_or(100).clamp(1, MAX_PATHS) as usize;
        let relations = match req.relations {
            Some(rs) => Some(
                rs.iter()
                    .map(|r| {
                        cs_memory::entity::normalize_relation(r)
                            .ok_or_else(|| Error::validation("empty relation filter"))
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            None => None,
        };
        let now = self.now();
        let as_of = req.as_of.unwrap_or(now);
        let valid_at = req.valid_at.unwrap_or(now);

        let mut truncated_by = Vec::new();
        let note = |l: TraversalLimit, t: &mut Vec<TraversalLimit>| {
            if !t.contains(&l) {
                t.push(l);
            }
        };
        let mut nodes = BTreeMap::from([(root.id, root.clone())]);
        let mut order = Vec::new();
        let mut paths: HashMap<EntityId, GraphPath> = HashMap::from([(
            root.id,
            GraphPath {
                entities: vec![root.id],
                edges: Vec::new(),
            },
        )]);
        let mut edges: Vec<KnowledgeEdge> = Vec::new();
        let mut seen_edges = HashSet::new();
        let mut frontier = vec![root.id];

        for _ in 0..depth {
            if frontier.is_empty() {
                break;
            }
            let remaining = max_edges - edges.len();
            if remaining == 0 {
                note(TraversalLimit::MaxEdges, &mut truncated_by);
                break;
            }
            let batch = self
                .store
                .entity_edges(&EdgeQuery {
                    access: ctx.clone(),
                    entity_ids: frontier.clone(),
                    direction: req.direction,
                    relations: relations.clone(),
                    as_of_transaction_time: as_of,
                    as_of_valid_time: valid_at,
                    include_candidates: req.include_candidates,
                    limit: u32::try_from(remaining + 1).unwrap_or(u32::MAX),
                })
                .await?;
            if batch.len() > remaining {
                note(TraversalLimit::MaxEdges, &mut truncated_by);
            }
            let in_frontier: HashSet<EntityId> = frontier.iter().copied().collect();
            let mut next = Vec::new();
            for e in batch.into_iter().take(remaining) {
                if !seen_edges.insert(e.id) {
                    continue;
                }
                let (here, other) = match req.direction {
                    EdgeDirection::Out => (e.from, e.to),
                    EdgeDirection::In => (e.to, e.from),
                    EdgeDirection::Both if in_frontier.contains(&e.from) => (e.from, e.to),
                    EdgeDirection::Both => (e.to, e.from),
                };
                if !nodes.contains_key(&other) {
                    if nodes.len() >= max_nodes {
                        note(TraversalLimit::MaxNodes, &mut truncated_by);
                        continue;
                    }
                    let Some(entity) = self
                        .store
                        .get_entity(other)
                        .await?
                        .filter(|x| ctx.can_read(&x.scope))
                    else {
                        continue; // never expand through what the caller cannot read
                    };
                    let mut path = paths[&here].clone();
                    path.entities.push(other);
                    path.edges.push(e.id);
                    paths.insert(other, path);
                    nodes.insert(other, entity);
                    order.push(other);
                    next.push(other);
                }
                edges.push(e);
            }
            next.sort();
            frontier = next;
        }
        if order.len() > max_paths {
            note(TraversalLimit::MaxPaths, &mut truncated_by);
        }
        let paths = order
            .iter()
            .take(max_paths)
            .filter_map(|id| paths.remove(id))
            .collect();
        Ok(Neighborhood {
            root: root.id,
            depth,
            direction: req.direction,
            as_of,
            valid_at,
            nodes: nodes.into_values().collect(),
            edges,
            paths,
            truncated_by,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_strip_one_suffix_but_keep_short_names() {
        assert_eq!(identity_stem("authservice"), "auth");
        assert_eq!(identity_stem("acmeinc"), "acme");
        assert_eq!(identity_stem("auth"), "auth");
        // "co" alone would leave too little.
        assert_eq!(identity_stem("abco"), "abco");
    }
}
