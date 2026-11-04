# JobPilot AI: Security Review for Release 1.0

Status: review of `feat/v1.0-release` at `f1c8f55`. Date: 2026-11-03. The updater was reviewed as committed in `e9f163f` (`apps/desktop/src-tauri/src/update.rs`). The restart race and the Windows teardown gap found in that task's own review are being fixed separately and are not repeated here. Sources: [PRD](../prd.md) section 12 (FR-12), [threat model](threat-model.md) sections 7, 10, and 11.

Method: for each FR-12 requirement and each mitigation M-01 to M-40, the code and its named tests were read, and every test name the threat model cites was checked to exist in the tree. Then the targeted checks below were made by reading the code and, where a bypass looked possible, by a throwaway test against a temporary `JOBPILOT_HOME` with `JOBPILOT_SECRET_SERVICE=jobpilot-test`. The probes were deleted before this commit. No real `claude`, no real site, and no real keychain service were used.

Owner decisions taken as given, and so not findings unless the code is weaker than stated: the Claude Code apply mode on every site with no per-application check (with the stops for CAPTCHA, MFA, identity, and legal text, approved facts only, browser tools only, and the emergency pause); unsigned installers until certificates exist; the residuals in [release 0.6.0](../releases/0.6.0.md) "Known issues" and threat model section 11.

## 1. Findings

