# JobPilot AI Architecture Package and Release 0.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the ten architecture documents for owner approval, then ship Release 0.1: a tested, offline desktop app for macOS and Windows with local storage, search, backup, export, deletion, secrets, and a CLI sharing the same services.

**Architecture:** A Rust library (`jobpilot-core`) owns all behaviour. The Tauri app and the `jobpilot` CLI are thin adapters over it, so GUI and terminal cannot diverge. React talks only to Tauri commands. All data lives under one local root directory.

**Tech Stack:** Rust 1.99.1, Tauri 2.12, SQLx 0.9 (bundled SQLite, FTS5), keyring 4, clap 4, React 19, Vite 8, TypeScript, Tailwind 4, shadcn/ui, Zustand 5, TanStack Query 5, Vitest 5, pnpm 12, GitHub Actions, tauri-action v1.

**Spec:** `docs/superpowers/specs/2026-11-01-jobpilot-master-spec.md`

**Scope:** This plan covers the spec's "first deliver" list and Release 0.1. Releases 0.2 to 1.0 each get their own plan after Task A6 approval, because each depends on decisions made there.

## Global Constraints

- No mandatory API keys, API credits, hosted backend, or proprietary cloud account.
- Works with no network for every 0.1 feature. No network call exists in 0.1 code.
- Targets: macOS (arm64, x86_64) and Windows x64.
- License: Apache-2.0.
- App identifier: `io.github.gvsrusa.jobpilot`. Binary name: `jobpilot`.
- Toolchain: `rust-toolchain.toml` pins `1.99.1`. `packageManager` pins `pnpm@12.16.1`.
- SQL uses runtime `sqlx::query`, not `query!`, so builds need no `DATABASE_URL` or `sqlx-cli`.
- `.gitattributes` forces `*.sql text eol=lf`; SQLx migration hashes differ across CRLF and LF.
- Secret values never cross IPC to the webview, never reach logs, exports, or the audit log. If Decision D5 accepts the recovery-key file (0.2), that file is the one exception, conditional on D5.
- Consequential actions (backup, restore, export, delete, secret change) write an audit row, except delete-all, which removes the log.
- Never claim a feature works until its test has passed on both OSes (CI is the Windows evidence).
- Commits and PR bodies end with the attribution lines from the session instructions.
- Ponytail rule: no abstraction without a second caller. Mark deliberate limits with a `ponytail:` comment naming the ceiling.

## Review Focus

