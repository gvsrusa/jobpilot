# Changelog

Format follows Keep a Changelog. Versions follow Semantic Versioning.

## [Unreleased]

### Added
- CI on macOS and Windows, and a tag-triggered draft release workflow producing dmg, nsis, and msi installers.
- Development guide, contributing guide, and Connector SDK stub.

## [0.3.0] - unreleased

Career memory, application tracking, and emergency pause. See `docs/releases/0.3.0.md` for what was and was not exercised.

### Added
- Application tracking by hand and offline: add, edit, filter, status changes with history, and notes. Commands `app add|list|show|history|edit|status|note|answers|remove`, and an Applications view.
- Career memory: questions and answers you approve, with provenance, freshness, and sensitivity classes (`normal`, `sensitive`, `legal`, `work_authorization`). A sensitive or legal answer is sealed at rest and used only after you confirm it for one application. Work authorization is never stored in memory: it is read from your profile for one exact country, and absent reads "not stated". Search by meaning with a local embedding model, else by words. Commands `memory ...` and a Memory view.
- Memory refresh: a model may double-check classes (one attempt each, at most 20 per press) and may only raise a class. Only you start it, and it says how many Claude calls it will make first. A class you set is final.
- A deterministic workflow runner with static permission sets, execution logs, checkpoints, timeouts, retries, and cancellation. The resume, cover letter, career memory, tracking, and analytics agents run on it. Commands `agents list|show|log|cancel|resume` and an Agents view.
- An analytics report of counts and rates over your applications, built on request: `analytics report|show`, and a summary in the Applications view.
- Emergency pause from the title bar of every screen, the native Safety menu, `Ctrl/Cmd+Shift+H`, and `jobpilot pause set|status|clear`. It stops every run and every model call in every JobPilot process, survives a restart, and is audited. Clearing it asks through a native dialog in the app.
- Migrations 0006 to 0011. Schema version 11.

### Changed
- A restore keeps the emergency pause, and a restore that revokes external consent also turns Claude Code off in settings, so the AI screen does not show it on and failing.
- The applied date a status change stamps, and the "applied in the last 30 days" count, follow your local calendar instead of UTC.
- The analytics text shows the median response time in days.
- Application status changes are refused with `paused` while the emergency pause is on. Editing fields and notes stays open.

### Known limits
- On macOS, `Cmd+Shift+H` may not fire while the web view is hung (`wry` 0.57.1 passes the key to the web view first). Use the Safety menu or `jobpilot pause set`. A `claude` session you start in the terminal panel is not stopped by the pause.
- Rule words can miss a paraphrased sensitive question. See the release notes.

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
