-- Cognitive Substrate: M2 baseline schema.
--
-- PostgreSQL is the authoritative store. Embeddings are a derived index.
-- Revisions are bitemporal: valid_time (world) and transaction_time
-- (substrate), both half-open [start, end) ranges. A revision is current
-- when upper_inf(transaction_time). History is never overwritten; a
-- revision is superseded by closing its transaction-time range.

CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE TYPE memory_domain AS ENUM
    ('episodic', 'semantic', 'procedural', 'relational', 'temporal', 'working');
CREATE TYPE memory_status AS ENUM ('candidate', 'active', 'rejected', 'tombstoned');
CREATE TYPE visibility AS ENUM ('private', 'agent', 'project', 'tenant');
CREATE TYPE belief_status AS ENUM ('active', 'disputed', 'retracted');
CREATE TYPE evidence_direction AS ENUM ('supports', 'refutes', 'contextual');

-- ---------------------------------------------------------------------------
-- Memory records
-- ---------------------------------------------------------------------------

CREATE TABLE memory_records (
    revision_id                UUID PRIMARY KEY,
    memory_id                  UUID NOT NULL,
    revision_number            INTEGER NOT NULL CHECK (revision_number > 0),

    tenant_id                  UUID NOT NULL,
    owner_user_id              UUID,
    agent_id                   UUID,
    project_id                 UUID,
    conversation_id            UUID,
    visibility                 visibility NOT NULL,

    domain                     memory_domain NOT NULL,
    content                    JSONB NOT NULL,
    provenance                 JSONB NOT NULL,

    valid_time                 TSTZRANGE NOT NULL,
    transaction_time           TSTZRANGE NOT NULL,

    status                     memory_status NOT NULL,
    status_reason              TEXT,
    supersedes_revision_id     UUID REFERENCES memory_records (revision_id),
    superseded_by_revision_id  UUID,

    created_at                 TIMESTAMPTZ NOT NULL,
    expires_at                 TIMESTAMPTZ,

    CHECK (NOT isempty(valid_time)),
    CHECK (NOT isempty(transaction_time)),
    CHECK (expires_at IS NULL OR expires_at > created_at),
    CHECK (domain <> 'working' OR expires_at IS NOT NULL),
    UNIQUE (memory_id, revision_number),

    -- One logical memory never has two versions present at the same time.
    CONSTRAINT memory_transaction_no_overlap
        EXCLUDE USING gist (memory_id WITH =, transaction_time WITH &&)
);

CREATE UNIQUE INDEX idx_memory_current_revision
    ON memory_records (memory_id) WHERE upper_inf(transaction_time);
CREATE INDEX idx_memory_scope
    ON memory_records (tenant_id, project_id, agent_id, conversation_id);
CREATE INDEX idx_memory_domain_status ON memory_records (domain, status);
CREATE INDEX idx_memory_valid_time ON memory_records USING gist (valid_time);
CREATE INDEX idx_memory_transaction_time ON memory_records USING gist (transaction_time);
CREATE INDEX idx_memory_content_fts
    ON memory_records USING gin (to_tsvector('simple', content::text));

-- Embeddings are per revision and per model; dimensionality is recorded and
-- checked rather than fixed, so models can be upgraded independently.
CREATE TABLE memory_embeddings (
    revision_id    UUID NOT NULL REFERENCES memory_records (revision_id) ON DELETE CASCADE,
    model_name     TEXT NOT NULL,
    model_version  TEXT NOT NULL,
    dimensions     INTEGER NOT NULL CHECK (dimensions > 0),
    embedding      vector NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (revision_id, model_name, model_version),
    CHECK (vector_dims(embedding) = dimensions)
);

-- ---------------------------------------------------------------------------
-- Evidence and beliefs
-- ---------------------------------------------------------------------------

CREATE TABLE evidence (
    id            UUID PRIMARY KEY,
    tenant_id     UUID NOT NULL,
    source_type   TEXT NOT NULL,
    source        JSONB NOT NULL,
    actor         JSONB NOT NULL,
    observed_at   TIMESTAMPTZ NOT NULL,
    recorded_at   TIMESTAMPTZ NOT NULL,
    extraction    JSONB,
    content_hash  TEXT,
    summary       TEXT
);

CREATE INDEX idx_evidence_tenant ON evidence (tenant_id);

CREATE TABLE beliefs (
    id                          UUID PRIMARY KEY,
    lineage_id                  UUID NOT NULL,

    tenant_id                   UUID NOT NULL,
    owner_user_id               UUID,
    agent_id                    UUID,
    project_id                  UUID,
    conversation_id             UUID,
    visibility                  visibility NOT NULL,

    proposition                 TEXT NOT NULL,
    polarity                    TEXT NOT NULL CHECK (polarity IN ('supports', 'refutes')),
    confidence                  DOUBLE PRECISION NOT NULL
                                    CHECK (confidence >= 0.0 AND confidence <= 1.0),
    confidence_interpretation   TEXT NOT NULL CHECK (confidence_interpretation IN (
                                    'calibrated_probability', 'heuristic_score',
                                    'human_assessment', 'model_estimate')),
    confidence_method           TEXT NOT NULL,

    revision_number             INTEGER NOT NULL CHECK (revision_number > 0),
    supersedes_belief_id        UUID REFERENCES beliefs (id),
    superseded_by_belief_id     UUID,
    revision_reason             TEXT NOT NULL,
    status                      belief_status NOT NULL,
    revalidation_interval_days  INTEGER CHECK (revalidation_interval_days > 0),

    valid_time                  TSTZRANGE NOT NULL,
    transaction_time            TSTZRANGE NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL,

    CHECK (NOT isempty(valid_time)),
    CHECK (NOT isempty(transaction_time)),
    UNIQUE (lineage_id, revision_number),
    CONSTRAINT belief_transaction_no_overlap
        EXCLUDE USING gist (lineage_id WITH =, transaction_time WITH &&)
);

