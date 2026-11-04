# JobPilot Release 1.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship JobPilot 1.0 as a stable open-source distribution. That means signing-ready release builds, a verified auto-update, a security review, upgrade tests from every earlier release, and the full public docs set.

**Architecture:**
- No new core behaviour beyond one setting, `updates.check_on_launch`.
- The desktop app gains `tauri-plugin-updater` and `tauri-plugin-process`. Updates are minisign-verified against a public key pinned in `tauri.conf.json`.
- The release workflow builds the updater artifacts and `latest.json`. It signs and notarizes whenever the owner's certificate secrets exist.
- Supply-chain gates (cargo-deny, pnpm audit) join CI.

**Tech Stack:** Rust 1.99.1, Tauri 2.12, tauri-plugin-updater 2, tauri-plugin-process 2, minisign-verify, cargo-deny, React 19, Vitest, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-01-jobpilot-master-spec.md`; release scope in `docs/milestones.md` "Release 1.0"; PRD `docs/prd.md` FR-1.8, FR-12.*, FR-13.*.

## Global Constraints

- Exit tests (milestones.md):
  - installers are signed (Windows) and signed and notarized (macOS);
  - auto-update delivers and verifies a new version;
  - the security review is complete and its findings are resolved or documented;
  - the published docs are: PRD, architecture, database schema, Connector SDK, security model, development guide, contribution guide, license, and release notes;
  - the license is Apache 2.0;
  - release builds come from GitHub Actions on Windows and macOS.
- Gates: build, tests, migrations (upgrade tests from every earlier release), Windows and macOS, docs.
- Local-first: no network call without a person's action or an opt-in setting. The update check at launch is **off by default**. "Check for updates" is a button.
- Updater public key (verbatim, the `plugins.updater.pubkey` value):
  `dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDlDMjc2NjMxMjQwRTNBMDQKUldRRU9nNGtNV1lublBtTWZTMGtoTlpZZXZhd09yZXVrVTRaQnZOTlZKRmxDdXRRWDdXRDl6N1AK`
- Updater endpoint (verbatim): `https://github.com/gvsrusa/jobpilot/releases/latest/download/latest.json`
- Secrets:
  - The repo holds `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The private key never enters the repo or a subagent.
  - Signing secret names (owner supplies them later): `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`, `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`.
- Schema per release (upgrade sources): 0.1 = 1, 0.2 = 5, 0.3 = 11, 0.4 = 12, 0.5 = 15, 0.6 = 17. 1.0 adds no migration.
- Version 1.0.0 in `Cargo.toml` (workspace), `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`.
- Unchanged hard rules: no CAPTCHA/MFA bypass, never fabricate, never infer work authorization, consent records, emergency pause.
- Tests use a temporary `JOBPILOT_HOME` and `JOBPILOT_SECRET_SERVICE=jobpilot-test`. Never use the default data folder or the real keychain service.
- Subagents never push, tag, or set secrets. Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Rulings made while planning

- Ruling: signing is wired but gated on the owner's certificate secrets. Without them the build stays ad-hoc (macOS) or unsigned (Windows), and the exit test is reported as "pending certificates".
  - Why: no Apple Developer ID or Windows certificate exists, and buying one is the owner's call.
  - Cost if wrong: the signed-installer exit test stays open until the owner adds the secrets. No code rework is needed.
- Ruling: the update check at launch is opt-in, default off.
  - Why: the PRD offline table lists "Auto-update and release checks" as network, and the local-first rule says no unprompted network.
  - Cost if wrong: one default flips.
- Ruling: the 0.6 parked minors (M-2, M-3, M-5, Task 11 m-C/m-E, N1, the final "Apply" button residual) stay documented and out of the 1.0 scope.
  - Why: the milestone scope does not name them.
  - Cost if wrong: a later patch release.
- Ruling: end-to-end update delivery is proven by the controller.
  - Steps: install `v1.0.0-rc.1` on macOS, publish `v1.0.0`, then update from inside the app. The Windows update goes on the owner's hand-check list. A unit test pins that the configured public key verifies a good signature and refuses a tampered one.
  - Why: Tauri's updater runs only in a real app window.
  - Cost if wrong: the Windows half stays unverified.

## Review Focus

1. Offline, or GitHub unreachable, when "Check for updates" is pressed: the app shows a plain error such as `update_check_failed` and nothing else changes. The test lives in Task 2 (`update_check_offline_reports_error`).
2. Updater payload with a bad signature: refused and nothing installed. Tested in Task 2 (`a_tampered_file_is_refused`, against the pinned key).
3. Install while an apply run is active or the emergency pause is set: refused with `update_blocked_by_run`, so no process restarts mid-application. Tested in Task 2 (`install_refused_while_paused_or_running`).
4. A database from any earlier release opens in 1.0 without loss: every source schema upgrades, writes its `pre-migrate-N-` snapshot, and keeps its rows. Tested in Task 1.
5. A release build without signing secrets: the workflow still succeeds, uploads unsigned installers, and the release body says they are unsigned. Covered in Task 3 by the conditional steps and the dry-run check.

---

### Task 1: Upgrade tests from every earlier release

**Files:**
- Modify: `crates/core/tests/db.rs`. Add one test next to `a_0_5_database_upgrades_to_0_6` and reuse its `temp_root` and `snapshots` helpers.

**Interfaces:**
- Consumes: `MIGRATOR`, `Db::open_with`, `Db::open`, `Migrator::with_migrations`.

- [ ] **Step 1: Write the test `every_earlier_release_upgrades_to_1_0`.**
  - It loops over `[("0.1", 1), ("0.2", 5), ("0.3", 11), ("0.4", 12), ("0.5", 15), ("0.6", 17)]`, using a fresh root each time.
  - Open with the migrations `<= v`, then assert `schema_version() == v`.
  - Insert one `documents` row (table from 0001), plus one `applications` row when `v >= 6`. Then close.
  - Reopen with `Db::open`, then assert:
    - `schema_version() == jobpilot_core::db::app_schema_version()`;
    - the rows are still present;
    - for `v < 17`, `snapshots(&paths, &format!("pre-migrate-{v}-")).len() == 1`; for `v == 17`, no new snapshot.
  - Each failure message names the release.
- [ ] **Step 2:** `cargo test -p jobpilot-core --test db every_earlier_release` must pass. If it passes the first time, mutate it to confirm it can fail (for example, assert the wrong count), then revert.
- [ ] **Step 3:** Commit: `test: upgrade from every earlier release to 1.0`.

### Task 2: Verified auto-update in the desktop app

**Files:**
- Modify:
  - `apps/desktop/src-tauri/Cargo.toml`: add `tauri-plugin-updater = "2"`, `tauri-plugin-process = "2"`, and dev-dependency `minisign-verify` (the crate the updater uses).
  - `apps/desktop/src-tauri/tauri.conf.json`: `bundle.createUpdaterArtifacts: true`; `plugins.updater.pubkey` and `plugins.updater.endpoints` set to the verbatim values above; `plugins.updater.windows.installMode: "passive"`.
  - `apps/desktop/src-tauri/capabilities/default.json`: no updater permission for JS. The Rust commands below are the only path.
  - `apps/desktop/src-tauri/src/lib.rs`: register the plugins and commands.
  - `apps/desktop/src-tauri/src/commands.rs`
  - `apps/desktop/src/views/StatusView.tsx`
  - `apps/desktop/src/lib/ipc.ts`
  - `crates/core/src/settings.rs` (a const key)
- Create:
  - `apps/desktop/src-tauri/src/update.rs`
  - `apps/desktop/src-tauri/tests/updater_key.rs`
  - `apps/desktop/src-tauri/tests/fixtures/update/payload.bin`
  - `apps/desktop/src-tauri/tests/fixtures/update/payload.bin.sig`. The controller creates these two fixtures with the real private key before dispatch. Do not regenerate them.
- Test:
  - `apps/desktop/src-tauri/tests/updater_key.rs`
  - the `update.rs` unit tests
  - `apps/desktop/src/views/views10.test.tsx`

**Interfaces:**
- Produces:
  - `pub const UPDATES_CHECK_ON_LAUNCH: &str = "updates.check_on_launch";` in core `settings`. A missing value means `false`.
  - Tauri command `update_check() -> Result<Option<UpdateInfo>, CmdError>`, where `UpdateInfo { version: String, notes: Option<String>, date: Option<String> }`.
  - Tauri command `update_install() -> Result<(), CmdError>`. It downloads, verifies (the plugin does this), installs, and then calls `app.restart()`. It writes the audit row `update_installed` with `{from, to}`.
  - Tauri commands `update_auto_get() -> bool` and `update_auto_set(on: bool)`.
  - Error codes: `update_check_failed`, `update_install_failed`, `update_blocked_by_run`.
  - Pure helper `fn install_blocked(paused: bool, active_runs: usize) -> bool` in `update.rs`.

- [ ] **Step 1: Write the failing tests.**
  - `updater_key.rs`, test `the_pinned_key_verifies_a_release_signature`:
    - read `plugins.updater.pubkey` from `tauri.conf.json` (base64 to text, then `minisign_verify::PublicKey::decode`);
    - verify `payload.bin` against `payload.bin.sig` (base64-decoded); it must be `Ok`.
  - `a_tampered_file_is_refused`: flip one byte of the payload; verification must be `Err`.
  - `update.rs`, test `install_refused_while_paused_or_running`:
    - `install_blocked(true, 0)` is true;
    - `install_blocked(false, 1)` is true;
    - `install_blocked(false, 0)` is false.
  - `views10.test.tsx`:
    - `update_check_offline_reports_error`: the mocked `update_check` rejects with `update_check_failed`; StatusView shows "Couldn't check for updates" and no install button.
    - `update_available_offers_install`: the mock returns `{version:"1.0.1"}`; the view shows "Version 1.0.1 is available" and an "Install and restart" button that calls `update_install`.
    - `launch_check_toggle_defaults_off`: the "Check for updates when JobPilot starts" switch starts unchecked, and toggling it calls `update_auto_set(true)`.
- [ ] **Step 2:** Run `cargo test -p jobpilot-desktop --test updater_key` and `pnpm -C apps/desktop test views10`. The tests should fail (missing key, command, view).
- [ ] **Step 3: Implement.**
  - `update.rs` uses `tauri_plugin_updater::UpdaterExt`.
  - Install reads the pause state and the active apply-run count from core (the existing pause and apply-run queries), so the decision is made in core, not in JS.
  - At startup, if `UPDATES_CHECK_ON_LAUNCH` is true, spawn one check. It only emits `update-available` and never installs on its own.
  - StatusView gets an "Updates" section with the current version, a "Check for updates" button, the result line, "Install and restart", and the switch.
- [ ] **Step 4:** The tests from Step 2 pass. The Tauri debug build passes (`pnpm -C apps/desktop tauri build --debug --no-bundle`). `scripts/ci-local.sh --fast` is green.
- [ ] **Step 5:** Commit: `feat: verified auto-update with an opt-in launch check`.

### Task 3: Release workflow, signing-ready

**Files:**
- Modify: `.github/workflows/release.yml`, `apps/desktop/src-tauri/tauri.conf.json` (Windows `signCommand` is not used; see below), `README.md` "Installing" section.

**Interfaces:**
- Consumes: the secret names in Global Constraints. `createUpdaterArtifacts` comes from Task 2.

- [ ] **Step 1: Updater signing.** Pass `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to `tauri-action` from secrets, and set `includeUpdaterJson: true`.
- [ ] **Step 2: macOS signing.**
  - Pass the six `APPLE_*` env vars to `tauri-action`, which imports the certificate and notarizes when they are set.
  - When `APPLE_SIGNING_IDENTITY` is empty, the ad-hoc `"-"` from the config stays.
  - Add a step after the build: `codesign --verify --deep --strict` on the built `.app`. If `APPLE_ID` is set, also run `spctl -a -vv -t exec`.
