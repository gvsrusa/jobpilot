-- rule 7: one row while the pause is on: who set it and when, no personal data. Backup copies
-- it, export writes it, delete-all removes it. A restore never clears it (backup.rs).
CREATE TABLE emergency_pause (
  id TEXT PRIMARY KEY CHECK (id = 'me'),
  set_at TEXT NOT NULL,
  set_by TEXT NOT NULL CHECK (set_by IN ('gui','cli'))
);

-- rule 7: drafts.run_id is the id of the agent run that made a generated version, and nothing
-- else; backup, export, and delete-all treat it as they treat the rest of the row. A run that
-- repeats its draft step after a failed checkpoint write finds its version by it and calls no
-- model again.
ALTER TABLE drafts ADD COLUMN run_id TEXT;
CREATE INDEX drafts_by_run ON drafts (run_id) WHERE run_id IS NOT NULL;