CREATE UNIQUE INDEX idx_belief_current_revision
    ON beliefs (lineage_id) WHERE upper_inf(transaction_time);
CREATE INDEX idx_beliefs_scope_status ON beliefs (tenant_id, project_id, agent_id, status);
CREATE INDEX idx_beliefs_transaction_time ON beliefs USING gist (transaction_time);

CREATE TABLE belief_evidence (
    belief_id    UUID NOT NULL REFERENCES beliefs (id),
    evidence_id  UUID NOT NULL REFERENCES evidence (id),
    direction    evidence_direction NOT NULL,
    strength     DOUBLE PRECISION CHECK (strength IS NULL OR (strength >= 0.0 AND strength <= 1.0)),
    PRIMARY KEY (belief_id, evidence_id)
);

-- Relational adjacency list. An edge from -> to means `to` depends on `from`.
CREATE TABLE belief_dependencies (
    from_belief_id  UUID NOT NULL REFERENCES beliefs (id),
    to_belief_id    UUID NOT NULL REFERENCES beliefs (id),
    relation        TEXT NOT NULL CHECK (relation IN (
                        'supports', 'requires', 'derived_from',
                        'contradicts', 'generalizes', 'specializes')),
    strength        DOUBLE PRECISION CHECK (strength IS NULL OR (strength >= 0.0 AND strength <= 1.0)),
    created_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (from_belief_id, to_belief_id, relation),
    CHECK (from_belief_id <> to_belief_id)
);

CREATE INDEX idx_belief_dependencies_incoming ON belief_dependencies (to_belief_id);

-- ---------------------------------------------------------------------------
-- Append-only cognitive event log
-- ---------------------------------------------------------------------------

CREATE TABLE cognitive_events (
    id              UUID PRIMARY KEY,
    stream_id       UUID NOT NULL,
    sequence        BIGINT NOT NULL CHECK (sequence > 0),
    tenant_id       UUID NOT NULL,
    scope           JSONB NOT NULL,
    actor           JSONB NOT NULL,
    event_type      TEXT NOT NULL,
    payload         JSONB NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    causation_id    UUID REFERENCES cognitive_events (id),
    correlation_id  UUID,
    schema_version  INTEGER NOT NULL CHECK (schema_version > 0),
    UNIQUE (stream_id, sequence)
);

CREATE INDEX idx_cognitive_events_tenant ON cognitive_events (tenant_id, id);

-- Enforced at the storage boundary, not merely documented. Deployments
-- should additionally run the application under a role without UPDATE or
-- DELETE privileges on this table.
CREATE FUNCTION cognitive_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'cognitive_events is append-only (% rejected)', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

CREATE TRIGGER cognitive_events_no_update_delete
    BEFORE UPDATE OR DELETE ON cognitive_events
    FOR EACH ROW EXECUTE FUNCTION cognitive_events_append_only();

CREATE TRIGGER cognitive_events_no_truncate
    BEFORE TRUNCATE ON cognitive_events
    FOR EACH STATEMENT EXECUTE FUNCTION cognitive_events_append_only();

-- ---------------------------------------------------------------------------
-- Recall traces
-- ---------------------------------------------------------------------------

CREATE TABLE recall_traces (
    id                 UUID PRIMARY KEY,
    tenant_id          UUID NOT NULL,
    policy             JSONB NOT NULL,
    scoring_version    INTEGER NOT NULL,
    query_hash         TEXT NOT NULL,
    query_text         TEXT,
    embedding_model    TEXT,
    embedding_version  TEXT,
    data_snapshot      TEXT NOT NULL,
    executed_at        TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_recall_traces_tenant ON recall_traces (tenant_id, executed_at DESC);

CREATE TABLE recall_trace_candidates (
    trace_id       UUID NOT NULL REFERENCES recall_traces (id) ON DELETE CASCADE,
    rank           INTEGER NOT NULL CHECK (rank > 0),
    memory_id      UUID NOT NULL,
    revision_id    UUID NOT NULL,
    score          DOUBLE PRECISION NOT NULL,
    dense_score    DOUBLE PRECISION,
    lexical_score  DOUBLE PRECISION,
    recency_score  DOUBLE PRECISION,
    selected       BOOLEAN NOT NULL,
    reason         TEXT NOT NULL,
    PRIMARY KEY (trace_id, rank)
);

-- ---------------------------------------------------------------------------
-- Idempotency
-- ---------------------------------------------------------------------------

CREATE TABLE idempotency_keys (
    tenant_id    UUID NOT NULL,
    kind         TEXT NOT NULL,
    key          TEXT NOT NULL,
    resource_id  UUID NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, kind, key)
);