- [ ] **Step 3: Windows signing.**
  - When `WINDOWS_CERTIFICATE` is set, a PowerShell step before the build decodes the base64 PFX and imports it into `Cert:\CurrentUser\My`.
  - It writes the thumbprint into `tauri.conf.json` `bundle.windows.certificateThumbprint`, with `digestAlgorithm: "sha256"` and `timestampUrl: "http://timestamp.digicert.com"`.
  - A step after the build runs `Get-AuthenticodeSignature` on the NSIS installer and fails unless the status is `Valid`.
  - Every signing step is guarded by `if: env.WINDOWS_CERTIFICATE != ''`, with the secret mapped to job env. The same pattern applies to Apple.
- [ ] **Step 4: Release body.** It says either "Signed and notarized" or "Unsigned: see README" depending on which secrets exist. Compute it in a prior step's output.
- [ ] **Step 5: Check.** `actionlint .github/workflows/release.yml` must be clean (install with `brew install actionlint` if missing). A `workflow_dispatch` trigger with input `dry_run` builds without creating a release, so the controller can run it before tagging.
- [ ] **Step 6:** Commit: `ci: signing-ready release builds with updater artifacts`.

### Task 4: Supply-chain gates

**Files:**
- Create: `deny.toml`.
- Modify: `.github/workflows/ci.yml` (a `supply-chain` job on ubuntu-latest), `scripts/ci-local.sh` (also runs `cargo deny check` when it is installed), and `Cargo.toml` / `package.json` for any upgrades the gates force.

