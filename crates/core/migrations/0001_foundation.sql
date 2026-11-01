CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE audit_log (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  actor TEXT NOT NULL,
  action TEXT NOT NULL,
  target TEXT,
  detail TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE secret_index (
  name TEXT PRIMARY KEY,
  created_at TEXT NOT NULL
);

-- seq is the FTS5 key. SQLite does not promise VACUUM keeps implicit rowids, and every
-- backup is a VACUUM INTO, so the key must be a declared INTEGER PRIMARY KEY.
CREATE TABLE documents (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

-- ponytail: unicode61 does not segment CJK; move to trigram or a segmenter when those users appear.
CREATE VIRTUAL TABLE documents_fts USING fts5(
  title,
  body,
  content = 'documents',
  content_rowid = 'seq',
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER documents_fts_insert AFTER INSERT ON documents BEGIN
  INSERT INTO documents_fts (rowid, title, body)
  VALUES (new.seq, new.title, new.body);
END;

CREATE TRIGGER documents_fts_delete AFTER DELETE ON documents BEGIN
  INSERT INTO documents_fts (documents_fts, rowid, title, body)
  VALUES ('delete', old.seq, old.title, old.body);
END;

CREATE TRIGGER documents_fts_update AFTER UPDATE ON documents BEGIN
  INSERT INTO documents_fts (documents_fts, rowid, title, body)
  VALUES ('delete', old.seq, old.title, old.body);
  INSERT INTO documents_fts (rowid, title, body)
  VALUES (new.seq, new.title, new.body);
END;
