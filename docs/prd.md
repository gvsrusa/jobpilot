# JobPilot AI: Product Requirements Document

Status: draft for owner approval. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md). Requirement IDs have the form `FR-<spec section>.<n>`. Later documents cite these IDs. Nothing in this document is implemented yet.

## 1. Target users

- International job seekers who search across several countries, currencies, time zones, and languages.
- Job seekers who want to keep their profile, documents, and application history on their own machine.
- Job seekers who already pay for an AI subscription (Claude Code, Gemini CLI, OpenAI Codex CLI) or run local models, and want to use those instead of a new account.
- Job seekers who need visa or sponsorship preferences handled carefully and never guessed.

## 2. Problem

Job searching spans many job boards, company career pages, and applicant tracking systems. The user rewrites resumes, retypes the same answers into each form, and loses track of what was sent where. Existing tools tend to require a hosted account, upload personal data to a vendor, or automate applications in ways that break site rules or invent claims about the candidate. International searches add work authorization, multiple currencies, and country-specific employment types that most tools handle poorly.

## 3. Goals

- G1. Cover the full lifecycle: Discover, Analyze, Match, Personalize, Apply, Track, Improve.
- G2. Run on Windows and macOS with all user data stored locally.
- G3. Work with no mandatory API key, API credit, hosted backend, or proprietary cloud account.
- G4. Let the user choose among four AI backends, including a no-AI fallback.
- G5. Generate only truthful application material, with each claim traced to evidence.
- G6. Automate only where a connector has verified permission and the user has authorized it.
- G7. Give the user ownership of their data and control over AI providers and automation permissions.

## 4. Non-goals

- A hosted service, account system, or cloud sync.
- Mandatory use of any paid API or subscription.
- Unauthorized scraping or automation, CAPTCHA bypass, MFA bypass, or access-control circumvention.
- Reverse-engineering AI subscription authentication, extracting tokens, or treating subscription CLIs as unrestricted inference APIs.
- Inferring work authorization or visa eligibility.
- Fabricating qualifications, employment history, or answers.
- Platforms other than Windows and macOS (the spec names only these two).
- Building everything in one pass. Work ships as independently testable releases (see [milestones](milestones.md)).

## 5. Fixed product rules

These two rules are taken verbatim from the spec and are not negotiable in any release.

- Never infer work authorization or visa eligibility.
- Never fabricate professional qualifications or employment history.

## 6. Functional requirements

### Section 1. Desktop application

- **FR-1.1** The application is built on Tauri 2.
- **FR-1.2** The user interface uses React and TypeScript.
- **FR-1.3** The front end is built with Vite.
- **FR-1.4** The backend is written in Rust.
- **FR-1.5** Styling uses Tailwind CSS and shadcn/ui.
- **FR-1.6** Client state uses Zustand.
- **FR-1.7** Server and async state uses TanStack Query.
- **FR-1.8** The project ships Windows and macOS installers.

### Section 2. Local-first infrastructure

- **FR-2.1** Data is stored in SQLite accessed through SQLx.
- **FR-2.2** Local full-text search uses SQLite FTS5.
- **FR-2.3** Documents are stored on the local disk.
- **FR-2.4** Embeddings are computed locally.
- **FR-2.5** The database is managed by versioned migrations.
- **FR-2.6** The user can back up and recover their data.
- **FR-2.7** The user can export all their data.
- **FR-2.8** The user can delete all their data.
- **FR-2.9** Credentials are held in the OS secure credential store.
- **FR-2.10** Local profile management works without an internet connection.
- **FR-2.11** Document editing works without an internet connection.
- **FR-2.12** Application history is viewable and editable without an internet connection.
- **FR-2.13** Local AI operations work without an internet connection when a compatible model is installed.

### Section 3. AI architecture

