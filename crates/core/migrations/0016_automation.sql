-- rule 7: apply_items and apply_pauses hold Untrusted excerpts and Personal paths, plain. Backup
-- copies them with the database, export writes them, and delete-all removes them with the
-- root. No table here holds a value of a sealed answer or a work-authorization row.
-- autonomous_grants holds a connector name, who granted, and when: no text.
CREATE TABLE apply_runs (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  agent_run_id TEXT,
  mode TEXT NOT NULL CHECK (mode IN ('copilot','autonomous','claude_code')),
  actor TEXT NOT NULL CHECK (actor IN ('gui','cli')),
  state TEXT NOT NULL DEFAULT 'ready' CHECK (state IN ('ready','running','ended')),
  created_at TEXT NOT NULL,
  ended_at TEXT
);

CREATE TABLE apply_items (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  apply_run_id TEXT NOT NULL REFERENCES apply_runs(id) ON DELETE CASCADE,
  application_id TEXT REFERENCES applications(id) ON DELETE SET NULL,
  position INTEGER NOT NULL,
  host TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued','active','filled','submitted',
    'uncertain','skipped','manual_required','failed')),
  submit_clicks INTEGER NOT NULL DEFAULT 0,
  tool_calls INTEGER NOT NULL DEFAULT 0,
  resume_path TEXT,
  letter_path TEXT,
  auth_released INTEGER NOT NULL DEFAULT 0 CHECK (auth_released IN (0,1)),
  policy_note TEXT,
  confirmation_text TEXT,
  confirmation_url TEXT,
  captured_at TEXT,
  status_written INTEGER NOT NULL DEFAULT 0 CHECK (status_written IN (0,1)),
  reason TEXT,
  UNIQUE (apply_run_id, position)
);

CREATE TABLE apply_pauses (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  item_id TEXT NOT NULL REFERENCES apply_items(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('legal','sensitive','identity','captcha','mfa','login',
    'other')),
  origin TEXT NOT NULL CHECK (origin IN ('page','model','guard')),
  excerpt TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  resolved_at TEXT,
  resolved_by TEXT CHECK (resolved_by IN ('gui','cli','system')),
  resolution TEXT CHECK (resolution IN ('continue','take_over','timeout','stopped'))
);

CREATE TABLE autonomous_grants (
  connector TEXT PRIMARY KEY,
  granted_by TEXT NOT NULL,
  granted_at TEXT NOT NULL
);