1. Windows user folder with spaces or non-ASCII (`C:\Users\José Ñ\`): app opens normally. Pinned by `opens_under_unicode_path_with_spaces` (Task B2).
2. Search text with FTS5 syntax (`c++ "dev`, `AND`, `*`, `-`): returns hits or nothing, never an error. Pinned by `search_accepts_any_user_text` (Task B3).
3. Restoring a truncated, corrupt, or newer-schema backup: refused, live data untouched. A valid restore leaves no stale `-wal`/`-shm`. Pinned by `restore_rejects_*` and `restore_removes_stale_wal` (Task B4).
4. CLI running while the app restores or deletes (or the reverse): `InUse` error, no corruption. Pinned by `restore_refused_while_another_handle_open` (Task B4).
5. Older app opens a newer database, or a migration fails: recovery screen with data intact, not a blank window. Pinned by `refuses_newer_schema`, `snapshot_precedes_migration`, `failed_migration_keeps_snapshot_and_data` (Task B2), and `shows_recovery_screen` (Task B8).

## Verified Environment Findings (2026-11-01)

| Finding | Consequence |
|---|---|
| Repo has no commits; local branch is `master`, intended default is `main`; remote is empty | Task A1 points HEAD at `main` before the first commit |
| `rustc` 1.86; sqlx 0.9 needs 1.94, tauri 2.12 needs 1.90 | `rust-toolchain.toml` pins 1.99.1; rustup installs it automatically |
| `pnpm` fails: corepack 0.30 cannot verify the current signing key | Task B1 Step 1 updates corepack |
| `codex` shim fails: vendored binary missing. Ollama not running | No impact on 0.1. Fix before 0.2 and 0.4 work |
| `plugin:github` MCP fails with HTTP 400 | Use `git` and `gh` |

## Part A: Architecture Package

Docs are the deliverable, so each task's "test" is a checklist plus the shared Mermaid check:

```bash
npx -y @mermaid-js/mermaid-cli -i <doc>.md -o /tmp/mmd-check.md
```

Expected: exit 0. Branch for all Part A work: `docs/architecture`.

### Task A1: Repository root, PRD, and milestone plan

**Files:**
- Create: `.gitattributes`, `.gitignore`, `LICENSE`, `README.md`, `docs/prd.md`, `docs/milestones.md`

**Interfaces:**
- Produces: requirement IDs `FR-<section>.<n>` used by every later doc.

- [ ] **Step 1: Create the root commit on `main`**

```bash
git symbolic-ref HEAD refs/heads/main
```

Add `.gitattributes` (with `*.sql text eol=lf`), `.gitignore`, the Apache-2.0 `LICENSE`, a short `README.md`, and the spec and this plan. Commit: `chore: initialize repository with spec and plan`. Then `git switch -c docs/architecture`.

- [ ] **Step 2: Write `docs/prd.md`**

Each requirement line uses the format `- **FR-5.1** text`. Must contain: target users; problem; goals and non-goals; one `FR-` entry per spec bullet in sections 1 to 13; offline matrix (feature, works offline yes/no); success metrics; and these fixed product rules, verbatim from the spec: "Never infer work authorization or visa eligibility." and "Never fabricate professional qualifications or employment history."

- [ ] **Step 3: Write `docs/milestones.md`**

One section per release with scope, exit tests, and the spec's five milestone gates (build, tests, migrations, Windows/macOS, docs and risks report). Use this mapping, which resolves what the spec leaves open:

| Release | Scope |
|---|---|
| 0.1 | Shell, core DB, FTS5, settings, documents, backup, export, delete, keychain, CLI skeleton, CI, unsigned installers |
| 0.2 | Profile, field encryption, resume parsing and generation, Ollama, task router, embeddings, xterm.js PTY panel, CLI sidecar in the installer |
| 0.3 | Career memory, application tracking, deterministic workflow runner, execution logs, emergency pause |
| 0.4 | External AI backends (design depends on Decision D1 in Task A6), API providers (hosted rows depend on Decision D4), consent UI |
| 0.5 | Connector SDK, read-only discovery, job intelligence (spec section 6) |
| 0.6 | Playwright runtime, permissions engine, Manual and Copilot modes; Autonomous mode only for connectors passing review |
| 1.0 | Signing and notarization, auto-update, security review, public docs |

- [ ] **Step 4: Verify every spec section maps to a release and a `FR-` ID**

Run: `grep -c '^- \*\*FR-' docs/prd.md`
Expected: at least 90, and each of sections 1 to 13 appears.

- [ ] **Step 5: Commit**

```bash
git add docs/prd.md docs/milestones.md
git commit -m "docs: add PRD and milestone plan"
```

### Task A2: Architecture, repository structure, components, and ER diagram

**Files:**
- Create: `docs/architecture.md`, `docs/database-schema.md`

**Interfaces:**
- Consumes: `FR-` IDs from Task A1.
- Produces: table names and module boundaries that Part B implements exactly.

- [ ] **Step 1: Write `docs/architecture.md`**

Must contain: Mermaid context and container diagrams; a sequence diagram for "GUI and CLI reach the same service"; the repository tree below; a component section per crate and frontend area; an ADR list covering: core-library-plus-adapters, runtime SQLx queries, one data root rather than Tauri path APIs (so the CLI resolves it too), and advisory file lock for multi-process safety.

```text
crates/core/          jobpilot-core: all behaviour, migrations/
crates/cli/           jobpilot binary
apps/desktop/         React app
apps/desktop/src-tauri/  Tauri adapter
docs/                 published documentation
.github/workflows/    ci.yml, release.yml
```

- [ ] **Step 2: Write `docs/database-schema.md`**

Must contain: a Mermaid `erDiagram` of the 0.1 tables (`settings`, `audit_log`, `secret_index`, `documents`, `documents_fts`) and a separately labelled proposed diagram for later entities (candidate, resume, job, application, answer, connector, agent run); migration rules (forward-only, snapshot before migrating, never edit an applied file).

- [ ] **Step 3: Run the Mermaid check on both files, then commit**

```bash
git add docs/architecture.md docs/database-schema.md
git commit -m "docs: add architecture and database schema"
```

### Task A3: Policy register and AI provider specification

**Files:**
- Create: `docs/policy-register.md`, `docs/ai-providers.md`

**Interfaces:**
- Consumes: findings below.
- Produces: Decision D1 and D4 options for Task A6; `Capability` and policy-status vocabulary for releases 0.2 to 0.6.

Findings so far, to be re-read and cited from primary sources:

| Integration | Finding | Source | Status |
|---|---|---|---|
| Claude Code | Consumer-plan OAuth is for ordinary use of native Anthropic apps. Third-party developers may not offer Claude.ai login or "route requests through Free, Pro, Max, or Ultra plan credentials on behalf of their users". Developers should use API keys | code.claude.com/docs/en/legal-and-compliance, authentication section only | Blocks headless routing on consumer plans |
| Gemini CLI | Direct third-party access to its backing services is called a violation that can lead to suspension. Unpaid and Google One users were moved to Antigravity CLI on 2026-06-18. API-key auth is supported | geminicli.com/docs/resources/tos-privacy | Blocks consumer-login routing; confirm paid tiers |
| Antigravity CLI | A forum reply says wrapping `agy` in a third-party app on consumer login is not permitted. The author's official status is unconfirmed | discuss.ai.google.dev thread 190447; must verify against antigravity.google/terms and FAQ | Unverified |
| Codex CLI | ChatGPT sign-in and API-key sign-in both exist. Third-party wrapping not yet confirmed either way | help.openai.com article 11369540, OpenAI Terms of Use not yet read | Unknown |
| Greenhouse | Public job listing endpoints exist. Submit uses an HTTP Basic secret key that the docs say must be proxied by "your own servers" | docs.greenhouse.io/job-board.html | Read-only without employer key |
| Ashby | Public job-posting API exists | developers.ashbyhq.com/docs/public-job-posting-api | Read candidate; submit unknown |

- [ ] **Step 1: Write `docs/policy-register.md`**

One row per integration in spec sections 3B, 3C, and 5 (including Lever, RemoteOK, We Work Remotely, Workday, Indeed, LinkedIn). Columns: integration, operation, primary source URL, date read, quoted clause, verdict (`allowed` / `user-assisted` / `blocked` / `unknown`), consequence. Only primary sources can set `allowed` or `blocked`. `unknown` behaves as blocked in the product. State plainly that this is engineering review, not legal advice.

- [ ] **Step 2: Write `docs/ai-providers.md`**

Must contain: the four backends; `TaskRouter` contract (task type, JSON schema, capability declaration, timeout, retry, fallback order ending in No-AI); output validation; per-provider consent record (provider, data categories, timestamp, revocation); and Decisions D1 and D4 with the options listed in Task A6.

- [ ] **Step 3: Check, then commit**

Run: `grep -c 'unknown\|allowed\|blocked\|user-assisted' docs/policy-register.md`
Expected: at least 15 (one verdict per integration and operation).

```bash
git add docs/policy-register.md docs/ai-providers.md
git commit -m "docs: add policy register and AI provider specification"
```

### Task A4: Threat model and agent workflow specification

**Files:**
- Create: `docs/security/threat-model.md`, `docs/agents.md`

**Interfaces:**
- Consumes: boundaries from Task A2.
- Produces: permission vocabulary for the 0.3 runner and 0.6 permissions engine.

- [ ] **Step 1: Write `docs/security/threat-model.md`**

STRIDE per trust boundary: webview to Rust, Rust to subprocess, app to model, job page to agent, app to filesystem. Must treat job descriptions and web pages as untrusted prompt-injection input. Must record the encryption recommendation for Decision D6: field-level AES-GCM for sensitive columns with the key in the OS keychain (0.2), with the case for and against SQLCipher; state that under field encryption FTS content is limited to non-sensitive text. Must state keychain loss makes encrypted fields unrecoverable and define the recovery-key export.

- [ ] **Step 2: Write `docs/agents.md`**

A table for all nine agents with: inputs, outputs, allowed tools, permission scope, timeout, retry policy, checkpoint, cancellation. A Mermaid state diagram of a run. Mandatory pauses copied from spec section 9: legal declarations, unverified sensitive information, identity verification, CAPTCHA, MFA.

- [ ] **Step 3: Run the Mermaid check on `docs/agents.md`, then commit**

```bash
git add docs/security/threat-model.md docs/agents.md
git commit -m "docs: add threat model and agent workflows"
```

### Task A5: UI wireframes

**Files:**
- Create: `docs/wireframes.md`

- [ ] **Step 1: Write ASCII wireframes**

Cover: app shell and navigation, recovery screen, status, documents, data and privacy, profile, job list, job detail with score explanation, resume compare with evidence, application review with pause state, terminal panel, AI provider consent, local AI setup. Mark which belong to 0.1. Note keyboard navigation and focus order for each.

- [ ] **Step 2: Commit**

```bash
git add docs/wireframes.md
git commit -m "docs: add UI wireframes"
```

### Task A6: Owner approval gate

- [ ] **Step 1: Open a PR from `docs/architecture` to `main`**

```bash
git push -u origin main docs/architecture
gh pr create --base main --head docs/architecture --title "docs: architecture package"
```

- [ ] **Step 2: Stop and get written owner decisions**

Part B must not start before D2, D3, D5, and D6 are answered. D1 and D4 may stay open until the 0.4 plan.

- **D1, subscription AI.** Spec section 3B asks for Claude Code, Gemini CLI, and Codex integrations, and also says to validate terms. Current findings conflict for consumer logins. Recommended: (a) ship CLI adapters in API-key mode only; (b) add a user-launched interactive terminal session for the owner's own CLI, with no background routing, subject to primary-source review; (c) enable any consumer-login path only after a primary source or written vendor answer allows it, recorded in the policy register. Alternatives: API providers and Ollama only, or seek written vendor permission first. Option (a) cannot ship today unless D4 selects standard BYOK or a vendor answers in writing, because all three adapters are `unknown`. FR-3.10 cannot be met as a subscription integration. Sign in with ChatGPT is the only vendor-documented automated hosted path, and it is a preview.
- **D2, autonomous submission and Copilot.** Applicant-side keys are not available from Greenhouse, Lever, or Ashby, and no site has an `allowed` Copilot row. Recommended: Autonomous stays default-off and can have zero eligible connectors at 1.0; Manual is the shipping path; Copilot runs on real sites only after a site row opens. **Question for the owner:** is "silent site = unknown = no Copilot fill" the standard you want, since it removes FR-9.3 from every real site? Or may Copilot run where no clause forbids it, recorded as "no clause found"? The first keeps the register as written and waits on vendors. The second opens Copilot on the Greenhouse, Lever, and Ashby hosted forms, puts the risk of a later objection on the user, and needs the register's Copilot rule changed; Indeed stays `blocked`, and Workday and LinkedIn stay shut because a clause may reach them or was not read.
- **D3, unsigned 0.x builds.** Recommended: 0.x installers are ad-hoc signed on macOS and unsigned on Windows, so users see OS warnings. Paid signing arrives at 1.0. Added cost: ad-hoc signing gives each build a different code identity, so every 0.x upgrade and the CLI can trigger keychain prompts or denials for the 0.2 data key.
- **D4, user-held API keys.** Spec section 3C offers optional API providers, but the register's rule leaves OpenAI, Anthropic, and Google `unknown`, OpenRouter with zero eligible models, and only loopback endpoints running. That rule came from a project ruling, not from the owner. Options: **strict** (current): a key opens only where a primary clause covers an individual key owner using third-party client software. **Standard BYOK**: a user-held key in a local client is the user's own use, and JobPilot shows the vendor's terms on the consent screen. Consequence of strict: the product today is Ollama, loopback endpoints, and No-AI, with three read connectors (Lever, RemoteOK, We Work Remotely), Manual apply only, and Sign in with ChatGPT if D1(c) is adopted. FR-3.15 to FR-3.17 wait on vendor answers that may never come. Consequence of standard BYOK: hosted providers can ship in 0.4 once the register's rule and rows are re-decided, but "not for consumer use" stays unresolved, so a vendor could act against the user's own key or account. Free Gemini keys stay `blocked`, and non-loopback endpoints stay `unknown`, either way. Recommended: standard BYOK for direct API calls, with the written questions still sent. Reasoning and evidence are in `docs/ai-providers.md`.
- **D5, recovery-key file.** The recovery file in threat model section 9 places the data key in a file, which conflicts with the constraint "Secret values never ... reach ... exports" and with a plain reading of FR-12.2. Options: (a) accept it as the one exception: a separate file, made only by explicit action, outside the data root, never part of a data export; (b) refuse it: no exception, and keychain loss makes encrypted fields unrecoverable with no way back; (c) passphrase-wrap it: a stolen file is protected, but the user gains a passphrase to lose, and it must be typed somewhere other than the webview. Recommended: (a). Every exception note in the docs is conditional on this answer.
- **D6, encryption approach.** Threat model section 8 compares two designs. Against SQLCipher: a whole-file key in the keychain makes keychain loss strand the whole database rather than some columns; the CLI process also needs the key; the `bundled-sqlcipher` build is unverified on both OSes. For SQLCipher: it would also encrypt plain personal text and the FTS index, which field-level encryption leaves readable in a copied database. Recommended: field-level encryption as now.
- **Owner item, schema rule 7.** Accept or strike rule 7 in `docs/database-schema.md` ("A migration that creates a table holding personal data must say how backup, export, and delete-all handle it."). The rule came from review, not from the spec.

- [ ] **Step 3: Merge the PR after approval and record decisions in `docs/milestones.md`**

## Part B: Release 0.1 Desktop Foundation

Branch for all Part B work: `feat/v0.1-foundation`, from `main` after Task A6.

### Task B1: Workspace and desktop shell

**Files:**
- Create: `rust-toolchain.toml`, `Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`, `apps/desktop/**`
- Test: `apps/desktop/src/App.test.tsx`

**Interfaces:**
- Produces: crates `jobpilot-core` and `jobpilot-cli` (binary `jobpilot`); root scripts `pnpm test`, `pnpm typecheck`, `pnpm tauri`.

- [ ] **Step 1: Repair pnpm**

```bash
npm install -g corepack@latest && corepack enable && pnpm --version
```

Expected: prints a version, no `keyid` error. Note: this changes a global tool on the owner's machine.

- [ ] **Step 2: Scaffold the app**

```bash
mkdir apps && cd apps
pnpm dlx create-tauri-app desktop -m pnpm -t react-ts --identifier io.github.gvsrusa.jobpilot --tauri-version 2 -y
```

Add `apps/desktop/src-tauri`, `crates/core`, and `crates/cli` as Cargo workspace members. Set `productName` to `JobPilot AI` and the Tauri Cargo package name to `jobpilot-desktop`. Install Tailwind through `@tailwindcss/vite`, then:

```bash
pnpm dlx shadcn@latest init --cwd apps/desktop --base radix --no-monorepo --yes
```

Add Zustand, TanStack Query, Vitest, and Testing Library. If `typescript@7` breaks a tool, pin the newest 6.x and record it in the commit.

- [ ] **Step 3: Lock down the webview**

In `tauri.conf.json`, set a CSP of `default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src ipc: http://ipc.localhost`. Keep `capabilities/default.json` to `core:default` and remove the template's `opener` plugin.

- [ ] **Step 4: Write smoke tests**

`App.test.tsx` test `renders_app_title` asserts the text `JobPilot AI`.

- [ ] **Step 5: Verify the shell builds and opens**

```bash
pnpm install && pnpm typecheck && pnpm test && cargo test --workspace && pnpm tauri build --no-bundle
```

Expected: all pass. Then run `pnpm tauri dev` and confirm a window titled JobPilot AI opens.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: scaffold Tauri workspace with core and CLI crates"
```

### Task B2: Core paths, errors, and database opening

**Files:**
- Create: `crates/core/src/error.rs`, `crates/core/src/paths.rs`, `crates/core/src/db.rs`, `crates/core/migrations/0001_foundation.sql`, `crates/core/tests/db.rs`

**Interfaces:**
- Consumes: workspace from Task B1.
- Produces:

```rust
pub enum Error {
    Io(std::io::Error), Db(sqlx::Error), SchemaTooNew { db: i64, app: i64 },
    MigrationFailed(String), InvalidBackup(String), InUse, Unavailable, Secret(String), NotFound,
    ConfirmationRequired,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error { pub fn code(&self) -> &'static str } // e.g. "in_use"
// Serialize as {"code": ..., "message": ...}

pub struct AppPaths { /* root: PathBuf */ }
impl AppPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self;
    pub fn from_env() -> Result<Self>; // JOBPILOT_HOME, else dirs::data_dir()/io.github.gvsrusa.jobpilot
    pub fn root(&self) -> &Path;
    pub fn db(&self) -> PathBuf;        // root/jobpilot.db
    pub fn documents(&self) -> PathBuf; // root/documents
    pub fn backups(&self) -> PathBuf;   // root/backups
    pub fn exports(&self) -> PathBuf;   // root/exports
    pub fn lock(&self) -> PathBuf;      // root/jobpilot.lock
}

pub struct Db { pub pool: SqlitePool, /* lock: File, paths: AppPaths */ }
impl Db {
    pub async fn open(paths: &AppPaths) -> Result<Db>; // open_with(paths, &MIGRATOR)
    #[doc(hidden)] pub async fn open_with(paths: &AppPaths, migrator: &Migrator) -> Result<Db>;
    pub async fn close(self); // closes the pool and releases the file lock
    pub async fn schema_version(&self) -> Result<i64>;
    pub fn paths(&self) -> &AppPaths;
}
pub static MIGRATOR: Migrator; // sqlx::migrate!()
pub fn app_schema_version() -> i64;
pub(crate) fn lock_exclusive(paths: &AppPaths) -> Result<std::fs::File>;
pub(crate) async fn snapshot(paths: &AppPaths, pool: &SqlitePool, label: &str) -> Result<PathBuf>;
```

- [ ] **Step 1: Write failing tests in `crates/core/tests/db.rs`**

Each test uses a fresh temp root (`tempfile` dev-dependency).

```rust
#[tokio::test] async fn opens_and_migrates_fresh_root() {
    // creates all dirs; schema_version() == app_schema_version()
}
#[tokio::test] async fn fts5_is_available() {
    // CREATE VIRTUAL TABLE t USING fts5(x) succeeds
}
#[tokio::test] async fn pragmas_are_set() {
    // journal_mode = wal, foreign_keys = 1
}
#[tokio::test] async fn opens_under_unicode_path_with_spaces() {
    // root = tmp/"José Ñ 日本語"; Db::open succeeds
}
#[tokio::test] async fn refuses_newer_schema() {
    // insert _sqlx_migrations row version 9999; reopen;
    // matches!(err, Error::SchemaTooNew { db: 9999, .. }); row data untouched
}
#[tokio::test] async fn snapshot_precedes_migration() {
    // open; add a row; open_with a Migrator of MIGRATOR plus a valid version 9000;
    // a backups/pre-migrate-* folder exists and its database holds the row
}
#[tokio::test] async fn failed_migration_keeps_snapshot_and_data() {
    // same, but version 9000 is invalid SQL: Error::MigrationFailed, snapshot exists,
    // and the row is still readable through a raw SqlitePool on the live file
}
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test -p jobpilot-core --test db`
Expected: compile errors for missing `Db` and `AppPaths`.

- [ ] **Step 3: Implement the modules and migration**

Dependencies: `sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "sqlite", "sqlite-bundled", "macros", "migrate"] }`, `tokio`, `thiserror`, `serde`, `serde_json`, `dirs`.

`0001_foundation.sql` creates:

```sql
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE audit_log (id INTEGER PRIMARY KEY, at TEXT NOT NULL, actor TEXT NOT NULL,
  action TEXT NOT NULL, target TEXT, detail TEXT NOT NULL DEFAULT '{}');
