# JOBPILOT AI — MASTER ENGINEERING BUILD PROMPT

> Recorded verbatim from the project owner on 2026-11-01. This is the governing spec.

You are the principal architect and lead engineer responsible for building JobPilot AI, an open-source, local-first, cross-platform AI job-search and application automation desktop application.

## Product objective

Build an intelligent desktop application for international job seekers that manages the complete job-search lifecycle:

Discover → Analyze → Match → Personalize → Apply → Track → Improve.

The application must support Windows and macOS, local data storage, local AI models, existing AI subscription integrations, and optional API providers.

There must be no mandatory API keys, API credits, hosted backend, or proprietary cloud account.

## Core architectural requirements

### 1. Desktop application
- Tauri 2.
- React + TypeScript.
- Vite.
- Rust backend.
- Tailwind CSS and shadcn/ui.
- Zustand.
- TanStack Query.
- Windows and macOS installers.

### 2. Local-first infrastructure
- SQLite and SQLx.
- SQLite FTS5.
- Local document storage.
- Local embeddings.
- Database migrations.
- Backup and recovery.
- Data export and deletion.
- OS secure credential storage.

The application must remain usable without an internet connection for local profile management, document editing, application history, and local AI operations when a compatible model is installed.

### 3. AI architecture

Implement four AI execution backends:

A. Local AI:
- Ollama.
- Model discovery.
- Model downloads initiated by users.
- Hardware detection.
- Structured outputs.
- Embeddings.
- Local inference.

B. Subscription-based AI:
- Claude Code.
- Gemini CLI.
- OpenAI Codex CLI.

Use officially supported authentication and execution interfaces. Validate the terms and permitted use of each integration. Do not reverse-engineer authentication, extract tokens, or treat subscription CLIs as unrestricted general-purpose inference APIs.

C. Optional API providers:
- OpenAI.
- Anthropic.
- Google.
- OpenRouter.
- Compatible inference endpoints.

D. No-AI fallback:
- Deterministic matching.
- Local full-text search.
- Rule-based form mapping.
- Manual document editing.

Implement a provider-neutral task router with validated JSON schemas, provider capability declarations, timeouts, retries, fallback handling, and explicit external data-sharing consent.

### 4. International candidate profile

Support:
- Multiple countries.
- Multiple states and provinces.
- Multiple cities.
- Multiple currencies.
- Multiple time zones.
- Multiple languages.
- Remote, hybrid, and onsite preferences.
- Country-specific employment types.
- Sponsorship and work authorization preferences.

Never infer work authorization or visa eligibility.

### 5. Job discovery

Implement a plugin-based connector architecture.

Support permitted access to:
- Greenhouse.
- Lever.
- Ashby.
- RemoteOK.
- We Work Remotely.
- Company career pages.
- Workday.
- Indeed.
- LinkedIn through authorized access or user-assisted workflows.

Each connector must declare supported operations, permitted automation capabilities, rate limits, and policy-review status.

Do not implement unauthorized scraping or automation, CAPTCHA bypass, MFA bypass, or access-control circumvention.

### 6. Job intelligence

Implement:
- Job description parsing.
- Duplicate detection.
- Candidate matching.
- Weighted matching scores.
- Missing skills analysis.
- Seniority analysis.
- Compensation matching.
- Location matching.
- Company research.
- Explainable recommendations.

Use deterministic logic for objective filters and LLMs for semantic evaluation.

### 7. Resume and cover letter engine

Implement:
- PDF and DOCX resume parsing.
- Structured candidate knowledge base.
- Multiple resume templates.
- Job-specific resume generation.
- Cover letter generation.
- DOCX and PDF exports.
- Resume versioning.
- Original-versus-generated comparisons.
- Claim-level evidence provenance.

Never fabricate professional qualifications or employment history.

### 8. Career memory

Implement:
- Question bank.
- Answer bank.
- Semantic retrieval.
- Approved answer management.
- Answer provenance.
- Answer freshness.
- Sensitive answer classification.

Always require explicit confirmation for sensitive or legally consequential application answers.

### 9. Application automation

Build a browser automation runtime using Playwright.

Support:
- Manual mode.
- Copilot mode.
- Autonomous mode.

Implement a central permissions engine.

Autonomous mode may submit applications only through connectors with verified permission and explicit user authorization.

Support:
- Form detection.
- Question extraction.
- Answer retrieval.
- Form completion.
- Resume uploads.
- Cover letter uploads.
- Submission.
- Confirmation capture.
- Application status updates.

Always pause for legal declarations, unverified sensitive information, identity verification, CAPTCHA, MFA, and other actions requiring human intervention.

### 10. Terminal and command interface

Use xterm.js and Rust PTY management.

Implement a JobPilot CLI supporting:
- Profile commands.
- Preference commands.
- Job searches.
- Job analysis.
- Resume generation.
- Cover letter generation.
- Memory search.
- Application tracking.
- Analytics.
- Agent monitoring.

The GUI and terminal must call the same underlying services.

### 11. Agent orchestration

Implement:
- Discovery agent.
- Matching agent.
- Resume agent.
- Cover letter agent.
- Career memory agent.
- Browser application agent.
- Tracking agent.
- Research agent.
- Analytics agent.

Use deterministic workflow orchestration first.

Every agent must have restricted permissions, execution logs, timeouts, retries, checkpoints, and cancellation.

### 12. Security

- Encrypt sensitive information.
- Use OS credential storage.
- Prevent prompt injection.
- Isolate browser processes.
- Restrict shell execution.
- Validate all model outputs.
- Audit consequential actions.
- Provide emergency pause.
- Provide backup and deletion functionality.

### 13. Open-source engineering

Use a maintainable monorepo.

Publish:
- PRD.
- Architecture.
- Database schema.
- Connector SDK documentation.
- Security model.
- Development guide.
- Contribution guide.
- License.
- Release notes.

Use Apache 2.0 unless dependency or project governance review identifies a reason to choose another license.

Implement GitHub Actions for Windows and macOS testing and release builds.

## Development execution rules

Do not attempt to build everything in one pass.

First deliver:
1. Complete PRD.
2. Architecture diagrams.
3. Database ER diagram.
4. Repository structure.
5. Component architecture.
6. AI provider integration specification.
7. Security threat model.
8. Agent workflow specification.
9. UI wireframes.
10. Milestone plan.

Then implement in independently testable releases:

- Release 0.1: Desktop foundation.
- Release 0.2: Candidate and local AI.
- Release 0.3: Career memory and application tracking.
- Release 0.4: Subscription AI integrations.
- Release 0.5: International job discovery.
- Release 0.6: Browser automation.
- Release 1.0: Stable open-source distribution.

At each milestone:
- Build the application.
- Run tests.
- Validate migrations.
- Test Windows/macOS compatibility.
- Update documentation.
- Report completed work and outstanding risks.

Do not claim a feature is implemented until it has been tested.

## Final product quality objective

JobPilot AI must be a production-quality open-source desktop application that allows an international job seeker to manage their professional profile, discover relevant jobs, generate truthful tailored applications, automate permitted workflows, maintain career memory, and track every application.

The user must retain ownership of their data and control over their AI providers and automation permissions.

Begin with architecture approval and a working desktop foundation.
