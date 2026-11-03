# Changelog

Format follows Keep a Changelog. Versions follow Semantic Versioning.

## [Unreleased]

## [0.6.0] - unreleased

Browser automation. See `docs/releases/0.6.0.md` for what was and was not exercised.

### Added
- The browser runtime: `@playwright/mcp` 0.0.84 pinned by a committed lockfile, installed on request by `jobpilot browser setup` (or Set up browser) with the owner's Node 20 or later, and Chromium downloaded beside it. Nothing is bundled. A visible browser, a fresh profile per run deleted at the end, downloads off, and no anti-detection flag.
- A permissions engine (`permissions`) that decides each browser action by mode, site, register row, verified list, and grant, and refuses by default.
- Manual mode on every site: a review screen and `jobpilot apply material` and `apply record`, with answers copied by Rust (a sealed one only after a native confirmation) and cleared from the clipboard after 60 s.
- Copilot mode on Greenhouse, Lever, and Ashby hosted forms (decision D2, recorded as "no clause found"): form parsing from the accessibility snapshot, rule mapping, the `map_form_field` task for the rest, fill and upload, then a review stop where the person submits. `jobpilot apply copilot`.
- The Autonomous gate: grants (`jobpilot apply autonomous list|grant|revoke`), and a run path (`apply autonomous run`, Tauri `autonomous_start`) refused while no site is verified, which is today. One submission per application, and no model call.
- The Claude Code apply mode (owner decision of 2026-11-01): `Apply with Claude Code...` on Applications, or `jobpilot apply preflight|start|status|show|resolve|settle|cancel`. The owner's own `claude` runs in the terminal panel behind JobPilot's `apply-mcp` proxy, on all sites, with no per-application check once a run starts. Approved facts only, checked in Rust; browser tools only; the two chosen files only; stops for sign-in, CAPTCHA, MFA, identity, and legal declarations that only a person ends; every application logged and tracked; emergency pause kills every process of the run.
- A sealed-answer release asked once per run in a native dialog (CLI `--release-sealed`).
- Screens: the application review (wireframe 11, four modes), the apply run (wireframe 19), and the pause panel. The terminal panel header reads `Claude Code apply run` while it hosts one.
- Migrations 0016 and 0017. Schema version 17.
- Tests on a scripted fake `claude`, a fake Playwright server, and, in the `Browser end to end` CI step, the real server and Chromium on fixture forms. No test reaches a site.

### Changed
- Emergency pause also kills an apply session and its browser, from the desktop or the CLI, and the startup sweeps orphans of a crashed run.
- An apply run does not resume; the owner starts a new run.
- Version 0.6.0.

## [0.5.0] - unreleased

Job discovery and job intelligence. See `docs/releases/0.5.0.md` for what was and was not exercised.

### Added
- Connector SDK: a declaration of operations, automation, rate limit, and policy status, two pure functions, and a gate that reads the build-time policy register. `docs/connector-sdk.md`.
- Read connectors for Lever, RemoteOK, and We Work Remotely (`allowed` rows), and for Greenhouse and Ashby under ruling R1, a controller ruling and not an owner decision (`silent-read`). One fetcher sends every request: pause check, gate, declared host, a rate limit kept in the database, and no redirect, cookie, or proxy. Workday, Indeed, LinkedIn, and company pages take a pasted posting only.
- A job store: sources, jobs, companies, versioned job texts, duplicate groups, and match results. Migrations 0013 to 0015. Schema version 15.
- Deterministic parsing of place, pay, seniority, and skills; a weighted rules score with an explanation; objective filters; and a stated-only work authorization line.
- Discovery, matching, and research agents on the workflow runner, and two router tasks, `semantic_match` and `summarize_company`. A model's evaluation is shown apart from the score.
- `jobpilot job` commands (paste, import, list, search, show, remove, split, sources, source, fetch, analyze, research, company, prefs) and the Jobs view on Ctrl/Cmd+0, with a job detail that explains its score.
- Injection corpus files 20 to 22 and tests for connector text, the fit score, and company claims.
- CI on macOS and Windows, and a tag-triggered draft release workflow producing dmg, nsis, and msi installers.
- Development guide and contributing guide.

### Changed
- A model never sets pay: only the deterministic parser does. A model sets work mode or seniority only when the word is in the posting and is not negated. On a revised posting only your own fields stay.
- Import refuses an http link and a link with a user name or a port.
- `Retry-After` is capped at 24 hours.
- The instruction-like markers were narrowed so ordinary postings stop raising findings: `as an ai` is gone, and `system:` counts only as a leading order, so `System: Linux servers` is a plain label.
- Version 0.5.0.

### Fixed
- Windows: keychain calls no longer overlap, within a process or between the app and the CLI. Credential Manager could answer "no such entry" for a stored key, which reads as a lost key and offers discard.

## [0.4.0] - unreleased

External AI backends, all optional and all behind consent. See `docs/releases/0.4.0.md` for what was and was not exercised.

### Added
- Hosted API providers on your own key: OpenAI, Anthropic, Google (paid key), and OpenRouter, plus one endpoint you name on a loopback address. Native TLS (Schannel, Security.framework), HTTPS only, no redirect, no proxy. Keys live in the OS keychain; the app reads the clipboard in Rust and shows a native dialog, `jobpilot ai key set` reads a hidden prompt, and no key crosses the webview or argv.
- The Gemini CLI adapter, with every tool denied by a policy file, a run-time guard, and a version floor of 0.40.0. The Codex CLI adapter ships inert: it declares no `no_tools`, so every task skips it.
- Consent per provider and sign-in method for each new backend, with a required attestation for a Google key and for the Gemini CLI, a native dialog, and "Show exactly what will be sent". A consent record carries the register row's text, so a changed row suspends it. Migration 0012. Schema version 12.
- `Retry-After` on hosted providers: one retry on 429, 503, or 529 when the header gives whole seconds up to 30.
- CI step `TLS handshake` on both systems, from the ignored `real_tls_handshake` test.
- `jobpilot ai key|model|endpoint|cli-path|consent show|give|revoke` for the new providers, and the same on the AI screen.

### Changed
- Memory refresh states the true ceiling: pending questions times the external providers that could take each, since a failed reply goes on to the next provider. The 20-question limit is stated separately.
- The AI screen is headed "AI providers".
- An existing Claude Code consent is suspended once on upgrade and needs one re-confirm.
- The release re-read of AI provider terms is waived by the owner; it stays for the site rows.
- Version 0.4.0.

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
