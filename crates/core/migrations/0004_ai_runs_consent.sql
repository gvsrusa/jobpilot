-- rule 7: task_runs holds no prompts, outputs, or secrets: reason is a code. consent is
-- Personal class. Backup copies both with the whole database, export writes both, and
-- delete-all removes them with the root.
CREATE TABLE task_runs (
  id TEXT PRIMARY KEY,
  task_type TEXT NOT NULL,
  provider TEXT NOT NULL,
  data_categories TEXT NOT NULL,
  started_at TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  attempt INTEGER NOT NULL,
  outcome TEXT NOT NULL,
  reason TEXT
);

CREATE TABLE consent (
  provider TEXT NOT NULL,
  categories TEXT NOT NULL,
  given_at TEXT NOT NULL,
  revoked_at TEXT,
  terms_date TEXT NOT NULL,
  PRIMARY KEY (provider, given_at)
);