CREATE TABLE secret_index (name TEXT PRIMARY KEY, created_at TEXT NOT NULL);
CREATE TABLE documents (id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
```

Plus `documents_fts` as an external-content FTS5 table over `title, body` with `tokenize='unicode61 remove_diacritics 2'` and insert, update, delete triggers. Add a `ponytail:` comment: unicode61 does not segment CJK; move to trigram or a segmenter when those users appear.

`Db::open` order: create dirs; try a shared lock on `lock()` using `File::try_lock_shared` (`WouldBlock` maps to `InUse`); connect with `create_if_missing(true)`, WAL, `busy_timeout(5s)`, foreign keys on; read the max applied version and return `SchemaTooNew` if above `app_schema_version()`; if migrations are pending, swap to the exclusive lock, call `snapshot(paths, pool, "pre-migrate-<from>")` when `from > 0`, run `sqlx::migrate!()`, then return to the shared lock.

`snapshot` makes `backups/<label>-<UTC stamp>/` holding `VACUUM INTO 'jobpilot.db'`, a copy of `documents/`, and `manifest.json` with `{format: 1, schema_version, created_at}`. The stamp comes from SQLite `strftime`. Append `-1`, `-2` if `create_dir` finds the folder exists. Build the VACUUM path with a bound parameter. Nothing deletes snapshots automatically: `pre-migrate`, `pre-restore`, and from 0.2 `pre-discard` folders stay until the user removes them or delete-all runs with `include_backups`.

- [ ] **Step 4: Run tests and confirm they pass**

Run: `cargo test -p jobpilot-core --test db`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): open migrated SQLite database under one data root"
```

### Task B3: Settings, audit log, and searchable documents

**Files:**
- Create: `crates/core/src/settings.rs`, `crates/core/src/audit.rs`, `crates/core/src/documents.rs`, `crates/core/tests/documents.rs`

**Interfaces:**
- Consumes: `Db`, `Result` from Task B2.
- Produces:

```rust
pub async fn settings::get(db: &Db, key: &str) -> Result<Option<serde_json::Value>>;
pub async fn settings::set(db: &Db, key: &str, value: &serde_json::Value) -> Result<()>;

pub async fn audit::record(db: &Db, actor: &str, action: &str, target: Option<&str>,
    detail: serde_json::Value) -> Result<()>;
pub async fn audit::recent(db: &Db, limit: i64) -> Result<Vec<AuditEntry>>;

pub struct Document { pub id: String, pub title: String, pub body: String,
    pub created_at: String, pub updated_at: String } // Serialize, Deserialize
pub struct SearchHit { pub id: String, pub title: String, pub snippet: String } // Serialize
pub async fn documents::create(db: &Db, title: &str, body: &str) -> Result<Document>;
pub async fn documents::get(db: &Db, id: &str) -> Result<Document>; // NotFound
pub async fn documents::list(db: &Db) -> Result<Vec<Document>>;     // newest first
pub async fn documents::update(db: &Db, id: &str, title: &str, body: &str) -> Result<Document>;
pub async fn documents::delete(db: &Db, id: &str) -> Result<()>;
pub async fn documents::search(db: &Db, query: &str) -> Result<Vec<SearchHit>>;
pub fn documents::fts_query(input: &str) -> Option<String>;
```

Actor values: `"gui"`, `"cli"`, `"system"`.

- [ ] **Step 1: Write failing tests in `crates/core/tests/documents.rs`**

```rust
#[tokio::test] async fn crud_round_trip() { /* create, get, update, list, delete; get -> NotFound */ }
#[tokio::test] async fn search_finds_title_and_body_with_diacritics() {
    // create("Résumé notes", "Rust engineer"); search("resume") and search("rust") each hit
}
#[tokio::test] async fn search_tracks_update_and_delete() {
    // after update, old word misses and new word hits; after delete, no hit
}
#[tokio::test] async fn search_accepts_any_user_text() {
    for q in ["c++ \"dev", "AND", "*", "-", "\"", "NEAR(", "title:x", "", "   "] {
        assert!(documents::search(&db, q).await.is_ok(), "{q}");
    }
}
#[test] fn fts_query_quotes_terms() {
    assert_eq!(fts_query("c++ \"dev"), Some("\"c++\" \"\"\"dev\"".into()));
    assert_eq!(fts_query("  "), None);
}
#[tokio::test] async fn settings_round_trip() { /* set, get, overwrite; missing -> None */ }
#[tokio::test] async fn audit_records_in_reverse_order() { /* two records; recent(1) is the latest */ }
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test -p jobpilot-core --test documents`
Expected: compile errors for missing modules.

- [ ] **Step 3: Implement the three modules**

Generate ids with `lower(hex(randomblob(16)))` and timestamps with `strftime('%Y-%m-%dT%H:%M:%fZ','now')` in SQL, so no `uuid` or `chrono` dependency is needed. `fts_query` splits on whitespace, doubles `"`, wraps each term in `"`, joins with spaces, and returns `None` when no terms remain; `search` then returns an empty list. Search returns at most 50 hits ordered by `bm25` with `snippet()` for excerpts. Export the modules from `lib.rs`.

- [ ] **Step 4: Run tests and confirm they pass**

Run: `cargo test -p jobpilot-core --test documents`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): add settings, audit log, and full-text documents"
```

### Task B4: Backup and recovery

**Files:**
- Create: `crates/core/src/backup.rs`, `crates/core/tests/backup.rs`

**Interfaces:**
- Consumes: `Db`, `snapshot`, `lock_exclusive`, `audit::record`.
- Produces:

```rust
pub struct BackupInfo { pub path: PathBuf, pub created_at: String, pub schema_version: Option<i64> } // Serialize
pub async fn backup::create(db: &Db, actor: &str) -> Result<BackupInfo>;
pub async fn backup::list(paths: &AppPaths) -> Result<Vec<BackupInfo>>; // newest first, skips invalid
pub async fn backup::restore(db: Db, source: &Path, actor: &str) -> Result<Db>;
// For the recovery screen, where no Db could open:
pub async fn backup::restore_offline(paths: &AppPaths, source: &Path, actor: &str) -> Result<Db>;
```

`restore` takes `Db` by value because live handles must close first. On any error the handle is closed and live data is unchanged, so callers run `Db::open` again.

- [ ] **Step 1: Write failing tests in `crates/core/tests/backup.rs`**

```rust
#[tokio::test] async fn backup_then_restore_round_trip() {
    // doc A; backup; doc B and a file in documents/; restore;
    // only A exists, the extra file is gone, FTS finds A
}
#[tokio::test] async fn restore_keeps_safety_snapshot() {
    // after restore, a backups/pre-restore-* folder holds doc B
}
#[tokio::test] async fn restore_rejects_truncated_database() {
    // truncate backup db to half; InvalidBackup; doc B still present
}
#[tokio::test] async fn restore_rejects_missing_manifest() { /* InvalidBackup; live data intact */ }
#[tokio::test] async fn restore_rejects_newer_schema() {
    // manifest.schema_version = 9999; SchemaTooNew; live data intact
}
#[tokio::test] async fn restore_removes_stale_wal() {
    // plant jobpilot.db-wal with junk after close; restore; opens clean
}
#[tokio::test] async fn restore_refused_while_another_handle_open() {
    // second Db::open on same root; restore -> InUse; second handle still reads
}
#[tokio::test] async fn list_skips_invalid_folders() { /* junk folder ignored, newest first */ }
#[tokio::test] async fn restore_offline_recovers_when_db_cannot_open() {
    // plant _sqlx_migrations version 9999 so Db::open fails; restore_offline succeeds;
    // the unopenable files are kept in a backups/pre-restore-* folder, and list returns it
}
#[tokio::test] async fn restore_keeps_secret_index_of_live_secrets() {
    // raw secret_index row A; backup; raw row B; restore; SELECT from secret_index has A and B
}
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test -p jobpilot-core --test backup`
Expected: compile errors for missing `backup` module.

- [ ] **Step 3: Implement `backup.rs`**

`restore` order is fixed:

1. Validate before touching live data: read the manifest, compare `schema_version`, open the source read-only, and run `PRAGMA integrity_check`.
2. Write `snapshot(.., "pre-restore")` while the pool is still open, and read the live `secret_index` rows into memory.
3. `db.close()`, then `lock_exclusive`. `InUse` here means another process is attached.
4. Copy the source database, any `-wal` and `-shm` beside it, and documents to temp names inside `root`.
5. Delete `jobpilot.db-wal` and `jobpilot.db-shm`, then rename temp files over the live paths.
6. Release the lock and `Db::open`, which migrates older backups forward.
7. Insert the saved `secret_index` rows with `INSERT OR IGNORE`, because secret values are never in a backup and their keychain entries are untouched, so `delete_all` must still find them. Then write the audit row in the newly opened database.

`restore_offline` follows the same order with no pool, because the database cannot open. Step 2 copies `jobpilot.db`, `jobpilot.db-wal`, and `jobpilot.db-shm` when present, plus `documents/`, into `pre-restore`, and writes `manifest.json` whose `schema_version` is read through a raw read-only connection, falling back to `null` if unreadable. The same connection reads the live `secret_index` rows for step 7; if they cannot be read, restore continues without them, and those keychain entries are no longer indexed. A snapshot with `schema_version: null` is listed, but restoring it is refused with `InvalidBackup` unless it opens: step 1 then reads the version from the opened source. A failure in steps 1 to 3 leaves live data unchanged. Take the temp copy before any rename so disk-full fails safely.

- [ ] **Step 4: Run tests and confirm they pass**

Run: `cargo test -p jobpilot-core --test backup`
Expected: 10 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): add validated backup and restore"
```

