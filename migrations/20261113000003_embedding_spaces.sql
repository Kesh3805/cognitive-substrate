-- Cognitive Substrate: M3.x embedding spaces, backfill and belief vectors.
--
-- Vectors belong to an immutable embedding space identified by
-- (model_name, model_version) = provider/model @ revision+dims+norm+pp.
-- Similarity is only ever computed within one space.

-- Embedding backfill jobs.
ALTER TABLE processing_jobs DROP CONSTRAINT processing_jobs_kind_check;
ALTER TABLE processing_jobs
    ADD CONSTRAINT processing_jobs_kind_check
    CHECK (kind IN ('perceive', 'resolve', 'embed'));

-- What exactly was embedded (preprocessing version + prefixed text), and
-- when the vector became available: recall as of T only uses vectors
-- embedded at or before T, so backfills never change replayed results.
ALTER TABLE memory_embeddings
    ADD COLUMN content_hash TEXT,
    ADD COLUMN embedded_at TIMESTAMPTZ;

-- Vectors written before embedding spaces existed came from the hashing
-- embedder, whose output is unchanged under preprocessing v1: re-label them
-- with their full space identity.
UPDATE memory_embeddings
   SET model_name = 'cs/hashing',
       model_version = '1+d' || dimensions || '+l2+pp1'
 WHERE model_name = 'cs-hashing' AND model_version = '1';

CREATE INDEX idx_memory_embeddings_space
    ON memory_embeddings (model_name, model_version);

CREATE TABLE belief_embeddings (
    belief_id      UUID NOT NULL REFERENCES beliefs (id) ON DELETE CASCADE,
    model_name     TEXT NOT NULL,
    model_version  TEXT NOT NULL,
    dimensions     INTEGER NOT NULL CHECK (dimensions > 0),
    embedding      vector NOT NULL,
    content_hash   TEXT,
    embedded_at    TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (belief_id, model_name, model_version),
    CHECK (vector_dims(embedding) = dimensions)
);

-- Recall traces record whether the dense channel ran, and the exact query
-- vector used (dropped together with the query text when not retained).
ALTER TABLE recall_traces
    ADD COLUMN dense JSONB NOT NULL DEFAULT '{"status": "disabled"}'::jsonb,
    ADD COLUMN query_embedding vector;
