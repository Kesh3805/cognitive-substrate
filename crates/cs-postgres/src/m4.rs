//! M4 persistence: entities, aliases, and the source-linked projection
//! (mentions and edges).
//!
//! Lock order extends M3's: … → memory_records → entities (by id) →
//! entity_aliases → resolution_proposals → cognitive_events. Merges lock
//! both entities in id order.

use cs_core::{
    AccessContext, EdgeId, EntityId, EntityMentionId, Error, EventDraft, MemoryId,
    MemoryRevisionId, Result,
};
use cs_memory::entity::{
    AliasSource, ENTITY_NORMALIZATION_VERSION, Entity, EntityAlias, EntityKind, EntityMention,
    EntityStatus, KnowledgeEdge, MentionRole, PROJECTION_VERSION, Projection, entity_id_for,
    partition_scope,
};
use cs_storage::{EdgeDirection, EdgeQuery, EntityMerge};
use sqlx::{Row, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PgStore, Tx, access_sql, append_event, db_err, from_range, parse, project_ids, scope_from_row,
    to_range,
};

const ENTITY_COLS: &str = "id, tenant_id, owner_user_id, agent_id, project_id, conversation_id, \
    visibility::text AS visibility, partition_key, kind, canonical_name, key, merged_into, version, \
    created_at, updated_at";

const ALIAS_COLS: &str =
    "partition_key, key, entity_id, surface, normalization_version, source, created_at";

const MENTION_COLS: &str = "id, entity_id, surface, key, role, memory_id, revision_id";

fn entity_from_row(r: &PgRow) -> Result<Entity> {
    let kind: String = r.try_get("kind").map_err(db_err)?;
    let merged: Option<Uuid> = r.try_get("merged_into").map_err(db_err)?;
    Ok(Entity {
        id: EntityId(r.try_get("id").map_err(db_err)?),
        scope: scope_from_row(r)?,
        partition: r.try_get("partition_key").map_err(db_err)?,
        kind: parse(&kind, EntityKind::parse, "entity kind")?,
        canonical_name: r.try_get("canonical_name").map_err(db_err)?,
        key: r.try_get("key").map_err(db_err)?,
        status: merged.map_or(EntityStatus::Active, |into| EntityStatus::Merged {
            into: EntityId(into),
        }),
        version: u32::try_from(r.try_get::<i32, _>("version").map_err(db_err)?)
            .map_err(Error::storage)?,
        created_at: r.try_get("created_at").map_err(db_err)?,
        updated_at: r.try_get("updated_at").map_err(db_err)?,
    })
}

fn alias_from_row(r: &PgRow) -> Result<EntityAlias> {
    let source: String = r.try_get("source").map_err(db_err)?;
    Ok(EntityAlias {
        entity_id: EntityId(r.try_get("entity_id").map_err(db_err)?),
        surface: r.try_get("surface").map_err(db_err)?,
        key: r.try_get("key").map_err(db_err)?,
        normalization_version: u32::try_from(
            r.try_get::<i32, _>("normalization_version")
                .map_err(db_err)?,
        )
        .map_err(Error::storage)?,
        source: parse(&source, AliasSource::parse, "alias source")?,
        created_at: r.try_get("created_at").map_err(db_err)?,
    })
}

fn mention_from_row(r: &PgRow) -> Result<EntityMention> {
    let role: String = r.try_get("role").map_err(db_err)?;
    Ok(EntityMention {
        id: EntityMentionId(r.try_get("id").map_err(db_err)?),
        entity_id: EntityId(r.try_get("entity_id").map_err(db_err)?),
        surface: r.try_get("surface").map_err(db_err)?,
        key: r.try_get("key").map_err(db_err)?,
        role: parse(&role, MentionRole::parse, "mention role")?,
        memory_id: MemoryId(r.try_get("memory_id").map_err(db_err)?),
        revision_id: MemoryRevisionId(r.try_get("revision_id").map_err(db_err)?),
    })
}

fn edge_from_row(r: &PgRow) -> Result<KnowledgeEdge> {
    Ok(KnowledgeEdge {
        id: EdgeId(r.try_get("id").map_err(db_err)?),
        scope: scope_from_row(r)?,
        from: EntityId(r.try_get("from_entity_id").map_err(db_err)?),
        relation: r.try_get("relation").map_err(db_err)?,
        to: EntityId(r.try_get("to_entity_id").map_err(db_err)?),
        memory_id: MemoryId(r.try_get("memory_id").map_err(db_err)?),
        revision_id: MemoryRevisionId(r.try_get("revision_id").map_err(db_err)?),
        valid_time: from_range(r.try_get("valid_time").map_err(db_err)?)?,
    })
}