### Task B5: Secrets, export, and deletion

**Files:**
- Create: `crates/core/src/secrets.rs`, `crates/core/src/transfer.rs`, `crates/core/tests/transfer.rs`

**Interfaces:**
- Consumes: `Db`, `audit::record`, `lock_exclusive`.
- Produces:

```rust
pub async fn secrets::set(db: &Db, name: &str, value: &str, actor: &str) -> Result<()>;
pub fn secrets::get(name: &str) -> Result<Option<String>>; // Rust callers only; no IPC or CLI path
pub async fn secrets::delete(db: &Db, name: &str, actor: &str) -> Result<()>;
pub async fn secrets::names(db: &Db) -> Result<Vec<String>>;

pub async fn transfer::export_all(db: &Db, actor: &str) -> Result<PathBuf>;
pub async fn transfer::delete_all(db: Db, confirm: &str, include_backups: bool) -> Result<()>;
```

Keychain service: `io.github.gvsrusa.jobpilot`. Test override: `JOBPILOT_SECRET_SERVICE` env var selects another service name.

- [ ] **Step 1: Write failing tests in `crates/core/tests/transfer.rs`**

```rust
#[tokio::test] async fn export_covers_every_user_table() {
    // parse data.json; tables == all sqlite_master tables except _sqlx_* and documents_fts*
}
#[tokio::test] async fn export_has_documents_and_secret_names() {
    // doc present; a raw secret_index row appears by name
}
#[tokio::test] async fn delete_requires_exact_confirmation() {
    // "delete" -> ConfirmationRequired; data intact
}
#[tokio::test] async fn delete_all_removes_data_and_can_keep_backups() {
    // include_backups=false: db, documents, and exports gone, backups kept; true: root empty but for lock
}
#[tokio::test] async fn delete_refused_while_another_handle_open() { /* InUse; data intact */ }
#[tokio::test] #[ignore = "touches the OS keychain"] async fn keychain_round_trip() {
    // set, get, names; export text lacks the value; delete; then get -> None
}
#[tokio::test] #[ignore = "touches the OS keychain"] async fn delete_all_clears_keychain() {}
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test -p jobpilot-core --test transfer`
Expected: compile errors for missing modules.

