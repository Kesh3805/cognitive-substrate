-- Cognitive Substrate: context-scoped intervention reads (M6.4).
--
-- Rules, requalification blocks and prediction resolution read the
-- interventions of one (environment, treatment, context). The context lives
-- in the intervention's JSONB body; this expression index serves that read
-- in intent order without scanning the environment's other contexts.

CREATE INDEX idx_interventions_context
    ON interventions (tenant_id, environment, treatment, (body->>'context'), recorded_at, id);
