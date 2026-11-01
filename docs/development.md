# Development guide

## Prerequisites

- Rust 1.99.1. `rust-toolchain.toml` pins it, so `rustup` installs it on first use. It includes `clippy` and `rustfmt`.
- Node 24 and pnpm 12.16.1 (`packageManager` in `package.json`; enable with `corepack enable`).
- Tauri 2 system requirements: Xcode command line tools on macOS; WebView2 and the MSVC build tools on Windows.

## Setup

```bash
pnpm install --frozen-lockfile
pnpm tauri dev      # run the desktop app with hot reload
```

## Checks

CI runs these on macOS and Windows. Run them before pushing.

```bash
pnpm typecheck
pnpm test
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm tauri build --no-bundle
```

Thirty-two tests touch the real OS keychain and are `#[ignore]`d. Nothing else does: `Db::open` never reaches the keychain, so `cargo test --workspace` writes no entries. Run them against a throwaway service name, never the app's own:

```bash
JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test --workspace -- --ignored
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

1. Add the next file, for example `0002_<name>.sql`. Number sequentially.
2. Never edit a migration that has shipped. SQLx stores a hash of each applied file; a change breaks every existing database. Fix forward with a new file.
3. Keep line endings LF. `.gitattributes` forces `*.sql text eol=lf`, since CRLF changes the hash.
4. Use runtime `sqlx::query`, not `query!`, so builds need no `DATABASE_URL` or `sqlx-cli`.
5. Add a test that opens a database at the previous version and migrates it.