- [ ] **Step 3: Implement both modules**

Use `keyring = "4"` default `v1` API (`Entry::new`, `set_password`, `get_password`, `delete_credential`). A missing entry maps to `Ok(None)`. Map keyring errors to `Error::Secret` with no value in the message. Run keyring calls in `spawn_blocking`.

`export_all` writes `exports/jobpilot-export-<stamp>/` atomically (temp folder, then rename) with `data.json` as `{format: 1, schema_version, exported_at, tables: {name: [rows]}}` and a copy of `documents/`. Discover tables from `sqlite_master` and build `json_object` from `pragma_table_info`, so future tables cannot be missed.

`delete_all` consumes `Db`; on any error the handle is closed and callers reopen. It checks `confirm == "DELETE"` first, then reads secret names, closes the pool, takes `lock_exclusive`, deletes keychain entries, then removes files. A keychain failure returns the error before files are removed, so the call can be retried. Delete-all always removes `exports/`; `include_backups` controls only `backups/`.

- [ ] **Step 4: Run tests, including real keychain tests on macOS**

```bash
cargo test -p jobpilot-core --test transfer
JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test -p jobpilot-core --test transfer -- --ignored
```

Expected: 5 passed, then 2 passed. Macs may show a keychain prompt on the first run.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): add keychain secrets, data export, and full deletion"
```

### Task B6: JobPilot CLI

**Files:**
- Modify: `crates/cli/src/main.rs`
- Test: `crates/cli/tests/cli.rs`

**Interfaces:**
- Consumes: all `jobpilot-core` services. Actor is `"cli"`.
- Produces: this command surface, each accepting `--json`:

```text
jobpilot status
jobpilot doc list | add <title> [--body <text>] | search <query>
jobpilot backup create | list | restore <path>
jobpilot data export | delete --confirm DELETE [--include-backups]
```

Exit codes: 0 success, 1 error. Errors go to stderr as `error[<code>]: <message>`.

- [ ] **Step 1: Write failing tests in `crates/cli/tests/cli.rs`**

Run the binary through `env!("CARGO_BIN_EXE_jobpilot")` with `JOBPILOT_HOME` set to a temp dir, using only `std::process::Command`.

```rust
#[test] fn status_json_reports_schema() { /* schema_version equals app_schema_version() */ }
#[test] fn doc_added_by_cli_is_found_by_library() {
    // proves GUI and CLI share services: CLI add, then core documents::search hits
}
#[test] fn backup_restore_round_trip() {}
#[test] fn delete_without_confirm_fails_and_keeps_data() {}
#[test] fn errors_exit_nonzero_with_code() {
    // restore a missing path: exit 1, stderr has "error["
}
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test -p jobpilot-cli`
Expected: failures for unknown subcommands.

- [ ] **Step 3: Implement with `clap` derive and `#[tokio::main]`**