- [ ] **Step 1:** `deny.toml`:
  - `[advisories]`: deny vulnerabilities, warn on unmaintained.
  - `[licenses]`: allow the licenses the current tree actually uses, as listed by `cargo deny list`, all Apache-2.0-compatible. Any copyleft license found is reported in the task report, not allowed silently.
  - `[bans]`: `multiple-versions = "warn"`.
  - `[sources]`: crates.io only, plus any git source already in `Cargo.lock` named explicitly.
- [ ] **Step 2:** `cargo install cargo-deny --locked`, then `cargo deny check`. Fix each advisory by upgrading. An advisory with no fix is ignored with an `ignore = [{ id, reason }]` entry naming why JobPilot is not exposed.
- [ ] **Step 3:** `pnpm audit --prod --audit-level high` passes, or each remaining high advisory is resolved through `pnpm.overrides` with a comment.
- [ ] **Step 4:** Add the CI job that runs both, and green it locally.
- [ ] **Step 5:** Commit: `ci: cargo-deny and pnpm audit gates`.

### Task 5: Security review (FR-12)

A review task: the agent reads code and writes findings, and changes no product code.

**Files:**
- Create: `docs/security/review-1.0.md`.

- [ ] **Step 1:** For each FR-12 requirement in `docs/prd.md`, and for each mitigation M-1..M-40 in `docs/security/threat-model.md`, find the code that implements it and its test. Record "verified (file:line, test)", "gap", or "documented residual".
- [ ] **Step 2: Targeted checks.** For each one, read the code and try the obvious bypass in a test against a temporary root:
  - Tauri capability surface (`capabilities/default.json`, every `#[tauri::command]`);
  - CSP;
  - the PTY and shell restrictions;
  - the child env allow-list;
  - secrets never reaching logs or exports;
  - backup and restore keeping the pause;
  - the apply-mcp guards;
  - the updater (Task 2): key pinning and install blocking.