async fn lock_entity(tx: &mut Tx<'_>, id: EntityId) -> Result<Entity> {
    let row = sqlx::query(&format!(
        "SELECT {ENTITY_COLS} FROM entities WHERE id = $1 FOR UPDATE"
    ))
    .bind(id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?
    .ok_or_else(|| Error::not_found("entity", id))?;
    entity_from_row(&row)
}

async fn bump(tx: &mut Tx<'_>, id: EntityId, at: OffsetDateTime) -> Result<()> {
    sqlx::query("UPDATE entities SET version = version + 1, updated_at = $2 WHERE id = $1")
        .bind(id.0)
        .bind(at)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Follow merge redirects to the surviving entity.
async fn canonical(tx: &mut Tx<'_>, mut id: EntityId) -> Result<EntityId> {
    for _ in 0..64 {
        let next: Option<Option<Uuid>> =
            sqlx::query_scalar("SELECT merged_into FROM entities WHERE id = $1")
                .bind(id.0)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_err)?;
        match next.flatten() {
            Some(into) => id = EntityId(into),
            None => break,
        }
    }
    Ok(id)
}

/// Resolve a name to an entity in the projection's partition, creating the
/// entity (deterministic id) and its canonical alias if the name is new.
async fn resolve(
    tx: &mut Tx<'_>,
    p: &Projection,
    surface: &str,
    key: &str,
    now: OffsetDateTime,
) -> Result<EntityId> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT entity_id FROM entity_aliases WHERE partition_key = $1 AND key = $2",
    )
    .bind(&p.partition)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;
    let id = match existing {
        Some(id) => EntityId(id),
        None => {
            let id = entity_id_for(&p.partition, key);
            let s = partition_scope(&p.scope);
            sqlx::query(
                "INSERT INTO entities (id, tenant_id, owner_user_id, agent_id, project_id, \
                 conversation_id, visibility, partition_key, kind, canonical_name, key, version, \
                 created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, NULL, $6::visibility, $7, 'unknown', $8, $9, 1, $10, $10) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(id.0)
            .bind(s.tenant_id.0)
            .bind(s.owner_user_id.map(|u| u.0))
            .bind(s.agent_id.map(|a| a.0))
            .bind(s.project_id.map(|x| x.0))
            .bind(s.visibility.as_str())
            .bind(&p.partition)
            .bind(surface)
            .bind(key)
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "INSERT INTO entity_aliases (partition_key, key, entity_id, surface, \
                 normalization_version, source, created_at) \
                 VALUES ($1, $2, $3, $4, $5, 'canonical', $6) ON CONFLICT DO NOTHING",
            )
            .bind(&p.partition)
            .bind(key)
            .bind(id.0)
            .bind(surface)
            .bind(ENTITY_NORMALIZATION_VERSION as i32)
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
            // A concurrent writer may have claimed the name first.
            let winner: Uuid = sqlx::query_scalar(
                "SELECT entity_id FROM entity_aliases WHERE partition_key = $1 AND key = $2",
            )
            .bind(&p.partition)
            .bind(key)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_err)?;
            EntityId(winner)
        }
    };
    canonical(tx, id).await
}