Keep `main.rs` dispatch-only: parse, `AppPaths::from_env`, `Db::open`, call core, print. No business logic. No secret subcommand in 0.1.

- [ ] **Step 4: Run tests and confirm they pass**

Run: `cargo test -p jobpilot-cli`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/cli
git commit -m "feat(cli): add status, document, backup, and data commands"
```

### Task B7: Tauri command adapter

**Files:**
- Create: `apps/desktop/src-tauri/src/state.rs`, `apps/desktop/src-tauri/src/commands.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/capabilities/default.json`
- Test: `apps/desktop/src-tauri/src/commands.rs` (unit module)

**Interfaces:**
- Consumes: all `jobpilot-core` services. Actor is `"gui"`.
- Produces: these commands, all returning `Result<T, jobpilot_core::Error>`:

```rust
pub struct AppState { pub paths: AppPaths, pub db: tokio::sync::RwLock<Option<Db>>,
    pub startup_error: std::sync::Mutex<Option<StartupError>> }
pub struct StartupError { pub code: String, pub message: String } // Serialize, Clone
pub struct Status { pub state: String, pub schema_version: Option<i64>, pub app_version: String,
    pub data_root: String, pub error: Option<StartupError> } // state: "ready" | "recovery"

status() -> Status                    // never fails, works with no Db
retry_open() -> Status
documents_list() -> Vec<Document>
documents_create(title: String, body: String) -> Document
documents_update(id: String, title: String, body: String) -> Document
documents_delete(id: String) -> ()
documents_search(query: String) -> Vec<SearchHit>
settings_get(key: String) -> Option<Value>
settings_set(key: String, value: Value) -> ()
backup_create() -> BackupInfo
backup_list() -> Vec<BackupInfo>
backup_restore(source: String) -> Status
data_export() -> String
data_delete(confirm: String, include_backups: bool) -> Status
secret_set(name: String, value: String) -> ()
secret_delete(name: String) -> ()
secret_names() -> Vec<String>
audit_recent(limit: i64) -> Vec<AuditEntry>
```

There is deliberately no `secret_get` command.

- [ ] **Step 1: Write failing tests**

Enable tauri's `test` feature as a dev dependency. Keep helpers thin and test the command layer through `tauri::test::mock_builder` and `get_ipc_response`.

```rust
#[test] fn status_is_recovery_when_db_failed_to_open() {
    // state with db=None and startup_error=schema_too_new -> state == "recovery"
}
#[test] fn data_commands_return_unavailable_in_recovery() {}
#[test] fn no_secret_read_command_is_registered() {
    // invoking "secret_get" returns an error
}
#[test] fn restore_swaps_the_live_handle() {
    // document made after backup is gone after backup_restore; Db slot is Some
}
#[test] fn restore_works_from_recovery_state() {
    // db=None and a valid backup: backup_restore returns state "ready"
}
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test -p jobpilot-desktop`
Expected: compile errors for missing commands.

- [ ] **Step 3: Implement state, commands, and startup**

In `setup`, resolve `AppPaths::from_env`, attempt `Db::open`, and on failure store a `StartupError` instead of panicking. A missing `Db` returns `Error::Unavailable`. `backup_restore` and `data_delete` take the write lock, `.take()` the `Db`, run core, and always reopen before returning. With no `Db` in the slot, `backup_restore` calls `restore_offline`, which is what makes the recovery screen's Restore work. Add `tauri-plugin-single-instance` so a second launch focuses the window. Add `tauri-plugin-dialog` with only `dialog:allow-open` in capabilities, for picking a backup folder.

- [ ] **Step 4: Run tests and confirm they pass**

Run: `cargo test -p jobpilot-desktop`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri crates/core
git commit -m "feat(desktop): expose core services through Tauri commands"
```

