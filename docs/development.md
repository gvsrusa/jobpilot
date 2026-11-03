# Development guide

## Prerequisites

- Rust 1.99.1. `rust-toolchain.toml` pins it, so `rustup` installs it on first use. It includes `clippy` and `rustfmt`.
- Node 24 and pnpm 12.16.1 (`packageManager` in `package.json`; enable with `corepack enable`).
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

1. Add the next file, for example `0006_<name>.sql` (the last shipped is `0015`, schema version 15). Number sequentially.
2. Never edit a migration that has shipped. SQLx stores a hash of each applied file; a change breaks every existing database. Fix forward with a new file.
3. Keep line endings LF. `.gitattributes` forces `*.sql text eol=lf`, since CRLF changes the hash.
4. Use runtime `sqlx::query`, not `query!`, so builds need no `DATABASE_URL` or `sqlx-cli`.
5. Add a test that opens a database at the previous version and migrates it.
