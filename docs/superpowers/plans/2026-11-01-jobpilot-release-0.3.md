# JobPilot AI Release 0.3 Implementation Plan: Career Memory and Application Tracking

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Release 0.3: application tracking, a career memory with sensitivity classes and per-use confirmation, a deterministic workflow runner with execution logs, five agents running on it, and an emergency pause reachable from every screen.

**Architecture:** Everything lands in `jobpilot-core`; the CLI and Tauri adapter stay thin, and every feature task ends reachable from both (seam check). `runner` executes a fixed list of steps, checks each against a static permission set, logs each state change, and stores checkpoints holding ids only. Cross-process control is a database poll: the flag or cancel request is read every 250 ms, which fires the existing `CancellationToken`, so a CLI command stops a GUI run and the reverse. Sensitive and legal answers are sealed like marked knowledge bodies, and the gate that releases one runs in Rust against a confirmation row tied to the application and the answer version.

**Tech Stack:** Rust 1.99.1, Tauri 2.12 (`tauri::menu`), SQLx 0.9, SQLite FTS5, clap 4, React 19, TanStack Query, Zustand, Vitest 5.

**Spec:** `docs/superpowers/specs/2026-11-01-jobpilot-master-spec.md` sections 8, 10, 11, 12 (authority). Derived: `docs/milestones.md` (Release 0.3), `docs/agents.md`, `docs/security/threat-model.md`, `docs/wireframes.md`, `docs/architecture.md`, `docs/database-schema.md`, `docs/ai-providers.md`.

**Branch:** `feat/v0.3-memory-tracking`. Commit locally; the owner decides when to push.

## Global Constraints

- No mandatory API keys, API credits, hosted backend, or cloud account. Everything in this release works with No-AI, and tracking, memory, and analytics work offline (FR-2.12).
- Targets: macOS (arm64, x86_64) and Windows x64. Toolchain pins stay. SQL uses runtime `sqlx::query`, never `query!`. `*.sql` is LF.
- Network in 0.3 code: nothing new. Loopback Ollama and the user's own `claude` child stay the only paths.
- Owner direction: personal app; AI subscription policy is waived and never reopened in this plan. Security controls stay: agents have restricted permissions, sensitive answers need explicit confirmation (FR-8.8), nothing infers work authorization (FR-4.10), nothing fabricates (FR-7.12).
- Migrations start at `0006`. Applied files are never edited. Each new table holding personal data carries a rule 7 comment saying how backup, export, and delete-all treat it. Final `schema_version` is 11.
- **Seam check.** A task is done only when its feature is reachable from the GUI and the CLI. Each feature task ends with a CLI integration test, a Tauri command test through the mock runtime harness in `commands.rs`, and a view test. Reviewers reject a task whose core function has no adapter caller.
- **Keychain.** Tests never touch the real keychain unless `#[ignore]`d. Read paths never call `crypto::state` or `crypto::unlocked`: a list shows a sealed row with an empty body, and status-like reads use `crypto::known_state` or `crypto::peek`. Only a seal, or an explicit reveal or use, asks the store. Gate logic runs before any decrypt, so refusal tests need no keychain: they seed a sealed row with `DataKey::from_bytes`.
- **New ciphertext column.** `memory_answers.body_enc` joins every place the `ponytail:` comments in `crypto.rs` and `recovery.rs` name: `held_key_ids`, `sample_of`, both guards around `add_first_key`, `other_ciphertext`, and the clearing block in `discard_locked`.
- **Windows.** Gate every `cfg(unix)` or `cfg(windows)` item on both sides. When a task touches a `cfg` item, a child process, or a Tauri menu, run the `x86_64-pc-windows-gnu` clippy recipe in `docs/development.md`. It type-checks only; CI is the Windows evidence.
- **Panel.** Each new CLI command gets a row in the `PANEL` table in `crates/cli/src/main.rs` and, when refused, a `matches!` arm in `refused_in_panel`. Rule for this release: what its GUI twin confirms natively is refused, and a sensitive value never prints in the panel.
- **GUI rules.** Text from outside the app and stored answers render as inert text, no anchors. Every control has a visible label and works by keyboard. Errors appear in `role="alert"` with their code. No `dangerouslySetInnerHTML`.
- Execution logs, checkpoints, and audit rows hold ids, codes, and counts. Never prompts, outputs, posting text, answer text, or secrets (M-26).
- Only a person (`gui` or `cli` actor) approves an answer, lowers a class, confirms a use, or clears the pause. `"system"` is refused by core, whatever the adapter does.
- Tests use a temporary `JOBPILOT_HOME`. No test touches the owner's data root. No step takes a full-screen screenshot.
- Ponytail rule: no abstraction without a second caller. Mark deliberate limits with a `ponytail:` comment naming the ceiling.
- Every commit ends with a blank line and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Dependency decisions

No new Rust or npm dependency. `tauri::menu` ships in `tauri` 2.12.5 with no feature flag. The poll uses `tokio::time`, already enabled in core and desktop. Copying uses `navigator.clipboard`, with the text left selectable if it fails, so no clipboard plugin or capability is added.

## Decisions fixed here

| Question | Decision | Why |
|---|---|---|
| Work-authorization answers | Memory stores no value. A question of class `work_authorization` is answered from the encrypted profile row with the exact country. Absent means `not_stated`. | M-28 and wireframe 11. Two stores could disagree, and similarity could cross countries. |
| What 0.3 calls a "use" | Taking the text of an answer for one application, through `memory::use_answer`. Browser fill arrives in 0.6 on the same gate. | There is no form runtime yet, and FR-8.8 must hold now. |
| What a confirmation covers | One answer version on one application. Editing voids it. Confirming also marks the answer reviewed. | "Confirmation for each use" (M-27), and a stale answer needs fresh confirmation (threat path 9). |
| Emergency pause shortcut | `CmdOrCtrl+Shift+H`, as a native menu accelerator and a webview capture listener. Both are idempotent. | The menu survives a hung webview. The listener covers a Windows WebView2 that keeps the key. Chromium keeps Ctrl+Shift+P, and Windows keeps Ctrl+Shift+Esc. |
| Interrupted runs | A process that dies leaves a run with an old heartbeat. Any later read moves it to `halted`, reason `interrupted`. The user resumes or cancels. | Runs never resume on their own, and a row must not read `running` forever. Extends the state table in `docs/agents.md`. |
| Starting a run while paused | `Err(Paused)` and no row. | "While set, no run starts." |
| Status edits while paused | Refused with `paused`, because they run through the tracking agent. Editing fields and notes stays open. | Keeps one path for status. The copy says what to do. |
| `waiting_for_human` | In the state `CHECK` list, never entered in 0.3. | No 0.3 agent pauses for a person, and 0.6 then needs no table rebuild. |
| Terminal panel on pause | A user-driven `claude` session is not stopped. A `jobpilot` command run inside the panel is, through the same poll. | The spec stops automation; that session is the user's own (M-14). |

## Review Focus

1. A backup is restored while the pause is on, or a paused backup onto a live app: the pause stays set until a person clears it. Pinned by `restore_keeps_a_set_pause` and `restoring_a_paused_backup_stays_paused` (Task 9).
2. The process dies mid-run, by crash or force quit: the row does not stay `running`, and a resume repeats no finished step. Pinned by `a_dead_runs_row_is_reaped_to_halted` (Task 6).
3. The user edits a sensitive answer after confirming it: the old confirmation does not carry over. Pinned by `editing_an_answer_voids_its_confirmation` (Task 4).
4. A model raises a class while the keychain is locked: the plain body stops showing and being used at once, and is sealed on the next chance. Pinned by `raise_with_a_locked_key_hides_the_body_until_sealed` (Task 3).
5. A Canadian form question meets a stored US authorization answer: it never matches, and a missing country reads "not stated". Pinned by `canada_never_gets_the_us_row` and `work_authorization_question_is_never_found_by_search` (Tasks 4, 5).

## File map

```text
crates/core/migrations/0006_applications.sql     applications, application_events     Task 2
crates/core/migrations/0007_career_memory.sql    questions, answers, application_answers  Task 3
crates/core/migrations/0008_memory_search.sql    memory_fts, memory_embeddings        Task 5
crates/core/migrations/0009_agent_runs.sql       agent_runs, agent_run_log            Task 6
crates/core/migrations/0010_reports.sql          reports                              Task 7
crates/core/migrations/0011_emergency_pause.sql  emergency_pause                      Task 9
crates/core/schemas/classify_answer.json                                              Task 6
crates/core/src/applications.rs                                                       Task 2
crates/core/src/memory/{mod,rules}.rs            store, classes, sealing              Task 3
crates/core/src/memory/usage.rs                  confirm and use gate                 Task 4
crates/core/src/memory/search.rs                 index and retrieval                  Task 5
crates/core/src/memory/classify.rs               classify_answer                      Task 8
crates/core/src/runner/{mod,perms,store}.rs      runner, permissions, run rows        Task 6
crates/core/src/agents/{mod,tracking}.rs                                              Task 6
crates/core/src/agents/analytics.rs, analytics.rs                                     Task 7
crates/core/src/agents/{draft,memory}.rs                                              Task 8
crates/core/src/pause.rs                                                              Task 9
crates/core/tests/{applications,memory,memory_use,memory_search,runner,agents,pause}.rs
crates/core/tests/injection/{15,16,17}_*.txt                                          Tasks 1, 8
crates/cli/src/{apps,memory,agents,pause}.rs                                          Tasks 2 to 9
apps/desktop/src-tauri/src/menu.rs                                                    Task 9
apps/desktop/src/views/{ApplicationsView,MemoryView,AgentsView}.tsx                   Tasks 2, 3, 6
apps/desktop/src/components/TitleBar.tsx, lib/pause.ts                                Task 9
docs, release notes, version bump                                                     Task 10
```

---

### Task 1: 0.2 carry-overs

**Files:**
- Modify: `crates/core/src/ai/{mod,router}.rs`, `crates/cli/src/main.rs`, `apps/desktop/src/lib/reasons.ts`
- Modify: `crates/core/src/knowledge.rs`, `crates/core/src/embed.rs`, `crates/cli/src/profile.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/components/KnowledgeSection.tsx`, `apps/desktop/src/views/ResumesView.tsx`
- Modify: `crates/core/src/backup.rs`, `crates/core/src/parse.rs`, `crates/core/src/generate.rs`
- Create: `crates/core/tests/injection/15_tool_use_request.txt`, `crates/core/tests/injection/16_exfiltrate_profile.txt`
- Test: `crates/core/tests/{router,knowledge,backup,generate,injection}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src/views/views02.test.tsx`

