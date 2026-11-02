-- rule 7: register_row is register text, not personal data. Backup copies it with the row,
-- export writes it, delete-all removes it with the root. The default '' equals no build's text,
-- so a record from before this column is suspended until the user confirms again.
ALTER TABLE consent ADD COLUMN register_row TEXT NOT NULL DEFAULT '';
