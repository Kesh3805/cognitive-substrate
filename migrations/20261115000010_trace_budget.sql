-- Cognitive Substrate: M5.5 adaptive candidate budgets. A trace records
-- what an adaptive budget decided (first-pass size, lexical candidates
-- found, whether it expanded), so replay can re-derive the decision.

ALTER TABLE recall_traces ADD COLUMN budget JSONB;