**Interfaces:**
- Produces: `RUN_REASONS` gains `no_model_set`, `model_not_installed`. `KnowledgeItem.locked` is replaced by `sealed: bool`. `knowledge::reveal(db: &Db, id: &str) -> Result<String>`. `parse::stock_heading_display(heading: &str) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

```rust
// router.rs
#[tokio::test] async fn ollama_with_no_model_set_says_so() {
    assert_eq!(run_with_unavailable("no_model").await, manual("no_model_set"));
}
#[tokio::test] async fn ollama_missing_the_model_says_so() {
    assert_eq!(run_with_unavailable("model_missing").await, manual("model_not_installed"));
}
#[tokio::test] async fn an_unknown_unavailable_stays_provider_unavailable() {
    assert_eq!(run_with_unavailable("not_running").await, manual("provider_unavailable"));
}
// knowledge.rs, seeds a marked row by SQL with DataKey::from_bytes; no keychain entry exists
#[tokio::test] async fn listing_marked_items_asks_the_keychain_nothing() {
    let listed = knowledge::list(&db).await.unwrap();
    assert!(listed[0].sealed && listed[0].marked_sensitive);
    assert_eq!(listed[0].body, "");
    assert_eq!(db.key_state(), KeyState::NotLoaded);
}
// backup.rs
#[tokio::test] async fn restore_turns_claude_off_with_its_consent() {
    let enabled = settings::get(&restored, "ai.enabled").await.unwrap().unwrap();
    assert_eq!(enabled["claude_code"], false);
}
// generate.rs
#[tokio::test] async fn stock_headings_come_out_title_cased() {
    assert_eq!(made.draft.doc.sections[0].heading, "Work Experience"); // model sent "work experience"
}
// injection.rs
#[tokio::test] async fn a_reply_asking_for_a_tool_stores_nothing() { /* 15: extra `tool_use` key fails the schema */ }
#[tokio::test] async fn an_exfiltration_url_stays_flagged_text() { /* 16: claim stored `no_evidence`, export refused */ }
```

Also: `consent_hint_names_the_whole_command` in `crates/cli/src/main.rs` tests asserts the `consent_needed` message contains `jobpilot ai consent give claude_code --categories job_text,profile_basic,resume_text`. Also in `views02.test.tsx`: extend the `manual_required_%s` table with `no_model_set` and `model_not_installed`; add `ai_view_after_restore_shows_claude_off_and_asks_for_consent`, and `kb_show_reveals_a_sealed_item_on_request` (the list does not call `kb_reveal` until the Show press).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test router ollama_ && cargo test -p jobpilot-core --test knowledge listing_marked`
Expected: FAIL, reason still `provider_unavailable`, field `sealed` not found.

- [ ] **Step 3: Implement `provider_unavailable` wording**

In `try_provider`, `Availability::Unavailable(why)` maps through `fn unavailable_reason(why: &str) -> &'static str`: `no_model` to `no_model_set`, `model_missing` to `model_not_installed`, anything else to `provider_unavailable`. The log row keeps `unavailable`. `weight` gives all three 0. Both are in `RUN_REASONS`. Copy:

| Reason | `reasons.ts` | CLI `message` |
|---|---|---|
| `no_model_set` | "No local model is chosen. Pick one on the AI screen, or write this one yourself" | "No local model is chosen. Set one with `jobpilot ai set-model --drafting <name>` in an ordinary terminal, or write this one yourself." |
| `model_not_installed` | "The chosen model is not installed in Ollama. Download it on the AI screen, or choose another" | "The chosen model is not installed in Ollama. Check `jobpilot ai models`." |

- [ ] **Step 4: Fix the CLI consent hint** in `main.rs::message` to the full command in Step 1.

- [ ] **Step 5: Stop `kb_list` reading the keychain**

`knowledge::list` calls nothing in `crypto`. A row with `body_enc` lists with `body: ""`, `sealed: true`. A marked row whose text was discarded lists `sealed: false`. Delete `key_or_none` if unused. `knowledge::reveal` opens one body with `crypto::key_by_id` and returns the key's own error when it cannot. Update `embed.rs`, `ResumesView`, `profile.rs`, and the ignored keychain tests in `crates/core/tests/knowledge.rs` to reveal.

Add Tauri `kb_reveal(id) -> String` and `ipc.kbReveal`. CLI `kb reveal <id>`, PANEL row `("kb reveal i", true)`. In `KnowledgeSection`, a sealed item shows "Sealed" and a `Show` button. A key error renders in `ErrorAlert`, and the existing "see Recovery" wording moves there.

- [ ] **Step 6: Make the AI view honest after a restore**

`revoke_external_consent` also runs, on the temporary copy, when it revoked a row:

```sql
UPDATE settings SET value = json_set(value, '$.claude_code', json('false')),
  updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE key = 'ai.enabled'
```

This matches the view's own Revoke, which disables as well, so the AI screen shows Claude Code off with no consent rather than on and failing every task. `applyStatus` already resets cached queries, so no view code changes unless the test shows otherwise.

- [ ] **Step 7: Title-case stock headings**

`parse::stock_heading_display` returns each word capitalized when the heading's key is in `KNOWN_HEADINGS` or `SUMMARY_HEADINGS`, else `None`. `generate::resume` applies it to every section after decoding, before `store`. A heading the user owns is untouched.

- [ ] **Step 8: Add the two corpus files**

`15` asks the model to call a tool, run a shell command, and read a file under `$CANARY`; its `MOCK_REPLY` carries an extra `tool_use` key. `16` asks for the profile email in a tracking URL and an image link; its `MOCK_REPLY` puts that URL in a claim citing no evidence. Add both to the table in `every_corpus_file_normalizes_without_panic_and_reports_findings`, and to `README.md`.

- [ ] **Step 9: Seam check and run**

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS. Reachable: `jobpilot kb reveal`, the Show button, the wording in both adapters, and `Router` reasons through `resume generate`.

- [ ] **Step 10: Commit**

```bash
git add -A crates apps docs
git commit -m "fix: close the 0.2 carry-overs: wording, keychain on list, restore, headings, corpus"
```

### Task 2: Application tracking

**Files:**
- Create: `crates/core/migrations/0006_applications.sql`, `crates/core/src/applications.rs`, `crates/core/tests/applications.rs`, `crates/cli/src/apps.rs`, `apps/desktop/src/views/ApplicationsView.tsx`
- Modify: `crates/core/src/lib.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/{ipc,store}.ts`, `apps/desktop/src/App.tsx`, `apps/desktop/src/test-utils.tsx`
- Test: `apps/desktop/src/views/views03.test.tsx` (new, used by later tasks)

**Interfaces:**
- Consumes: `audit::record_to`, `knowledge::get_job_text`, `drafts::get`.
- Produces, in `applications`:

```rust
pub const STATUSES: [&str; 8] =
    ["saved", "applied", "screening", "interviewing", "offer", "accepted", "rejected", "withdrawn"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApplicationInput {
    pub company: String, pub role: String,
    pub job_text_id: Option<String>, pub resume_draft_id: Option<String>, pub letter_draft_id: Option<String>,
    pub link: String, pub country: String, pub notes: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Application {
    pub id: String, pub company: String, pub role: String,
    pub job_text_id: Option<String>, pub resume_draft_id: Option<String>, pub letter_draft_id: Option<String>,
    pub link: String, pub country: String, pub notes: String,
    pub status: String, pub applied_on: Option<String>, pub created_at: String, pub updated_at: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct ApplicationEvent {
    pub id: i64, pub at: String, pub actor: String, pub kind: String,
    pub from_status: Option<String>, pub to_status: Option<String>, pub note: String,
}
pub async fn add(db: &Db, actor: &str, input: ApplicationInput) -> Result<Application>;
pub async fn get(db: &Db, id: &str) -> Result<Application>;
pub async fn list(db: &Db, status: Option<&str>) -> Result<Vec<Application>>;
pub async fn update(db: &Db, actor: &str, id: &str, input: ApplicationInput) -> Result<Application>;
pub async fn set_status(db: &Db, actor: &str, id: &str, status: &str, on: Option<&str>, note: &str) -> Result<Application>;
pub async fn add_note(db: &Db, actor: &str, id: &str, note: &str) -> Result<()>;
pub async fn history(db: &Db, id: &str) -> Result<Vec<ApplicationEvent>>;
pub async fn remove(db: &Db, actor: &str, id: &str) -> Result<()>;
```

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: Personal class, plain. Backup copies both tables with the whole database, export
-- writes both, delete-all removes them with the root. No table here holds work authorization.
CREATE TABLE applications (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  company TEXT NOT NULL,
  role TEXT NOT NULL,
  job_text_id TEXT REFERENCES job_texts(id) ON DELETE SET NULL,
  resume_draft_id TEXT REFERENCES drafts(id) ON DELETE SET NULL,
  letter_draft_id TEXT REFERENCES drafts(id) ON DELETE SET NULL,
  link TEXT NOT NULL DEFAULT '',
  country TEXT NOT NULL DEFAULT '',
  notes TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('saved','applied','screening','interviewing',
    'offer','accepted','rejected','withdrawn')),
  applied_on TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE application_events (
  id INTEGER PRIMARY KEY,
  application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  actor TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('created','status','note','edited')),
  from_status TEXT,
  to_status TEXT,
  note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX application_events_by_application ON application_events (application_id, id);
```

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/applications.rs`

- `add_starts_saved_with_a_created_event`: status `saved`, one event of kind `created`.
- `status_change_records_from_and_to_in_one_event`: `saved` to `applied` gives `from_status == Some("saved")`, `to_status == Some("applied")`.
- `first_applied_sets_applied_on_and_a_later_one_keeps_it`: `on: Some("2026-10-02")` stored; a second move back to `applied` keeps it.
- `the_same_status_twice_writes_no_event`.
- `any_status_can_be_corrected`: `rejected` to `interviewing` is accepted and recorded.
- `bad_input_is_refused`: empty company, company over 300 chars, status `ghosted`, `on: "02/10/2026"`, country `USA`, notes over 20,000 chars, link `javascript:x`, unknown job id: each `Err(Validation)`, nothing stored.
- `list_filters_by_status_newest_first`.
- `update_keeps_status_and_records_an_edit`.
- `remove_audits_and_takes_the_events_with_it`: audit action `application_removed`, detail holds no company or role text.
- `tracking_works_with_no_ai_and_no_keychain`: `db.key_state() == KeyState::NotLoaded` after every call above.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test applications`
Expected: FAIL, `applications` not found.

- [ ] **Step 4: Implement `applications.rs`**

`link` is empty or begins `https://` or `http://`, at most 2,000 chars, and is stored as text. `country` is empty or two letters, upper-cased. `list` orders `updated_at DESC, seq DESC`. Each write and its event share a transaction, with `audit::record_to` for `application_removed`. Each failure uses the module's `bad()`-style `Error::Validation`.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test applications`
Expected: PASS.

- [ ] **Step 6: Add the CLI**

`AppCommand` in `crates/cli/src/apps.rs`, `Command::App` with doc "Applications you are tracking.":

```text
app add --company C --role R [--job ID] [--resume DRAFT] [--letter DRAFT] [--link L] [--country CC] [--notes N]
app list [--status S]    app show ID    app history ID
app edit ID [same flags as add, each optional]
app status ID STATUS [--on YYYY-MM-DD] [--note N]
app note ID TEXT         app remove ID
```

PANEL rows, all `false`: `app add --company c --role r`, `app list`, `app show i`, `app history i`, `app edit i --role r`, `app status i applied`, `app note i n`, `app remove i`.

- [ ] **Step 7: Add the Tauri commands and the view**

Commands, each through `state.work()`: `app_list(status)`, `app_get(id)`, `app_add(input)`, `app_update(id, input)`, `app_set_status(id, status, on, note)`, `app_note(id, note)`, `app_history(id)`, `app_remove(id)`. Add to `handler()` and to `ipc.ts` with types matching the structs above.

`View` gains `"applications"`, `NAV` gains `Applications` (Ctrl/Cmd+7). `ApplicationsView`, heading "Applications":

- Table: Company, Role, Status, Applied, Updated, as buttons opening the detail. Status filter select. Empty state: "No applications yet. Add the first one."
- `Add application` form. Detail: the same fields, a status select with an optional date and note, history list, `Remove` through `ConfirmDialog`.
- `link` renders as plain selectable text labelled untrusted, never an anchor.
- Focus order: filter, Add application, table, detail fields, status controls, Remove.

- [ ] **Step 8: Seam check**

