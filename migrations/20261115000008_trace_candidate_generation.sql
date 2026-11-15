-- Cognitive Substrate: experiment 006 changed the default candidate
-- generation from `recency_v1` to the lexical channel. A recall trace
-- recorded before the policy had a `candidates` field ran under v1, so it
-- must say so explicitly: otherwise reading it back would fill in the new
-- default, and its replay would no longer be exact.

UPDATE recall_traces
SET policy = policy || '{"candidates": {"strategy": "recency_v1"}}'::jsonb
WHERE NOT (policy ? 'candidates');
