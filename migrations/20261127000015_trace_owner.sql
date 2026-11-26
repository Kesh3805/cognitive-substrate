-- Cognitive Substrate: recall traces belong to the principal that ran them.
--
-- A trace holds the query text, the query vector and the ids and scores of
-- every candidate the caller could read. It is readable only by the same
-- principal (tenant, user and agent; see `RecallTrace::readable_by`).
--
-- Both columns are nullable and are not backfilled: the owner of a trace
-- recorded before this migration is unknown. Such traces name neither a user
-- nor an agent, and the engine treats them as readable by no one (reads and
-- replays return `not_found`), rather than readable by the whole tenant.

ALTER TABLE recall_traces ADD COLUMN owner_user_id UUID;
ALTER TABLE recall_traces ADD COLUMN owner_agent_id UUID;