- **FR-3.1** The application provides four AI execution backends: local, subscription, API provider, and no-AI fallback.
- **FR-3.2** Local AI integrates Ollama.
- **FR-3.3** Local AI discovers installed and available models.
- **FR-3.4** Model downloads start only when the user initiates them.
- **FR-3.5** Local AI detects hardware to guide model choice.
- **FR-3.6** Local AI supports structured outputs.
- **FR-3.7** Local AI produces embeddings.
- **FR-3.8** Local AI runs inference on the user's machine.
- **FR-3.9** Subscription AI supports Claude Code.
- **FR-3.10** Subscription AI supports Gemini CLI.
- **FR-3.11** Subscription AI supports OpenAI Codex CLI.
- **FR-3.12** Subscription integrations use only officially supported authentication and execution interfaces.
- **FR-3.13** The terms and permitted use of each subscription integration are validated and recorded before it ships.
- **FR-3.14** The application does not reverse-engineer authentication, extract tokens, or treat subscription CLIs as unrestricted general-purpose inference APIs.
- **FR-3.15** Optional API providers include OpenAI.
- **FR-3.16** Optional API providers include Anthropic.
- **FR-3.17** Optional API providers include Google.
- **FR-3.18** Optional API providers include OpenRouter.
- **FR-3.19** Optional API providers include compatible inference endpoints.
- **FR-3.20** The no-AI fallback provides deterministic matching.
- **FR-3.21** The no-AI fallback provides local full-text search.
- **FR-3.22** The no-AI fallback provides rule-based form mapping.
- **FR-3.23** The no-AI fallback provides manual document editing.
- **FR-3.24** A provider-neutral task router dispatches AI tasks to backends.
- **FR-3.25** Each task has a validated JSON schema for its output.
- **FR-3.26** Each provider declares its capabilities.
- **FR-3.27** The router enforces timeouts.
- **FR-3.28** The router applies retries.
- **FR-3.29** The router handles fallback when a backend fails or is unavailable.
- **FR-3.30** Data leaves the machine only after explicit external data-sharing consent.

### Section 4. International candidate profile

- **FR-4.1** The profile supports multiple countries.
- **FR-4.2** The profile supports multiple states and provinces.
- **FR-4.3** The profile supports multiple cities.
- **FR-4.4** The profile supports multiple currencies.
- **FR-4.5** The profile supports multiple time zones.
- **FR-4.6** The profile supports multiple languages.
- **FR-4.7** The profile records remote, hybrid, and onsite preferences.
- **FR-4.8** The profile supports country-specific employment types.
- **FR-4.9** The profile records sponsorship and work authorization preferences as the user states them.
- **FR-4.10** The application never infers work authorization or visa eligibility.

### Section 5. Job discovery

- **FR-5.1** Discovery uses a plugin-based connector architecture.
- **FR-5.2** A connector exists for Greenhouse, within permitted access.
- **FR-5.3** A connector exists for Lever, within permitted access.
- **FR-5.4** A connector exists for Ashby, within permitted access.
- **FR-5.5** A connector exists for RemoteOK, within permitted access.
- **FR-5.6** A connector exists for We Work Remotely, within permitted access.
- **FR-5.7** Company career pages are supported, within permitted access.
- **FR-5.8** A connector exists for Workday, within permitted access.
- **FR-5.9** A connector exists for Indeed, within permitted access.
- **FR-5.10** LinkedIn is supported only through authorized access or user-assisted workflows.
- **FR-5.11** Each connector declares its supported operations.
- **FR-5.12** Each connector declares its permitted automation capabilities.
- **FR-5.13** Each connector declares its rate limits.
- **FR-5.14** Each connector declares its policy-review status.
- **FR-5.15** The application does not implement unauthorized scraping or automation.
- **FR-5.16** The application does not bypass CAPTCHA.
- **FR-5.17** The application does not bypass MFA.
- **FR-5.18** The application does not circumvent access controls.

### Section 6. Job intelligence