| ID | Severity | Where | Scenario | Suggested fix | Outcome |
|---|---|---|---|---|---|
| S-1 | Important | `crates/core/src/backup.rs:565` (`migrate_and_fill`), `crates/core/src/db.rs:358` (`copy_dir`), `crates/core/src/ai/claude_code.rs:341` (`find_binary_quietly`), `crates/core/src/apply/session.rs:469` | **A restored backup chooses which program runs as `claude`, and the startup sweep runs it with no further prompt.** The desktop sets `ai.claude_path` (and `ai.gemini_path`, `ai.codex_path`) only through a Rust file picker, because "the picker is the confirmation: the webview cannot name a path". A restore copies the backup's `settings` table as it is, and `copy_dir` copies `documents/` with its file modes. A crafted backup folder can therefore hold `documents/claude` (mode 0755, or `claude.cmd` on Windows) and `ai.claude_path = <default root>/documents/claude`; the default root is predictable. If the backup also holds an `apply_runs` row that is not `ended`, the next launch's `sweep_at_start` finds an orphan and runs `<planted> purge <root>/apply/session --yes`, provided the session folder exists, as it does once the owner has used apply mode. With no such row the planted program runs on the next Claude Code task, apply run, or panel `claude`. Restore revokes external consent, but neither the sweep nor the panel `claude` checks consent. The only gate is the native "Restore backup" dialog. **Proven by a probe**: a backup made in root A with that setting, plus an executable `documents/claude`, restored into root B. `find_binary_quietly` returned the planted path, the copy kept mode 0755, and `apply::session::sweep_orphans` executed it with `purge <B>/apply/session --yes`. The same restore also brings back `updates.check_on_launch`, so a backup can turn on a launch-time network check that only a person may turn on. | On restore, treat every owned setting the way the endpoint and consent already are: keep the live values of `ai.claude_path`, `ai.gemini_path`, `ai.codex_path`, `ai.ollama_url`, and `updates.check_on_launch` (or clear the program paths), and audit what changed. As a second wall, have `claude_code::checked` and `child::checked` refuse a program inside the data root, and have `copy_dir` drop execute bits. Add a test that restores a backup with a planted path and asserts the live one survives. | Fixed in `23127f2`. Restore keeps the live `ai.claude_path`, `ai.gemini_path`, `ai.codex_path`, and `updates.check_on_launch` (absent stays absent; unreadable live values drop the backup's) and audits `settings_kept_by_restore`. `claude_code::find_binary_quietly` and `child::find_cli` refuse a configured path inside the data root (`child::outside_root`). `copy_dir` drops execute bits on Unix. Tests: `restore_keeps_the_live_program_paths_and_launch_check`, `offline_restore_keeps_the_live_program_paths_and_launch_check`, `restore_with_the_same_program_paths_audits_no_kept_setting`, `restored_documents_lose_their_execute_bits`, `a_program_path_inside_the_data_root_is_refused` (`backup.rs`). `ai.ollama_url` is not kept: it names no program and passes the loopback rules (M-21). `JOBPILOT_CLAUDE_BIN` and the other override variables are not checked against the root: a backup cannot set them. |
| S-2 | Important | `crates/core/src/apply/guard.rs:378` (`refused_args`), `crates/core/src/apply/proxy.rs:672` | **`browser_click` with `doubleClick: true` on a submit control is one counted click and two clicks on the page.** M-39 says a second click on a control named to submit opens a pause. The pinned Playwright schema offers `doubleClick`. `refused_args` does not refuse it, so the proxy adds 1 to `submit_clicks`, opens no pause, and forwards `{"doubleClick": true}`. A steered or careless session can submit twice on a form whose button stays live for the second click. **Proven by a probe** on the `form.yml` fixture: the forwarded call carried `doubleClick: true`, `submit_clicks` was 1, and no pause opened. `button: "middle"` or a modifier on a submit link also open a second page that the proxy's own snapshot does not watch. | Refuse `doubleClick` in `refused_args` (or count it as two submit clicks), and refuse `button` other than `left` and any `modifiers` on a submit target. Pin it with a proxy test next to `a_second_submit_click_pauses`. | Fixed in `e0661c7`. `refused_args` refuses `doubleClick`, a `button` other than `left`, and any non-empty `modifiers` on `browser_click` (`click_kind_refused`); no other pinned tool has these fields. Tests: `a_click_is_one_plain_left_click` (`apply_guard.rs`), `a_double_or_modified_click_on_submit_is_refused_and_not_counted` (`apply_proxy.rs`). |
| S-3 | Important | `.github/workflows/release.yml:77`, `:113`, `:137`; `apps/desktop/src-tauri/tauri.conf.json` `beforeBuildCommand` | **The updater signing key is in reach of code the repo does not pin.** `TAURI_SIGNING_PRIVATE_KEY` and its password sit in the env of the `tauri-apps/tauri-action@v1` steps. That action, `swatinem/rust-cache@v2`, and `pnpm/action-setup@v6` are referenced by mutable tags. The action also runs `beforeBuildCommand` (`pnpm build && node scripts/sidecar.mjs`), so every Vite plugin and build-time npm package runs with the key in its environment. The Windows certificate and password are mapped at job level, so every step sees them, `pnpm install` included (pnpm 12 blocks dependency build scripts by default, which narrows this). Whoever holds the updater key can sign an update that every installed copy accepts (M-40), so this is the one key whose theft reaches every user. | Pin every third-party action to a full commit SHA. Build with no signing key in the env, then sign the updater artifacts in a separate step that runs only `tauri signer sign` (or minisign) with the key, and upload. Map the Windows certificate into the env of the import step only. | Fixed in part in `cd390ca`. Every action in `release.yml` and `ci.yml` is pinned to a full commit SHA with its tag in a comment, and the Windows certificate and password are mapped into the import step only (other steps see `HAS_WINDOWS_SIGNING`). actionlint clean. Documented residual: the updater key stays in the env of the two `tauri-action` steps, because that action signs the updater artifacts while it builds, so the pinned action and the `beforeBuildCommand` it runs (`pnpm build && node scripts/sidecar.mjs`, with every Vite plugin and build-time package) can read it. A separate signing step was not built for 1.0. |
| S-4 | Minor | `apps/desktop/src-tauri/capabilities/default.json` | **The webview holds more capability than M-01 says.** M-01 states "the dialog plugin's `dialog:allow-open` only". The capability also grants `core:default`, which expands to `core:menu:default` (create, replace, and `set_enabled`/`set_accelerator` on menu items, `set_as_app_menu`), `core:tray:default`, `core:image:default` (`from_path` reads an image file the webview names), `core:path:default`, and `core:webview:default`. With `core:menu:default`, script in the webview can replace the app menu or disable **Safety > Emergency pause**, which M-25 calls the path that "does not depend on the webview". The precondition is script in the webview, and none was found: the CSP is `default-src 'self'` with no inline script, and the frontend has no `innerHTML`, `dangerouslySetInnerHTML`, link `href`, or `window.open` sink (searched). `jobpilot pause set` still works from any terminal. | List exactly what the frontend uses: `core:event:allow-listen`, `core:event:allow-unlisten`, `core:event:allow-emit` (the `pty:ack` event), `core:window:allow-close`, and `dialog:allow-open`. Drop `core:default`. Correct M-01's text. | Task 6b. |
| S-5 | Minor | `apps/desktop/src-tauri/src/commands.rs:1879` (`apply_start`), `:1938` (`apply_resolve_pause`), `pty.rs:383` (`pty_open_apply`); `update.rs:143` and `:189` at `e9f163f` | **Some consequential acts need only an IPC call.** With no sealed answers to release, `apply_start` then `pty_open_apply` start a Claude Code run that submits on every site, with no native dialog. `apply_resolve_pause` with `continue` ends a CAPTCHA, MFA, identity, or legal stop with no native dialog, and `jobpilot apply resolve` is not refused in the panel, which the webview can type into. `update_auto_set` turns on the launch-time network check, and `update_install` installs a verified update, both with no native dialog. Each is reachable only by script in the webview (see S-4 on why none was found). Threat model M-13 confirms delete, restore, and discard natively, and section 4 rule 5 says the mandatory pauses "cannot be answered by a model", which holds: the session has no tool that resolves a pause. | Either record these in threat model section 11 as residuals that rest on M-04, or add a native confirmation to starting an apply run (it authorizes every submission in it) and to Continue on a `legal` or `identity` stop, and refuse `apply resolve` in the panel. | Documented residual. Each act needs script running in the webview, and none was found: the CSP allows no inline or remote script and the frontend has no HTML sink (M-04, section 2.2). Native confirmations for these acts are left for a later release. |
| S-6 | Minor | `apps/desktop/src-tauri/src/commands.rs:369` (`backup_restore`) | **`backup_restore` takes a free path from the webview.** M-02 says no command takes a free path. The webview opens its own folder picker (`lib/ipc.ts:1002`, through `dialog:allow-open`) and sends the string. The native "Restore backup" dialog names it, and validation refuses links, FIFOs, and a bad schema, so this is defence in depth, but with S-1 the only check between a folder an attacker placed on disk and code execution is that dialog. | Pick the folder in Rust (a `native::open_dir`, as `resume_import` does for files), and have the command take no path. | Task 6b. |
| S-7 | Minor | `crates/core/src/db.rs:146` | **The data root is not created user-only.** M-06 says "user-only by default". `fs::create_dir_all` uses the process umask, so the root, `jobpilot.db`, `documents/`, and `backups/` get 0755 and 0644 on macOS. The default root under `~/Library/Application Support` is protected by `~/Library` (0700), and Windows `%APPDATA%` has a user-only ACL, so the default case holds through the parent. A `JOBPILOT_HOME` elsewhere (for example `/Users/Shared`) is readable by other local users. Exports (0600) and the recovery file (0600) are set explicitly. | Create the root with mode 0700 on Unix, or say in M-06 that "user-only" is inherited from the default parent folder. | Fixed in `61614b6`. `claim_root` creates the root, and any missing parent, with mode 0700 on Unix. An existing root keeps its mode. Test: `a_new_data_root_is_user_only` (`db.rs`). |
| S-8 | Minor | `docs/security/threat-model.md` | **Register drift.** M-33 cites `no_src_holds_a_bypass`; the test is `no_source_holds_a_bypass` (`crates/core/tests/connector_rules.rs:142`). M-01's capability text is out of date (S-4). M-40 still reads "Planned 1.0" while the updater is built (`e9f163f`) and signing waits on certificates. | Fix the three rows when the 1.0 docs are updated. | Fixed in `5f8a40c`: M-33 test name, M-01 capability text (narrowing is S-4), M-40 "1.0: built; signing pending certificates". M-06 also records the 0700 root. |

No critical finding. Nothing found lets page text, a posting, or model output end a pause, read a secret, run a shell, or upload a file other than the two staged ones.

## 2. Targeted checks

### 2.1 Tauri command surface and capabilities

- 152 commands are registered at `f1c8f55` (`commands.rs:73`); `no_command_returns_a_key_or_recovery_bytes` pins the list against `ALLOWED_COMMANDS`.
- Paths: `resume_import`, `ai_set_claude_path`, `ai_set_cli_path`, `recovery_key_export`, and `recovery_key_import` take the path from a Rust-owned dialog, and the webview names none. `backup_restore` is the exception (S-6).
- URLs: `job_open` and `apply_open` take an id and read the stored link, which must be `https` (`connectors::link_ok`, `permissions::link_allowed`). `ai_set_ollama_url` and `ai_set_endpoint` pass the loopback rules (M-21) before a native dialog.
- Shell: no command takes a shell string. `pty_open` starts only the `jobpilot` sidecar with `shell`; `pty_open_apply` takes a size and a run id that must be `ready` (`pty.rs:383`). `pty_write` input goes to `jobpilot shell`, which parses with `shlex` and clap in-process (`crates/cli/src/shell.rs:36`), allows `claude` with only `--version`, `--help`, `--resume`, or `--continue`, and refuses a nested `shell`.
- Settings: `settings_set` refuses keys under `ai.`, `jobs.`, `recovery_key_exported_`, and `updates.` (`commands.rs:71`), and every security-relevant key found is under one of them. The pause flag is its own table and cannot be set through settings.
- Sealed text: `kb_reveal`, `memory_reveal`, and `memory_confirm_use` ask natively each time; `apply_copy` writes the clipboard from Rust; `apply_start` releases sealed values only through `native::choose`.
- Capabilities: see S-4. The opener, clipboard, updater, and process plugins have no webview permission.

### 2.2 CSP

`default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src ipc: http://ipc.localhost` (`tauri.conf.json:21`). No inline or remote script, no remote image, frame, or connection. `'unsafe-inline'` styles allow CSS injection only if markup were injected, and none can be (no HTML sink in `apps/desktop/src`). Devtools are not enabled in release builds (no `devtools` feature). The terminal uses xterm.js with a no-op `linkHandler` (`TerminalPanel.tsx:16`), and the CLI passes every printed line through `text::sanitize`, which strips CSI, OSC, DCS, and C1 controls (`crates/cli/src/text.rs:8`). Verified.

### 2.3 PTY and shell restrictions

- `refused_in_panel` (`crates/cli/src/main.rs:108`) refuses restore, delete, recovery-key, discard, pause clear, browser setup, apply start, session, Copilot, Autonomous grant and run, `apply-mcp`, every `ai` command that changes what runs, sealed reveals, and every command that takes a path. `every_command_is_classified` fails a new command that takes a path and is not refused. Gaps: `apply resolve` (S-5).
- The panel marker `JOBPILOT_PANEL=1` is set for the shell and left out for an apply host (`pty.rs:303`, `panel_env_leaves_the_marker_out_for_an_apply_host`). A tool-enabled `claude` the owner starts in the panel can clear it; that is documented residual "Terminal".
- Documented residual, unchanged: a `claude` started in the panel is the owner's own session and is not stopped by the pause.

### 2.4 Child environment

- `child::child_env` copies only `BASE_ENV` plus the OS system names and the adapter's extras (`crates/core/src/ai/child.rs:61`), values never logged. Claude adds `CLAUDE_CONFIG_DIR` and `CLAUDE_SECURESTORAGE_CONFIG_DIR` only.
- The apply session child (`apply/session.rs:155`) adds `SHELL`, `LOGNAME`, `USERNAME`, `PATHEXT`, and the two switches that keep `CLAUDE.md` and auto memory out; it is spawned with `env_clear` (`session.rs:247`), as is `claude purge`.
- The PTY child is spawned with `env_clear`, then the Claude list, the terminal names, `JOBPILOT_HOME`, `TERM`, and the marker (`pty.rs:426`). No API key, no `JOBPILOT_*` override, and no `JOBPILOT_SECRET_SERVICE` outside debug builds.
- The Playwright child gets `child_env(&[])` plus the browsers folder (`browser/runtime.rs:219`).
- Pinned by `child_env_is_an_allow_list` (`claude_env.rs`) and `child_env_holds_no_key_and_keeps_memory_out` (`apply_session.rs`), which passed in this review's run, and `the_child_env_is_the_allow_list` (`gemini_cli.rs`, not run here).

### 2.5 Secrets in logs, exports, and audit rows

- No `secret_get` command. Provider keys enter through `ai_key_from_clipboard` (Rust clipboard read behind a native dialog). `secret_set` refuses the reserved `ai-key-` prefix.
- Logging: the product code has 11 `eprintln!` calls, none of which prints a value, a key, a prompt, or page text (searched in `crates/core/src` and `apps/desktop/src-tauri/src`).
- Audit rows: no `audit::record` call puts an error body, an answer, or page text in `detail` (searched). The restore row stores the source path.
- Export: `transfer::export_report` writes every table except BLOB columns (ciphertext, key-checks, vectors) and blanks sealed answer bodies, then writes work authorization and sealed answers to their own 0600 files with a warning, as designed (threat model section 8). `secret_index` holds names only. No key reaches an export.
- Verified by `provider_key_round_trips_and_appears_in_no_row_or_export` (keychain step), `a_key_in_an_error_body_reaches_no_row_message_or_audit`, and `run_rows_hold_no_text_a_step_was_given`.

### 2.6 Backup and restore keep the pause

`keep_pause` (`backup.rs:608`) keeps a live pause whatever the backup says, sets it when the live state could not be read, and `merge_pause` closes the gap between the first read and the lock. The probe for S-1 also set the pause in root B before restoring a backup with none, and the restored database was paused. Restore also revokes every external consent (`backup.rs:673`). What it does not keep is S-1.

### 2.7 The apply-mcp guards

Read in full: `apply/proxy.rs`, the argument and target rules in `apply/guard.rs`, and the session argv.

- The tool list is 3 apply tools plus 13 pinned browser tools filtered by name; any other name is `tool_not_allowed` (`proxy.rs:619`).
- Arguments: `filename` and `isolatedContext` are refused at any depth (`has_key`), `browser_type.submit`, `slowly`, snapshot scoping, keys other than Tab, Shift+Tab, Escape, and non-`https` URLs (`http` on `127.0.0.1` in debug builds only) are refused. Gap: `doubleClick`, `button`, and `modifiers` on `browser_click` (S-2).
- Targets: Playwright accepts "a ref or a unique element selector" as `target`. `target_known` accepts only a ref of the form `e12` or `f1e12` that appears exactly once outside quotes in the last snapshot, so a CSS selector and a ref planted inside an element's name are both refused. Verified by reading `is_ref` and `index_of` (`guard.rs:566`, `:584`).
- Values: `browser_type.text`, `browser_fill_form` textbox and slider values, and `browser_handle_dialog.promptText` pass `value_ok`. A field declared with any other type is judged as an option by `option_guard`, which needs a page role of `combobox` or `listbox`, so a declared type cannot route free text past `value_ok`.
- Uploads: `upload_ok` canonicalizes each path and requires one of the two staged files (`guard.rs:429`); claude has no file tool.
- Pause: every browser call first checks the pause flag and cancellation (`proxy.rs:524`); a held reply blocks the single stdio loop, so no other call runs during a stop.
- Documented residuals, unchanged: navigation to any `https` host with a value in the URL, `browser_handle_dialog` after a Legal stop, choices under Normal questions, and the guard gaps listed in threat model section 11.

### 2.8 Updater (reviewed at `e9f163f`)

- Key pinning: `plugins.updater.pubkey` in `tauri.conf.json` equals the plan's key verbatim, the endpoint is the plan's GitHub URL, and `tauri_plugin_updater::Builder::new()` takes both from config with no runtime override. No webview permission names the updater or process plugin.
- Install blocking: `update_install` refuses with `update_blocked_by_run` before the download and again after it (`update.rs:106`, `:143`), through `install_blocked(paused, active_runs)`; an orphan sweep runs first so a dead host cannot block for ever.
- The launch check is off unless `updates.check_on_launch` is true, and `settings_set` cannot set it; a restore can (S-1), and `update_auto_set` takes no native dialog (S-5).
- The signing key's exposure in CI is S-3.

## 3. FR-12 requirements

| FR | Requirement | Verdict | Where, and the test |
|---|---|---|---|
| FR-12.1 | Sensitive information is encrypted | Verified, with documented residuals | AES-256-GCM per column with row-bound associated data (`crypto.rs`; `ciphertext_has_the_documented_layout`, `swapped_row_id_fails_to_decrypt`). Free-form text is plain unless marked (section 11). |
| FR-12.2 | Secrets in OS credential storage | Verified | `secrets.rs`, `crypto.rs` key entries `data-key-<id>`; no read path to the webview (`no_command_returns_a_key_or_recovery_bytes`). Recovery file is the D5 exception. |
| FR-12.3 | Prevents prompt injection | Verified as designed (no detection claimed), documented residuals | Router `no_tools` rule, marked blocks, deterministic output checks (M-16 to M-20, M-23, M-31, M-34); apply-mode exception under owner decision, checked by the proxy (M-37, M-38). S-2 is a proxy gap. |
| FR-12.4 | Browser processes are isolated | Verified, with documented residual (no egress limit) | Per-run Playwright server and profile, deleted at end (M-35 tests). |
| FR-12.5 | Shell execution is restricted | Verified, with gap | Panel allow-list (M-14), argv arrays and env allow-lists (M-15), no-tools CLIs (M-31). Gap S-1: a restored setting chooses the program that runs. |
| FR-12.6 | Model outputs validated | Verified | `validate_rejects_oversize_and_unknown_fields`, `output_urls_and_commands_are_data_only`, claim and authorization checks (M-16 to M-18). |
| FR-12.7 | Consequential actions audited | Verified | M-05 rows for restore, export, pause, consent, external calls, submissions, pause resolutions, `update_installed`. |
| FR-12.8 | Emergency pause stops all automation | Verified, with gap | DB flag polled at 250 ms, three kill paths for apply runs (M-25 tests). Gap S-4: webview script could disable the menu item; the CLI path stays. |
| FR-12.9 | Backup | Verified, with gap | Restore validates, snapshots, keeps the pause, revokes consent (M-07). Gap S-1. |
| FR-12.10 | Delete | Verified | `delete_all` behind a native dialog and the typed confirm; this root's keys only (M-08 tests). |

## 4. Mitigations M-01 to M-40

"Verified" means the code was read and the named tests exist in the tree; where this review ran them, it says so in section 5. The threat model's own status labels (Built 0.x, Implemented 0.2) and Windows evidence are not re-stated here.

| ID | Verdict | Code | Notes |
|---|---|---|---|
| M-01 | Gap (S-4) | `capabilities/default.json`, `tauri.conf.json:21` | CSP verified; capability set wider than stated. |
| M-02 | Gap (S-6) | `commands.rs` | Every path but `backup_restore`'s comes from a Rust dialog. |
| M-03 | Verified | `commands.rs` `ai_key_from_clipboard`, `secrets.rs` | `no_command_returns_a_key_or_recovery_bytes`. |
| M-04 | Verified | `apps/desktop/src` | No HTML sink; inert text only (searched). |
| M-05 | Verified | `audit.rs` and callers | 6 of the cited tests exist; the other cited names are audit actions, also present. |
| M-06 | Partly (S-7) | `db.rs:146` | Lock and `in_use` verified; user-only inherited from the parent folder. |
| M-07 | Verified, gap S-1 | `backup.rs` | `restore_reports_backup_key_state_first`, pause kept (probe). |
| M-08 | Verified | `transfer.rs` `delete_all` | `delete_all_removes_this_roots_data_keys_only`. |
| M-09 | Documented | UI copy | Not re-checked in a window. |
| M-10 | Verified | `crypto.rs` | Three cited tests exist. |
| M-11 | Verified, documented residual | `knowledge.rs`, `generate.rs` | Free-form text not marked automatically. |
| M-12 | Verified | `recovery.rs:93` (`O_NOFOLLOW`, 0600) | `export_refuses_devices_and_symlinks`, `export_refuses_the_panel_marker`. |
| M-13 | Verified | `commands.rs` `backup_restore`, `data_delete`, `discard_locked_fields` | Native dialog before any change. |
| M-14 | Verified, gap S-5 (`apply resolve`) | `crates/cli/src/main.rs:108`, `shell.rs`, `pty.rs` | All 8 cited tests exist. |
| M-15 | Verified | `ai/child.rs`, `apply/session.rs`, `pty.rs` | 18 of 18 cited tests exist. |
| M-16 | Verified | `ai` validation | Both cited tests exist. |
| M-17 | Verified | `drafts.rs`, `generate.rs` | Three cited tests exist. |
| M-18 | Verified | `jobs`, `generate.rs` | Nine cited tests exist. |
| M-19 | Verified, owner exception | `generate.rs` `OPEN`/`CLOSE`/`neutralise`, `proxy.rs` `wrap` | Brief text outside the markers is a documented residual. |
| M-20 | Verified | `knowledge.rs`, `drafts.rs` | Both cited tests exist. |
| M-21 | Verified | `ai/ollama.rs`, `ai/api.rs` | `tls_version_min` is covered only by the CI handshake step, as documented. |
| M-22 | Verified, documented residual | `parse.rs` | Memory unbounded, accepted. |
| M-23 | Verified (scripted replies) | `crates/core/tests/injection` | Corpus files 01 to 24 exist. |
| M-24 | Verified | `runner` | Six cited tests exist. |
| M-25 | Verified, gap S-4 | `pause.rs`, `menu.rs`, `lib.rs:24` | `pause_set_closes_the_apply_session_before_the_row` and the other 17 cited tests exist. |
| M-26 | Verified | `runner` | Five cited tests exist. |
| M-27 | Verified | `memory`, `commands.rs` `memory_confirm_use`, `apply_start` | Fifteen cited tests exist. |
| M-28 | Verified | `memory`, `profile.rs` | Five cited tests exist. |
| M-29 | Verified | `ai/router.rs` | Thirteen cited tests exist. |
| M-30 | Verified, gap S-1 | `ai/consent.rs`, `backup.rs:673` | Restore revokes consent; it does not restore program paths safely. |
| M-31 | Verified | `ai/claude_code.rs`, `ai/gemini_cli.rs`, `apply/session.rs:123` | Twelve cited tests exist. |
| M-32 | Verified | `ai/child.rs` | By construction and the cited tests. |
| M-33 | Verified, doc drift S-8 | `connectors` | `no_src_holds_a_bypass` is `no_source_holds_a_bypass`. |
| M-34 | Verified | `jobs` normalize | Four cited tests exist. |
| M-35 | Verified, documented residual | `browser/runtime.rs` | No egress limit, by decision 11. |
| M-36 | Verified | `permissions.rs` | Nine cited tests exist. |
| M-37 | Verified | `apply/guard.rs`, `proxy.rs` | Selector targets and declared-type tricks refused (2.7). |
| M-38 | Verified, documented residual | `apply/guard.rs` `scan` | Word lists. |
| M-39 | Gap (S-2) | `proxy.rs:672` | `doubleClick` on a submit control is one counted click. |
| M-40 | Built, pending certificates; gap S-3 | `update.rs` at `e9f163f`, `release.yml` | Updater verifies against the pinned key; installers unsigned until the owner adds certificates. |

## 5. What ran

- Probes (deleted before this commit): `probe_restore_brings_back_a_cli_path_and_an_executable` and `probe_restore_then_startup_sweep_runs_the_planted_program` (S-1, and the pause check in 2.6), and `zz_t5_probe_double_click_on_submit_counts_once` (S-2). All three passed, which means each bypass is real.
- Core suites run on macOS against a temporary root: `apply_guard`, `apply_prep`, `apply_proxy`, `apply_session`, `backup`, `child`, `claude_code`, `claude_env`, `pause`, `permissions`, `recovery`, `recovery_panel`, `router`, `secrets_mock`, and `transfer`: 433 passed, 0 failed, 31 ignored (the `--ignored` keychain and real-CLI tests).
- Not run: the desktop crate's tests (another task had uncommitted changes in `apps/desktop/src-tauri/src`), the `--ignored` keychain and `browser_e2e_*` tests, and Windows. No native dialog, menu, or real window was driven.