### Task B8: React interface

**Files:**
- Create: `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/lib/store.ts`, `apps/desktop/src/views/StatusView.tsx`, `apps/desktop/src/views/DocumentsView.tsx`, `apps/desktop/src/views/DataView.tsx`, `apps/desktop/src/views/RecoveryView.tsx`
- Modify: `apps/desktop/src/App.tsx`, `apps/desktop/src/main.tsx`
- Test: `apps/desktop/src/App.test.tsx`, `apps/desktop/src/views/views.test.tsx`

**Interfaces:**
- Consumes: Task B7 commands, wireframes from Task A5.
- Produces:

```ts
export type AppError = { code: string; message: string };
export const ipc: { /* one typed function per Task B7 command */ };
export const useUiStore: UseBoundStore<StoreApi<{ view: 'status' | 'documents' | 'data';
  setView(v: 'status' | 'documents' | 'data'): void }>>;
```

Hand-written types in `ipc.ts`, where `BackupInfo.schema_version` is `number | null`. `ponytail: hand-kept in sync with commands.rs; add tauri-specta when the command count makes drift a real problem.`

- [ ] **Step 1: Write failing tests**

Use `mockIPC` from `@tauri-apps/api/mocks`.

```ts
test('shows_recovery_screen when status is recovery', ...)
// status -> {state:'recovery', error:{code:'schema_too_new',...}};
// recovery heading and data root path shown; nav hidden

test('documents_create_then_list', ...)
test('search_shows_snippets_and_empty_state', ...)
test('delete_needs_typed_DELETE', ...)
// button disabled until input === 'DELETE'; ipc receives confirm:'DELETE'

test('restore_warns_before_replacing_data', ...)
test('shows_error_message_from_ipc', ...)
// documents_list rejects {code:'in_use',message:'...'}; alert role shows message

test('secret_ui_never_displays_values', ...)
// after secret_set, the entered value is not in the document text
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `pnpm --filter desktop test`
Expected: failures for missing views.

- [ ] **Step 3: Implement the views**

TanStack Query owns all server state. Mutations invalidate affected queries; restore and delete invalidate everything. Zustand holds only `view`. Use shadcn `Button`, `Input`, `Textarea`, `Dialog`, `Alert`, `Table`. Debounce search by 200 ms. Each control has a label and works by keyboard. No remote fonts or assets.

- [ ] **Step 4: Run tests and confirm they pass**

```bash
pnpm typecheck && pnpm --filter desktop test
```

Expected: all pass.

- [ ] **Step 5: Exercise the real app offline**

Disable networking, then run `pnpm tauri dev`. Create a document, search it, back up, restore, export, delete all. Check the webview console for CSP violations and tighten or fix them. Confirm `jobpilot status` works beside the open app and `jobpilot backup restore` returns `in_use`. Capture one screenshot per view.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop
git commit -m "feat(desktop): add status, documents, data, and recovery views"
```