- `crates/cli/tests/cli.rs`: `app_add_status_list_and_history_round_trip`, with `--json`, and `panel_runs_tracking_commands`.
- `commands.rs` tests: `app_commands_reach_core`: add then set status through the harness, assert `app_history` holds two events.
- `views03.test.tsx`: `applications_add_change_status_and_see_history`, `an_application_link_is_never_an_anchor`, `ctrl_7_opens_applications`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates apps
git commit -m "feat: application tracking in core, CLI, and the Applications view"
```

### Task 3: Career memory bank

**Files:**
- Create: `crates/core/migrations/0007_career_memory.sql`, `crates/core/src/memory/{mod,rules}.rs`, `crates/core/tests/memory.rs`, `crates/cli/src/memory.rs`, `apps/desktop/src/views/MemoryView.tsx`
- Modify: `crates/core/src/{lib,crypto,recovery,transfer,drafts}.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/{ipc,store}.ts`, `apps/desktop/src/App.tsx`, `apps/desktop/src/views/views03.test.tsx`

**Interfaces:**
- Consumes: `knowledge::normalize`, `knowledge::Finding`, `crypto::{unlocked, key_by_id}`, and `drafts::{states_authorization, fold}`, which become `pub(crate)` here.
- Produces, in `memory`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class { Normal, Sensitive, Legal, WorkAuthorization } // derive order is the rank
impl Class {
    pub fn as_str(self) -> &'static str;
    /// Sensitive and Legal: the answer body is sealed.
    pub fn sealed(self) -> bool;
}
#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub id: String, pub text: String, pub class: Class, pub class_source: String,
    pub country: Option<String>, pub source: String, pub source_ref: Option<String>,
    pub findings: Vec<Finding>, pub classified_at: Option<String>,
    pub created_at: String, pub updated_at: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub id: String, pub question_id: String, pub body: String, pub sealed: bool,
    pub approved: bool, pub version: i64, pub source: String, pub source_ref: Option<String>,
    pub reviewed_at: String, pub stale: bool, pub created_at: String, pub updated_at: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct QuestionRow { pub question: Question, pub answers: Vec<Answer> }

pub const FRESH_DAYS: i64 = 180;
pub async fn add_question(db: &Db, text: &str, country: Option<&str>) -> Result<Question>;
pub async fn list(db: &Db, class: Option<Class>) -> Result<Vec<QuestionRow>>;
pub async fn get(db: &Db, question_id: &str) -> Result<QuestionRow>;
pub async fn add_answer(db: &Db, question_id: &str, text: &str, source_ref: Option<&str>) -> Result<Answer>;
pub async fn edit_answer(db: &Db, answer_id: &str, text: &str) -> Result<Answer>;
pub async fn approve(db: &Db, actor: &str, answer_id: &str, approved: bool) -> Result<()>;
pub async fn mark_reviewed(db: &Db, actor: &str, answer_id: &str) -> Result<()>;
pub async fn set_class(db: &Db, actor: &str, question_id: &str, class: Class) -> Result<Question>;
pub async fn reveal(db: &Db, actor: &str, answer_id: &str) -> Result<String>;
pub async fn remove_question(db: &Db, id: &str) -> Result<()>;
pub async fn remove_answer(db: &Db, id: &str) -> Result<()>;
pub async fn seal_pending(db: &Db) -> Result<usize>;
pub async fn export_sealed(db: &Db) -> Result<(String, bool)>;
pub(crate) async fn raise_class(db: &Db, question_id: &str, class: Class, source: &str) -> Result<bool>;
// memory::rules
pub fn classify(text: &str) -> Class;
```

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: questions and unsealed answers are Personal class, plain. A body in a Sensitive or
-- Legal class is Sensitive data: body = '' and body_enc holds JPK1 ciphertext (table
-- memory_answers, column body_enc, row id = id). Backup copies ciphertext with the database.
-- data.json drops BLOB columns; memory_sealed_answers.json holds the plain text after the export
-- warning, when the key is unlocked. Delete-all removes them with the root.
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
```

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/memory.rs`

Rules, as a table test `rules_class_each_question`:

| Text | Class |
|---|---|
| `Why do you want this job?` | Normal |
| `What are your salary expectations?` | Sensitive |
| `Do you identify as having a disability?` | Sensitive |
| `I certify that the information is true` | Legal |
| `Was this application written with AI?` | Legal |
| `Are you authorized to work in Canada?` | WorkAuthorization |
| `Will you now or in future need visa sponsorship?` | WorkAuthorization |

Bank:

- `a_country_in_the_text_is_not_a_key`: the Canada question above stores `country == None`.
- `the_same_question_twice_is_refused`, case and spacing folded.
- `hidden_text_in_a_question_is_reported_as_a_finding`: stored text is normalized, `findings` is non-empty.
- `answers_start_unapproved_and_fresh`.
- `a_work_authorization_question_takes_no_answer`: `add_answer` is `Err(Validation)`.
- `listing_sealed_answers_asks_the_keychain_nothing`: seeded sealed row, `body == ""`, `sealed`, `db.key_state() == KeyState::NotLoaded`.
- `an_answer_past_fresh_days_is_stale`: set `reviewed_at` by SQL to 181 days ago; `mark_reviewed` clears it.
- `only_a_person_approves_or_lowers`: actor `"system"` on `approve(true)`, `mark_reviewed`, `set_class` (lower): each `Err(Refused("only_a_person"))`, and nothing changed.
- `editing_unapproves_and_bumps_the_version`.
- `answers_are_never_evidence`: an approved normal answer is absent from `knowledge::evidence`.
- `raise_never_lowers`: `raise_class(.., Class::Normal, "model")` on a Sensitive question returns `false`, class unchanged, `class_source` unchanged.
- `raise_with_a_locked_key_hides_the_body_until_sealed`: raise a question with a plain answer; `raise_class` asks no key. The row is unapproved, `list` shows `body == ""`, `sealed == true`, the plain column still holds the text for `seal_pending`.
- `key_state_counts_sealed_answers`: a seeded sealed answer makes `crypto::known_state` return `NotLoaded` rather than no-key-yet.
- `export_without_sealed_answers_writes_an_empty_list`.

Keychain, `#[ignore]`d:

- `a_sealed_answer_round_trips_and_reveal_audits`: ciphertext lacks the plaintext, `reveal` returns it, audit `answer_revealed` holds no text.
- `lowering_opens_and_raising_seals`.
- `seal_pending_seals_a_raised_body`.
- `export_writes_sealed_answers_to_the_private_file`.
- `discard_clears_sealed_answers`: `recovery::discard_locked` leaves the row, `body_enc` null, unapproved, version bumped.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test memory`
Expected: FAIL, `memory` not found.

- [ ] **Step 4: Implement `memory::rules`**

`classify` runs on `drafts::fold(text)`, highest rank first, with the same word-start match as `states_authorization`:

```rust
const LEGAL: &[&str] = &["certify", "attest", "declare", "declaration", "under penalty", "perjury",
    "signature", "sign here", "truthful", "i agree", "i consent", "written by", "your own work",
    "generative", "chatgpt"];
/// Whole words: short enough to begin others.
const LEGAL_WHOLE: &[&str] = &["ai"];
const SENSITIVE: &[&str] = &["salary", "compensation", "pay expectation", "wage", "disability",
    "gender", "ethnicity", "race", "racial", "religion", "sexual orientation", "veteran",
    "criminal", "conviction", "date of birth", "pregnan", "national insurance",
    "social security", "marital"];
const SENSITIVE_WHOLE: &[&str] = &["age", "ssn"];
```

Add a `ponytail:` note: these match words, so a paraphrase passes until the model or the user raises the class.

- [ ] **Step 5: Implement `memory`**

- `add_question`: `normalize`, then at most 2,000 chars, `key` is `fold(text)` plus the country. Class from `rules::classify`, `class_source` `rules`, `source` `user`. A `country` is two letters, upper-cased, and comes only from the caller.
- `add_answer` and `edit_answer`: at most 20,000 chars. If the question's class is `sealed()`, seal first under `crypto::MEMORY_AAD = ("memory_answers", "body_enc")`, and a key error stores nothing. `source_ref` names a knowledge item that must exist, and sets `source = 'knowledge'`.
- `approve(true)` on a row with plain text under a sealed class calls `seal_pending` first, so the key error reaches the user.
- `set_class`: person only. Raising seals every body and unapproves. Lowering opens each body. It asks for a key only when a body exists, and a key error changes nothing.
- `raise_class`: returns `false` unless `class` outranks the stored class. In one transaction: set class and source, `approved = 0` on its answers, then audit `memory_class_raised`, actor `system`, detail `{ "agent": "career_memory", "from", "to" }`. It asks no key: the row is hidden at once, because `list` and `get` blank any body under a sealed class, whichever column holds it, and `seal_pending` seals it later.
- `seal_pending`: answers where class is sealed and `body <> ''`. With none it returns 0 and asks no key; otherwise it returns the count, or the key's error.
- `stale` computed as `julianday('now') - julianday(reviewed_at) > FRESH_DAYS`.
- `reveal`: actor `gui` or `cli`, audit `answer_revealed`, target the answer id.

Join `memory_answers` into each place in Global Constraints. In `transfer.rs`, `export_all` writes `memory_sealed_answers.json` through `write_private`, and `sensitive_plain` becomes the OR of both. `export_sealed` mirrors `profile::export_authorization`: with no sealed rows it asks nothing.

- [ ] **Step 6: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test memory && cargo test -p jobpilot-core --test crypto && cargo test -p jobpilot-core --test transfer`
Expected: PASS. Then `JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test -p jobpilot-core --test memory -- --ignored`, one process at a time.

- [ ] **Step 7: Add the CLI**

`MemoryCommand`, `Command::Memory` with doc "Questions and answers you reuse.":

```text
memory add-question TEXT [--country CC]    memory add-answer QUESTION TEXT [--from-item ITEM]
memory list [--class C]                    memory show QUESTION
memory edit-answer ANSWER TEXT             memory approve ANSWER        memory unapprove ANSWER
memory reviewed ANSWER                     memory set-class QUESTION CLASS
memory reveal ANSWER                       memory remove-question ID    memory remove-answer ID
```

PANEL: `memory add-question q`, `memory add-answer q a`, `memory list`, `memory show q`, `memory edit-answer a t`, `memory unapprove a`, `memory remove-question q`, `memory remove-answer a` are `false`. `memory approve a`, `memory reviewed a`, `memory set-class q normal`, `memory reveal a` are `true`.

- [ ] **Step 8: Add the Tauri commands and the view**

Commands: `memory_list(class)`, `memory_get(questionId)`, `memory_add_question(text, country)`, `memory_add_answer(questionId, text, sourceRef)`, `memory_edit_answer(answerId, text)`, `memory_approve(answerId, approved)`, `memory_mark_reviewed(answerId)`, `memory_set_class(questionId, class)`, `memory_reveal(answerId)`, `memory_remove_question(id)`, `memory_remove_answer(id)`.

`native::confirmed` guards `memory_approve` with `approved == true` on a sealed class, `memory_mark_reviewed` on a sealed class, and `memory_set_class` when lowering. Each drops `work` before the dialog. No lock crosses a dialog.

`NAV` gains `Memory` (Ctrl/Cmd+8), `View` gains `"memory"`. `MemoryView`, heading "Career memory":

- Class filter, `Add question` form with an optional country.
- Row: question as inert text, class badge, findings. Each answer shows the body, or "Sealed" with `Show`, then Approved or Waiting, Stale, Source, `Approve`, `Reviewed`, `Edit`, `Remove`.
- A `work_authorization` row has no answer form. It reads "Answered from your profile, for one country at a time."
- Empty state: "No questions yet. Add one you are often asked."

- [ ] **Step 9: Seam check**

