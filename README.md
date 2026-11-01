# JobPilot AI

JobPilot AI is a personal, non-commercial, open-source, local-first desktop application for international job seekers. The long-term goal covers the job-search lifecycle: discover, analyze, match, personalize, apply, track, improve. This is one person's project, not a product with support or a roadmap guarantee.

Your data stays on your machine. There are no mandatory API keys, no API credits, no hosted backend, and no proprietary cloud account. No network call exists in the 0.1 code.

## What works today (0.1)

Only the foundation exists. Nothing job-specific is built yet.

- Desktop app with status, documents, data, and recovery views. Tests pass in CI on macOS and Windows. The installers are not yet tested.
- Local SQLite database with versioned migrations, document storage, and full-text search.
- Backup, restore, export, and delete-all of your data, with an audit log.
- The `jobpilot` command line: `status`, `doc`, `backup`, and `data` commands.
- OS keychain access for secrets. Secret values never reach the webview, logs, or exports.

Not built yet: job discovery, matching, resume tools, AI providers, application automation. See [milestones](docs/milestones.md).

## Installing (unsigned)

0.x installers are not code-signed with a paid certificate (Decision D3). Expect OS warnings.

- macOS: the app is ad-hoc signed. Gatekeeper will refuse the first launch. Right-click the app, choose Open, then confirm. Each 0.x build has a different code identity, so keychain prompts or denials can appear after an upgrade.
- Windows: SmartScreen shows "Windows protected your PC". Choose More info, then Run anyway.

Installers come from the draft GitHub Releases of this repository. Paid signing is planned for 1.0.

## Building

See [docs/development.md](docs/development.md) and [CONTRIBUTING.md](CONTRIBUTING.md).

## Documents

- [Master spec](docs/superpowers/specs/2026-11-01-jobpilot-master-spec.md)
- [Architecture and foundation plan](docs/superpowers/plans/2026-11-01-jobpilot-architecture-and-foundation.md)
- [PRD](docs/prd.md)
- [Milestones](docs/milestones.md)
- [Connector SDK (stub)](docs/connector-sdk.md)
- [Changelog](CHANGELOG.md)

## Stack

Tauri 2, Rust, SQLite with SQLx and FTS5, React, TypeScript, Vite, Tailwind CSS, shadcn/ui, Zustand.

## License

Apache-2.0. See [LICENSE](LICENSE).
