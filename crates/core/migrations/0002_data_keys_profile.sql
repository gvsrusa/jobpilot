-- rule 7: data_keys holds no plaintext secret; backup copies it; export lists key ids only;
-- delete-all removes the root and this root's keychain entries.
CREATE TABLE data_keys (key_id TEXT PRIMARY KEY, created_at TEXT NOT NULL,
  key_check BLOB NOT NULL, active INTEGER NOT NULL DEFAULT 0);
-- rule 7: profile.basic is Personal class, plain; copied by backup, exported, deleted with the root.
CREATE TABLE profile (id TEXT PRIMARY KEY CHECK (id = 'me'), basic TEXT NOT NULL, updated_at TEXT NOT NULL);
-- rule 7: work_authorization is Sensitive class; ciphertext only in backups; export writes it plain
-- after a warning; delete-all removes it with the root.
CREATE TABLE work_authorization (country TEXT PRIMARY KEY, ciphertext BLOB NOT NULL,
  key_id TEXT NOT NULL REFERENCES data_keys(key_id), updated_at TEXT NOT NULL);
