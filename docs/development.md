# Development guide

## Prerequisites

- Rust 1.99.1. `rust-toolchain.toml` pins it, so `rustup` installs it on first use. It includes `clippy` and `rustfmt`.
- Node 24 and pnpm 12.16.1 (`packageManager` in `package.json`; enable with `corepack enable`). The browser runtime (release 0.6) runs on the Node it finds, and needs 20 or later: the strictest `engines.node` in its lockfile (`NODE_MIN`). A Node that does not answer `--version` reads as too old, with its version shown as `unknown`.
- Tauri 2 system requirements: Xcode command line tools on macOS; WebView2 and the MSVC build tools on Windows.

## Setup

```bash
pnpm install --frozen-lockfile
cargo build -p jobpilot-cli   # the terminal panel runs the CLI; tauri dev does not build it
pnpm tauri dev      # run the desktop app with hot reload
```

### The CLI sidecar

The installer bundles the `jobpilot` CLI as a Tauri sidecar (`externalBin: binaries/jobpilot`), and the terminal panel runs it. `pnpm --filter desktop sidecar` builds it in release mode and copies it to `apps/desktop/src-tauri/binaries/jobpilot-<target triple>`. `pnpm tauri build` runs the script through `beforeBuildCommand`, and with `--target <triple>` it passes the triple in `TAURI_ENV_TARGET_TRIPLE`, which the script honours. CI runs the script as its own step, before clippy, because the desktop crate's build script expects the file. `binaries/` is not committed.

## Checks

CI runs these on macOS and Windows. `scripts/ci-local.sh` runs all of them on a Mac before pushing; it type-checks Windows but cannot link or run it. `--fast` skips the keychain suites and the Tauri build. The workflow also runs a `TLS handshake` step on both systems, described below.

```bash
pnpm typecheck
pnpm test
pnpm --filter desktop sidecar
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm tauri build --no-bundle
```

Tests that touch the real OS keychain are `#[ignore]`d. Nothing else does: `Db::open` never reaches the keychain, so `cargo test --workspace` writes no entries. Run them against a throwaway service name, never the app's own, and never with `--release`, and not while another agent or process is running them (parallel runs hang and can raise macOS dialogs):

```bash
JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test --workspace -- --ignored --skip real_claude --skip real_gemini --skip real_tls --skip make_fixtures --skip browser_e2e --skip serve_fixtures
```

The ignored tests live in `crates/core/tests/` (`crypto`, `recovery`, `profile`, `knowledge`, `memory`, `memory_use`, `drafts`, `backup`, `transfer`, `secrets_store`, `secrets_locked`, `embed`, `parse`, `injection`, `claude_code`, `gemini_cli`, `net`), in `crates/cli/tests/cli.rs`, and in `apps/desktop/src-tauri/src/commands.rs`. Run them one process at a time, with `--test-threads=1`, on a Mac with the login keychain unlocked, because a second process, or a locked keychain, can hang or show a system dialog:

```bash
JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test --workspace -- --ignored --test-threads=1 --skip real_claude --skip real_gemini --skip real_tls --skip make_fixtures --skip browser_e2e --skip serve_fixtures
```

Release 0.3 adds sealed answers (`memory_answers.body_enc`), so `memory`, `memory_use`, and `crypto` hold the tests that touch the keychain for them. Read paths never ask it: `plain_work_makes_no_keychain_call` in `crypto.rs` is the one test that may read the call count.

`real_claude_has_no_tools_smoke`, `real_gemini_has_no_tools_smoke`, `real_tls_handshake`, and `make_fixtures` are also ignored, and are skipped by name (`real_claude`, `real_gemini`, `real_tls`, `make_fixtures`), so CI never calls the real `claude` or `gemini` or rewrites the committed resume fixtures, and the keychain steps never open the TLS check. The `browser_e2e_*` tests and `serve_fixtures` are skipped by name too: they have their own step, below. `locked_keychain_is_unavailable_not_missing` swaps the default macOS keychain for a throwaway one, so it runs only with `JOBPILOT_ALLOW_KEYCHAIN_SWAP=1` and is not run in CI.

### The browser runtime (release 0.6)

