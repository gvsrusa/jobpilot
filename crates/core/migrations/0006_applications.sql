-- rule 7: Personal class, plain. Backup copies both tables with the whole database, export
-- writes both, delete-all removes them with the root. No table here holds work authorization.
CREATE TABLE applications (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  company TEXT NOT NULL,
  role TEXT NOT NULL,
  job_text_id TEXT REFERENCES job_texts(id) ON DELETE SET NULL,
  resume_draft_id TEXT REFERENCES drafts(id) ON DELETE SET NULL,
  letter_draft_id TEXT REFERENCES drafts(id) ON DELETE SET NULL,
  link TEXT NOT NULL DEFAULT '',
  country TEXT NOT NULL DEFAULT '',
  notes TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('saved','applied','screening','interviewing',
    'offer','accepted','rejected','withdrawn')),
  applied_on TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE application_events (
  id INTEGER PRIMARY KEY,
  application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  actor TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('created','status','note','edited')),
  from_status TEXT,
  to_status TEXT,
  note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX application_events_by_application ON application_events (application_id, id);
