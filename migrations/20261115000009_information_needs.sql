-- Cognitive Substrate: M5.4 information needs and knowledge gaps.
--
-- A need is stored as its full document (`body`) plus the columns that
-- identify it, authorize it and order it. Session keys are kept apart so
-- demand counts each session once. Needs are erased directly (their
-- signatures can contain sensitive query terms).

CREATE TABLE information_needs (
    id               UUID PRIMARY KEY,
    tenant_id        UUID NOT NULL,
    owner_user_id    UUID,
    agent_id         UUID,
    project_id       UUID,
    conversation_id  UUID CHECK (conversation_id IS NULL),
    visibility       visibility NOT NULL,
    partition_key    TEXT NOT NULL,
    signature        TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('retrieval_failure', 'unresolved',
                         'pending_promotion', 'candidate_gap', 'validated_gap', 'satisfied',
                         'dismissed')),
    last_seen        TIMESTAMPTZ NOT NULL,
    version          INTEGER NOT NULL CHECK (version >= 1),
    body             JSONB NOT NULL,
    UNIQUE (partition_key, signature)
);

CREATE INDEX idx_needs_tenant ON information_needs (tenant_id, last_seen DESC, id);

CREATE TABLE information_need_sessions (
    need_id      UUID NOT NULL REFERENCES information_needs (id) ON DELETE CASCADE,
    session_key  TEXT NOT NULL CHECK (length(session_key) BETWEEN 1 AND 128),
    PRIMARY KEY (need_id, session_key)
);
