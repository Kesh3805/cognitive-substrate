-- Cognitive Substrate: M4.3 knowledge compilation.
--
-- A derivation records how a compiled memory was produced: the operator,
-- the thresholds it applied, the support it found and the root memories
-- behind it. Independence keys are opaque hashes; no content is stored.

ALTER TABLE processing_jobs DROP CONSTRAINT processing_jobs_kind_check;
ALTER TABLE processing_jobs
    ADD CONSTRAINT processing_jobs_kind_check
    CHECK (kind IN ('perceive', 'resolve', 'embed', 'project', 'compile'));

CREATE TABLE derivations (
    id                 UUID PRIMARY KEY,
    tenant_id          UUID NOT NULL,
    owner_user_id      UUID,
    agent_id           UUID,
    project_id         UUID,
    conversation_id    UUID CHECK (conversation_id IS NULL),
    visibility         visibility NOT NULL,
    partition_key      TEXT NOT NULL,
    operator           TEXT NOT NULL,
    signature          TEXT NOT NULL,
    subject_entity_id  UUID NOT NULL,
    relation           TEXT NOT NULL,
    object_entity_id   UUID NOT NULL,
    -- Not a foreign key: purging the compiled memory deletes the
    -- derivation explicitly, in the same transaction.
    derived_memory_id  UUID NOT NULL,
    thresholds         JSONB NOT NULL,
    support            JSONB NOT NULL,
    witnesses          JSONB NOT NULL,
    status             TEXT NOT NULL
                       CHECK (status IN ('active', 'revalidation_required', 'invalidated')),
    status_reason      TEXT,
    version            INTEGER NOT NULL CHECK (version >= 1),
    created_at         TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL,
    UNIQUE (partition_key, operator, signature)
);

CREATE INDEX idx_derivations_derived ON derivations (derived_memory_id);
CREATE INDEX idx_derivations_tenant ON derivations (tenant_id, created_at DESC, id);

-- Every supporting root, for revalidation lookups on purge or revision.
CREATE TABLE derivation_roots (
    derivation_id   UUID NOT NULL REFERENCES derivations (id) ON DELETE CASCADE,
    root_memory_id  UUID NOT NULL,
    PRIMARY KEY (derivation_id, root_memory_id)
);

CREATE INDEX idx_derivation_roots_root ON derivation_roots (root_memory_id);
