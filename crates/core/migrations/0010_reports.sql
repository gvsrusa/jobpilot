-- rule 7: counts and rates only, no company, role, or note. Backup copies it, export writes it,
-- delete-all removes it. complete = 0 marks a report still being built.
CREATE TABLE reports (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0,1)),
  figures TEXT NOT NULL DEFAULT '{}'
);