The apply modes drive Chromium through the Playwright MCP server, which is not bundled. `jobpilot browser setup` installs it into `<root>/browser-runtime`: it finds Node and npm (`JOBPILOT_NODE_BIN` in a debug build only, then `PATH`, the usual folders, then one login-shell lookup), runs `npm ci --ignore-scripts` on the committed `crates/core/assets/browser-runtime/package.json` and `package-lock.json`, then `node node_modules/playwright/cli.js install chromium` with `PLAYWRIGHT_BROWSERS_PATH` inside that folder. `jobpilot browser status` reads the state: `no_node`, `node_too_old`, `not_installed`, `wrong_version`, or `ready`. Setup is refused while the emergency pause is on, and in the terminal panel. Backup and export skip `browser-runtime/` and `apply/`; delete-all removes them. Never run either command without a temporary `JOBPILOT_HOME`.

**Fakes.** No test starts the real `claude`. `crates/core/tests/mock-claude-apply/claude` (Node, with a `claude.cmd` twin) is a scripted fake: it starts the `jobpilot` server named in its `--mcp-config` (the real `jobpilot apply-mcp`), runs the steps of `JOBPILOT_FAKE_SCENARIO` (`{"tool", "args"}`, `{"sleep_ms"}`, `{"exit"}`), and appends every reply to `JOBPILOT_FAKE_REPLIES`, so a test can check what the proxy refused. It records `purge` calls too, and ignores SIGTERM and SIGHUP, as a stubborn session would. In step arguments, `"@ref:<name>"` resolves to the ref of the control with exactly that name in the last snapshot, and `"@brief:<path>"` reads a field of the last `apply_next` reply. `crates/core/tests/mock-mcp/server.mjs` is a fake Playwright MCP server on stdio: it answers `tools/list` with a fixed list, serves the snapshot in `MOCK_SNAPSHOT` (read on every call), and has hooks for the edge cases (`MOCK_EXIT_AFTER`, `MOCK_BIG`, `MOCK_NOISE`, `MOCK_AFTER_FILL`, `MOCK_CLICK_REF`, `MOCK_AFTER_CLICK`, `MOCK_DIE_ON_CLICK`, `MOCK_LATE`, `MOCK_LATE_AT`, `MOCK_START_DELAY_MS`, `MOCK_CALLS`, which also logs each start, and a `slow` call that never answers). The proxy lists its browser tools from `crates/core/assets/playwright_mcp_tools.json`, pinned to `@playwright/mcp` 0.0.84, and starts the server on the first browser call; `browser_e2e_pinned_tools_match_the_real_server` checks the copy against the real server. The proxy, session, Copilot, and Autonomous suites (`apply_proxy`, `apply_session`, `copilot`, `autonomous`) use these; `apply_session` needs the sidecar built, which `cargo test --workspace` does.

**Bumping the pin.** The pin is `@playwright/mcp` 0.0.84 with `playwright` `1.65.0-alpha-2026-10-30`.
1. Change the version in `crates/core/assets/browser-runtime/package.json`, regenerate the lock with `npm install --package-lock-only` in that folder, and update `PLAYWRIGHT_MCP`, `PLAYWRIGHT`, and, if the lock's strictest `engines.node` moved, `NODE_MIN` in `crates/core/src/browser/runtime.rs`. `the_pin_matches_the_committed_lock` and `the_node_floor_is_the_strictest_engine_in_the_lock` check them.
2. Re-run the `tools/list` check against the new server: run `jobpilot browser setup` on a temporary root, start `node <root>/browser-runtime/node_modules/@playwright/mcp/cli.js --config <a playwright.json>`, send `initialize` and `tools/list` as newline JSON, and compare the names and input schemas with the 13 tools in `guard::BROWSER_TOOLS`. A new tool stays out by itself; a changed argument (a new path or code argument on a forwarded tool) needs a guard first. Check that the server still adds `--disable-blink-features=AutomationControlled` only when no `--disable-blink-features` argument is given.
3. Run the `browser_e2e_*` suite below.
4. Update the Vendor facts of the release plan and the threat model's pin residual with what you read and the date.

### Apply mode in a real browser (release 0.6)

