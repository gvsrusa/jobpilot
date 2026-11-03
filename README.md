# JobPilot AI

JobPilot AI is a personal, non-commercial, open-source, local-first desktop application for international job seekers. The long-term goal covers the job-search lifecycle: discover, analyze, match, personalize, apply, track, improve. This is one person's project, not a product with support or a roadmap guarantee.

Your data stays on your machine. There are no mandatory API keys, no API credits, no hosted backend, and no proprietary cloud account. The network use in 0.5 is a local Ollama server on this machine, the `claude` and optional `gemini` programs you already have installed, and, only if you add your own key and consent, a hosted provider's API (OpenAI, Anthropic, Google, or OpenRouter) or a server you name on your own computer. Release 0.5 adds one more: when you press Fetch, JobPilot reads a job source's public listings (see below). Release 0.6 adds browser automation: when you set it up and start a run, JobPilot downloads the Playwright browser runtime and Chromium once, and a visible browser opens application forms (see below). Every one of these is off until you turn it on or press it.

## What works today (0.6)

Release 0.1 is the foundation. Release 0.2 adds your candidate profile and a first resume tool. Release 0.3 adds application tracking, career memory, a workflow runner with agent logs, a small analytics report, and an emergency pause. Release 0.4 adds more AI backends, all optional and all behind your consent. Release 0.5 adds job discovery and a job store with a score you can read. Release 0.6 adds application automation: Manual, Copilot, an Autonomous gate that nothing passes yet, and a Claude Code apply mode. The 0.6 build is a candidate: its tests pass, but every apply test ran on local fixture forms with a scripted stand-in for `claude`, no real site was opened, and several things were never run by a person (see the [0.6 release notes](docs/releases/0.6.0.md), the [0.5 release notes](docs/releases/0.5.0.md), the [0.4 release notes](docs/releases/0.4.0.md), the [0.3 release notes](docs/releases/0.3.0.md), and the [0.2 release notes](docs/releases/0.2.0.md)).

