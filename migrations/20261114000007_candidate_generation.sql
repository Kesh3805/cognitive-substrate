-- Cognitive Substrate: M5.1/M5.2 candidate generation.
--
-- `index_terms` holds the distinct terms of a revision's index text under
-- the shared lexical contract (`cs_memory::lexical`, tokenizer v1). It is
-- computed by the application, never by Postgres full-text search, so the
-- in-memory and Postgres stores agree on which records share terms with a
-- query. Rows written before this migration are backfilled at startup.

ALTER TABLE memory_records ADD COLUMN index_terms TEXT[];
CREATE INDEX idx_memory_index_terms ON memory_records USING gin (index_terms);

-- Which generation channels proposed each recall candidate
-- (comma-separated; empty for traces recorded before channels existed).
ALTER TABLE recall_trace_candidates ADD COLUMN channels TEXT NOT NULL DEFAULT '';