- **FR-6.1** The application parses job descriptions into structured fields.
- **FR-6.2** The application detects duplicate jobs.
- **FR-6.3** The application matches jobs to the candidate.
- **FR-6.4** Match scores are weighted across criteria.
- **FR-6.5** The application reports missing skills.
- **FR-6.6** The application analyzes seniority.
- **FR-6.7** The application matches compensation to the candidate's preferences.
- **FR-6.8** The application matches location to the candidate's preferences.
- **FR-6.9** The application provides company research.
- **FR-6.10** Recommendations are explainable, showing why a job scored as it did.
- **FR-6.11** Objective filters use deterministic logic.
- **FR-6.12** Semantic evaluation uses LLMs when an AI backend is available.

### Section 7. Resume and cover letter engine

- **FR-7.1** The application parses PDF resumes.
- **FR-7.2** The application parses DOCX resumes.
- **FR-7.3** Parsed data feeds a structured candidate knowledge base.
- **FR-7.4** Multiple resume templates are available.
- **FR-7.5** The application generates job-specific resumes.
- **FR-7.6** The application generates cover letters.
- **FR-7.7** Resumes and cover letters export to DOCX.
- **FR-7.8** Resumes and cover letters export to PDF.
- **FR-7.9** Resumes are versioned.
- **FR-7.10** The user can compare the original resume to a generated one.
- **FR-7.11** Each claim in generated material carries evidence provenance.
- **FR-7.12** The application never fabricates professional qualifications or employment history.

### Section 8. Career memory

- **FR-8.1** The application keeps a question bank.
- **FR-8.2** The application keeps an answer bank.
- **FR-8.3** Answers are retrieved by semantic similarity.
- **FR-8.4** The user manages which answers are approved.
- **FR-8.5** Each answer records its provenance.
- **FR-8.6** Each answer tracks its freshness.
- **FR-8.7** Answers are classified by sensitivity.
- **FR-8.8** Sensitive or legally consequential application answers always require explicit user confirmation.

### Section 9. Application automation

- **FR-9.1** Browser automation uses Playwright.
- **FR-9.2** Manual mode is supported.
- **FR-9.3** Copilot mode is supported.
- **FR-9.4** Autonomous mode is supported.
- **FR-9.5** A central permissions engine decides what automation may do.
- **FR-9.6** Autonomous mode submits applications only through connectors with verified permission and explicit user authorization.
- **FR-9.7** The runtime detects forms.
- **FR-9.8** The runtime extracts questions.
- **FR-9.9** The runtime retrieves answers from career memory.
- **FR-9.10** The runtime completes forms.
- **FR-9.11** The runtime uploads resumes.
- **FR-9.12** The runtime uploads cover letters.
- **FR-9.13** The runtime submits applications.
- **FR-9.14** The runtime captures submission confirmations.
- **FR-9.15** The runtime updates application status.
- **FR-9.16** The runtime always pauses for legal declarations.
- **FR-9.17** The runtime always pauses for unverified sensitive information.
- **FR-9.18** The runtime always pauses for identity verification.
- **FR-9.19** The runtime always pauses for CAPTCHA.
- **FR-9.20** The runtime always pauses for MFA.
- **FR-9.21** The runtime always pauses for any other action that needs human intervention.

### Section 10. Terminal and command interface

- **FR-10.1** The application embeds a terminal using xterm.js.
- **FR-10.2** Terminal sessions are managed by a Rust PTY layer.
- **FR-10.3** The JobPilot CLI provides profile commands.
- **FR-10.4** The CLI provides preference commands.
- **FR-10.5** The CLI provides job search commands.
- **FR-10.6** The CLI provides job analysis commands.
- **FR-10.7** The CLI provides resume generation commands.
- **FR-10.8** The CLI provides cover letter generation commands.
- **FR-10.9** The CLI provides memory search commands.
- **FR-10.10** The CLI provides application tracking commands.
- **FR-10.11** The CLI provides analytics commands.
- **FR-10.12** The CLI provides agent monitoring commands.
- **FR-10.13** The GUI and the terminal call the same underlying services.

