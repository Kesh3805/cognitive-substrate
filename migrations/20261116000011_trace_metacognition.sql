-- M6.1: the metacognitive decision a recall recorded (answer / investigate /
-- abstain, its reasons and signals). NULL when the policy did not ask.
ALTER TABLE recall_traces ADD COLUMN metacognition JSONB;