- [ ] **Step 3:** Findings table: id S-n, severity (critical / important / minor), file:line, scenario, and suggested fix.
- [ ] **Step 4:** Commit: `docs: 1.0 security review`.

### Task 6: Resolve security findings

- [ ] For each critical and important finding from Task 5: write a failing test, fix, and pass. Minor findings are fixed when small, or otherwise documented in `review-1.0.md` as residuals with a reason.
- [ ] Update each finding's row with its outcome and commit hash. Update threat model M-40 to "1.0: built" with the signing caveat.
- [ ] `scripts/ci-local.sh --fast` is green. Commit per fix: `fix(security): <finding>`.

### Task 7: Version 1.0.0 and public docs

**Files:**
- Modify:
  - versions in `Cargo.toml`, `apps/desktop/package.json`, `tauri.conf.json`, and `Cargo.lock`, via `cargo update -w`;
  - `README.md`: the install section, a docs index linking every published doc, and the update section;
  - `CHANGELOG.md`;
  - `docs/milestones.md`: 1.0 marked built, with the exit-test table;
  - `docs/prd.md`: offline table row "Auto-update and release checks", note opt-in;
  - `docs/development.md`: release process, the secrets list, and how to rotate the updater key;
  - `docs/policy-register.md`: D3 superseded for 1.0 by "signed when certificates exist".
- Create:
  - `SECURITY.md`: how to report a vulnerability through GitHub private advisories; supported version 1.0.x;
  - `docs/releases/1.0.0.md`: the release note in the same shape as 0.6.0.md, with an exit-test results table (rows filled by the controller in Task 8, written now as `PENDING-T8`).

- [ ] **Step 1:** Bump the versions. `cargo test -p jobpilot-cli` passes. The CLI `--version` test expects 1.0.0; update its expectation.
- [ ] **Step 2:** Write the docs. Confirm every doc in the exit list exists and is linked from the README.
- [ ] **Step 3:** Run `scripts/ci-local.sh --fast` and commit: `release: 1.0.0 version and docs`.

### Task 8: Release gates (controller)

Only the controller runs this task: it tags, publishes, and drives the app.

- [ ] Open the PR for `feat/v1.0-release` and wait for green CI on macOS and Windows.
- [ ] Run the release workflow with `dry_run`, then tag `v1.0.0-rc.1`.
  - Check the draft release: it has the `.dmg` (arm64, x86_64), `.msi`, NSIS `.exe`, `.sig` files, and `latest.json`.
  - Run `codesign --verify` on the downloaded app.
  - Publish it as a prerelease.
- [ ] Install rc.1 on macOS into a temporary location, with a fresh `JOBPILOT_HOME` and the test keychain service. Run the 0.6-root upgrade with the CLI.
- [ ] Merge the PR, tag `v1.0.0`, and publish it as the latest release. In the rc.1 app, use "Check for updates", then "Install and restart". Then:
  - confirm the relaunched app shows 1.0.0;
  - confirm the audit log has `update_installed`.
  - Driving the window: macOS accessibility clicks, only when JobPilot is frontmost. If the window can't be driven, this item moves to the hand-check list.
- [ ] Fill in the `PENDING-T8` rows. List these as owner hand-checks: signing certificates, the Windows update, and the remaining 0.6 hand-checks.