### Section 11. Agent orchestration

- **FR-11.1** A discovery agent exists.
- **FR-11.2** A matching agent exists.
- **FR-11.3** A resume agent exists.
- **FR-11.4** A cover letter agent exists.
- **FR-11.5** A career memory agent exists.
- **FR-11.6** A browser application agent exists.
- **FR-11.7** A tracking agent exists.
- **FR-11.8** A research agent exists.
- **FR-11.9** An analytics agent exists.
- **FR-11.10** Orchestration is deterministic workflow first.
- **FR-11.11** Every agent runs with restricted permissions.
- **FR-11.12** Every agent writes execution logs.
- **FR-11.13** Every agent has timeouts.
- **FR-11.14** Every agent has retries.
- **FR-11.15** Every agent has checkpoints.
- **FR-11.16** Every agent can be cancelled.

### Section 12. Security

- **FR-12.1** Sensitive information is encrypted.
- **FR-12.2** Secrets are held in OS credential storage.
- **FR-12.3** The application prevents prompt injection from job postings, web pages, and other untrusted text.
- **FR-12.4** Browser processes are isolated.
- **FR-12.5** Shell execution is restricted.
- **FR-12.6** All model outputs are validated before use.
- **FR-12.7** Consequential actions are audited.
- **FR-12.8** An emergency pause stops all automation.
- **FR-12.9** The user can back up their data.
- **FR-12.10** The user can delete their data.

### Section 13. Open-source engineering

- **FR-13.1** The project uses a maintainable monorepo.
- **FR-13.2** The project publishes a PRD.
- **FR-13.3** The project publishes an architecture document.
- **FR-13.4** The project publishes a database schema document.
- **FR-13.5** The project publishes Connector SDK documentation.
- **FR-13.6** The project publishes a security model.
- **FR-13.7** The project publishes a development guide.
- **FR-13.8** The project publishes a contribution guide.
- **FR-13.9** The project publishes its license.
- **FR-13.10** The project publishes release notes.
- **FR-13.11** The license is Apache 2.0 unless dependency or governance review finds a reason to choose another.
- **FR-13.12** GitHub Actions run tests on Windows and macOS.
- **FR-13.13** GitHub Actions produce release builds for Windows and macOS.

## 7. Offline matrix

"Yes" means the feature works with no network. Features marked "Conditional" need a compatible local model to be installed. Rows for later releases describe intent and are not yet built.

| Feature | Works offline |
|---|---|
| Profile management | Yes |
| Document editing and storage | Yes |
| Application history and tracking | Yes |
| Local full-text search (FTS5) | Yes |
| Backup, restore, export, delete | Yes |
| Secret storage in OS keychain | Yes |
| Deterministic matching and rule-based form mapping | Yes |
| Resume parsing (PDF, DOCX) | Yes |
| Resume and cover letter export (DOCX, PDF) | Yes |
| Local AI inference, structured outputs, embeddings | Conditional (model installed) |
| Semantic retrieval of career memory | Conditional (local embedding model installed) |
| Ollama model download | No |
| Subscription AI (Claude Code, Gemini CLI, Codex CLI) | No |
| API providers | No |
| Job discovery through connectors | No |
| Company research from the web | No |
| Browser application automation against live sites | No |
| Auto-update and release checks | No by default. The launch check is opt-in (`updates.check_on_launch`, off); Check for updates in Status is a press. Neither runs unless asked. |

## 8. Success metrics

Metrics are measurable per release. Targets are proposals for the owner to confirm; the spec sets none.

- Each release passes its exit tests on macOS and Windows in CI before it is described as working.
- All 0.1 features run with the network disabled, and 0.1 code makes no network call.
- A fresh install reaches a usable profile and document store with no account and no API key.
- Backup followed by restore reproduces the database and documents exactly; a migration test passes from every earlier schema version.
- Zero application submissions occur without verified connector permission and explicit user authorization.
- Zero generated claims lack evidence provenance.
- Every consequential action has an audit row, except delete-all, which removes the log.
- No secret value appears in logs, exports, the audit log, or IPC messages to the webview. The recovery-key file, accepted by the owner under Decision D5, is the one exception.

