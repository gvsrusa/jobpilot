# JobPilot AI: Milestone Plan

Status: draft for owner approval. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) and [PRD](prd.md). Requirement IDs are `FR-<section>.<n>` from the PRD. Releases 0.1 to 0.4 are built; 0.5 onward are plans.

## Process

Work ships in independently testable releases. A feature is not called implemented until its test has passed. CI on macOS and Windows is the evidence for both platforms.

### Pre-code deliverables

The spec requires these before implementation starts: complete PRD, architecture diagrams, database ER diagram, repository structure, component architecture, AI provider integration specification, security threat model, agent workflow specification, UI wireframes, and the milestone plan (this document). They belong to the `docs/architecture` work and need owner approval.

### Milestone gates

Every release must clear all five gates before it is declared done:

1. Build: the application builds on macOS and Windows.
2. Tests: the full test suite passes on both operating systems.
3. Migrations: migrations apply cleanly to a fresh database and upgrade an existing one.
4. Windows and macOS: compatibility is tested on both.
5. Docs and risks report: documentation is updated, and completed work and outstanding risks are reported.

## Spec section coverage

| Spec section | Topic | Releases |
|---|---|---|
| 1 | Desktop application | 0.1 (signed in 1.0) |
| 2 | Local-first infrastructure | 0.1, 0.2, 0.3 |
| 3 | AI architecture | 0.1, 0.2, 0.4, 0.5, 0.6 |
| 4 | International candidate profile | 0.2 |
| 5 | Job discovery | 0.5 |
| 6 | Job intelligence | 0.5 |
| 7 | Resume and cover letter engine | 0.2 |
| 8 | Career memory | 0.3 |
| 9 | Application automation | 0.6 |
| 10 | Terminal and command interface | 0.1, 0.2, 0.3, 0.5 |
| 11 | Agent orchestration | 0.3, 0.5, 0.6 |
| 12 | Security | 0.1, 0.2, 0.3, 0.6 (reviewed in 1.0) |
| 13 | Open-source engineering | 0.1, 0.5, 1.0 |

## Release 0.1: Desktop foundation

Scope: Tauri shell, core database, FTS5, settings, documents, backup, export, delete, OS keychain, CLI skeleton, CI, unsigned installers.

Requirements: FR-1.1 to FR-1.8, FR-2.1 to FR-2.3, FR-2.5 to FR-2.9, FR-2.11, FR-3.21, FR-3.23, FR-10.13, FR-12.2, FR-12.7, FR-12.9, FR-12.10, FR-13.1 to FR-13.4, FR-13.6 to FR-13.9, FR-13.12, FR-13.13.

Exit tests:
- App launches on macOS (arm64, x86_64) and Windows x64 from the unsigned installers.
- Migrations apply to a fresh database; the database is created at the expected path.
- FTS5 search returns results over stored documents.
- Settings and documents persist across restarts.
- Backup then restore reproduces the data; export writes all user data; delete-all removes it.
- Secrets round-trip through the OS keychain and appear in no log, export, or audit row.
- Backup, restore, export, delete, and secret changes write audit rows, except delete-all, which removes the log.
- The CLI skeleton runs and calls the same services as the GUI.
- Every 0.1 feature works with the network disabled, and 0.1 code makes no network call.
- CI is green on macOS and Windows.

Gates: all five apply. Build means the unsigned installers on macOS and Windows; migrations means the initial core schema on a fresh database; Windows and macOS means CI green on both.
## Release 0.2: Candidate and local AI

