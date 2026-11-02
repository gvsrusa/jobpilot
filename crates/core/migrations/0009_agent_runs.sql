-- rule 7: agent_runs and agent_run_log hold ids, enum choices, codes, and durations. Never a
-- prompt, an output, posting text, an answer, or a secret (M-26). `input` keeps what a resume
-- needs: for a status update that is the application id, the status, and the date, never its
-- note. Backup copies them, export writes them, delete-all removes them.
-- `waiting_for_human` is reserved for Release 0.6. `generation` counts resumes: a process
-- writes only under the generation it started with, so the one a resume replaced cannot.
-- `cancel_actor` is who asked first; `run_cancelled` is audited when the run actually ends so.
CREATE TABLE agent_runs (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  agent TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued','checking','running','checkpointed','retrying',
    'waiting_for_human','halted','completed','manual_required','refused','failed','cancelled')),
  actor TEXT NOT NULL CHECK (actor IN ('gui','cli')),
  input TEXT NOT NULL,
  step INTEGER NOT NULL DEFAULT 0,
  steps INTEGER NOT NULL,
  checkpoint TEXT NOT NULL DEFAULT '{}',
  generation INTEGER NOT NULL DEFAULT 0,
  reason TEXT,
  cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0,1)),
  cancel_actor TEXT CHECK (cancel_actor IN ('gui','cli')),
  heartbeat_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE agent_run_log (
  seq INTEGER PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  agent TEXT NOT NULL,
  step TEXT NOT NULL,
  state TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  provider TEXT,
  data_categories TEXT NOT NULL DEFAULT '[]',
  outcome TEXT NOT NULL
);
CREATE INDEX agent_run_log_by_run ON agent_run_log (run_id, seq);