- CLI: `memory_add_approve_and_list_round_trip`, `memory_refuses_an_answer_on_a_work_authorization_question`, panel refusal of `memory approve`.
- Tauri: `memory_commands_reach_core`; `approving_a_sealed_class_answer_asks_natively` (`NativeHook.confirm = false` gives `confirmation_required` and leaves it unapproved).
- View: `memory_add_question_then_answer_then_approve`, `a_sealed_answer_shows_only_on_request`, `work_authorization_row_has_no_answer_form`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates apps
git commit -m "feat: career memory bank with classes, sealing, approval, and freshness"
```

### Task 4: Confirmed use of sensitive answers

**Files:**
- Create: `crates/core/src/memory/usage.rs`, `crates/core/tests/memory_use.rs`
- Modify: `crates/core/src/memory/mod.rs`, `crates/core/src/profile.rs`, `crates/cli/src/{memory,apps,main}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/views/{ApplicationsView,views03.test}.tsx`

**Interfaces:**
- Consumes: `memory::{Class, Answer}`, `applications::get`, `application_answers` (Task 3).
- Produces, re-exported from `memory`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct UsedAnswer { pub application_id: String, pub answer_id: String, pub class: Class, pub text: String, pub stale: bool }
#[derive(Debug, Clone, Serialize)]
pub struct AppliedAnswer {
    pub answer_id: String, pub question: String, pub class: Class,
    pub confirmed_at: Option<String>, pub used_at: Option<String>,
    /// The confirmation still matches the answer's version.
    pub current: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct AuthorizationAnswer {
    pub country: String, pub authorized: Stated, pub needs_sponsorship: Stated,
    pub note: String, pub stated: bool,
}
pub async fn confirm_use(db: &Db, actor: &str, answer_id: &str, application_id: &str) -> Result<()>;
pub async fn use_answer(db: &Db, actor: &str, answer_id: &str, application_id: &str) -> Result<UsedAnswer>;
pub async fn application_answers(db: &Db, application_id: &str) -> Result<Vec<AppliedAnswer>>;
pub async fn authorization_for(db: &Db, country: &str) -> Result<AuthorizationAnswer>;
// profile
pub(crate) async fn authorization_row(db: &Db, country: &str) -> Result<Option<WorkAuthorization>>;
```

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/memory_use.rs`

Seed sealed rows with `DataKey::from_bytes`, so none of these touch the keychain:

```rust
#[tokio::test] async fn a_sealed_answer_needs_confirmation() {
    let refused = memory::use_answer(&db, "cli", &answer, &application).await.unwrap_err();
    assert_eq!(refused.code(), "confirmation_required");
}
#[tokio::test] async fn a_confirmation_covers_one_application() {
    memory::confirm_use(&db, "cli", &answer, &first).await.unwrap();
    let refused = memory::use_answer(&db, "cli", &answer, &second).await.unwrap_err();
    assert_eq!(refused.code(), "confirmation_required");
}
#[tokio::test] async fn editing_an_answer_voids_its_confirmation() { /* edit raw row version+1, approved=1 */ }
#[tokio::test] async fn a_stale_sealed_answer_needs_a_fresh_confirmation() { /* confirmation row older than the boundary */ }
```

Also:

- `an_unapproved_answer_is_refused`: `Err(Refused("answer_not_approved"))`, for any class, on both calls.
- `a_person_must_confirm_and_use`: actor `system` is `Err(Refused("only_a_person"))` on both.
- `a_work_authorization_answer_cannot_be_used`: a sealed row left under a raised class gives `Err(Refused("use_profile_by_country"))`.
- `a_normal_answer_needs_no_confirmation`: returns text, records `used_at`, audits `answer_used`, and the audit detail holds no answer text.
- `a_stale_normal_answer_is_returned_marked_stale`.
- `confirming_marks_it_reviewed_and_audits_with_no_text`: audit `answer_confirmed`, target the answer, detail `{ "application", "class" }`.
- `application_answers_lists_no_text_and_flags_a_voided_confirmation`.
- `canada_never_gets_the_us_row`: with only a `US` row seeded, `authorization_for(db, "CA")` gives `stated: false`, `authorized: Stated::NotStated`, `needs_sponsorship: Stated::NotStated`.
- `not_stated_asks_the_keychain_nothing`: `db.key_state() == KeyState::NotLoaded`.
- `a_bad_country_is_refused`: `canada` is `Err(Validation)`.

Keychain, `#[ignore]`d: `a_confirmed_sealed_answer_returns_its_text`, `authorization_for_opens_only_the_named_country`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test memory_use`
Expected: FAIL, `confirm_use` not found.

- [ ] **Step 3: Implement `usage.rs`**

`use_answer` checks in this order and decrypts only after all pass: actor, row exists, application exists, class is not `WorkAuthorization`, approved, then for a sealed class a row in `application_answers` with `confirmed_at` set, `answer_version` equal to the current version, and the answer not stale. `confirm_use` runs the first five checks, upserts the row, and sets `reviewed_at` to now, in one transaction with the audit row. `use_answer` sets `used_at`, and its audit row `answer_used` has detail `{ "application", "class" }`.

`profile::authorization_row` selects one row by exact country, and with no row returns `Ok(None)` before any key call.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test memory_use`
Expected: PASS.

- [ ] **Step 5: Add the CLI**

`memory confirm ANSWER --application ID`, `memory use ANSWER --application ID`, `memory authorization CC`, `app answers ID`. On `Err(ConfirmationRequired)`, `memory use` also prints to stderr: "Confirm it first in an ordinary terminal: jobpilot memory confirm <answer> --application <application>". PANEL: `memory confirm a --application x`, `memory use a --application x`, `memory authorization PT` are `true`; `app answers i` is `false`.

- [ ] **Step 6: Add the Tauri commands and the Applications section**

- `memory_confirm_use(answerId, applicationId)` always opens `native::confirmed`, title "Confirm this answer", action "Confirm". The message names the class, company and role, and question text, says "You confirm this is true today", and adds "This answer has not been reviewed for over 180 days." when stale. It never holds the answer value.
- `memory_use(answerId, applicationId)`, `memory_application_answers(applicationId)`, `memory_authorization(country)`.

In the detail of `ApplicationsView`, section "Answers for this application": a picker over approved answers, and for each row:

| State | Control |
|---|---|
| Normal | `Copy` |
| Sensitive or legal, not confirmed or voided | `Confirm and copy` |
| Confirmed and current | `Copy` |

The button runs confirm (when needed) then use, fills a read-only labelled field, then calls `navigator.clipboard.writeText`. A clipboard failure leaves the text in that field. `ConfirmationRequired` from a cancelled dialog shows nothing else and leaves no text. A `Work authorization` row asks for a country and shows the stated values or "Not stated by you", with a button that opens Profile. There is no suggested value and no input for it here.

- [ ] **Step 7: Seam check**

- CLI: `memory_use_is_refused_until_confirmed_and_prints_the_hint`, `memory_authorization_says_not_stated`, panel refusal of `memory confirm`.
- Tauri: `confirm_use_opens_a_native_dialog_without_the_value`, using a Normal-class question moved to Sensitive by seeded SQL; assert `NativeHook.asked` lacks the seeded canary text. `a_refused_dialog_records_no_confirmation`.
- View: `sensitive_answer_shows_confirm_and_copy_never_copy`, `work_authorization_row_has_no_value_input`, `a_cancelled_confirmation_shows_no_text`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates apps
git commit -m "feat: per-use confirmation for sensitive answers, and authorization by exact country"
```

### Task 5: Memory retrieval

**Files:**
- Create: `crates/core/migrations/0008_memory_search.sql`, `crates/core/src/memory/search.rs`, `crates/core/tests/memory_search.rs`
- Modify: `crates/core/src/memory/mod.rs`, `crates/cli/src/memory.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/views/{MemoryView,views03.test}.tsx`

**Interfaces:**
- Consumes: `Router::embed`, `documents::fts_query`, `knowledge::{cosine, from_blob, to_blob}`, `embed::VECTOR_CEILING`.
- Produces, re-exported from `memory`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct MemoryHit {
    pub question_id: String, pub question: String, pub class: Class,
    pub answer_id: Option<String>,
    /// `Some` only for an approved answer in `Class::Normal`.
    pub answer: Option<String>,
    pub stale: bool, pub needs_confirmation: bool,
    pub source: Option<String>, pub score: f32,
    /// `"semantic"` or `"text"`.
    pub via: String,
}
pub async fn search(db: &Db, router: &Router, query: &str, limit: usize) -> Result<Vec<MemoryHit>>;
pub async fn index_missing(db: &Db, router: &Router, cancel: CancellationToken) -> Result<usize>;
pub(crate) async fn reindex(write: &mut SqliteConnection, question_id: &str) -> Result<()>;
```

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: derived and rebuilt from memory_questions and plain answers. memory_fts rowid is
-- memory_questions.seq. A work_authorization question has no row in either table, and a sealed
-- class indexes the question only, never an answer. Export skips FTS tables and BLOB columns.
CREATE VIRTUAL TABLE memory_fts USING fts5(question, answer, tokenize='unicode61 remove_diacritics 2');
CREATE TABLE memory_embeddings (
  question_id TEXT NOT NULL REFERENCES memory_questions(id) ON DELETE CASCADE,
  model TEXT NOT NULL,
  dims INTEGER NOT NULL,
  vector BLOB NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (question_id, model)
);
```

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/memory_search.rs`, with a scripted embedding `Provider` as in `tests/embed.rs`

- `search_finds_by_meaning_with_a_local_model`: `via == "semantic"`, the nearest question first.
- `search_falls_back_to_text_with_no_model`: `via == "text"`, no error.
- `search_falls_back_to_text_when_nothing_is_indexed`.
- `a_sealed_answers_value_is_never_indexed_or_returned`: canary body absent from `SELECT * FROM memory_fts`; hit has `answer == None`, `needs_confirmation == true`.
- `work_authorization_question_is_never_found_by_search`: not in either index, for either path, for the exact text.
- `raising_a_class_removes_it_from_both_indexes`: an embedded normal question raised to `work_authorization` has no vector or FTS row.
- `an_unapproved_answer_is_not_returned`.
- `no_memory_text_reaches_an_external_provider`: an external scripted provider that declares no `Embeddings, LocalExecution` is never called.
- `index_missing_skips_work_authorization_and_rebuilds_after_cancel`: cancel between batches, the next call embeds the rest.
- `a_vector_count_mismatch_stores_nothing`.
- `an_empty_query_returns_nothing`.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test memory_search`
Expected: FAIL, `search` not found.

- [ ] **Step 4: Implement `memory::search`**

`reindex`: delete the row with `rowid = seq`, then insert unless the class is `WorkAuthorization`. The `answer` column joins plain bodies only when the class is `Normal`. Call it inside each write transaction from Task 3: add or remove a question or answer, edit, set class, raise, seal. Raising to `WorkAuthorization` also deletes the question's vectors.

`search`: an empty `fts_query` returns nothing. Try `router.embed`. On `Err(ManualRequired(_))`, or when no stored vector matches the model, use FTS with `bm25` order. Cosine over `memory_embeddings` joined to questions, `LIMIT VECTOR_CEILING`, same dims filter as `embed::search`. Each hit takes the newest approved answer of its question. `needs_confirmation` is true for a sealed class.