/// Apply a projection idempotently. A revision projected before, or one that
/// no longer exists (purged), is skipped.
pub(crate) async fn apply_projection(
    tx: &mut Tx<'_>,
    p: &Projection,
    now: OffsetDateTime,
) -> Result<()> {
    let claimed = sqlx::query(
        "INSERT INTO entity_projections (revision_id, projection_version, projected_at) \
         SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM memory_records WHERE revision_id = $1) \
         ON CONFLICT (revision_id) DO NOTHING",
    )
    .bind(p.revision_id.0)
    .bind(PROJECTION_VERSION as i32)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    for m in &p.mentions {
        let entity = resolve(tx, p, &m.surface, &m.key, now).await?;
        sqlx::query(
            "INSERT INTO entity_mentions (id, entity_id, surface, key, role, memory_id, revision_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (revision_id, key, role) DO NOTHING",
        )
        .bind(EntityMentionId::new_v7().0)
        .bind(entity.0)
        .bind(&m.surface)
        .bind(&m.key)
        .bind(m.role.as_str())
        .bind(p.memory_id.0)
        .bind(p.revision_id.0)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    for e in &p.edges {
        let from = resolve(tx, p, &e.from.surface, &e.from.key, now).await?;
        let to = resolve(tx, p, &e.to.surface, &e.to.key, now).await?;
        if from == to || !seen.insert((from, e.relation.clone(), to)) {
            continue;
        }
        let s = &p.scope;
        sqlx::query(
            "INSERT INTO knowledge_edges (id, tenant_id, owner_user_id, agent_id, project_id, \
             conversation_id, visibility, from_entity_id, relation, to_entity_id, memory_id, \
             revision_id, valid_time) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::visibility, $8, $9, $10, $11, $12, $13)",
        )
        .bind(EdgeId::new_v7().0)
        .bind(s.tenant_id.0)
        .bind(s.owner_user_id.map(|u| u.0))
        .bind(s.agent_id.map(|a| a.0))
        .bind(s.project_id.map(|x| x.0))
        .bind(s.conversation_id.map(|c| c.0))
        .bind(s.visibility.as_str())
        .bind(from.0)
        .bind(&e.relation)
        .bind(to.0)
        .bind(p.memory_id.0)
        .bind(p.revision_id.0)
        .bind(to_range(&p.valid_time))
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    Ok(())
}

/// Validate and apply one accepted entity merge inside the decision commit.
pub(crate) async fn merge_entities(tx: &mut Tx<'_>, m: &EntityMerge) -> Result<()> {
    if m.survivor == m.merged {
        return Err(Error::validation("an entity cannot be merged into itself"));
    }
    let (first, second) = if m.survivor < m.merged {
        (m.survivor, m.merged)
    } else {
        (m.merged, m.survivor)
    };
    let a = lock_entity(tx, first).await?;
    let b = lock_entity(tx, second).await?;
    let (survivor, merged) = if a.id == m.survivor { (a, b) } else { (b, a) };
    if survivor.partition != merged.partition {
        return Err(Error::validation(
            "entities in different partitions cannot be merged",
        ));
    }
    if survivor.version != m.survivor_version
        || merged.version != m.merged_version
        || survivor.status != EntityStatus::Active
        || merged.status != EntityStatus::Active
    {
        return Err(Error::conflict("entity changed since the proposal"));
    }
    let (s, x) = (m.survivor.0, m.merged.0);
    sqlx::query(
        "UPDATE entities SET merged_into = $2, version = version + 1, updated_at = $3 WHERE id = $1",
    )
    .bind(x)
    .bind(s)
    .bind(m.at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    bump(tx, m.survivor, m.at).await?;
    for sql in [
        "UPDATE entity_aliases SET entity_id = $2, source = 'merge' WHERE entity_id = $1",
        "UPDATE entity_mentions SET entity_id = $2 WHERE entity_id = $1",
        // Edges between the two would become self-loops: drop them.
        "DELETE FROM knowledge_edges WHERE (from_entity_id = $1 AND to_entity_id = $2) \
             OR (from_entity_id = $2 AND to_entity_id = $1)",
        "UPDATE knowledge_edges SET from_entity_id = $2 WHERE from_entity_id = $1",
        "UPDATE knowledge_edges SET to_entity_id = $2 WHERE to_entity_id = $1",
    ] {
        sqlx::query(sql)
            .bind(x)
            .bind(s)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

// ---- operations (called from the CognitiveStore impl) --------------------------

impl PgStore {
    pub(crate) async fn m4_get_entity(&self, id: EntityId) -> Result<Option<Entity>> {
        sqlx::query(&format!("SELECT {ENTITY_COLS} FROM entities WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?
            .as_ref()
            .map(entity_from_row)
            .transpose()
    }

    pub(crate) async fn m4_entities_by_key(
        &self,
        access: &AccessContext,
        key: &str,
    ) -> Result<Vec<Entity>> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT DISTINCT entity_id FROM entity_aliases WHERE key = $1")
                .bind(key)
                .fetch_all(&mut *tx)
                .await
                .map_err(db_err)?;
        let mut canon = Vec::with_capacity(ids.len());
        for id in ids {
            canon.push(canonical(&mut tx, EntityId(id)).await?.0);
        }
        tx.commit().await.map_err(db_err)?;
        let clause = access_sql(super::TABLE_ACCESS_COLS, [2, 3, 4, 5]);
        sqlx::query(&format!(
            "SELECT {ENTITY_COLS} FROM entities WHERE id = ANY($1) AND {clause} ORDER BY id"
        ))
        .bind(canon)
        .bind(access.tenant_id.0)
        .bind(access.user_id.map(|u| u.0))
        .bind(access.agent_id.map(|a| a.0))
        .bind(project_ids(access))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(entity_from_row)
        .collect()
    }

    pub(crate) async fn m4_entity_aliases(&self, id: EntityId) -> Result<Vec<EntityAlias>> {
        sqlx::query(&format!(
            "SELECT {ALIAS_COLS} FROM entity_aliases WHERE entity_id = $1 ORDER BY key COLLATE \"C\""
        ))
        .bind(id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(alias_from_row)
        .collect()
    }

    pub(crate) async fn m4_entity_mentions(
        &self,
        id: EntityId,
        limit: u32,
    ) -> Result<Vec<EntityMention>> {
        sqlx::query(&format!(
            "SELECT {MENTION_COLS} FROM entity_mentions WHERE entity_id = $1 ORDER BY id LIMIT $2"
        ))
        .bind(id.0)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(mention_from_row)
        .collect()
    }

    pub(crate) async fn m4_add_entity_alias(
        &self,
        id: EntityId,
        expected_version: u32,
        alias: &EntityAlias,
        event: EventDraft,
    ) -> Result<Entity> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let e = lock_entity(&mut tx, id).await?;
        if e.version != expected_version || e.status != EntityStatus::Active {
            return Err(Error::conflict("entity changed; reload it"));
        }
        let holder: Option<Uuid> = sqlx::query_scalar(
            "SELECT entity_id FROM entity_aliases WHERE partition_key = $1 AND key = $2 FOR UPDATE",
        )
        .bind(&e.partition)
        .bind(&alias.key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if let Some(holder) = holder {
            if canonical(&mut tx, EntityId(holder)).await? == id {
                return Ok(e); // idempotent
            }
            return Err(Error::conflict(
                "that name already identifies another entity in this partition",
            ));
        }
        sqlx::query(
            "INSERT INTO entity_aliases (partition_key, key, entity_id, surface, \
             normalization_version, source, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&e.partition)
        .bind(&alias.key)
        .bind(id.0)
        .bind(&alias.surface)
        .bind(alias.normalization_version as i32)
        .bind(alias.source.as_str())
        .bind(alias.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        bump(&mut tx, id, alias.created_at).await?;
        let out = lock_entity(&mut tx, id).await?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(out)
    }

    pub(crate) async fn m4_set_entity_kind(
        &self,
        id: EntityId,
        expected_version: u32,
        kind: EntityKind,
        at: OffsetDateTime,
        event: EventDraft,
    ) -> Result<Entity> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let e = lock_entity(&mut tx, id).await?;
        if e.version != expected_version || e.status != EntityStatus::Active {
            return Err(Error::conflict("entity changed; reload it"));
        }
        let row = sqlx::query(&format!(
            "UPDATE entities SET kind = $2, version = version + 1, updated_at = $3 \
             WHERE id = $1 RETURNING {ENTITY_COLS}"
        ))
        .bind(id.0)
        .bind(kind.as_str())
        .bind(at)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        let out = entity_from_row(&row)?;
        append_event(&mut tx, event).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(out)
    }

    pub(crate) async fn m4_partition_entities(
        &self,
        partition: &str,
        limit: u32,
    ) -> Result<Vec<Entity>> {
        sqlx::query(&format!(
            "SELECT {ENTITY_COLS} FROM entities WHERE partition_key = $1 ORDER BY id LIMIT $2"
        ))
        .bind(partition)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(entity_from_row)
        .collect()
    }

    pub(crate) async fn m4_entity_edges(&self, q: &EdgeQuery) -> Result<Vec<KnowledgeEdge>> {
        let clause = access_sql(
            [
                "e.tenant_id",
                "e.owner_user_id",
                "e.agent_id",
                "e.project_id",
                "e.visibility::text",
            ],
            [1, 2, 3, 4],
        );
        let (dir, other) = match q.direction {
            EdgeDirection::Out => ("e.from_entity_id = ANY($5)", "e.to_entity_id"),
            EdgeDirection::In => ("e.to_entity_id = ANY($5)", "e.from_entity_id"),
            EdgeDirection::Both => (
                "(e.from_entity_id = ANY($5) OR e.to_entity_id = ANY($5))",
                "CASE WHEN e.from_entity_id = ANY($5) THEN e.to_entity_id ELSE e.from_entity_id END",
            ),
        };
        sqlx::query(&format!(
            "SELECT e.id, e.tenant_id, e.owner_user_id, e.agent_id, e.project_id, \
                 e.conversation_id, e.visibility::text AS visibility, e.from_entity_id, e.relation, \
                 e.to_entity_id, e.memory_id, e.revision_id, e.valid_time \
             FROM knowledge_edges e JOIN memory_records m ON m.revision_id = e.revision_id \
             WHERE {dir} AND {clause} \
               AND ($6::text[] IS NULL OR e.relation = ANY($6)) \
               AND e.valid_time @> $7 AND m.transaction_time @> $8 \
               AND (m.status = 'active' OR ($9 AND m.status = 'candidate')) \
             ORDER BY e.relation COLLATE \"C\", {other}, e.id LIMIT $10"
        ))
        .bind(q.access.tenant_id.0)
        .bind(q.access.user_id.map(|u| u.0))
        .bind(q.access.agent_id.map(|a| a.0))
        .bind(project_ids(&q.access))
        .bind(q.entity_ids.iter().map(|e| e.0).collect::<Vec<_>>())
        .bind(q.relations.clone())
        .bind(q.as_of_valid_time)
        .bind(q.as_of_transaction_time)
        .bind(q.include_candidates)
        .bind(i64::from(q.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .iter()
        .map(edge_from_row)
        .collect()
    }
}
