-- Cognitive Substrate: M4.1 entities and the source-linked knowledge graph.
--
-- Identity (entities, aliases) is governed state: it changes only through
-- evented, version-checked operations and accepted merge proposals.
-- Mentions and edges are a *projection* of memory revisions: derived,
-- rebuildable, and deleted with the revision they came from.

ALTER TABLE processing_jobs DROP CONSTRAINT processing_jobs_kind_check;
ALTER TABLE processing_jobs
    ADD CONSTRAINT processing_jobs_kind_check
    CHECK (kind IN ('perceive', 'resolve', 'embed', 'project'));

ALTER TABLE resolution_proposals DROP CONSTRAINT resolution_proposals_kind_check;
ALTER TABLE resolution_proposals
    ADD CONSTRAINT resolution_proposals_kind_check
    CHECK (kind IN ('duplicate', 'supersession', 'contradiction', 'entity_merge'));

-- ---------------------------------------------------------------------------
-- Identity
-- ---------------------------------------------------------------------------

CREATE TABLE entities (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    -- Entities are partition-scoped; never conversation-scoped.
    conversation_id  UUID CHECK (conversation_id IS NULL),
    visibility       visibility NOT NULL,
    partition_key    TEXT NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('unknown', 'person', 'organization', 'team',
                         'service', 'component', 'technology', 'project', 'document',
                         'concept', 'location')),
    canonical_name   TEXT NOT NULL CHECK (length(canonical_name) BETWEEN 1 AND 512),
    key              TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 512),
    merged_into      UUID REFERENCES entities (id),
    version          INTEGER NOT NULL CHECK (version >= 1),
    created_at       TIMESTAMPTZ NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL,
    CHECK (merged_into IS NULL OR merged_into <> id)
);

CREATE INDEX idx_entities_partition ON entities (partition_key, id);

-- One name (normalized key) identifies at most one entity per partition.
CREATE TABLE entity_aliases (
    partition_key          TEXT NOT NULL,
    key                    TEXT NOT NULL,
    entity_id              UUID NOT NULL REFERENCES entities (id),
    surface                TEXT NOT NULL,
    normalization_version  INTEGER NOT NULL,
    source                 TEXT NOT NULL CHECK (source IN ('canonical', 'explicit', 'merge')),
    created_at             TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (partition_key, key)
);

CREATE INDEX idx_entity_aliases_entity ON entity_aliases (entity_id);
CREATE INDEX idx_entity_aliases_key ON entity_aliases (key);

-- ---------------------------------------------------------------------------
-- Projection (derived from memory revisions)
-- ---------------------------------------------------------------------------

-- Marks a revision as projected; makes the projection job idempotent.
CREATE TABLE entity_projections (
    revision_id         UUID PRIMARY KEY REFERENCES memory_records (revision_id) ON DELETE CASCADE,
    projection_version  INTEGER NOT NULL,
    projected_at        TIMESTAMPTZ NOT NULL
);

CREATE TABLE entity_mentions (
    id           UUID PRIMARY KEY,
    entity_id    UUID NOT NULL REFERENCES entities (id),
    surface      TEXT NOT NULL,
    key          TEXT NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('subject', 'object', 'participant')),
    memory_id    UUID NOT NULL,
    revision_id  UUID NOT NULL REFERENCES memory_records (revision_id) ON DELETE CASCADE,
    UNIQUE (revision_id, key, role)
);

CREATE INDEX idx_entity_mentions_entity ON entity_mentions (entity_id, id);

CREATE TABLE knowledge_edges (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    conversation_id  UUID,
    visibility       visibility NOT NULL,
    from_entity_id   UUID NOT NULL REFERENCES entities (id),
    relation         TEXT NOT NULL CHECK (length(relation) BETWEEN 1 AND 64),
    to_entity_id     UUID NOT NULL REFERENCES entities (id),
    memory_id        UUID NOT NULL,
    revision_id      UUID NOT NULL REFERENCES memory_records (revision_id) ON DELETE CASCADE,
    valid_time       TSTZRANGE NOT NULL,
    CHECK (from_entity_id <> to_entity_id)
);

CREATE INDEX idx_edges_from ON knowledge_edges (from_entity_id, relation);
CREATE INDEX idx_edges_to ON knowledge_edges (to_entity_id, relation);
CREATE INDEX idx_edges_revision ON knowledge_edges (revision_id);