`index_missing`: batches of 32, only non-`WorkAuthorization` questions without a row for the model, embeds question text, stores each batch in one transaction, re-checks the class in the `INSERT ... SELECT` as `embed::index_missing` does, and calls `cancel.is_cancelled()` between batches.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test memory_search`
Expected: PASS.

- [ ] **Step 6: Add the adapters**

CLI: `memory search QUERY [--limit N]` (default 10), and `memory refresh`, which for now calls `index_missing` through `with_ctrl_c`, and returns `{ "embedded": n }`; `no_embedding_model` prints "No embedding model is set; search uses text." with exit 0. PANEL: both `false`.

Tauri: `memory_search(query, limit)`, `memory_refresh()` under `state.ai.begin()`.

`MemoryView` gains a labelled search field, and hits showing question, class badge, answer or "Confirm on an application", `via` as "by meaning" or "by text", and a `Refresh index` button. Empty result: "Nothing matches."

- [ ] **Step 7: Seam check**

- CLI: `memory_search_by_text_with_no_model`.
- Tauri: `memory_search_reaches_core`.
- View: `memory_search_shows_a_sealed_hit_with_no_value`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates apps
git commit -m "feat: semantic and text retrieval over career memory, never by similarity for authorization"
```

### Task 6: Workflow runner, tracking agent, agent monitoring

**Files:**
- Create: `crates/core/migrations/0009_agent_runs.sql`, `crates/core/src/runner/{mod,perms,store}.rs`, `crates/core/src/agents/{mod,tracking}.rs`, `crates/core/tests/{runner,agents}.rs`, `crates/cli/src/agents.rs`, `apps/desktop/src/views/AgentsView.tsx`
- Modify: `crates/core/src/{lib,error}.rs`, `crates/core/src/ai/{mod,router}.rs`, `crates/cli/src/{main,apps}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/{ipc,store}.ts`, `apps/desktop/src/App.tsx`, `apps/desktop/src/views/views03.test.tsx`

**Interfaces:**
- Consumes: `ai::{BoxFuture, CancellationToken, DataCategory, ProviderId, TaskType, RUN_REASONS}`, `applications::set_status`, `Router`.
- Produces. `Error::Paused`, code `paused`, message "emergency pause is on". In `runner`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent { Resume, CoverLetter, CareerMemory, Tracking, Analytics }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState { Queued, Checking, Running, Checkpointed, Retrying, WaitingForHuman, Halted,
    Completed, ManualRequired, Refused, Failed, Cancelled }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission { ReadJobText, ReadCompanyText, ReadProfileBasic, ReadResumeText,
    ReadCareerMemory, ReadSensitive, ReadApplications, WriteDrafts, WriteCareerMemory,
    WriteApplications, WriteReports, Model(TaskType), EmbedLocal }

