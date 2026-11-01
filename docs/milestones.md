# JobPilot AI: Milestone Plan

Status: draft for owner approval. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) and [PRD](prd.md). Requirement IDs are `FR-<section>.<n>` from the PRD. Nothing here is implemented yet.

## Process

Work ships in independently testable releases. A feature is not called implemented until its test has passed. CI on macOS and Windows is the evidence for both platforms.

### Pre-code deliverables

The spec requires these before implementation starts: complete PRD, architecture diagrams, database ER diagram, repository structure, component architecture, AI provider integration specification, security threat model, agent workflow specification, UI wireframes, and the milestone plan (this document). They belong to the `docs/architecture` work and need owner approval.

### Milestone gates

Every release must clear all five gates before it is declared done:

1. Build: the application builds on macOS and Windows.
2. Tests: the full test suite passes on both operating systems.
3. Migrations: migrations apply cleanly to a fresh database and upgrade an existing one.
4. Windows and macOS: compatibility is tested on both.
5. Docs and risks report: documentation is updated, and completed work and outstanding risks are reported.

## Spec section coverage

| Spec section | Topic | Releases |
|---|---|---|
| 1 | Desktop application | 0.1 (signed in 1.0) |
| 2 | Local-first infrastructure | 0.1, 0.2, 0.3 |
| 3 | AI architecture | 0.1, 0.2, 0.4, 0.5, 0.6 |
| 4 | International candidate profile | 0.2 |
| 5 | Job discovery | 0.5 |
| 6 | Job intelligence | 0.5 |
| 7 | Resume and cover letter engine | 0.2 |
| 8 | Career memory | 0.3 |
| 9 | Application automation | 0.6 |
| 10 | Terminal and command interface | 0.1, 0.2, 0.3, 0.5 |
| 11 | Agent orchestration | 0.3, 0.5, 0.6 |
| 12 | Security | 0.1, 0.2, 0.3, 0.4, 0.6 (reviewed in 1.0) |
| 13 | Open-source engineering | 0.1, 0.5, 1.0 |

## Release 0.1: Desktop foundation

Scope: Tauri shell, core database, FTS5, settings, documents, backup, export, delete, OS keychain, CLI skeleton, CI, unsigned installers.

Requirements: FR-1.1 to FR-1.8, FR-2.1 to FR-2.3, FR-2.5 to FR-2.9, FR-2.11, FR-3.21, FR-3.23, FR-10.13, FR-12.2, FR-12.7, FR-12.9, FR-12.10, FR-13.1 to FR-13.4, FR-13.6 to FR-13.9, FR-13.12, FR-13.13.

Exit tests:
- App launches on macOS (arm64, x86_64) and Windows x64 from the unsigned installers.
- Migrations apply to a fresh database; the database is created at the expected path.
- FTS5 search returns results over stored documents.
- Settings and documents persist across restarts.
- Backup then restore reproduces the data; export writes all user data; delete-all removes it.
- Secrets round-trip through the OS keychain and appear in no log, export, or audit row.
- Backup, restore, export, delete, and secret changes write audit rows, except delete-all, which removes the log.
- The CLI skeleton runs and calls the same services as the GUI.
- Every 0.1 feature works with the network disabled, and 0.1 code makes no network call.
- CI is green on macOS and Windows.

Gates: all five apply. Build means the unsigned installers on macOS and Windows; migrations means the initial core schema on a fresh database; Windows and macOS means CI green on both.
## Release 0.2: Candidate and local AI

Scope: profile, field encryption, resume parsing and generation, Ollama, task router, embeddings, xterm.js PTY panel, CLI sidecar bundled in the installer.

Requirements: FR-2.4, FR-2.10, FR-2.13, FR-3.1 to FR-3.8, FR-3.24 to FR-3.29, FR-4.1 to FR-4.10, FR-7.1 to FR-7.12, FR-10.1 to FR-10.4, FR-10.7, FR-10.8, FR-12.1, FR-12.5, FR-12.6.

Exit tests:
- Profile stores multiple countries, regions, cities, currencies, time zones, languages, and work-mode preferences; no code path infers work authorization.
- Sensitive profile fields are encrypted at rest.
- PDF and DOCX resumes parse into the knowledge base.
- Generated resumes and cover letters export to DOCX and PDF, are versioned, compare against the original, and carry claim-level provenance; no claim lacks evidence.
- Ollama discovery, hardware detection, user-initiated download, structured output, and embeddings work against a local model.
- The task router validates outputs against JSON schemas, enforces timeouts and retries, and handles fallback when a backend fails.
- The PTY panel runs the bundled CLI; restricted shell execution rejects disallowed commands.
- Offline operation passes with a local model installed.

Gates: all five apply. Build includes the bundled CLI sidecar; migrations add profile and encrypted-field tables on top of 0.1 data; Windows and macOS means PTY, keychain, and Ollama paths pass on both.
## Release 0.3: Career memory and application tracking

Scope: career memory, application tracking, deterministic workflow runner, execution logs, emergency pause.

Requirements: FR-2.12, FR-8.1 to FR-8.8, FR-10.9 to FR-10.12, FR-11.3, FR-11.4, FR-11.5, FR-11.7, FR-11.9, FR-11.10 to FR-11.16, FR-12.8.

