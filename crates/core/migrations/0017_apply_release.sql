-- rule 7: ids only. The sealed answers a person released for one item at start, as a JSON
-- array of answer ids. No text, and apply_items is already in backup, export, and delete-all.
ALTER TABLE apply_items ADD COLUMN released_answers TEXT NOT NULL DEFAULT '[]';
