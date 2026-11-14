-- Cognitive Substrate: M4.4 procedural learning.
--
-- Executions are recorded at intent and finished once; verifications are
-- separate records with an oracle tier; evaluation cases form suites.
-- All of it belongs to a capability (a procedural memory) and is deleted
-- with it on purge.

CREATE TABLE capability_executions (
    id                     UUID PRIMARY KEY,
    capability_id          UUID NOT NULL,
    capability_revision    UUID NOT NULL,
    procedure_fingerprint  UUID NOT NULL,
    tenant_id              UUID NOT NULL,
    owner_user_id          UUID,
    agent_id               UUID,
    project_id             UUID,
    conversation_id        UUID,
    visibility             visibility NOT NULL,
    case_id                UUID,
    input_class            TEXT NOT NULL CHECK (length(input_class) BETWEEN 1 AND 128),
    actor                  JSONB NOT NULL,
    status                 TEXT NOT NULL CHECK (status IN ('started', 'completed', 'aborted')),
    abort_reason           TEXT CHECK (abort_reason IN
                               ('precondition_failed', 'timeout', 'cancelled', 'error')),
    started_at             TIMESTAMPTZ NOT NULL,
    finished_at            TIMESTAMPTZ,
    CHECK ((status = 'started') = (finished_at IS NULL)),
    CHECK ((status = 'aborted') = (abort_reason IS NOT NULL))
);

CREATE INDEX idx_executions_capability ON capability_executions (capability_id, started_at, id);

CREATE TABLE execution_verifications (
    id            UUID PRIMARY KEY,
    execution_id  UUID NOT NULL REFERENCES capability_executions (id) ON DELETE CASCADE,
    tier          TEXT NOT NULL CHECK (tier IN ('deterministic', 'independent', 'self_assessed')),
    verdict       TEXT NOT NULL CHECK (verdict IN ('passed', 'failed')),
    verifier      JSONB NOT NULL,
    detail        TEXT CHECK (length(detail) <= 1000),
    recorded_at   TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_verifications_execution ON execution_verifications (execution_id, recorded_at, id);

CREATE TABLE evaluation_cases (
    id               UUID PRIMARY KEY,
    capability_id    UUID NOT NULL,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    conversation_id  UUID,
    visibility       visibility NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('positive', 'negative', 'boundary', 'regression')),
    input_class      TEXT NOT NULL CHECK (length(input_class) BETWEEN 1 AND 128),
    description      TEXT CHECK (length(description) <= 1000),
    locked           BOOLEAN NOT NULL,
    origin           JSONB NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    UNIQUE (capability_id, kind, input_class)
);
