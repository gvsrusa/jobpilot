-- rule 7: version_of is a job id, so a job's older texts can be found and removed with it.
-- It holds an id only, no text, and job_texts is already Untrusted plain text in backup,
-- export, and delete-all. A text no job has revised has none.
ALTER TABLE job_texts ADD COLUMN version_of TEXT;
CREATE INDEX job_texts_by_version ON job_texts (version_of);