### Task B9: CI, installers, documentation, and release report

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `docs/development.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `docs/releases/0.1.0.md`, `docs/connector-sdk.md`
- Modify: `README.md`, `apps/desktop/src-tauri/tauri.conf.json`

**Interfaces:**
- Consumes: root scripts from Task B1.

- [ ] **Step 1: Write `ci.yml`**

Triggers: PRs and pushes to `main`. Matrix: `macos-latest`, `windows-latest`. Steps: checkout, `dtolnay/rust-toolchain@1.99.1` with `rustfmt, clippy`, `swatinem/rust-cache@v2`, pnpm and Node 24 setup, `pnpm install --frozen-lockfile`, `pnpm typecheck`, `pnpm test`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm tauri build --no-bundle`.

Add the ignored keychain tests. On `windows-latest` run them directly. On macOS create and unlock a temporary keychain first:

```bash
security create-keychain -p ci ci.keychain
security default-keychain -s ci.keychain
security unlock-keychain -p ci ci.keychain
security set-keychain-settings ci.keychain
```

- [ ] **Step 2: Write `release.yml`**

Trigger: tags `v*`. Use the tauri-action v1 matrix: `macos-latest` with `--target aarch64-apple-darwin`, `macos-latest` with `--target x86_64-apple-darwin`, `windows-latest`. Keep `releaseDraft: true`. Set bundle targets `dmg`, `nsis`, `msi`, and `macOS.signingIdentity: "-"`. Set the version to `0.1.0` in `tauri.conf.json`, both crates, and `package.json`.

- [ ] **Step 3: Write the contributor docs**

`docs/development.md`: prerequisites, setup, test commands, how to add a migration. `CONTRIBUTING.md`: branch and PR rules, the `FR-` traceability rule. `docs/connector-sdk.md`: a stub stating the SDK arrives in 0.5, with the declaration fields from spec section 5. `README.md`: what works today, and the unsigned-install warning from Decision D3.

- [ ] **Step 4: Push and prove both OSes**

```bash
git push -u origin feat/v0.1-foundation
gh pr create --base main --title "feat: release 0.1 desktop foundation"
gh pr checks --watch
```

Expected: both matrix jobs pass. A Windows failure gets its own fix commit; do not skip tests to reach green.

- [ ] **Step 5: Validate migrations and a clean install**

Download the draft installers from a `v0.1.0-rc.1` tag. On each OS install, launch, create a document, relaunch, confirm it persists, and uninstall. Windows needs a real or virtual machine; if one is unavailable, say so in the report and leave 0.1 as release candidate.

- [ ] **Step 6: Write `docs/releases/0.1.0.md` and `CHANGELOG.md`**

Report, per the spec's milestone gate: what was built, test counts per OS with run links, migration results, manual checks performed, what was not tested, and outstanding risks (unsigned installers, CJK search, secrets recovery, D1).

- [ ] **Step 7: Commit, then request owner approval to tag `v0.1.0`**

```bash
git add -A
git commit -m "docs: add CI, release workflow, and 0.1 release report"
git push
```

## Deferred From Release 0.1

| Item | Target | Reason |
|---|---|---|
| Field encryption | 0.2 | No sensitive field exists until the profile does |
| xterm.js PTY panel, CLI in the installer | 0.2 | CLI needs real commands first |
| Emergency pause | 0.3 | Nothing runs unattended until the workflow runner exists |
| Auto-update, code signing | 1.0 | Requires signing keys and paid certificates |
| Linux | Not planned | Outside the spec |
