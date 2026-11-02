-- rule 7: questions and unsealed answers are Personal class, plain. A body in any class above
-- normal is Sensitive data: body = '' and body_enc holds JPK1 ciphertext (table
-- memory_answers, column body_enc, row id = id). Backup copies ciphertext with the database,
-- and a body raised while the key was locked with it in plain until seal_pending.
-- data.json drops BLOB columns and blanks a plain body under a sealed class; memory_sealed_answers.json
-- holds the plain text after the export warning, when the key is unlocked. Delete-all removes them with the root.
CREATE TABLE memory_questions (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  key TEXT NOT NULL UNIQUE,
  text TEXT NOT NULL,
  class TEXT NOT NULL CHECK (class IN ('normal','sensitive','legal','work_authorization')),
  class_source TEXT NOT NULL CHECK (class_source IN ('rules','model','user')),
  country TEXT CHECK (country IS NULL OR length(country) = 2),
  source TEXT NOT NULL CHECK (source IN ('user','form')),
  source_ref TEXT,
  findings TEXT NOT NULL DEFAULT '[]',
  classified_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE memory_answers (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  question_id TEXT NOT NULL REFERENCES memory_questions(id) ON DELETE CASCADE,
  body TEXT NOT NULL,
  body_enc BLOB,
  key_id TEXT REFERENCES data_keys(key_id),
  approved INTEGER NOT NULL DEFAULT 0 CHECK (approved IN (0,1)),
  version INTEGER NOT NULL DEFAULT 1,
  source TEXT NOT NULL CHECK (source IN ('user','knowledge')),
  source_ref TEXT,
  reviewed_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK (body_enc IS NULL OR (body = '' AND key_id IS NOT NULL))
);
-- rule 7: ids, versions, and times; no answer text. Task 4 of the 0.3 plan fills it.
CREATE TABLE application_answers (
  application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
  answer_id TEXT NOT NULL REFERENCES memory_answers(id) ON DELETE CASCADE,
  answer_version INTEGER NOT NULL,
  confirmed_at TEXT,
  confirmed_by TEXT CHECK (confirmed_by IN ('gui','cli')),
  used_at TEXT,
  PRIMARY KEY (application_id, answer_id)
);
