-- rule 7: embeddings are derived from unmarked knowledge text and are Personal class.
-- Backup copies them with the whole database, export writes them, and delete-all removes
-- them with the root. Marking an item deletes its rows. 0003 keyed a row by item alone;
-- a changed embedding model must keep the old rows, so the key becomes (item, model).
CREATE TABLE embeddings_new (
  item_id TEXT NOT NULL REFERENCES knowledge_items(id) ON DELETE CASCADE,
  model TEXT NOT NULL,
  dims INTEGER NOT NULL,
  vector BLOB NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (item_id, model)
);
INSERT INTO embeddings_new SELECT item_id, model, dims, vector, created_at FROM embeddings;
DROP TABLE embeddings;
ALTER TABLE embeddings_new RENAME TO embeddings;
