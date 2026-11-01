# Contributing

JobPilot AI is a personal project. Issues and pull requests are welcome, but there is no promise of a response time or of acceptance.

## Branches and pull requests

- Branch from `main`: `feat/<topic>`, `fix/<topic>`, or `docs/<topic>`. Never push to `main` directly.
- Keep a pull request to one change. Use conventional commit messages (`feat:`, `fix:`, `docs:`, `test:`, `chore:`).
- CI must pass on both macOS and Windows. Do not skip or delete tests to reach green; a Windows failure gets its own fix.
- Run the checks in [docs/development.md](docs/development.md) before opening the PR.
- Never claim a feature works until its test has passed on both operating systems.

## Requirement traceability

Every requirement in the PRD has an ID such as `FR-12`. Each pull request names the `FR-` IDs it implements or changes in its description, and each test that proves one references the ID in its name or a comment. A change with no `FR-` ID needs a sentence explaining why.

## Ground rules

- No mandatory API keys, hosted backend, or network call for local features.
- Secret values never cross IPC to the webview and never reach logs, exports, or the audit log.
- Consequential actions (backup, restore, export, delete, secret change) write an audit row.
- No unauthorized scraping, CAPTCHA or MFA bypass, or access-control circumvention.
- No abstraction without a second caller. Mark a deliberate limit with a `ponytail:` comment naming the ceiling.

By contributing you agree your work is licensed under Apache-2.0.
