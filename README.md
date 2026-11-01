# JobPilot AI

JobPilot AI is an open-source, local-first desktop application for international job seekers. It covers the job-search lifecycle: discover, analyze, match, personalize, apply, track, improve.

Your data stays on your machine. There are no mandatory API keys, no API credits, no hosted backend, and no proprietary cloud account. You choose which AI provider runs, if any, and what automation is allowed.

## Status

Pre-code. The repository currently holds the governing spec and the architecture and foundation plan. No application code exists yet. Nothing here is implemented or tested.

## Planned stack

Tauri 2, Rust, SQLite with SQLx and FTS5, React, TypeScript, Vite, Tailwind CSS, shadcn/ui, Zustand, TanStack Query, Playwright, xterm.js. Targets: macOS (arm64, x86_64) and Windows x64.

## Documents

- [Master spec](docs/superpowers/specs/2026-11-01-jobpilot-master-spec.md)
- [Architecture and foundation plan](docs/superpowers/plans/2026-11-01-jobpilot-architecture-and-foundation.md)
- [PRD](docs/prd.md) (added on the `docs/architecture` branch)
- [Milestones](docs/milestones.md) (added on the `docs/architecture` branch)

## License

Apache-2.0. See [LICENSE](LICENSE).
