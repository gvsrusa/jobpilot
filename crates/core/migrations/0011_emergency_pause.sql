-- rule 7: one row while the pause is on: who set it and when, no personal data. Backup copies
-- it, export writes it, delete-all removes it. A restore never clears it (backup.rs).
CREATE TABLE emergency_pause (
  id TEXT PRIMARY KEY CHECK (id = 'me'),
  set_at TEXT NOT NULL,
  set_by TEXT NOT NULL CHECK (set_by IN ('gui','cli'))
);
