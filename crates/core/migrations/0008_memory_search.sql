-- rule 7: derived and rebuilt from memory_questions and plain answers. memory_fts rowid is
-- memory_questions.seq. A work_authorization question has no row in either table, and a sealed
-- class indexes the question only, never an answer. Export skips FTS tables and BLOB columns.
-- Backup copies both with the database; delete-all removes them with the root.
CREATE VIRTUAL TABLE memory_fts USING fts5(question, answer, tokenize='unicode61 remove_diacritics 2');
CREATE TABLE memory_embeddings (
  question_id TEXT NOT NULL REFERENCES memory_questions(id) ON DELETE CASCADE,
  model TEXT NOT NULL,
  dims INTEGER NOT NULL,
  vector BLOB NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (question_id, model)
);
-- Questions that exist already, by the same rule `reindex` applies.
INSERT INTO memory_fts (rowid, question, answer)
SELECT q.seq, q.text,
       CASE WHEN q.class = 'normal' THEN coalesce((SELECT group_concat(a.body, char(10))
         FROM memory_answers a WHERE a.question_id = q.id AND a.body <> ''), '') ELSE '' END
FROM memory_questions q WHERE q.class <> 'work_authorization';