`crates/core/tests/apply_browser.rs` runs apply mode end to end: the scripted fake `claude` in `crates/core/tests/mock-claude-apply` drives the real `jobpilot apply-mcp`, the pinned Playwright server, and a real, visible Chromium. Every test is `#[ignore]`d, named `browser_e2e_*`, and needs `JOBPILOT_E2E_RUNTIME`: the `browser-runtime` folder of a temporary root that `jobpilot browser setup` filled. Each test links its own root's `browser-runtime` to it, so the download happens once. Never point it at your own data folder.

```bash
cargo build -p jobpilot-cli
export JOBPILOT_HOME=$(mktemp -d) JOBPILOT_SECRET_SERVICE=jobpilot-test
./target/debug/jobpilot browser setup
JOBPILOT_E2E_RUNTIME=$JOBPILOT_HOME/browser-runtime cargo test -p jobpilot-core --test apply_browser -- --ignored --test-threads=1 --skip serve_fixtures
```

CI runs this as its `Browser end to end` step on macOS and Windows, and `scripts/ci-local.sh` runs it in full mode. A Chromium window opens for each test; leave it alone. `serve_fixtures` needs no runtime: it serves the forms on `127.0.0.1` until killed and prints the port, for the real `claude` check of Task 14 (`--nocapture` shows the port).

**Fixtures.** `crates/core/tests/fixtures/forms/*.html` are the forms: one page in the shape of each site apply mode meets (`greenhouse`, `lever`, `ashby`, `workday`, `indeed`, `linkedin`, `company`, `two_pages`), one per mandatory stop (`captcha`, `mfa`, `identity`, `legal`, `login`), `after_click` (a stop that shows only after a click), and `injection`, `fake_success`, and `reject_once`. `linkedin` and `indeed` hold the form in a modal dialog, `greenhouse` has a split name, starred labels, and files behind `Attach` buttons, and `workday` has a button-and-listbox dropdown. They hold synthetic text: invented company names, no logo, no brand asset, and no capture of a real page. A page in the shape of a site imitates its layout only. A fixture's name is part of its address, which the stop scan reads, so a name holds no stop word. Every form's button reads `Submit application`, so a run records exactly one submit click, and each form posts to `/form`, which `fake_http` answers with a redirect to its own `/done` page.

**No test reaches a site.** `fake_http` serves the fixtures on `127.0.0.1`, the scripts navigate only there, and the hostile page's addresses (`file:`, a non-loopback `http`) are refused before any request. A test never starts the real `claude`.

### Job connectors (release 0.5)

**Fixtures.** `crates/core/tests/fixtures/connectors` holds the recorded shapes of the five read sources: `lever.json`, `remoteok.json`, `wwr.rss`, `greenhouse.json`, `ashby.json`, plus `lever_empty.json`, `lever_changed.json`, and `challenge.html` for the refusal cases. They are synthetic. Each holds invented postings in the shape the source sends, and never a real company's posting. Add no capture of a live source to the repository. Each source fixture carries one hidden `display:none` line, which the injection tests use.

**No test or CI step reaches a job source.** Every connector test uses a fixture, a pure parser, or a fake on `127.0.0.1` (`crates/core/tests/fake_http`). `Fetcher::for_test` accepts a loopback address only. `only_fetch_rs_touches_the_network` fails if any file but `fetch.rs` names a network type. The CI `TLS handshake` step contacts the four AI vendor hosts, and no job source. The live check of the five sources is a controller task for each release, run by hand on a temporary root.

**CLI.** `job` commands are in `crates/cli/src/jobs.rs`. To try them without touching your data, use a debug build with a temporary root and a test keychain service:

```bash
E=$(mktemp -d); export JOBPILOT_HOME="$E/home" JOBPILOT_SECRET_SERVICE=jobpilot-test
cargo run -p jobpilot-cli -- job sources
```

