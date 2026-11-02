-- rule 7: jobs, companies, job_sources, match_results, and company_notes are Untrusted or
-- Personal plain text (a posting's fields, a pasted note, a score and its explanation, which
-- never holds an authorization value). Backup copies them with the database, export writes
-- them, and delete-all removes them with the root. jobs_fts is derived and rebuilt from jobs
-- and job_texts, with rowid = jobs.seq: export skips it as an FTS table. connector_limits
-- holds only counters and times, no text.
CREATE TABLE companies (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  key TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL
);

CREATE TABLE job_sources (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  connector TEXT NOT NULL,
  target TEXT NOT NULL DEFAULT '',
  query TEXT NOT NULL DEFAULT '',
  cursor INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  UNIQUE (connector, target, query)
);

CREATE TABLE jobs (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  job_text_id TEXT NOT NULL REFERENCES job_texts(id),
  connector TEXT NOT NULL,
  source_id TEXT REFERENCES job_sources(id) ON DELETE SET NULL,
  board TEXT NOT NULL DEFAULT '',
  external_id TEXT,
  title TEXT NOT NULL,
  company_id TEXT REFERENCES companies(id),
  location TEXT NOT NULL DEFAULT '',
  country TEXT,
  work_mode TEXT CHECK (work_mode IN ('remote','hybrid','onsite')),
  employment_type TEXT,
  url TEXT,
  apply_url TEXT,
  attribution TEXT,
  posted_on TEXT,
  pay_min INTEGER,
  pay_max INTEGER,
  pay_currency TEXT,
  pay_period TEXT CHECK (pay_period IN ('hour','day','week','month','year')),
  seniority TEXT,
  skills TEXT NOT NULL DEFAULT '[]',
  mentions_authorization INTEGER NOT NULL DEFAULT 0,
  fields_by TEXT NOT NULL DEFAULT 'none' CHECK (fields_by IN ('none','connector','rules','model','user')),
  dedupe_key TEXT NOT NULL DEFAULT '',
  duplicate_of TEXT REFERENCES jobs(id) ON DELETE SET NULL,
  not_duplicate INTEGER NOT NULL DEFAULT 0 CHECK (not_duplicate IN (0,1)),
  first_seen_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  UNIQUE (connector, board, external_id)
);

CREATE VIRTUAL TABLE jobs_fts USING fts5(title, company, body, tokenize='unicode61 remove_diacritics 2');

CREATE TABLE match_results (
  seq INTEGER PRIMARY KEY,
  job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id) ON DELETE CASCADE,
  score INTEGER,
  compared_weight INTEGER NOT NULL,
  explanation TEXT NOT NULL,
  semantic TEXT,
  provider TEXT,
  inputs TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE company_notes (
  seq INTEGER PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
  text TEXT NOT NULL,
  origin TEXT NOT NULL CHECK (origin IN ('pasted','model')),
  source_ref TEXT,
  provider TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE connector_limits (
  connector TEXT NOT NULL,
  host TEXT NOT NULL,
  last_at_ms INTEGER NOT NULL DEFAULT 0,
  not_before_ms INTEGER NOT NULL DEFAULT 0,
  day TEXT NOT NULL DEFAULT '',
  day_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (connector, host)
);

-- Every text that exists is a job, with the text's id, so a draft's job_text_id still finds it.
INSERT INTO jobs (id, job_text_id, connector, title, fields_by, first_seen_at, last_seen_at)
SELECT id, id, 'import', title, 'none', created_at, created_at FROM job_texts ORDER BY seq;

INSERT INTO jobs_fts (rowid, title, company, body)
SELECT j.seq, j.title, '', t.normalized FROM jobs j JOIN job_texts t ON t.id = j.job_text_id;
