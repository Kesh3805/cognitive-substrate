-- Cognitive Substrate: M3 governed cognitive processing.

-- ---------------------------------------------------------------------------
-- Durable jobs (at-least-once, fenced leases)
-- ---------------------------------------------------------------------------

CREATE TYPE job_status AS ENUM ('queued', 'leased', 'succeeded', 'dead_lettered');

CREATE TABLE processing_jobs (
    id                UUID PRIMARY KEY,
    tenant_id         UUID NOT NULL,
    kind              TEXT NOT NULL CHECK (kind IN ('perceive', 'resolve')),
    principal         JSONB NOT NULL,
    payload           JSONB NOT NULL,
    dedupe_key        TEXT NOT NULL CHECK (length(dedupe_key) BETWEEN 1 AND 255),
    status            job_status NOT NULL,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts      INTEGER NOT NULL CHECK (max_attempts BETWEEN 1 AND 100),
    available_at      TIMESTAMPTZ NOT NULL,
    lease_owner       TEXT,
    lease_until       TIMESTAMPTZ,
    lease_generation  BIGINT NOT NULL DEFAULT 0,
    last_error        JSONB,
    result            JSONB,
    created_at        TIMESTAMPTZ NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL,
    CHECK (status <> 'leased' OR lease_until IS NOT NULL)
);

-- Dedupe applies to *active* jobs only: a dead-lettered or completed job
-- never blocks resubmission. Work-level idempotency lives in domain
-- constraints (e.g. extraction_runs).
CREATE UNIQUE INDEX idx_jobs_active_dedupe
    ON processing_jobs (tenant_id, kind, dedupe_key)
    WHERE status IN ('queued', 'leased');
CREATE INDEX idx_jobs_claimable
    ON processing_jobs (available_at, created_at) WHERE status = 'queued';
CREATE INDEX idx_jobs_expired_leases
    ON processing_jobs (lease_until) WHERE status = 'leased';
CREATE INDEX idx_jobs_tenant ON processing_jobs (tenant_id, created_at);

-- ---------------------------------------------------------------------------
-- Observations and extraction
-- ---------------------------------------------------------------------------

CREATE TABLE observations (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    conversation_id  UUID,
    visibility       visibility NOT NULL,
    content          TEXT,
    content_hash     TEXT NOT NULL,
    media_type       TEXT,
    source           JSONB NOT NULL,
    actor            JSONB NOT NULL,
    observed_at      TIMESTAMPTZ NOT NULL,
    recorded_at      TIMESTAMPTZ NOT NULL,
    purged_at        TIMESTAMPTZ,
    CHECK (content IS NOT NULL OR purged_at IS NOT NULL),
    CHECK (octet_length(content) <= 65536)
);

CREATE TABLE extraction_runs (
    observation_id     UUID NOT NULL REFERENCES observations (id),
    extractor          TEXT NOT NULL,
    extractor_version  TEXT NOT NULL,
    candidate_ids      UUID[] NOT NULL,
    completed_at       TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (observation_id, extractor, extractor_version)
);

-- ---------------------------------------------------------------------------
-- Resolution proposals
-- ---------------------------------------------------------------------------

CREATE TYPE proposal_status AS ENUM ('pending', 'accepted', 'rejected', 'stale');

CREATE TABLE resolution_proposals (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    conversation_id  UUID,
    visibility       visibility NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('duplicate', 'supersession', 'contradiction')),
    status           proposal_status NOT NULL,
    operation        JSONB NOT NULL,
    rationale        JSONB NOT NULL,
    detection        JSONB NOT NULL,
    based_on         JSONB NOT NULL,
    fingerprint      TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    decided_at       TIMESTAMPTZ,
    decided_by       JSONB,
    decision_reason  TEXT,
    -- Fingerprints bind exact revisions, so one is never proposed twice:
    -- a rejected finding stays rejected until its data changes.
    UNIQUE (tenant_id, fingerprint),
    CHECK ((status = 'pending') = (decided_at IS NULL))
);

CREATE INDEX idx_proposals_pending
    ON resolution_proposals (tenant_id, created_at) WHERE status = 'pending';

-- ---------------------------------------------------------------------------
-- Purge ledger: what was erased and when, never what it contained.
-- ---------------------------------------------------------------------------

CREATE TABLE purge_ledger (
    kind       TEXT NOT NULL CHECK (kind IN ('memory', 'evidence', 'observation')),
    record_id  UUID NOT NULL,
    purged_at  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (kind, record_id)
);

-- Belief recall reads by status and transaction time.
CREATE INDEX idx_beliefs_current
    ON beliefs (tenant_id, status) WHERE upper_inf(transaction_time);

-- Belief candidates considered by each recall (M3 belief recall).
ALTER TABLE recall_traces
    ADD COLUMN belief_candidates JSONB NOT NULL DEFAULT '[]'::jsonb;
