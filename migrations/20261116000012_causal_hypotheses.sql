-- Cognitive Substrate: M6.2 causal hypotheses and interventions.
--
-- A hypothesis keeps every version it was written at (status changes are
-- history, not overwrites). An intervention is written at intent; its
-- outcomes are recorded once per key. Both are stored as documents plus the
-- columns that authorize, order and select them.

CREATE TABLE hypothesis_versions (
    id               UUID NOT NULL,
    version          BIGINT NOT NULL CHECK (version >= 1),
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    visibility       visibility NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('proposed', 'supported', 'refuted',
                         'inconclusive', 'retired')),
    updated_at       TIMESTAMPTZ NOT NULL,
    body             JSONB NOT NULL,
    PRIMARY KEY (id, version)
);

CREATE INDEX idx_hypotheses_tenant ON hypothesis_versions (tenant_id, updated_at DESC, id);

CREATE TABLE interventions (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    visibility       visibility NOT NULL,
    environment      TEXT NOT NULL,
    treatment        TEXT NOT NULL,
    recorded_at      TIMESTAMPTZ NOT NULL,
    body             JSONB NOT NULL
);

CREATE INDEX idx_interventions_treatment
    ON interventions (tenant_id, environment, treatment, recorded_at, id);