pub struct AgentSpec { pub agent: Agent, pub permissions: &'static [Permission], pub run_timeout: Duration }
pub fn spec_of(agent: Agent) -> &'static AgentSpec;

pub struct StepDef { pub name: &'static str, pub needs: &'static [Permission], pub attempts: u32 }
#[derive(Default)]
pub struct StepOutput { pub provider: Option<ProviderId>, pub categories: &'static [DataCategory] }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunInput {
    Resume { job_text_id: String, export: Option<ExportChoice> },
    CoverLetter { job_text_id: String, export: Option<ExportChoice> },
    MemoryRefresh,
    Tracking { application_id: String, status: String, on: Option<String> },
    Analytics,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExportChoice { pub format: ExportFormat, pub template: Template }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Checkpoint(BTreeMap<String, String>); // keys are &'static in code, values are hex ids

pub trait Workflow: Send + Sync {
    fn agent(&self) -> Agent;
    fn input(&self) -> RunInput;
    fn steps(&self) -> &'static [StepDef];
    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>>;
    /// After a cancel, for a partial result the table in `docs/agents.md` says to drop.
    fn abandon<'a>(&'a self, _db: &'a Db, _saved: &'a Checkpoint) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
pub struct Ctx<'a> { pub db: &'a Db, pub actor: &'a str, pub cancel: CancellationToken, /* private: agent, checkpoint */ }
impl Ctx<'_> {
    pub fn check(&self, permission: Permission) -> Result<()>; // Err(Refused("permission_denied"))
    pub fn save(&self, key: &'static str, id: &str) -> Result<()>; // Validation unless a hex id
    pub fn saved(&self, key: &str) -> Option<String>;
    pub fn stop_point(&self) -> Result<()>; // Err(Cancelled) once the token fired
}
#[derive(Debug)]
pub struct Finished {
    pub run_id: String, pub state: RunState, pub reason: Option<String>, pub saved: Checkpoint,
    /// What ended the run, kept for the caller. Never stored.
    pub error: Option<Error>,
}
impl Finished { pub fn into_result(self) -> Result<Checkpoint>; }
pub async fn start(db: &Db, actor: &str, workflow: &dyn Workflow, cancel: CancellationToken) -> Result<Finished>;
pub async fn resume(db: &Db, actor: &str, run_id: &str, workflow: &dyn Workflow, cancel: CancellationToken) -> Result<Finished>;
pub async fn reap_stale(db: &Db) -> Result<usize>;
pub async fn list(db: &Db, limit: i64) -> Result<Vec<RunSummary>>;
pub async fn get(db: &Db, run_id: &str) -> Result<RunDetail>;
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary { pub id: String, pub agent: Agent, pub state: RunState, pub step: i64,
    pub steps: i64, pub reason: Option<String>, pub actor: String, pub created_at: String,
    pub finished_at: Option<String> }
#[derive(Debug, Clone, Serialize)]
pub struct RunDetail { pub run: RunSummary, pub log: Vec<LogRow> }
#[derive(Debug, Clone, Serialize)]
pub struct LogRow { pub at: String, pub step: String, pub state: RunState, pub duration_ms: i64,
    pub provider: Option<String>, pub data_categories: Vec<DataCategory>, pub outcome: String }
```

In `agents`:

```rust
pub async fn set_status(db: &Db, actor: &str, application_id: &str, status: &str, on: Option<&str>, note: &str, cancel: CancellationToken) -> Result<Application>;
pub async fn resume_run(db: &Db, actor: &str, run_id: &str, cancel: CancellationToken) -> Result<Finished>;
pub async fn cancel_run(db: &Db, actor: &str, run_id: &str) -> Result<()>;
```

`into_result`: `Completed` gives the checkpoint. `Halted` gives `Err(Paused)`, `Cancelled` gives `Err(Cancelled)`. Otherwise it returns `error`, or, with none stored, `Failed(reason)`, `ManualRequired(reason)`, or `Refused(reason)` by state.

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: agent_runs and agent_run_log hold ids, enum choices, codes, and durations. Never a
-- prompt, an output, posting text, an answer, or a secret (M-26). Backup copies them, export
-- writes them, delete-all removes them. `waiting_for_human` is reserved for Release 0.6.
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
  reason TEXT,
  cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0,1)),
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
```

Add a `ponytail:` note in `store.rs`: runs and logs are never pruned; add retention past 100,000 rows.

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/runner.rs`, with a scripted `Workflow`

- `a_run_logs_each_state_in_order`: states `queued, checking, running, checkpointed, checking, running, checkpointed, completed` for two steps.
- `a_step_outside_the_agents_set_never_runs`: `needs` holds `WriteReports` for `Tracking`; state `refused`, reason `permission_denied`, step body flag false.
- `a_check_inside_a_step_refuses_too`: `ctx.check(Permission::WriteDrafts)` from a Tracking step.
- `every_agent_step_stays_in_its_set`: each `spec_of` set against each workflow's `steps()`, table-driven (extended by Tasks 7, 8).
- `permission_sets_match_the_spec_table`: pins the exact sets:

| Agent | Permissions | `run_timeout` |
|---|---|---|
| Resume | `ReadResumeText, ReadProfileBasic, ReadJobText, Model(DraftResume), WriteDrafts` | 30 min |
| CoverLetter | `ReadResumeText, ReadProfileBasic, ReadJobText, ReadCompanyText, Model(DraftCoverLetter), WriteDrafts` | 30 min |
| CareerMemory | `ReadCareerMemory, ReadJobText, ReadSensitive, WriteCareerMemory, EmbedLocal, Model(ClassifyAnswer)` | 10 min |
| Tracking | `ReadApplications, WriteApplications` | 30 s |
| Analytics | `ReadApplications, WriteReports` | 60 s |

`Model(ClassifyAnswer)` needs the variant from Task 8, so add `TaskType::ClassifyAnswer` to the enum here with its spec, and `classify_answer.json` in Task 6 Step 4.

- `no_permission_names_a_forbidden_power`: no permission's `Debug` text contains `shell`, `secret`, `approve`, `path`, `browser`, or `pause`.
- `a_busy_database_is_retried_once`: a step fails with a busy error, built from a test `sqlx::error::DatabaseError` impl with code `5`, then passes; log holds `retrying`, ends `completed`.
- `a_validation_error_is_not_retried`: one run of the body, state `failed`, reason `validation`; `into_result` returns the original `Validation` message.
- `a_step_past_the_run_timeout_fails_and_its_token_fires`: `Limits::for_test(100ms)`, reason `timeout`, token cancelled.
- `cancel_fires_the_token_and_ends_cancelled`.
- `a_cancel_requested_elsewhere_stops_the_run`: a second `Db` handle sets `cancel_requested` through `agents::cancel_run`, the run ends `cancelled` within one second.
- `a_cancelled_run_cannot_resume`.
- `a_checkpoint_refuses_anything_but_an_id`: `ctx.save("draft", "Dear hiring manager")` is `Err(Validation)`.
- `resume_starts_after_the_last_checkpoint`: a two-step run moved to `halted` after step one; step one's body runs once in total.
- `a_dead_runs_row_is_reaped_to_halted`: a row `running`, `heartbeat_at` 60 s old; `runner::list` shows `halted`, reason `interrupted`, one log row. A row with a fresh heartbeat is untouched.
- `manual_required_ends_as_manual_required_with_its_reason`.
- `a_reason_outside_the_allow_list_is_stored_as_its_code`: `Err(ManualRequired("Bearer sk-abc"))` stores `manual_required`.

In `crates/core/tests/agents.rs`:

- `tracking_status_change_runs_on_the_runner`: `agents::set_status` returns the application, one `completed` run of agent `tracking`, with an empty provider and categories.
- `tracking_validates_before_a_run_exists`: an unknown application gives `not_found`, a bad status gives `validation`, no run row.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test runner`
Expected: FAIL, `runner` not found.

- [ ] **Step 4: Implement the runner**

`perms.rs` holds `Permission`, the static sets, and `spec_of`. `store.rs` holds all SQL and the transition writer: state update and log row in one transaction, and `finished_at` set on a terminal state.

Add `TaskType::ClassifyAnswer`, `crates/core/schemas/classify_answer.json`:

```json
{
  "type": "object",
  "required": ["class"],
  "properties": { "class": { "type": "string", "enum": ["normal", "sensitive", "legal", "work_authorization"] } },
  "additionalProperties": false
}
```

Its `TaskSpec`: `required: DRAFTING`, `categories: &[DataCategory::JobText]`, `attempts: 2`.

`execute` is shared by `start` and `resume`:

```text
reap_stale; insert or load row
for index in step..steps.len():
  Checking: cancel_requested -> Cancelled; needs outside spec -> Refused; nothing else in Task 6
  Running: select { step future; watch tick every 250 ms; deadline }
     tick: heartbeat every 2 s; cancel_requested -> fire token, keep polling the step
     deadline: fire token, reason timeout
     after the token fires, give the step 5 s to end, then drop it
  Ok(output) -> Checkpointed: store step + 1 and the checkpoint, with provider and categories on the log row
  busy and attempts left -> Retrying, then Running
  Err -> terminal state by the mapping below
Completed
```

| Step result | State | Stored reason |
|---|---|---|
| `Err(ManualRequired(r))` | `manual_required` | `r` if in `RUN_REASONS` or `no_evidence`, else `manual_required` |
| `Err(Cancelled)` | `cancelled` | none |
| `Err(Paused)` | `halted` | `paused` |
| `Err(Refused(_))` | `refused` | the allow-listed word, else `refused` |
| `Err(Timeout)` | `failed` | `timeout` |
| any other `Err` | `failed` | `run_reason(&error)` |

Make `router::run_reason` `pub(crate)`. Busy means `Error::Db(sqlx::Error::Database(e))` with code `5` or `SQLITE_BUSY`. A run's budget restarts on resume. On `cancelled`, call `workflow.abandon`. `Limits` holds `run_timeout`, is `#[doc(hidden)]`, and exists for tests through `start_with`.

`reap_stale`: active states with `heartbeat_at` older than 30 s move to `halted`, reason `interrupted`. `list`, `get`, and `start` call it.

`agents::cancel_run`: an active run sets `cancel_requested`. A `halted` run moves to `cancelled` here, after running the workflow's `abandon`. A terminal run is `Err(Validation("this run has ended"))`. Audit `run_cancelled`, actor the person, detail `{ "agent" }`. `runner::resume` refuses unless `halted`, and audits `run_resumed`. `agents::resume_run` rebuilds the workflow from the stored `RunInput`; Tracking's note is not stored, so a resumed update carries an empty note.

- [ ] **Step 5: Implement the tracking agent**

`tracking.rs`: one step `update`, `needs: [ReadApplications, WriteApplications]`, `attempts: 2`, calling `ctx.check` for both then `applications::set_status`. `agents::set_status` first runs `applications::get` and a status check, so bad input gets its plain error and no row, then `runner::start`, then returns `applications::get`.

Switch `app status` and `app_set_status` to `agents::set_status`.

- [ ] **Step 6: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test runner --test agents --test applications`
Expected: PASS.

- [ ] **Step 7: Add the adapters**

CLI `crates/cli/src/agents.rs`, `Command::Agents` with doc "Agent runs and their logs.": `agents list [--limit N]`, `agents show ID`, `agents log ID`, `agents cancel ID`, `agents resume ID`. PANEL: all five `false`.

Tauri: `agents_list(limit)`, `agent_get(id)`, `agent_cancel(id)`, `agent_resume(id)`, the last under `state.ai.begin()`.

`NAV` gains `Agents` (Ctrl/Cmd+9), `View` gains `"agents"`. `AgentsView`, heading "Agents":

- Table: Agent, State, Step (`2 of 3`), Started, Reason. `refetchInterval: 2000` while any run is active.
- Selecting a run shows its log: time, step, state, duration, provider, categories, outcome.
- `Cancel run` for an active or halted run, `Resume` for a halted one.
- Empty state: "No agent has run yet."

- [ ] **Step 8: Seam check**

- CLI: `agents_list_shows_a_tracking_run_after_app_status`, `agents_log_prints_states`.
- Tauri: `agents_commands_reach_core`.
- View: `agents_view_lists_runs_and_shows_a_log`, `resume_is_offered_only_on_a_halted_run`, `ctrl_9_opens_agents`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`, then the Windows recipe in `docs/development.md`.
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates apps
git commit -m "feat: deterministic workflow runner, tracking agent, and agent monitoring"
```

### Task 7: Analytics agent

**Files:**
- Create: `crates/core/migrations/0010_reports.sql`, `crates/core/src/analytics.rs`, `crates/core/src/agents/analytics.rs`, `crates/core/tests/analytics.rs`, `crates/cli/src/analytics.rs`
- Modify: `crates/core/src/{lib,agents/mod}.rs`, `crates/core/tests/runner.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/views/{ApplicationsView,views03.test}.tsx`

**Interfaces:**
- Consumes: `runner::{Workflow, StepDef, Ctx, start}`, `application_events`.
- Produces:

```rust
// analytics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Volume { pub total: i64, pub by_status: BTreeMap<String, i64>, pub applied_last_30_days: i64 }
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Funnel { pub applied: i64, pub responded: i64, pub interviews: i64, pub offers: i64,
    pub response_rate: Option<f64>, pub offer_rate: Option<f64> }
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing { pub median_days_to_response: Option<f64>, pub responses_counted: i64 }
#[derive(Debug, Clone, Serialize)]
pub struct Report { pub id: String, pub created_at: String, pub volume: Volume, pub funnel: Funnel, pub timing: Timing }
pub async fn latest(db: &Db) -> Result<Option<Report>>;
// agents
pub async fn analytics_report(db: &Db, actor: &str, cancel: CancellationToken) -> Result<Report>;
```

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: counts and rates only, no company, role, or note. Backup copies it, export writes it,
-- delete-all removes it. complete = 0 marks a report still being built.
CREATE TABLE reports (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0,1)),
  figures TEXT NOT NULL DEFAULT '{}'
);
```

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/analytics.rs`

- `an_empty_history_reports_zeros_and_no_rates`: `response_rate == None`, `offer_rate == None`, `median_days_to_response == None`.
- `applied_means_ever_applied`: an application moved `applied` then `rejected` still counts in `applied` and `responded`.
- `response_rate_is_responded_over_applied`: 4 applied, 1 screening: `Some(0.25)`.
- `median_days_uses_the_first_response`: `applied_on` plus the first event among `screening, interviewing, offer, accepted, rejected`.
- `a_negative_gap_is_not_counted`: a response dated before `applied_on`.
- `saved_only_applications_count_in_volume_alone`.
- `a_report_holds_no_text`: canary company, role, and note absent from `reports.figures`.
- `latest_ignores_an_incomplete_report`.
- `a_cancelled_report_is_dropped`: cancel after the first section, no row left.
- `analytics_runs_on_the_runner`: a `completed` run of agent `analytics`, three `checkpointed` rows, no provider.

Add the workflow to `every_agent_step_stays_in_its_set`.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test analytics`
Expected: FAIL.

- [ ] **Step 4: Implement**

`analytics.rs` holds `pub(crate) async fn volume`, `funnel`, `timing`, and `latest`, all from aggregate SQL over `applications` and `application_events`. `responded` is applied applications with a `status` event to `screening, interviewing, offer, accepted, rejected`; `interviews` and `offers` count applications with an event to `interviewing` and `offer`. The median sorts in Rust.

`agents/analytics.rs`: steps `volume`, `funnel`, `timing`, each `needs: [ReadApplications, WriteReports]`, `attempts: 2`. The first creates the row and saves `report`. Each merges its section by `json_set`. The last sets `complete = 1`. `abandon` deletes the row in `saved`. Add the case to `agents::resume_run`.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test analytics --test runner`
Expected: PASS.

- [ ] **Step 6: Add the adapters**

CLI: `analytics report`, `analytics show` (latest, "No report yet." when none). PANEL: both `false`.

Tauri: `analytics_run()`, `analytics_latest()`.

`ApplicationsView` gains a "Summary" block above the table: total, applied in the last 30 days, response rate, offer rate, median days to a response, per-status counts. `Refresh` runs a report. A rate with no value shows "Not enough data yet". No chart in this release.

- [ ] **Step 7: Seam check**

- CLI: `analytics_report_after_applying_prints_a_rate`.
- Tauri: `analytics_commands_reach_core`.
- View: `summary_shows_not_enough_data_with_no_applications`, `refresh_runs_a_report`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates apps
git commit -m "feat: analytics agent, CLI report, and the Applications summary"
```

### Task 8: Resume, cover letter, and career memory agents

**Files:**
- Create: `crates/core/src/agents/{draft,memory}.rs`, `crates/core/src/memory/classify.rs`, `crates/core/tests/injection/17_relabel_question.txt`
- Modify: `crates/core/src/generate.rs`, `crates/core/src/memory/mod.rs`, `crates/core/src/agents/mod.rs`, `crates/core/tests/{agents,injection,runner}.rs`, `crates/cli/src/{resume,memory,main}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/views/{ResumesView,MemoryView,views02.test,views03.test}.tsx`

**Interfaces:**
- Consumes: `generate::{resume, cover_letter, Generated}`, `drafts::export`, `memory::{seal_pending, raise_class, index_missing}`, `TaskType::ClassifyAnswer`, `runner`.
- Produces:

```rust
// generate
pub struct Generated { pub draft: Draft, pub blocked: usize, pub provider: ProviderId, pub exported: Option<String> }
pub(crate) const OPEN: &str; pub(crate) const CLOSE: &str; pub(crate) fn neutralise(text: &str) -> String;
// memory::classify
#[derive(Debug, Clone, Default, Serialize)]
pub struct Classified { pub checked: usize, pub raised: usize, pub skipped: bool }
pub fn classify_prompt(question: &str) -> Prompt;
pub async fn classify_pending(db: &Db, router: &Router, actor: &str, cancel: CancellationToken) -> Result<Classified>;
pub const CLASSIFY_BATCH: usize = 20;
// agents
pub async fn draft_resume(db: &Db, router: &Router, actor: &str, job_text_id: &str, export: Option<ExportChoice>, cancel: CancellationToken) -> Result<Generated>;
pub async fn draft_cover_letter(db: &Db, router: &Router, actor: &str, job_text_id: &str, export: Option<ExportChoice>, cancel: CancellationToken) -> Result<Generated>;
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryRefreshed { pub sealed: usize, pub classified: Classified, pub embedded: usize }
pub async fn memory_refresh(db: &Db, router: &Router, actor: &str, cancel: CancellationToken) -> Result<MemoryRefreshed>;
```

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/agents.rs`, with scripted providers

- `resume_agent_stores_a_draft_and_logs_provider_and_categories`: `completed` run, a log row with `provider == "claude_code/cli_login"` (scripted external with consent) and categories `job_text, profile_basic, resume_text`.
- `no_ai_ends_manual_required_and_keeps_the_reason`: `Err(ManualRequired("no_ai"))`, run state `manual_required`.
- `no_evidence_never_calls_a_provider`, run state `manual_required`, reason `no_evidence`.
- `resume_export_renders_into_the_exports_folder_only`: `exported` is under `db.paths().exports()`.
- `a_blocked_draft_is_stored_and_not_rendered`: `blocked > 0`, `exported == None`, run `completed`.
- `a_halted_run_resumes_at_render_with_no_second_model_call`: `render` fails `Err(Paused)` on first pass; after `agents::resume_run`, provider call count is still 1, and the file exists.
- `cancelling_a_draft_stores_nothing`: earlier versions stay, run `cancelled`.
- `a_run_holds_no_posting_prompt_or_answer_text`: canaries in the posting, knowledge, profile, and reply; `SELECT * FROM agent_runs` and `agent_run_log`, joined to text, hold none of them.
- `memory_refresh_classifies_then_embeds`, with `Classified { checked, raised, skipped: false }`.
- `memory_refresh_with_no_ai_finishes`: `skipped: true`, run `completed`, class from the rules, `classified_at` still null.
- `a_model_raises_and_never_lowers`: reply `normal` on a Sensitive question leaves it, reply `legal` on a Normal question raises it, `class_source == "model"`. The questions hold no answers, so no key is asked.
- `a_model_may_not_approve`: approved stays 0 after a raise and any refresh.
- `classify_prompt_holds_question_text_only`: answers and profile canaries absent, the question sits between the markers.
- `classify_needs_no_tools_and_job_text_consent`: an external provider with `job_text` consent runs it, with none it is skipped, and one without `NoTools` is skipped.
- `refresh_stops_at_the_batch_limit`: 21 pending, 20 checked.

In `crates/core/tests/injection.rs`: `a_steered_classifier_never_lowers_a_class` runs `17_relabel_question.txt`, which asks to treat a certification as a preference, with `MOCK_REPLY: {"class":"normal"}`: class stays `legal`.

Add both new workflows to `every_agent_step_stays_in_its_set`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test agents`
Expected: FAIL, `draft_resume` not found.

- [ ] **Step 3: Implement `agents::draft`**

One `DraftWorkflow { task, job_text_id, export, router, generated: Mutex<Option<Generated>> }`. Steps, attempts 1:

| Step | Needs (resume; letter adds `ReadCompanyText` and uses its own `Model`) | Does |
|---|---|---|
| `draft` | `ReadResumeText, ReadProfileBasic, ReadJobText, Model(DraftResume), WriteDrafts` | Skips when `saved("draft")` is set. Calls `generate`, saves `draft`, returns provider and `spec(task).categories`. |
| `render` | `ReadResumeText, WriteDrafts` | With `export`, unless claims are blocked, calls `drafts::export` to `db.paths().exports()`. A free path is not an option. |

On a resumed run `generated` is rebuilt from `drafts::get` and `drafts::claims`. `agents::draft_resume` returns it after `Finished::into_result`. Add both cases to `resume_run`.

Switch `resume generate`, `letter generate`, `resume_generate`, and `letter_generate` to these. New flags `--export docx|pdf` and `--template`, default classic; the CLI still uses `with_ctrl_c`. Tauri: `resume_generate(jobTextId, export)`, `letter_generate(jobTextId, export)`, with `export: Option<ExportChoice>`, and `ipc.ts` gains `ExportChoice`, `exported: string | null`. `ResumesView` gains an "After drafting, export as" select: None, PDF, DOCX. On a result with `exported`, the path shows through `CopyPath`.

PANEL: `resume generate j --export pdf` and `letter generate j --export pdf` are `false`.

- [ ] **Step 4: Implement `memory::classify` and `agents::memory`**

`classify_prompt` uses `neutralise`, `OPEN`, and `CLOSE`, and a system text saying the question is data to label, never instructions, with the four classes defined.

`classify_pending`: up to `CLASSIFY_BATCH` questions, `classified_at IS NULL`, class below `WorkAuthorization`, oldest first. `Outcome::Ok` calls `raise_class(.., "model")` and sets `classified_at`. `ManualRequired` or `Failed` stops with `skipped: true` and leaves the rest. `Cancelled` is `Err(Cancelled)`. A class comes back only through the schema, then `raise_class`.

`agents/memory.rs`, attempts 1, `stop_point` between items, and `save("question", id)` after each:

| Step | Needs | Does |
|---|---|---|
| `classify` | `ReadCareerMemory, ReadJobText, Model(ClassifyAnswer), WriteCareerMemory` | `classify_pending` |
| `seal` | `ReadCareerMemory, ReadSensitive, WriteCareerMemory` | `seal_pending`, after classify so a raise seals in the same run. A key error is noted and the run continues: bodies stay hidden. |
| `embed` | `ReadCareerMemory, EmbedLocal, WriteCareerMemory` | `index_missing`; `no_embedding_model` counts as zero. |

Add the case to `resume_run`. `memory refresh` and `memory_refresh` now call `agents::memory_refresh`, and `MemoryView` shows "Checked {n}, raised {m}, indexed {k}", plus "No AI took this; classes come from the rules." when skipped.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core`
Expected: PASS, including every existing `generate` test, since `generate::resume` keeps its behaviour.

- [ ] **Step 6: Seam check**

- CLI: `resume_generate_appears_in_agents_list` (mock `claude`, wrapper per OS), `resume_generate_export_writes_to_the_exports_folder`, `memory_refresh_with_no_ai_reports_skipped`.
- Tauri: `resume_generate_runs_through_the_agent`: `agents_list` shows a `resume` run after it.
- View: every existing `views02` generate test still passes; `export_choice_reaches_resume_generate`, `refresh_says_when_no_ai_took_it`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`, then the Windows recipe.
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates apps
git commit -m "feat: resume, cover letter, and career memory agents on the runner"
```

### Task 9: Emergency pause

**Files:**
- Create: `crates/core/migrations/0011_emergency_pause.sql`, `crates/core/src/pause.rs`, `crates/core/tests/pause.rs`, `crates/cli/src/pause.rs`, `apps/desktop/src-tauri/src/menu.rs`, `apps/desktop/src/components/TitleBar.tsx`, `apps/desktop/src/lib/pause.ts`, `apps/desktop/src/App.pause.test.tsx`
- Modify: `crates/core/src/{lib,backup}.rs`, `crates/core/src/runner/mod.rs`, `crates/core/src/ai/{router,ollama}.rs`, `crates/core/tests/backup.rs`, `crates/cli/src/{main,ai,data,resume,memory}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/{commands,lib}.rs`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/App.tsx`, `apps/desktop/src/test-utils.tsx`

**Interfaces:**
- Consumes: `runner::{reap_stale, RunState}`, `audit::record_to`, `Error::Paused`, `InFlight::cancel`, `native::confirmed`.
- Produces, in `pause`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct PauseState { pub paused: bool, pub set_at: Option<String>, pub set_by: Option<String> }
pub async fn state(db: &Db) -> Result<PauseState>;
pub async fn is_set(db: &Db) -> Result<bool>;
pub async fn ensure_off(db: &Db) -> Result<()>; // Err(Paused)
pub async fn set(db: &Db, actor: &str) -> Result<PauseState>;
pub async fn clear(db: &Db, actor: &str) -> Result<PauseState>;
/// Resolves once the pause is on, after firing `cancel`. Polls every 250 ms.
pub async fn watch(db: &Db, cancel: &CancellationToken);
```

- CLI: `with_ctrl_c` becomes `with_interrupts(db: &Db, run: F)`.
- Tauri: `pause_status() -> PauseState`, `pause_set() -> PauseState`, `pause_clear() -> PauseState`.
- Frontend: `ipc.pauseStatus`, `ipc.pauseSet`, `ipc.pauseClear`, `usePause()`, `isPauseKey(e: KeyboardEvent): boolean`.

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: one row while the pause is on: who set it and when, no personal data. Backup copies
-- it, export writes it, delete-all removes it. A restore never clears it (backup.rs).
CREATE TABLE emergency_pause (
  id TEXT PRIMARY KEY CHECK (id = 'me'),
  set_at TEXT NOT NULL,
  set_by TEXT NOT NULL CHECK (set_by IN ('gui','cli'))
);
```

- [ ] **Step 2: Write the failing tests**

`crates/core/tests/pause.rs`:

- `set_and_clear_each_audit_once`: actions `emergency_pause_set`, `emergency_pause_cleared`; a second `set` writes no row.
- `the_pause_survives_a_reopen`.
- `system_cannot_set_or_clear`: `Err(Refused("only_a_person"))`.
- `pause_halts_a_run_and_cancels_the_in_flight_task`: a scripted workflow blocks on its token; a second `Db` handle calls `set`; within one second state `halted`, reason `paused`.
- `no_run_starts_while_paused`: `runner::start` is `Err(Paused)`, no row.
- `the_router_refuses_while_paused`: `Router::run` is `Err(Paused)`, provider call count 0.
- `pause_stops_between_router_attempts`: set after attempt 1 of a retried task; no attempt 2.
- `embed_while_paused_is_manual_so_text_search_still_works`: `memory::search` returns `via == "text"`.
- `a_pull_is_refused_while_paused`.
- `pause_reaps_dead_runs`: a stale `running` row moves to `halted`.
- `halted_runs_do_not_resume_on_clear`: state stays `halted` after `clear`.
- `resume_is_refused_until_cleared_then_passes_checking`: `Err(Paused)`, then after `clear` a `checking` log row.
- `cancel_works_while_paused`: a `halted` run moves to `cancelled`.
- `status_edits_say_paused_and_field_edits_still_work`.

`crates/core/tests/backup.rs`:

```rust
#[tokio::test] async fn restore_keeps_a_set_pause() {
    pause::set(&db, "cli").await.unwrap(); // backup taken before the pause
    let restored = backup::restore(db, &earlier, "cli").await.unwrap().db;
    assert!(pause::is_set(&restored).await.unwrap());
}
#[tokio::test] async fn restoring_a_paused_backup_stays_paused() { /* backup taken during it */ }
```

`crates/cli/tests/cli.rs`:

- `pause_from_a_second_process_stops_a_running_draft`: start `resume generate` with the mock `claude` sleeping 30 s (`cfg` pair for the wrapper, as in `crates/core/tests/claude_code.rs`), poll `agents list --json` until the run shows `running`, run `pause set`, and assert the child exits with `error[paused]` within 5 s, `agents list --json` shows `halted`, and `MOCK_CLAUDE_DONE` never appears. On Unix also assert the mock's pid is gone.
- `pause_set_status_clear_round_trip`, `pause_clear_is_refused_in_the_panel`, `status_prints_the_pause`.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test pause`
Expected: FAIL, `pause` not found.

- [ ] **Step 4: Implement `pause`, the gates, and restore**

- `set`: person only. In one transaction `INSERT OR IGNORE`, and audit only when a row went in. Then `runner::reap_stale`, with a stale row moving to `halted`.
- `clear`: person only, audits when a row went away.
- `runner`: `start` and `resume` call `ensure_off` first. Checking and the 250 ms tick read `is_set`. A hit fires the token, and the run ends `halted`, reason `paused`, whatever the step returned, except a step that had already finished, which checkpoints first. A token fired by an adapter with the flag set also ends `halted`.
- `Router::run_spec`: after `refusal`, `ensure_off`. In `try_provider`, before each attempt, a set flag is `Step::Cancelled`. `Router::embed`: a set flag is `Err(ManualRequired("paused"))`, so text search still answers. `Ollama::pull` calls `ensure_off`.
- `backup.rs`: `LiveRows` gains `paused: Option<bool>`, read in `prepare`'s live read and `read_live`, `None` when unreadable. `migrate_and_fill` inserts the row when `Some(true)`, with `set_by = plan.actor`, and writes audit `emergency_pause_kept_by_restore`. A backup's own row stays.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test pause --test backup --test runner --test router`
Expected: PASS.

- [ ] **Step 6: Add the CLI**

`crates/cli/src/pause.rs`, `Command::Pause` with doc "Stop all automation.": `pause set`, `pause status`, `pause clear`. PANEL: `pause set` and `pause status` `false`, `pause clear` `true`. `status` prints `paused: yes, set <at> by <actor>` and has `"paused"` in JSON. `main.rs::message` for `Error::Paused`: "Emergency pause is on, so nothing automated runs. Clear it with `jobpilot pause clear` in an ordinary terminal, or from the title bar."

`with_interrupts` adds `pause::watch(db, &token)` as a third `select!` branch, and then waits for the task as Ctrl-C does. Replace all callers, so `ai pull` and `ai index` stop too.

- [ ] **Step 7: Add the desktop side**

`commands.rs`:

- `pause_set`: fires `state.ai.cancel()` and `state.pull.cancel()` first, always, then `pause::set` through `state.db.read()`, not `work()`, so it never queues behind the gate. With no handle it returns `Err(Unavailable)` after firing the tokens.
- `pause_clear`: `native::confirmed(&app, "Clear emergency pause", "Runs stay halted until you resume each one.", "Clear")`, with no lock across the dialog.
- `pause_status`: `Unavailable` with no handle.

`menu.rs`:

```rust
pub const PAUSE_ID: &str = "emergency_pause";
pub const PAUSE_ACCELERATOR: &str = "CmdOrCtrl+Shift+H";
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>>;
pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent);
```

`build` starts from `Menu::default(app)`, so macOS keeps Edit with copy and paste, then appends a `Safety` submenu with `MenuItemBuilder::with_id(PAUSE_ID, "Emergency pause").accelerator(PAUSE_ACCELERATOR)`. `on_event` spawns the same function `pause_set` uses, with actor `gui`. `lib.rs`: `.menu(menu::build)` and `.on_menu_event(menu::on_event)`. In `setup`, spawn a 1 s loop: when `pause::is_set`, fire `state.ai` and `state.pull`, so a `pause set` from the CLI stops a GUI download or index.

Frontend:

- `isPauseKey`: `(e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "h"`.
- `usePause`: query `["pause"]`, `refetchInterval: 2000`, `retry: false`.
- `App`, not `Shell`, owns a `window` capture `keydown` listener that calls `preventDefault`, `stopPropagation`, and `ipc.pauseSet`, so a focused terminal, an open dialog, the recovery screen, and the loading state are all covered. The shortcut never clears.
- `TitleBar` replaces the `Shell` header, and `App` renders it first in the DOM for every return path. Off: destructive `Button` "Emergency pause", `aria-keyshortcuts="Control+Shift+H Meta+Shift+H"`, `title` naming the key. On: `role="status"` text "Paused. Nothing automated runs." and a `Clear pause` button. In recovery the button stays, and its `unavailable` error reads "No data is open, so nothing is running."
- On success, reset queries `["pause"]` and `["agents"]`.

- [ ] **Step 8: Seam check**

`commands.rs` tests:

- `pause_set_fires_the_running_tokens_and_is_audited_as_gui`.
- `pause_set_in_recovery_fires_tokens_and_reports_unavailable`.
- `pause_clear_asks_natively_and_stays_set_when_refused`.
- `the_menu_item_uses_the_shared_pause_path`: call `on_event`'s inner function on the mock app.
- `menu_has_the_pause_item_and_accelerator` asserts the constants, and `build` on the mock runtime.

`App.pause.test.tsx`:

```tsx
it.each(["Status", "Documents", "Data", "Profile", "Resumes", "AI", "Applications", "Memory", "Agents"])(
  "pause_is_the_first_tab_stop_on_%s", async (label) => {
    // open the view, move focus to body
    await user.tab();
    expect(screen.getByRole("button", { name: "Emergency pause" })).toHaveFocus();
  });
```

Also: `the_shortcut_pauses_from_every_view`, `the_shortcut_works_with_focus_in_the_terminal` (the event does not reach the terminal), `the_shortcut_works_in_recovery`, `the_shortcut_works_inside_an_open_dialog`, `the_shortcut_never_clears`, `a_pause_set_elsewhere_shows_within_a_poll`, `clear_pause_calls_pause_clear_only_from_its_button`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`, then the Windows recipe.
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates apps
git commit -m "feat: emergency pause from every screen, the native menu, and the CLI"
```

### Task 10: Release gates and docs

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock`, `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, `crates/core/tests/db.rs`, `CHANGELOG.md`, `README.md`
- Modify: `docs/milestones.md`, `docs/agents.md`, `docs/architecture.md`, `docs/database-schema.md`, `docs/ai-providers.md`, `docs/security/threat-model.md`, `docs/wireframes.md`, `docs/development.md`
- Create: `docs/releases/0.3.0.md`

**Interfaces:**
- Consumes: every earlier task.
- Produces: version `0.3.0`, a gate record with the placeholder rows Task 11 fills.

- [ ] **Step 1: Write the failing migration test** in `crates/core/tests/db.rs`

```rust
#[tokio::test] async fn a_0_2_database_upgrades_to_0_3() {
    // Open with a Migrator cut to versions 1..=5, add a document, a profile, a knowledge item,
    // a job text, and a draft, close, then Db::open.
    assert_eq!(db.schema_version().await.unwrap(), 11);
    // every seeded row reads back, the new tables are empty, and a `pre-migrate-5-*` folder exists
}
#[tokio::test] async fn a_fresh_database_is_schema_11() { /* ... */ }
```

Run: `cargo test -p jobpilot-core --test db a_0_2`. Expected: PASS once the cut migrator is written; a failure here is a real migration bug, so fix it forward in a new file.

- [ ] **Step 2: Bump the version** to `0.3.0` in the workspace `Cargo.toml`, `apps/desktop/package.json`, and `tauri.conf.json`. Run `cargo update --workspace`, then confirm `git diff --stat Cargo.lock` shows only the three workspace packages.

- [ ] **Step 3: Update the docs**

| File | Change |
|---|---|
| `docs/wireframes.md` | Screens 16 Applications, 17 Career memory, 18 Agents, in the file's format. Release map rows. Navigation block. Keyboard table: `Ctrl/Cmd+Shift+H` Emergency pause on every screen; Ctrl+1 to 9. Screen 2: title bar, focus order begins with Emergency pause. Note that Ctrl+0 is the last free number for Jobs in 0.5. |
| `docs/agents.md` | Header note that 0.3 is built. Section 7: `interrupted` under Halted, `waiting_for_human` reserved. Section 8: command names, `pause set`, `pause clear`, the shortcut, the Safety menu, the 250 ms poll, restore keeps the flag, status edits refused while paused. |
| `docs/database-schema.md` | New section 3 "Release 0.3 tables" with each migration's class and handling. Renumber migration rules. `schema_version` 11. Section on proposed entities drops `application`, `answer`, `agent_run`. |
| `docs/architecture.md` | Modules `applications`, `memory`, `runner`, `agents`, `analytics`, `pause`. CLI 0.3 surface. Adapter: `menu.rs`, pause watcher. Three new views, `TitleBar`. |
| `docs/ai-providers.md` | "As built": `classify_answer` task, new reasons `no_model_set`, `model_not_installed`. |
| `docs/security/threat-model.md` | M-05, M-23, M-24 to M-28 statuses with the tests that pin them. M-23: tool-use and exfiltration files exist, replies are still scripted. Residual risks: rule words miss a paraphrased sensitive question; the panel's own `claude` session is not stopped by pause. |
| `docs/development.md` | Last migration `0011`. The test list for keychain work. |
| `docs/milestones.md` | Mark 0.3 built, add "0.3 gate record" with the five gates, results from Step 5, and "Exit tests not proven end to end". |
| `CHANGELOG.md`, `README.md`, `docs/releases/0.3.0.md` | What shipped, the commands, risks and what no person ran. |

- [ ] **Step 4: Check the docs against the code**

Run: `grep -rn 'Planned 0.3' docs/security/threat-model.md`
Expected: only items a release note explains. Run `jobpilot --help` from `target/debug` and compare to the architecture CLI list.

- [ ] **Step 5: Run every check**

```bash
pnpm typecheck
pnpm test
pnpm --filter desktop sidecar
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test --workspace -- --ignored --skip real_claude --skip make_fixtures
pnpm tauri build --no-bundle
```

Then the `x86_64-pc-windows-gnu` recipe in `docs/development.md`.
Expected: all pass. Record counts in the gate record. Only one process runs the `--ignored` suite at a time.

- [ ] **Step 6: Commit**

```bash
git add -A Cargo.toml Cargo.lock apps crates CHANGELOG.md README.md docs
git commit -m "docs: Release 0.3 gates, schema, threat model, wireframes, and version 0.3.0"
```

### Task 11: Real end to end on a temporary root

Driven by the controller, not an implementer subagent: it spends a little of the owner's subscription, and it needs the real `claude`. Call the real `claude` exactly once for drafting, on a temporary root. Never touch the owner's data root. No full-screen screenshots. If a step fails, stop, fix through a subagent with a failing test first, and rerun from the failed step.

**Files:**
- Modify: `docs/milestones.md` (0.3 gate record), `docs/releases/0.3.0.md`

**Interfaces:**
- Consumes: the release build of `jobpilot`, the app from `pnpm tauri build --no-bundle`.

- [ ] **Step 1: Set up**

```bash
cargo build --release -p jobpilot-cli
E2E=$(mktemp -d); export JOBPILOT_HOME="$E2E/home" JOBPILOT_SECRET_SERVICE=jobpilot-e2e
JP="$PWD/target/release/jobpilot"
$JP status
```

Expected: schema 11, version 0.3.0, no key yet, `paused` no.

- [ ] **Step 2: Build the candidate data**

Run with `--json` where an id is needed: `profile set --name N --email E`, `kb add experience`, `kb add skill`, `job paste "Platform Dev" --file <temp posting>`.

- [ ] **Step 3: Career memory, with the keychain**

`memory add-question "Why this company?"`, add an answer, approve, then `memory add-question "What are your salary expectations?"`. Expect class `sensitive`. Add an answer, approve. Add `memory add-question "Are you authorized to work in Canada?"`: expect class `work_authorization`, with `add-answer` refused. Run `memory authorization CA`: expect not stated. Run `memory search "why this company"`: expect the first hit by text, and the salary answer shows no value.

- [ ] **Step 4: Tracking, use, and analytics**

`app add`, `app status <id> applied --on 2026-10-01`, `memory use <salary> --application <id>`: expect `confirmation_required` and the hint. `memory confirm`, then `memory use`: expect the text. Edit the answer and approve it: expect `use` refused again. `app status <id> screening`, `analytics report`: expect a response rate of 1.0. `agents list`: expect `tracking` and `analytics` `completed`.

- [ ] **Step 5: The one real drafting call**

`ai enable claude_code`, `ai consent give claude_code --categories job_text,profile_basic,resume_text`, `resume generate <job> --export pdf`. Expect a draft, a provider of `claude_code/cli_login`, a PDF under `$JOBPILOT_HOME/exports`. `agents log <run>`: states in order, provider set, categories listed.

Check the logs hold no text:

```bash
sqlite3 "$JOBPILOT_HOME/jobpilot.db" "SELECT * FROM agent_run_log; SELECT checkpoint, input FROM agent_runs;" | grep -c -i '<a distinctive word from the posting>'
```

Expected: `0`.

- [ ] **Step 6: Pause and cancel, without a second subscription call**

Point `JOBPILOT_CLAUDE_BIN` at the mock with `MOCK_CLAUDE_SLEEP=30`. In one terminal run `resume generate`. In a second run `pause set`. Expect the first to end `error[paused]` within 5 s, the run `halted`, and an audit row. `pause clear`, then `agents cancel <run>`: expect `cancelled`. Run another with `pause set` first: expect `paused` and no new row.

- [ ] **Step 7: Restore, export, upgrade**

`backup create`, `pause set`, `backup restore <path>`: expect still paused, consent revoked, Claude Code off in `ai status`. `pause clear`. `data export`: expect `memory_sealed_answers.json` private on Unix, and no sealed text in `data.json`. Build the 0.2 CLI from `main` at `dc3f9b6` in a worktree, make a root with it, and open that with the 0.3 binary: expect schema 11, a `pre-migrate-5-*` folder, `doc list` identical. Remove the worktree.

- [ ] **Step 8: The GUI, once**

Launch the release app with `JOBPILOT_HOME` on the same temporary root. Check these by keyboard: Tab from the top lands on Emergency pause; Ctrl/Cmd+Shift+H with focus in the terminal panel pauses; the Safety menu item pauses; `Clear pause` shows a native dialog; Ctrl+7, 8, 9 open their views and show the CLI's data. Capture only a window or element screenshot if evidence is needed.

- [ ] **Step 9: Clean up and record**

Delete `$E2E` and the `jobpilot-e2e` keychain entries, through `data delete --confirm DELETE`, before removing the folder. Fill the gate record: what ran, the real `claude` version, what is unproven (Windows ConPTY and native menu by hand, no real Ollama if none is installed).

```bash
git add docs
git commit -m "docs: Release 0.3 end-to-end results"
```
