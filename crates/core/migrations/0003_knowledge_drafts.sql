-- rule 7, for every table below: backup copies the database whole (VACUUM INTO), so a marked
-- knowledge body is ciphertext there. Export writes each table without its BLOB columns
-- (body_enc, vector); a marked item exports with an empty body. Delete-all removes them
-- with the root.
-- seq is the integer key (see documents in 0001): VACUUM INTO does not promise implicit
-- rowids, so the order of rows must not depend on them.

-- Personal class. A marked body is Sensitive class: body = '' and body_enc holds JPK1
-- ciphertext (table knowledge_items, column body_enc, row id = id). Discard clears body_enc
-- and keeps the row. An unmarked item is the only kind that is ever evidence.
CREATE TABLE knowledge_items (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('experience','education','skill','project','achievement','note')),
  body TEXT NOT NULL,
  body_enc BLOB,
  key_id TEXT REFERENCES data_keys(key_id),
  source TEXT NOT NULL CHECK (source IN ('user','parsed')),
  source_ref TEXT,
  approved INTEGER NOT NULL DEFAULT 0 CHECK (approved IN (0,1)),
  marked_sensitive INTEGER NOT NULL DEFAULT 0 CHECK (marked_sensitive IN (0,1)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK (body_enc IS NULL OR (body = '' AND key_id IS NOT NULL AND marked_sensitive = 1))
);

-- Untrusted class: pasted job text. Only `normalized` may reach a model, inside a data block.
CREATE TABLE job_texts (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL,
  raw TEXT NOT NULL,
  normalized TEXT NOT NULL,
  findings TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL
);

-- Personal class. body is a JSON ResumeDoc.
CREATE TABLE drafts (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('resume','cover_letter')),
  lineage_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  origin TEXT NOT NULL CHECK (origin IN ('uploaded','generated','edited')),
  title TEXT NOT NULL,
  job_text_id TEXT REFERENCES job_texts(id),
  provider TEXT,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE (lineage_id, version)
);

CREATE TABLE claims (
  id TEXT PRIMARY KEY,
  draft_id TEXT NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  text TEXT NOT NULL,
  evidence_ids TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL CHECK (status IN ('ok','no_evidence','authorization_statement','removed'))
);

-- Derived from unmarked knowledge text. Marking an item deletes its vector.
CREATE TABLE embeddings (
  item_id TEXT PRIMARY KEY REFERENCES knowledge_items(id) ON DELETE CASCADE,
  model TEXT NOT NULL,
  dims INTEGER NOT NULL,
  vector BLOB NOT NULL,
  created_at TEXT NOT NULL
);
