#!/usr/bin/env bash
# The CI gate, run on this Mac. This is the pre-commit gate: Windows is type-checked here,
# and CI's Windows job runs it.
#
#   scripts/ci-local.sh          everything
#   scripts/ci-local.sh --fast   skips the keychain suites and the Tauri build
set -uo pipefail
cd "$(dirname "$0")/.."

fast=false
[ "${1:-}" = "--fast" ] && fast=true

failed=()
step() {
  local name=$1
  shift
  printf '\n==> %s\n' "$name"
  if "$@"; then
    printf 'ok   %s\n' "$name"
  else
    printf 'FAIL %s\n' "$name"
    failed+=("$name")
  fi
}

windows_check() {
  rustup target add x86_64-pc-windows-gnu >/dev/null 2>&1 || return 1
  local stub
  stub=$(mktemp -d)
  printf '#!/bin/sh\nexit 0\n' >"$stub/x86_64-w64-mingw32-gcc"
  printf '#!/bin/sh\nexit 0\n' >"$stub/x86_64-w64-mingw32-windres"
  # The archiver must write a valid empty archive, and no other `ar` may come
  # first on PATH, or the host's SQLite object gets emptied.
  printf '#!/bin/sh\nfor a in "$@"; do case "$a" in *.a) printf "!<arch>\\n" > "$a";; esac; done\nexit 0\n' \
    >"$stub/x86_64-w64-mingw32-ar"
  chmod +x "$stub"/*
  PATH="$stub:$PATH" \
    CC_x86_64_pc_windows_gnu="$stub/x86_64-w64-mingw32-gcc" \
    AR_x86_64_pc_windows_gnu="$stub/x86_64-w64-mingw32-ar" \
    CARGO_TARGET_DIR=target/windows-check \
    cargo clippy --target x86_64-pc-windows-gnu --workspace --all-targets -- -D warnings
}

keychain_suites() {
  # One thread: parallel keychain runs have hung here and shown macOS dialogs.
  JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test --workspace -- --ignored \
    --skip real_claude --skip real_gemini --skip real_tls --skip make_fixtures \
    --skip browser_e2e --skip serve_fixtures --test-threads=1
}

# The real Playwright server and a visible Chromium on the fixture forms. The runtime goes
# under a temporary root, never the data folder.
browser_e2e() {
  local home status
  home=$(mktemp -d)
  cargo build -p jobpilot-cli &&
    JOBPILOT_HOME="$home" JOBPILOT_SECRET_SERVICE=jobpilot-test \
      ./target/debug/jobpilot browser setup &&
    JOBPILOT_E2E_RUNTIME="$home/browser-runtime" JOBPILOT_SECRET_SERVICE=jobpilot-test \
      cargo test -p jobpilot-core --test apply_browser -- --ignored --test-threads=1 \
      --skip serve_fixtures
  status=$?
  rm -rf "$home"
  return $status
}

step 'install' pnpm install --frozen-lockfile
step 'typecheck' pnpm typecheck
step 'frontend tests' pnpm test
step 'format' cargo fmt --check
step 'clippy' cargo clippy --workspace --all-targets -- -D warnings
step 'rust tests' cargo test --workspace --no-fail-fast
step 'windows type-check' windows_check
# Supply-chain gates. cargo-deny is optional locally; CI always runs both.
if command -v cargo-deny >/dev/null 2>&1; then
  step 'cargo deny' cargo deny check
else
  printf '\nskip cargo deny (cargo install cargo-deny --locked)\n'
fi
step 'pnpm audit' pnpm audit --prod --audit-level high
if [ "$fast" = false ]; then
  step 'keychain suites' keychain_suites
  step 'browser end to end' browser_e2e
  step 'sidecar' pnpm --filter desktop sidecar
  step 'tauri build' pnpm tauri build --no-bundle
fi

printf '\n'
if [ ${#failed[@]} -gt 0 ]; then
  printf 'FAILED: %s\n' "${failed[*]}"
  exit 1
fi
printf 'All checks passed on this Mac. Windows was type-checked only.\n'