Exit tests:
- Question and answer banks support semantic retrieval, approval, provenance, freshness, and sensitivity classification.
- Sensitive or legally consequential answers cannot be used without explicit confirmation.
- Application history and status are viewable and editable offline (FR-2.12).
- The workflow runner executes deterministic workflows with restricted permissions, logs, timeouts, retries, checkpoints, and cancellation.
- Emergency pause stops every running workflow and is itself audited.
- The resume, cover letter, career memory, tracking, and analytics agents (FR-11.3, 11.4, 11.5, 11.7, 11.9) run as deterministic workflows on the runner. The discovery, matching, and research agents land in 0.5, and the browser application agent in 0.6, each with its feature.
- CLI memory search, application tracking, analytics, and agent monitoring commands call the same services as the GUI.

Gates: all five apply. Build includes the workflow runner; migrations add memory, tracking, and execution-log tables and upgrade a 0.2 database; Windows and macOS means pause and cancellation work on both.
## Release 0.4: External AI backends

Scope: external AI backends, API providers, consent UI. The design of the subscription-CLI integration depends on Decision D1, made in Task A6. This plan does not assume D1's outcome, and 0.4 scope for the subscription backends stays conditional on it.

Requirements: FR-3.9 to FR-3.19 (subscription items conditional on D1), FR-3.30, FR-12.3.

Exit tests:
- API providers (OpenAI, Anthropic, Google, OpenRouter, compatible endpoints) run through the task router with capability declarations, schema validation, timeouts, retries, and fallback.
- Consent UI blocks any external call until the user gives explicit data-sharing consent, and shows what will be sent.
- Prompt-injection tests pass for untrusted text sent to external backends.
- Subscription backends: scope and tests defined after D1; the terms review for each CLI is recorded before any of them ships.
- No mandatory key: the app still works end to end with no provider configured.

Gates: all five apply. Build includes the provider and consent code; migrations add provider settings and consent records; Windows and macOS means keychain-stored keys and CLI detection work on both.
## Release 0.5: International job discovery and intelligence

Scope: connector SDK, read-only discovery, job intelligence (spec section 6).

Requirements: FR-3.20, FR-5.1 to FR-5.18, FR-6.1 to FR-6.12, FR-10.5, FR-10.6, FR-11.1, FR-11.2, FR-11.8, FR-13.5.

Exit tests:
- The connector SDK defines operations, automation capabilities, rate limits, and policy-review status; connectors cannot register without them.
- Read-only connectors for the sources in FR-5.2 to FR-5.9 respect rate limits and permitted access only; LinkedIn is user-assisted or authorized access only.
- Tests confirm no CAPTCHA bypass, MFA bypass, or access-control circumvention.
- Job description parsing, duplicate detection, weighted match scores, missing skills, seniority, compensation, and location matching work, with explainable output.
- CLI job search and job analysis commands (FR-10.5, 10.6) call the same services as the GUI.
- Deterministic matching (FR-3.20) works with no AI backend.
- Objective filters are deterministic; semantic evaluation runs only when an AI backend is configured.

Gates: all five apply. Build includes the connector SDK; migrations add jobs, companies, and match tables; Windows and macOS means connector tests and the job CLI commands pass on both.
## Release 0.6: Browser automation

Scope: Playwright runtime, permissions engine, Manual and Copilot modes. Autonomous mode ships only for connectors that pass policy review.

Requirements: FR-3.22, FR-9.1 to FR-9.21, FR-11.6, FR-12.4.

Exit tests:
- Manual and Copilot modes detect forms, extract questions, retrieve answers, fill forms, upload resume and cover letter, and capture confirmations.
- The permissions engine allows Autonomous submission only for connectors with verified permission and explicit user authorization; all other connectors are refused.
- The runtime pauses for legal declarations, unverified sensitive information, identity verification, CAPTCHA, MFA, and other human-only steps.
- Browser processes are isolated; emergency pause halts them.
- Rule-based form mapping (FR-3.22) fills common fields with no AI backend.
- Application status updates after submission, and each submission is audited.

Gates: all five apply. Build includes the Playwright runtime and its browser binaries; migrations add permission and automation-run tables; Windows and macOS means isolated browser launch and pause pass on both.
## Release 1.0: Stable open-source distribution

Scope: signing and notarization, auto-update, security review, public docs. FR-1.8 installers were first delivered unsigned in 0.1; 1.0 signs them.

Requirements: FR-13.10, FR-13.11. The security review covers all of FR-12 as delivered in earlier releases.

Exit tests:
- Installers are signed (Windows) and signed and notarized (macOS).
- Auto-update delivers and verifies a new version.
- Security review is complete and its findings are resolved or documented.
- Published: PRD, architecture, database schema, Connector SDK docs, security model, development guide, contribution guide, license, and release notes.
- The license is confirmed as Apache 2.0, or a documented reason is given for a change.
- Release builds are produced by GitHub Actions on Windows and macOS.

Gates: all five apply. Build means signed installers; migrations means upgrade tests from every earlier release; Windows and macOS means signing, notarization, and update verified on both.