A release build ignores `JOBPILOT_SECRET_SERVICE`, so use a debug build for anything that touches the keychain. The `job show` JSON shape is in [architecture](architecture.md#62-cratescli-jobpilot).

### Claude Code

The headless adapter and the panel's `claude` launcher use the `claude` you installed. Resolution order: `JOBPILOT_CLAUDE_BIN` (an absolute path), the `ai.claude_path` setting, `PATH`, the usual install folders, then one lookup through your login shell. The adapter and launcher run it with your own login; JobPilot never reads its credentials. The tests use a mock `claude` (`crates/core/tests/mock-claude`) and need no login. To run the real smoke once, which spends a little of your subscription:

```bash
JOBPILOT_REAL_CLAUDE_SMOKE=1 cargo test -p jobpilot-core --test claude_code -- --ignored real_claude
```

### External AI (release 0.4)

**TLS backend.** The hosted client uses the operating system's TLS (`native-tls-no-alpn`): Schannel on Windows, Security.framework on macOS. No `rustls`, `aws-lc-sys`, or `ring` is in `Cargo.lock` (`only_native_tls_is_linked`), which keeps the stub-tool Windows type-check working. macOS native TLS tops out at TLS 1.2. `tls_version_min` has no local test: a handshake needs a certificate the OS trusts, so only `real_tls_handshake` covers it.

**Fakes.** Every network test talks to a local fake on `127.0.0.1` (`crates/core/tests/fake_http`, built on `std::net::TcpListener`, as `ollama.rs` does) or to a mock CLI script (`crates/core/tests/mock-claude`, `mock-gemini`, `mock-codex`, each with a `.cmd` twin that `.gitattributes` keeps CRLF). No test sends a real request or starts a real vendor CLI. The `ai_status` tests that reach an installed `claude --version` through `find_binary` are the one old exception; add no new path like it.

**Real runs, never from `cargo test --workspace`:**

```bash
# The TLS handshake: an unauthenticated GET to the four vendor hosts, any status passes. No key, no body, no cost.
JOBPILOT_REAL_TLS=1 cargo test -p jobpilot-core --test net -- --ignored real_tls_handshake

# The real Gemini CLI, once, on the owner's sign-in. Spends a little of it.
JOBPILOT_REAL_GEMINI_SMOKE=1 cargo test -p jobpilot-core --test gemini_cli -- --ignored real_gemini
```

CI runs the first as the `TLS handshake` step on both systems and skips both by name in the keychain steps. `real_gemini_has_no_tools_smoke` re-checks what a mock cannot: that the deny policy leaves the model no tool, the stream shape, exit 41, and the `\@` escape.

**Pointing at a CLI.** `JOBPILOT_GEMINI_BIN` and `JOBPILOT_CODEX_BIN` (an absolute path) beat the `ai.gemini_path` and `ai.codex_path` settings, `PATH`, and the usual install folders. The mock-CLI tests use them. The `jobpilot ai cli-path <gemini_cli|codex_cli> <path>` command and the Choose button set the settings.

**Keys in tests.** Providers under test take `KeySource::Fixed`. The `--ignored` keychain suites use `JOBPILOT_SECRET_SERVICE=jobpilot-test`, which only a debug or test build honours: a release binary always uses the real service. Run keychain work and any app run through a debug build.

### Type-checking the Windows build on a Mac

CI is the only place Windows code links and runs. To catch `cfg(windows)` type errors and clippy lints before pushing, check the `x86_64-pc-windows-gnu` target with stub C tools, because the bundled SQLite needs a MinGW compiler you do not have. This type-checks only: nothing links or runs, and `msvc`-only cfgs are not covered.

```bash
rustup target add x86_64-pc-windows-gnu
STUB=$(mktemp -d)
printf '#!/bin/sh\nexit 0\n' > "$STUB/x86_64-w64-mingw32-gcc"
printf '#!/bin/sh\nexit 0\n' > "$STUB/x86_64-w64-mingw32-windres"
# The stub archiver must write a valid empty archive, and no other `ar` may be first on PATH,
# or the host's SQLite object gets emptied.
printf '#!/bin/sh\nfor a in "$@"; do case "$a" in *.a) printf "!<arch>\\n" > "$a";; esac; done\nexit 0\n' > "$STUB/x86_64-w64-mingw32-ar"
chmod +x "$STUB"/*
PATH="$STUB:$PATH" CC_x86_64_pc_windows_gnu="$STUB/x86_64-w64-mingw32-gcc" \
  AR_x86_64_pc_windows_gnu="$STUB/x86_64-w64-mingw32-ar" \
  CARGO_TARGET_DIR="$STUB/target" \
  cargo clippy --target x86_64-pc-windows-gnu --workspace --all-targets -- -D warnings
```


## Releasing

A release is built by `.github/workflows/release.yml` (GitHub Actions on macOS and Windows; every action is pinned to a commit SHA).

1. Bump the version in `Cargo.toml` (workspace), `apps/desktop/package.json`, and `apps/desktop/src-tauri/tauri.conf.json`, run `cargo update -w`, and write `docs/releases/<version>.md` and the changelog entry.
2. Check the build without releasing: run the workflow by hand (`workflow_dispatch`). It accepts only `dry_run: true`, builds the installers, and creates no release.
3. Push a tag `v<version>`. The workflow builds a **draft** release with the `.dmg` (arm64 and x64), the `.msi`, the NSIS `.exe`, the updater `.sig` files, and `latest.json`. Read the draft, then publish it by hand.

Signing is by what exists. macOS is signed and notarized when the `APPLE_*` secrets exist, Windows is Authenticode-signed when `WINDOWS_CERTIFICATE` exists, and anything else ships unsigned. The release body is one neutral text for every platform (the platform jobs share one release), so check the draft's installers, not the body, for what is signed. A macOS build with an Apple certificate but no `APPLE_ID` is signed and not notarized.

**Repository secrets**

| Secret | Used for |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Signs the updater artifacts (minisign). Required for `latest.json` and the `.sig` files. |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` | Developer ID signing of the macOS build. |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | Notarization. |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Authenticode signing of the Windows installers. |

`GITHUB_TOKEN` is supplied by Actions. The updater key is visible to the build step (see S-3 in the [security review](security/review-1.0.md)).

**Rotating the updater key**

1. Generate a new pair: `pnpm tauri signer generate`.
2. Replace `plugins.updater.pubkey` in `apps/desktop/src-tauri/tauri.conf.json` with the new public key.
3. Set the repository secrets `TAURI_SIGNING_PRIVATE_KEY` (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) to the new private key.
4. An installed copy verifies an update only against the key it ships with. A build signed with the new key cannot be installed by an older copy. Rotation therefore needs a **bridge release**: sign one release with the OLD key whose `tauri.conf.json` already carries the NEW public key, publish it, and sign later releases with the new key. Users who skip the bridge must install by hand.

**Supply chain.** `deny.toml` configures `cargo-deny` (advisories, licenses, bans, sources). The `supply-chain` CI job runs `cargo-deny` and `pnpm audit --prod --audit-level high`; both must pass.

**Upgrade tests.** The core tests build a database at each earlier release's schema (1, 5, 11, 12, 15, and 17) from the current migrations up to that version and check that it migrates to the current schema. When a release changes the schema, add its version to the list in `every_earlier_release_upgrades_to_1_0`.

## Where data lives

All files live under one root: `JOBPILOT_HOME` if set, else the platform data directory for `io.github.gvsrusa.jobpilot`.

- JobPilot writes a `.jobpilot-root` marker in the root. It refuses to write to or delete a folder that lacks the marker, so a mistaken root is never damaged.
- A GUI launched from Finder or the Start menu does not inherit a `JOBPILOT_HOME` set in your shell, so it uses the platform data directory.
- `JOBPILOT_HOME` must point to a new (nonexistent or empty) folder. An existing folder with other files is rejected.

```bash
JOBPILOT_HOME=/tmp/jobpilot-dev cargo run -p jobpilot-cli -- status
```

## Adding a migration

Migrations live in `crates/core/migrations/` and run through `sqlx::migrate!`.

1. Add the next file, for example `0006_<name>.sql` (the last shipped is `0017`, schema version 17). Number sequentially.
2. Never edit a migration that has shipped. SQLx stores a hash of each applied file; a change breaks every existing database. Fix forward with a new file.
3. Keep line endings LF. `.gitattributes` forces `*.sql text eol=lf`, since CRLF changes the hash.
4. Use runtime `sqlx::query`, not `query!`, so builds need no `DATABASE_URL` or `sqlx-cli`.
5. Add a test that opens a database at the previous version and migrates it.