- Desktop app with status, documents, data, recovery, profile, resumes, AI, applications, memory, and agents views (Ctrl/Cmd+1 to 9), a Jobs view (Ctrl/Cmd+0), and a terminal panel (Ctrl+` shows it, F6 leaves it).
- Applications you track by hand: status, history, and notes, offline. A summary of counts and rates on request.
- Career memory: questions you are asked again and again, with answers you approve. A sensitive or legal answer is sealed, and used only after you confirm it for one application. Work authorization is read from your profile by exact country and never guessed.
- Emergency pause: the red button in the title bar, the Safety menu, `jobpilot pause set`, and `Ctrl/Cmd+Shift+H`. It stops every run and model call, in the app and the CLI. On a Mac the key may not fire while the web view is hung; use the menu or the CLI.
- Jobs (Ctrl/Cmd+0, and `jobpilot job ...`): Fetch reads the public listings of Lever, RemoteOK, We Work Remotely, Greenhouse, and Ashby, only when you press it and only within a stored rate limit. Greenhouse and Ashby read under a ruling the project's controller made, because their vendors say nothing about third-party readers; it is not an owner decision (see the [policy register](docs/policy-register.md)). Workday, Indeed, LinkedIn, and a company's own page are never read: you paste the posting. Jobs are scored by rules you can read (skills 40, seniority 20, location 20, pay 20), and a criterion that cannot be compared is left out, never counted against a job. A model can add a separate, labelled evaluation. It never changes the score.
- Applying (0.6). On an application's review screen you choose a mode. **Manual** works on every site: JobPilot opens the posting in your own browser, hands you the files and your approved answers (a sensitive one only after you confirm it, copied by JobPilot and cleared from the clipboard after 60 seconds), and records the outcome. **Copilot** fills the form in a visible browser on Greenhouse, Lever, and Ashby hosted forms and stops for you to review and submit; it is refused on Workday, Indeed, and LinkedIn. **Autonomous** is built as a gate and no site qualifies, so it is always refused today.
- **Claude Code apply mode** (0.6, the owner's decision for personal use). Select up to 25 saved applications and choose Apply with Claude Code. JobPilot starts your own `claude` in the terminal panel with a visible browser, on any site including Indeed, LinkedIn, and Workday, and it submits each application with no further question once you start the run. It uses only your approved answers and profile facts, checked by JobPilot before anything is typed; it has browser tools only, and can upload only the resume and cover letter chosen for that application. It stops for sign-in, CAPTCHA, codes, identity checks, and legal declarations, and only you end a stop. Every application is logged and tracked, and emergency pause kills the session and the browser. A site may restrict or ban an account that uses automation. Needs Node 20 or later and `jobpilot browser setup` (or Set up browser on the run screen) once.
- Local SQLite database with versioned migrations, document storage, and full-text search.
- Backup, restore, export, and delete-all of your data, with an audit log.
- A profile with several countries, cities, currencies, languages, and work modes. Work authorization is stored as you state it, encrypted, and never inferred. A recovery key file lets you reopen encrypted fields if your keychain is lost.
- Resume import from PDF and DOCX into a knowledge base; resumes and cover letters for a pasted posting, with each line tied to your own evidence. A line without evidence is flagged and blocks export. Export to DOCX and PDF.
- AI is optional and every feature works with none. Backends: Ollama on this machine (loopback only; you start any download); Claude Code through the official `claude` CLI on your own login; the optional Gemini CLI, run with every tool denied; OpenAI, Anthropic, Google (paid key), and OpenRouter on your own key; and one server you name on a loopback address. Job text goes only to a backend that has no tools, and only after you consent to that backend. The Codex CLI adapter is present and inert: it cannot prove it has no tools, so no task uses it. Keys live in your OS keychain and never reach the app window. No-AI is always last.
- The `jobpilot` command line (`status`, `doc`, `backup`, `data`, `profile`, `prefs`, `kb`, `job`, `resume`, `letter`, `app`, `memory`, `agents`, `analytics`, `ai`, `pause`, `browser`, `apply`), bundled with the app and also usable in a normal terminal.
- OS keychain access for secrets. Secret values never reach the webview, logs, or exports.

Claude Code note: JobPilot starts your installed `claude` and never reads or stores its login. Anthropic's consumer terms may allow training on your content unless you opt out; the consent screen says so. The owner of this project has waived subscription-policy concerns for personal use and accepts that risk. You should decide that for yourself.

Gemini CLI note: the CLI keeps a transcript of every prompt under `~/.gemini/tmp` for 30 days and has no setting to turn that off, so a posting you send through it sits there in plain text. The consent screen says so. A Gemini key must belong to a project with billing on: a free key can send your data to human reviewers, and JobPilot cannot tell the two apart, so you attest to it.

### First run with Claude Code

1. Install Claude Code, then run `claude` once, in a terminal or the panel, to sign in. JobPilot never sees that login.
2. In Profile, enter the name and email a resume should carry.
3. On the AI screen, under Claude Code, choose Review consent, then Allow what drafting needs (job text, basic profile, resume text), and confirm the system dialog. That records your consent and turns Claude Code on. A restore revokes it, so you give it again after one.
4. On the Resumes screen, choose Import resume.
5. In Knowledge, read the imported lines. Mark anything about visas, citizenship, or pay as sensitive, then use Approve all from this resume, or approve line by line. A draft can only use approved lines.
6. Choose Paste a job posting, then Generate. Check each line against its evidence, then export.

Memory refresh spends paid or subscription usage and only runs when you press it; it states the most calls it can make first. See [milestones](docs/milestones.md).

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
- [Connector SDK](docs/connector-sdk.md)
- [Release notes 0.6.0](docs/releases/0.6.0.md)
- [Release notes 0.5.0](docs/releases/0.5.0.md)
- [Release notes 0.4.0](docs/releases/0.4.0.md)
- [Release notes 0.3.0](docs/releases/0.3.0.md)
- [Release notes 0.2.0](docs/releases/0.2.0.md)
- [AI providers](docs/ai-providers.md) and [threat model](docs/security/threat-model.md)
- [Changelog](CHANGELOG.md)

## Stack

Tauri 2, Rust, SQLite with SQLx and FTS5, React, TypeScript, Vite, Tailwind CSS, shadcn/ui, Zustand.

## License

Apache-2.0. See [LICENSE](LICENSE).