## 9. Open items

- Decision D1 (external AI backend design), decisions D2 and D3, D4 (user-held API keys), D5 (recovery-key file), and D6 (encryption approach) were made by the owner on 2026-11-01 and are recorded in Task A6 of the plan, the [policy register](policy-register.md#owner-decisions-2026-11-01), and the [milestone plan](milestones.md). The FR text above is unchanged.
- Some requirements are met only in part, or not yet, under today's vendor and site terms; see the register's [Policy impact](policy-register.md#policy-impact) table: FR-3.9, FR-3.10, FR-3.11, FR-3.15 to FR-3.17, FR-5.7 to FR-5.9, FR-9.3, FR-9.4.
- **Section 9 as built in 0.6.** FR-9.1 is met through Playwright MCP: the pinned `@playwright/mcp` 0.0.84 server, run on the owner's Node and driven by Rust as an MCP client, with no Rust Playwright crate and no injected JavaScript. FR-9.2 (Manual) is met on every site. FR-9.3 (Copilot) is met in part: it runs on Greenhouse, Lever, and Ashby hosted forms under decision D2, recorded as "no clause found", and stays refused on Workday, Indeed, and LinkedIn. FR-9.4 and FR-9.6 (Autonomous) ship as a gate: no site is verified, so no site is eligible, and its run path is refused today. The owner added a fourth mode on 2026-11-01, the **Claude Code apply mode**, which is not in the FR text: the owner's own `claude`, with JobPilot's `apply-mcp` proxy and a visible browser, on all sites including Indeed, LinkedIn, and Workday, with no per-application check once a run starts, and with the FR-9.16 to FR-9.21 pauses, approved facts only, two files only, logging, and emergency pause kept ([policy register](policy-register.md#owner-decision-2026-11-01-claude-code-apply-mode-release-06)). Because of it, the success metric "Zero application submissions occur without verified connector permission and explicit user authorization" holds for Autonomous only: in the apply mode, the owner's start of a run is the authorization, by the owner's decision.
- **FR-5.2 and FR-5.4 as built in 0.5.** Both are met under ruling R1: a read connector for Greenhouse and one for Ashby, on a board the person names, at 2 seconds between requests and 300 a day, with user-assisted import beside it. Both vendors are silent on third-party readers, so the register row is `silent-read`, never `allowed`. R1 was a controller ruling made while planning 0.5. It is not an owner decision, and the owner can overturn it. FR-5.7 to FR-5.9 stand as they were: company career pages, Workday, and Indeed are user-assisted import only, and no read connector exists for them ([policy register](policy-register.md#policy-impact)).
- The Claude Code requirements (FR-3.9, FR-3.12 to FR-3.14, FR-3.30) moved from Release 0.4 into Release 0.2 by ruling of 2026-11-01, because the owner named Claude Code first and Ollama is not running on the owner's machine; the optional Gemini CLI and Codex CLI adapters stay in 0.4 ([milestone plan](milestones.md)).
- **FR-3.10 and FR-3.11 as built in 0.4.** FR-3.10 (Gemini CLI) is met as an optional adapter on the installed CLI's own sign-in, with every tool denied and the version floor 0.40.0, so it handles job text. The CLI has no consumer subscription login of its own under the register (an Antigravity CLI replaced it for unpaid and Google One users), and its transcript cannot be turned off; the real CLI has not been run unless the [0.4 release note](releases/0.4.0.md) says so. FR-3.11 (Codex CLI) is met in part: the adapter ships and runs, but it declares no `no_tools`, so every task that carries untrusted text skips it, and its run path and flags are unverified. The register rows are unchanged.