Scope: profile, field encryption, resume parsing and generation, Ollama, task router, embeddings, xterm.js PTY panel, CLI sidecar bundled in the installer, Claude Code terminal launch and headless adapter (moved from 0.4 by ruling of 2026-11-01: the owner named it first, and Ollama is not running on the owner's machine).

Requirements: FR-2.4, FR-2.10, FR-2.13, FR-3.1 to FR-3.9, FR-3.12 to FR-3.14, FR-3.24 to FR-3.30, FR-4.1 to FR-4.10, FR-7.1 to FR-7.12, FR-10.1 to FR-10.4, FR-10.7, FR-10.8, FR-12.1, FR-12.3, FR-12.5, FR-12.6.

Exit tests:
- Profile stores multiple countries, regions, cities, currencies, time zones, languages, and work-mode preferences; no code path infers work authorization.
- Sensitive profile fields are encrypted at rest.
- PDF and DOCX resumes parse into the knowledge base.
- Generated resumes and cover letters export to DOCX and PDF, are versioned, compare against the original, and carry claim-level provenance; no claim lacks evidence.
- Ollama discovery, hardware detection, user-initiated download, structured output, and embeddings work against a local model.
- The task router validates outputs against JSON schemas, enforces timeouts and retries, and handles fallback when a backend fails.
- Injection corpus passes against the local backend for parse and draft tasks, with pasted postings normalized on import.
- The PTY panel runs the bundled CLI; restricted shell execution rejects disallowed commands.
- Offline operation passes with a local model installed.
- Claude Code headless backend: the router calls the user's installed `claude` CLI on its own login, passes job text only with every tool disabled (an adapter test proves the argv, and an ignored smoke test proves it against the real CLI), and the app reads or stores no CLI token. The terminal panel starts `claude` for the user and the router never sends it work. An absent CLI shows as unavailable.
- A consent record for `claude_code/cli_login` naming the data categories exists before any external call, and revoking it stops the next call.
- The router works end to end with Claude Code alone, Ollama alone, or No-AI, and a test proves each.

Gates: all five apply. Build includes the bundled CLI sidecar and the consent code; migrations add profile, encrypted-field, and consent-record tables on top of 0.1 data; Windows and macOS means PTY, keychain, and Ollama paths pass on both.
### 0.2 gate record

Recorded on 2026-11-01 at the 0.2 candidate. Details and the full lists are in [release 0.2.0](releases/0.2.0.md).

| Gate | Result |
|---|---|
| 1. Build | macOS arm64: `pnpm tauri build --no-bundle` built a release app, and that app launched on a temporary data folder and showed v0.2.0, schema 5. The sidecar builds on the host triple. Windows and x86_64 builds exist only as CI runs; the installers were not built or installed. |
| 2. Tests | macOS, local: 589 passed, 0 failed, 76 ignored; frontend 87 passed. CI (macOS and Windows) last passed both jobs at `2da4305`; the run at the 0.2 head was still running when this was written. |
| 3. Migrations | Fresh database: `schema_version` 5. A copy of a 0.1 database made with the 0.1 CLI opened with the 0.2 binary at version 5, wrote `pre-migrate-1-*`, and kept `doc list` identical. |
| 4. Windows and macOS | Windows ran the automated suites in CI at `2da4305` only. ConPTY, the terminal panel, native dialogs, and Windows credential denial were never run by a person. |
| 5. Docs and risks | Updated; risks are in the release notes. |

Exit tests not proven end to end: "Ollama discovery ... work against a local model" (no Ollama server was installed; a fake server and a unit-tested wording only), "Offline operation passes with a local model installed" (same reason), and "The PTY panel runs the bundled CLI" (tested through the PTY layer and the CLI line interpreter, not through a painted panel).

## Release 0.3: Career memory and application tracking

Scope: career memory, application tracking, deterministic workflow runner, execution logs, emergency pause. **Built.** The gate record follows the exit tests.

Requirements: FR-2.12, FR-8.1 to FR-8.8, FR-10.9 to FR-10.12, FR-11.3, FR-11.4, FR-11.5, FR-11.7, FR-11.9, FR-11.10 to FR-11.16, FR-12.8.

Exit tests:
- Question and answer banks support semantic retrieval, approval, provenance, freshness, and sensitivity classification.
- Sensitive or legally consequential answers cannot be used without explicit confirmation.
- Application history and status are viewable and editable offline (FR-2.12).
- The workflow runner executes deterministic workflows with restricted permissions, logs, timeouts, retries, checkpoints, and cancellation.
- Emergency pause stops every running workflow and is itself audited.
- The resume, cover letter, career memory, tracking, and analytics agents (FR-11.3, 11.4, 11.5, 11.7, 11.9) run as deterministic workflows on the runner. The discovery, matching, and research agents land in 0.5, and the browser application agent in 0.6, each with its feature.
- CLI memory search, application tracking, analytics, and agent monitoring commands call the same services as the GUI.

Gates: all five apply. Build includes the workflow runner; migrations add memory, tracking, and execution-log tables and upgrade a 0.2 database; Windows and macOS means pause and cancellation work on both.
### 0.3 gate record

Recorded on 2026-11-01 at the 0.3 candidate. Details and the full lists are in [release 0.3.0](releases/0.3.0.md). Rows marked Task 11 are filled by the controller after a real run on a temporary root.

| Gate | Result |
|---|---|
| 1. Build | macOS arm64: `pnpm --filter desktop sidecar` and `pnpm tauri build --no-bundle` built `target/release/jobpilot-desktop` at v0.3.0 (exit 0). That app launched on a temporary root and ran the GUI checks in the [release note](releases/0.3.0.md). The Windows build of the 0.3 tree is CI run 41388510283 at `bd4915d` (the same tree as `afdfbbe`, the 0.3 head), which passed on macOS and Windows. |
| 2. Tests | macOS, local: `cargo test --workspace` 923 passed, 0 failed, 86 ignored; `pnpm test` 189 passed; `pnpm typecheck`, `cargo fmt --check`, and `cargo clippy --workspace --all-targets -- -D warnings` clean. Keychain suites (`--ignored --test-threads=1`, `JOBPILOT_SECRET_SERVICE=jobpilot-test`, skipping `real_claude` and `make_fixtures`): 84 passed, 0 failed. CI run 41388510283 at `bd4915d` (the same tree as `afdfbbe`, the 0.3 head) passed on macOS and Windows, including the Windows keychain step. |
| 3. Migrations | Fresh database: `schema_version` 11 (`a_fresh_database_is_schema_11`). A schema 5 (0.2) database with a document, profile, knowledge item, job text, and draft opens at 11, writes `pre-migrate-5-*`, keeps every row, and leaves the new tables empty (`a_0_2_database_upgrades_to_0_3`). A database made by the real 0.2 CLI at `dc3f9b6`, opened by the 0.3 binary: schema 11, its data intact, a `pre-migrate-5-*` folder. |
| 4. Windows and macOS | Windows: CI run 41388510283 at `bd4915d` (the same tree as `afdfbbe`) passed on Windows, including the Windows keychain step, so the 0.3 tests ran there; before that run, `x86_64-pc-windows-gnu` clippy was clean as a type-check. ConPTY for the new commands, the native menu, and native dialogs were not used on Windows. The pause menu item and shortcut on macOS: passed through accessibility automation, not by a person. The `Clear pause` native dialog was not proved. |
| 5. Docs and risks | Updated: wireframes (screens 16 to 18), agents, architecture, database schema, AI providers, threat model, development guide, README, changelog, and the release notes. Risks are in the release notes. |

Exit tests not proven end to end:
- "Emergency pause stops every running workflow" is proven in core and the adapter by tests. The native menu item and the shortcut were not driven by a person unless the Task 11 row says so. `Cmd+Shift+H` may not fire while the web process hangs (`wry` 0.57.1); the menu and `jobpilot pause set` are the reliable paths.
- "Question and answer banks support semantic retrieval" ran against a fake Ollama server. No real embedding model was installed.
- "Pause and cancellation work on both" systems: the Windows evidence is the test suite in CI run 41388510283. Nobody used the pause menu or dialogs on Windows.
- Sensitive-answer confirmation through the native dialog was not driven by a person unless the Task 11 row says so.

## Release 0.4: External AI backends

Scope: optional CLI adapters, API providers, consent UI extended to them. **Built.** The gate record follows the exit tests. Claude Code (D1) shipped in 0.2. Per the owner's decisions of 2026-11-01: optional Gemini CLI and Codex CLI adapters on whatever sign-in the installed CLI supports; direct API providers under standard BYOK (D4). Security controls stay: `no_tools` for job and company text, and consent records.

Requirements: FR-3.10, FR-3.11, FR-3.15 to FR-3.19.

Exit tests:
- Each direct API provider in FR-3.15 to FR-3.19 that the register's [owner decisions](policy-register.md#owner-decisions-2026-11-01) make eligible (OpenAI, Anthropic, Google paid key, OpenRouter, loopback endpoints) runs through the task router with capability declarations, schema validation, timeouts, retries, and fallback. A provider the decisions leave out (free Gemini key, non-loopback endpoints, Vertex AI, Ollama cloud models) is refused, and a test proves it.
- Consent UI extends the 0.2 record (FR-3.30, Claude Code) to API providers and the optional CLI adapters, blocks any external call until consent is given, and shows what will be sent.
- Prompt-injection tests pass for untrusted text sent to external backends. FR-12.3 lands in 0.2; this test extends it to hosted backends.
- Optional Gemini CLI and Codex CLI adapters: run on the installed CLI's sign-in when present, show unavailable when absent, and are skipped for tasks that need `no_tools` unless a no-tools configuration is proven.
- No mandatory key: the app still works end to end with no provider configured.

Gates: all five apply. Build includes the provider code; migrations add provider settings; Windows and macOS means keychain-stored keys and CLI detection work on both.
### 0.4 gate record

Details and the full lists are in [release 0.4.0](releases/0.4.0.md). Rows marked Task 11 are filled by the controller after a real run on a temporary root. A row that still shows a placeholder was not run.

| Gate | Result |
|---|---|
| 1. Build | `scripts/ci-local.sh --fast` and `pnpm --filter desktop sidecar && pnpm tauri build --no-bundle` at the Task 10 commit: see [release 0.4.0](releases/0.4.0.md#tests) for the figures. Debug-build E2E and the launched app: debug app built and launched at `1e84cfe`, driven once by keyboard. |
| 2. Tests | Local macOS: see [release 0.4.0](releases/0.4.0.md#tests). Keychain suites (`--ignored --test-threads=1`, `JOBPILOT_SECRET_SERVICE=jobpilot-test`, skipping `real_claude`, `real_gemini`, `real_tls` and `make_fixtures`): 88 passed, 0 failed at the Task 10 commit (see the release note); the controller's own run: CI keychain step at `1e84cfe`: macOS passed, Windows failed one test. |
| 3. Migrations | `a_fresh_database_is_schema_12` and `a_0_3_database_upgrades_to_0_4` (figures in the release note). An upgrade of a database made by the real 0.3 binary: Pass with the real 0.3.0 CLI: schema 11 to 12, `pre-migrate-11` backup written, Claude consent reads suspended, `ai consent give` lifts it.. |
| 4. Windows and macOS | CI run 41452177325 at `1e84cfe`: macOS passed, including keychain and TLS handshake steps, Windows passed `cargo test --workspace`, then failed one keychain test (`discard_requires_key_missing_and_matching_confirm`, `NoEntry`), so `TLS handshake` was skipped there (including `Keychain tests (Windows)` and the `TLS handshake` step, the Schannel proof). This row stays open until the controller reads that run. Real `gemini` and `codex` on Windows, the GUI, native dialogs, and the clipboard are not run by CI. |
| 5. Docs and risks | Updated: AI providers, policy register (prose only; no row), threat model, agents, architecture, database schema, development guide, wireframes (screens 13 and 14), PRD, README, changelog, and the release note. Risks are in the release note. |

Exit tests not proven end to end:
- "Each direct API provider ... runs through the task router": proven against local fakes. Whether the real OpenAI, Anthropic, Google, and OpenRouter APIs accept the request bodies is Not run: no provider key on this Mac. Hosted wires are proven against local fakes only, and the new draft reply shape has run on no real hosted model..
- "Optional Gemini CLI and Codex CLI adapters": Gemini is proven by a mock CLI and by reading the 0.40.0 bundle and docs. The real CLI was run only if the Task 11 row says so (Ran once with `gemini` 0.40.0: flags present, then `cli_account_refused` in 3 s, so the account is not signed in or not eligible. No-tools at run time is unproven; the app reads this as manual required.). Codex ships inert: it declares no `no_tools`, so every task skips it, and its run path and flags are unverified.
- "Blocks any external call until consent is given, and shows what will be sent": proven by core, CLI, and Tauri-harness tests. No person drove the native dialogs unless a Task 11 row says so.
## Release 0.5: International job discovery and intelligence

Scope: connector SDK, read-only discovery, job intelligence (spec section 6).

Requirements: FR-3.20, FR-5.1 to FR-5.18, FR-6.1 to FR-6.12, FR-10.5, FR-10.6, FR-11.1, FR-11.2, FR-11.8, FR-13.5.

Exit tests:
- The connector SDK defines operations, automation capabilities, rate limits, and policy-review status; connectors cannot register without them.
- A read-only connector exists for each source in FR-5.2 to FR-5.9 whose register read verdict is `allowed`, and respects rate limits and permitted access. A source at `unknown` or `blocked` has no read connector, a test proves it is refused, and its postings enter through user-assisted import. LinkedIn is user-assisted or authorized access only.
- Tests confirm no CAPTCHA bypass, MFA bypass, or access-control circumvention.
- Job description parsing, duplicate detection, weighted match scores, missing skills, seniority, compensation, and location matching work, with explainable output.
- CLI job search and job analysis commands (FR-10.5, 10.6) call the same services as the GUI.
- Deterministic matching (FR-3.20) works with no AI backend.
- Objective filters are deterministic; semantic evaluation runs only when an AI backend is configured.

Gates: all five apply. Build includes the connector SDK; migrations add jobs, companies, and match tables; Windows and macOS means connector tests and the job CLI commands pass on both.
## Release 0.6: Browser automation

Scope: Playwright runtime, permissions engine, Manual and Copilot modes, and the Claude Code apply mode (owner decision of 2026-11-01, below). Per D2, Copilot runs on a site where no clause forbids it, recorded as "no clause found" (Greenhouse, Lever, Ashby); a site row that is `blocked` stays blocked. Autonomous mode is default-off and ships only for connectors that pass policy review.

**Owner decision of 2026-11-01: Claude Code apply mode.** Release 0.6 also plans a mode in which the app starts the owner's own `claude` in its terminal panel with Playwright MCP and a visible browser, on all sites including Indeed, LinkedIn, and Workday. It overrides the `blocked` and `unknown` site rows and decision D2 for this mode only, on the owner's own accounts, and submits with no per-application check once the owner starts a run. These stay: only approved answers and profile facts; browser tools only, with files limited to the chosen resume and letter; stops for CAPTCHA, MFA, identity checks, and legal declarations, with no bypass; every application logged and tracked; and emergency pause stops the session. Stated risk: the site may restrict or ban the account. It is recorded in the [policy register](policy-register.md#owner-decision-2026-11-01-claude-code-apply-mode-release-06). **Planned, not built.** Its exit tests are written in the 0.6 plan.

Requirements: FR-3.22, FR-9.1 to FR-9.21, FR-11.6, FR-12.4.

Exit tests:
- Manual mode supplies documents and retrieved answers for the user to apply in their own browser, and records the outcome.
- Copilot mode detects forms, extracts questions, retrieves answers, fills forms, uploads resume and cover letter, and captures confirmations on sites whose register row for Copilot fill is `allowed` or records "no clause found". Sites whose row is `blocked`, quotes a clause that may reach them (Workday), or was not read (LinkedIn) are refused, and a test proves it. Each Copilot run records "no clause found". Tests run against local fixture forms for each eligible site.
- The permissions engine allows Autonomous submission only for connectors with verified permission and explicit user authorization; all other connectors are refused.
- The runtime pauses for legal declarations, unverified sensitive information, identity verification, CAPTCHA, MFA, and other human-only steps.
- Browser processes are isolated; emergency pause halts them.
- Rule-based form mapping (FR-3.22) fills common fields with no AI backend.
- Application status updates after submission, and each submission is audited.

Gates: all five apply. Build includes the Playwright runtime and its browser binaries; migrations add permission and automation-run tables; Windows and macOS means isolated browser launch and pause pass on both.
## Release 1.0: Stable open-source distribution

Scope: signing and notarization, auto-update, security review, public docs. FR-1.8 installers were first delivered unsigned in 0.1; 1.0 signs them.

Requirements: FR-13.10, FR-13.11. The security review covers all of FR-12 as delivered in earlier releases.

Exit tests:
- Installers are signed (Windows) and signed and notarized (macOS).
- Auto-update delivers and verifies a new version.
- Security review is complete and its findings are resolved or documented.
- Published: PRD, architecture, database schema, Connector SDK docs, security model, development guide, contribution guide, license, and release notes.
- The license is confirmed as Apache 2.0, or a documented reason is given for a change.
- Release builds are produced by GitHub Actions on Windows and macOS.

Gates: all five apply. Build means signed installers; migrations means upgrade tests from every earlier release; Windows and macOS means signing, notarization, and update verified on both.
