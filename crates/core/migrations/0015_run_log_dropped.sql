-- rule 7: a count only. Model output the checks cut (a strength with no evidence, a skill that
-- states a right to work), counted per step so a short result is never a silent one. It holds
-- no text, and agent_run_log is already in backup, export, and delete-all.
ALTER TABLE agent_run_log ADD COLUMN dropped INTEGER NOT NULL DEFAULT 0;
