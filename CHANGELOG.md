# Changelog

Format follows Keep a Changelog. Versions follow Semantic Versioning.

## [Unreleased]

### Added
- CI on macOS and Windows, and a tag-triggered draft release workflow producing dmg, nsis, and msi installers.
- Development guide, contributing guide, and Connector SDK stub.

## [0.2.0] - unreleased

Candidate and local AI. See `docs/releases/0.2.0.md` for what was and was not exercised.

### Added
- Candidate profile with multi-country fields, per-country work authorization, and AES-256-GCM encryption of sensitive fields. Recovery-key export and import, discard of locked fields, and key states for a missing, mismatched, or unavailable key.
- Resume import from PDF and DOCX, a knowledge base, pasted job texts normalized as untrusted, and resume and cover letter drafts with claim-level evidence. Unsupported claims are stored flagged and block export. DOCX and PDF export, versions, and comparison.
- Task router with Ollama (local models only, hardware fit, user-started downloads, embeddings), Claude Code (headless, `--tools "" --allowedTools StructuredOutput`, consent required), and No-AI.
- Terminal panel running the bundled `jobpilot` CLI as a sidecar, with a restricted line interpreter and a launcher for the user's `claude`.
- Profile, Resumes (with a Knowledge section to approve, mark, add, and remove items), and AI views; new CLI commands `profile`, `prefs`, `kb`, `job`, `resume`, `letter`, `ai`, `data recovery-key`.
- Migrations 0002 to 0005.
- CI builds the sidecar and skips the real `claude` smoke and fixture generator.

### Changed
- Restore reports the backup's key state first and keeps the outgoing key, and revokes external AI consent, audited as `consent_revoked_by_restore`.
- An enabled AI provider is routed whether or not `ai.order` names it, after the stored order. The AI screen shows that order.
- A task that ends `manual_required` says why: `consent_needed` or `provider_unavailable`, not `no_ai`, when one of those is the cause.
- Delete-all also removes this root's data keys from the credential store.

## [0.1.0] - unreleased

Foundation release. See `docs/releases/0.1.0.md` once published.

### Added
- Tauri desktop app with status, documents, data, and recovery views.
- Local SQLite database with migrations, documents, and full-text search.
- Backup, restore, export, and delete-all, with an audit log.
- `jobpilot` command line.
- OS keychain secret storage.
