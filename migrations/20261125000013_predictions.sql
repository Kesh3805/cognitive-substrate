-- Cognitive Substrate: M6.4 bounded predictions.
--
-- A prediction is written once, at an intervention's intent, with the rule
-- it relied on frozen in its body. Rules and demotions are computed on read
-- from predictions and once-only outcomes, so nothing else is stored.

CREATE TABLE predictions (
    id               UUID PRIMARY KEY,
    intervention_id  UUID NOT NULL REFERENCES interventions (id),
    hypothesis_id    UUID NOT NULL,
    -- Hash of (scope, environment, treatment, outcome, direction, context):
    -- demotions attach to the claim, not to a hypothesis record.
    lineage          TEXT NOT NULL,
    recorded_at      TIMESTAMPTZ NOT NULL,
    body             JSONB NOT NULL,
    UNIQUE (intervention_id, lineage)
);

CREATE INDEX idx_predictions_lineage ON predictions (lineage, recorded_at, id);
