# JobPilot AI

JobPilot AI is a personal, non-commercial, open-source, local-first desktop application for international job seekers. The long-term goal covers the job-search lifecycle: discover, analyze, match, personalize, apply, track, improve. This is one person's project, not a product with support or a roadmap guarantee.

Your data stays on your machine. There are no mandatory API keys, no API credits, no hosted backend, and no proprietary cloud account. The only network use in 0.2 is a local Ollama server on this machine and the `claude` program you already have installed, and only when you turn them on.

## What works today (0.2)

Release 0.1 is the foundation. Release 0.2 adds your candidate profile and a first resume tool. The 0.2 build is a candidate: its tests pass, but several things were never run by a person on Windows (see [release notes](docs/releases/0.2.0.md)).

- Desktop app with status, documents, data, recovery, profile, resumes, and AI views, and a terminal panel (Ctrl+` shows it, F6 leaves it).
- Local SQLite database with versioned migrations, document storage, and full-text search.
- Backup, restore, export, and delete-all of your data, with an audit log.
- A profile with several countries, cities, currencies, languages, and work modes. Work authorization is stored as you state it, encrypted, and never inferred. A recovery key file lets you reopen encrypted fields if your keychain is lost.
- Resume import from PDF and DOCX into a knowledge base; resumes and cover letters for a pasted posting, with each line tied to your own evidence. A line without evidence is flagged and blocks export. Export to DOCX and PDF.
- AI is optional and every feature works with none. Backends: Ollama on this machine (loopback only; you start any download), and Claude Code through the official `claude` CLI on your own login, which gets job text only with every tool turned off and only after you consent. No-AI is always last.
- The `jobpilot` command line (`status`, `doc`, `backup`, `data`, `profile`, `prefs`, `kb`, `job`, `resume`, `letter`, `ai`), bundled with the app and also usable in a normal terminal.
- OS keychain access for secrets. Secret values never reach the webview, logs, or exports.

Claude Code note: JobPilot starts your installed `claude` and never reads or stores its login. Anthropic's consumer terms may allow training on your content unless you opt out; the consent screen says so. The owner of this project has waived subscription-policy concerns for personal use and accepts that risk. You should decide that for yourself.

Not built yet: job discovery, matching, career memory, application automation, other AI providers. See [milestones](docs/milestones.md).

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
- [Release notes 0.2.0](docs/releases/0.2.0.md)
- [AI providers](docs/ai-providers.md) and [threat model](docs/security/threat-model.md)
- [Changelog](CHANGELOG.md)

## Stack

Tauri 2, Rust, SQLite with SQLx and FTS5, React, TypeScript, Vite, Tailwind CSS, shadcn/ui, Zustand.

## License

Apache-2.0. See [LICENSE](LICENSE).
