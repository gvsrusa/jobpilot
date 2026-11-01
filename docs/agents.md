# JobPilot AI: Agent Workflow Specification

Status: draft for owner approval. Date: 2026-11-01. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) sections 9 and 11, [PRD](prd.md), [milestones](milestones.md), [AI providers](ai-providers.md), [policy register](policy-register.md), and [threat model](security/threat-model.md). Requirement IDs (`FR-<section>.<n>`) come from the PRD. Nothing here is implemented yet. Task type names other than `parse_job`, `semantic_match`, and `draft_cover_letter` are proposals. All timeout values are proposals for the owner to confirm, except the per-attempt defaults, which come from [AI providers](ai-providers.md#task-definition) and are also proposed there.

## 1. What an agent is

An agent is a deterministic workflow in Rust, run by the workflow runner in `jobpilot-core` (FR-11.10). It is not an autonomous model. Step order is fixed in code. A step may call the task router, which returns validated JSON or a typed failure.

- Models get no tools. The "allowed tools" below are core functions the workflow calls. A model cannot call them, and nothing in its output triggers one.
- Agents do not start one another. The GUI and CLI start runs through the same service (FR-10.13).
- Every agent works with no AI backend, through its deterministic path or by ending in `manual_required`. The router never invents output to fill a gap.
- A person starts every run. No run starts from page content or model output.

## 2. Delivery by release

From the [milestones](milestones.md). Each agent lands with its feature.

| Release | Agents | Also lands |
|---|---|---|
| 0.2 | None (external backends start with Claude Code) | Task router, Claude Code headless adapter, consent record |
| 0.3 | Resume, cover letter, career memory, tracking, analytics | Workflow runner, execution logs, emergency pause |
| 0.5 | Discovery, matching, research | Connector SDK |
| 0.6 | Browser application | Playwright runtime, permissions engine |

Until 0.5, the resume and cover letter agents take posting text the user pastes. External backends start with Claude Code in 0.2 and widen in 0.4, so 0.3 agents use Claude Code, Ollama, or No-AI. After that, the router's checks decide.

## 3. Permission vocabulary

The 0.3 runner and the 0.6 permissions engine share these names. Concrete types come with each release plan.

| Permission | Meaning | First used |
|---|---|---|
| `read:job_text`, `read:company_text`, `read:profile_basic`, `read:resume_text`, `read:career_memory`, `read:sensitive` | Read that data class. Names match the [consent categories](ai-providers.md#consent-record). | 0.3 |
| `read:applications` | Read application history. | 0.3 |
| `read:matches` | Read stored match results. | 0.5 |
| `write:jobs`, `write:matches`, `write:company_notes` | Store discovered jobs, match results, and company notes. | 0.5 |
| `write:job_fields` | Store parsed fields on a job already stored. Cannot add a job. | 0.5 |
| `write:drafts` | Store generated documents as new versions. Never writes the knowledge base. | 0.3 |
| `write:career_memory` | Add questions and answers as unapproved. | 0.3 |
| `write:applications` | Add or change application records and status. | 0.3 |
| `write:reports` | Store computed analytics. | 0.3 |
| `model:<task_type>` | Send that task type through the router. All router checks still apply. | 0.3 |
| `embed:local` | Compute embeddings on a local model only (FR-2.4). | 0.3 |
| `net:connector` | Run a connector's read operation. | 0.5 |
| `apply:open_link` | Open a posting's link in the user's default browser. No automation. | 0.6 |
| `apply:supply` | Show or copy prepared files and approved text to the user. | 0.6 |
| `browser:drive` | Run Playwright in an isolated browser on a permitted origin or a local fixture. | 0.6 |
| `browser:fill`, `browser:upload` | Fill fields and upload named files. | 0.6 |
| `browser:submit` | Submit a form. Autonomous mode only. | 0.6 |

Enforcement:

- Release 0.3: the runner checks each call against the agent's static set in the table in section 4 (FR-11.11). A set is fixed in code and has no user setting.
- Release 0.5: the connector gate also checks `net:connector` against the build-time register.
- Release 0.6: the permissions engine (FR-9.5) also checks `apply:*` and `browser:*` for each action, against the site's register row, the mode, and the user's authorization. It refuses by default.

**No such permission exists** for a shell or terminal, reading a secret value, a free file path, free network access, approving an answer, lowering a sensitivity class, changing a register status, or answering a mandatory pause. No agent can be granted these. `read:sensitive` values stay in Rust and are never in a prompt with untrusted text ([threat model](security/threat-model.md#4-untrusted-input-and-prompt-injection)).

## 4. The nine agents

Timeouts exclude time waiting for a person. "Router policy" means the [retry rules](ai-providers.md#timeouts-and-retries): two attempts per provider on network errors, 429, 5xx, and one validation failure, honoring `Retry-After`, with no retry on authentication, policy, or consent refusals, cancellation, or a second validation failure. "Per attempt" means 60 seconds for remote and 300 seconds for local.

| Agent | Release | Inputs | Outputs | Allowed tools | Permission scope | Timeout | Retry policy | Checkpoint | Cancellation |
|---|---|---|---|---|---|---|---|---|---|
| Discovery (FR-11.1) | 0.5 | Searches the user saved, company sites the user named, postings the user pastes | Job rows marked untrusted with source and required attribution, duplicate groups (FR-6.2) | Read operations of connectors with an `allowed` read row, user-assisted import, deterministic duplicate check. No model. | `net:connector`, `read:job_text`, `write:jobs` | 30 seconds per request, 10 minutes per run | Two attempts per request on network errors, 429, 5xx, within the declared rate limit, honoring `Retry-After`. No retry on a refusal. | After each connector page: cursor and last job id | Abort the request. Keep stored jobs. |
| Matching (FR-11.2) | 0.5 | Stored jobs, profile, preferences, knowledge base, stated authorization values | Structured job fields, weighted score, missing skills, seniority, compensation and location match, reasons (FR-6.1 to FR-6.10) | Deterministic filters and scoring, `parse_job` and `semantic_match` through the router | `read:job_text`, `read:profile_basic`, `read:resume_text`, `read:sensitive`, `model:parse_job`, `model:semantic_match`, `write:job_fields`, `write:matches` | Per attempt for model steps, 30 minutes per run | Router policy. Falls back to deterministic scoring (FR-3.20). | After each job: job id and result id | Cancel the task, kill any child. Keep finished jobs. |
| Resume (FR-11.3) | 0.3 | Knowledge base, one posting, template choice | Versioned draft with claim-level evidence, comparison with the original, DOCX and PDF on request (FR-7.5 to FR-7.11) | Evidence lookup, `draft_resume` through the router, template rendering, document export | `read:resume_text`, `read:profile_basic`, `read:job_text`, `model:draft_resume`, `write:drafts` | Per attempt, 30 minutes per run | Router policy. A reply that is not valid JSON, breaks the schema, invents a heading, or whose claims do not match the document's lines counts as a validation failure and stores nothing. A claim that cites no usable evidence (including an id outside the evidence list), or states work authorization, is stored flagged and blocks export until the user edits or removes it (0.2). With no backend, `manual_required` opens the editor (FR-3.23). | After a validated draft, and after each render | Cancel the task, kill any child, discard the unsaved draft. Earlier versions stay. |
| Cover letter (FR-11.4) | 0.3 | Knowledge base, one posting, company notes if any | Versioned letter with claim-level evidence, DOCX and PDF on request (FR-7.6, FR-7.11) | Evidence lookup, `draft_cover_letter` through the router, template rendering, document export | `read:resume_text`, `read:profile_basic`, `read:job_text`, `read:company_text`, `model:draft_cover_letter`, `write:drafts` | Per attempt, 30 minutes per run | Router policy. A reply that is not valid JSON, breaks the schema, invents a heading, or whose claims do not match the document's lines counts as a validation failure and stores nothing. A claim that cites no usable evidence (including an id outside the evidence list), or states work authorization, is stored flagged and blocks export until the user edits or removes it (0.2). With no backend, `manual_required` opens the editor. | After a validated draft, and after each render | Cancel the task, kill any child, discard the unsaved draft. Earlier versions stay. |
| Career memory (FR-11.5) | 0.3 | Questions and answers the user enters, questions from past forms from 0.6 on, which keep untrusted provenance | Unapproved questions and answers, retrieval results with provenance and freshness, sensitivity class (FR-8.1 to FR-8.7) | Question and answer store, local embeddings, deterministic sensitivity rules, `classify_answer` through the router, which may raise a class and never lower it | `read:career_memory`, `read:job_text`, `read:sensitive`, `write:career_memory`, `embed:local`, `model:classify_answer` | Per attempt, 10 minutes per run | Router policy. Retrieval falls back to FTS5 on non-sensitive text. | After each batch of embeddings or classifications | Stop between items. Missing embeddings are rebuilt on the next run. |
| Browser application (FR-11.6) | 0.6 | Application record, posting link, mode, site's register row, approved answers, chosen files | By mode, see section 5: prepared material, or filled form with uploads; captured confirmation, pause records, status event for tracking (FR-9.7 to FR-9.15) | Manual: open link, supply material. Copilot, on an `allowed` row or local fixture: form detection, question extraction, rule-based mapping (FR-3.22), `map_form_field` through the router, fill, upload. Autonomous only: submit. | Manual: `apply:open_link`, `apply:supply`, `read:career_memory`, `read:resume_text`, `read:sensitive`, `read:applications`, `write:applications`. Copilot adds `read:job_text`, `browser:drive`, `browser:fill`, `browser:upload`, `model:map_form_field`. Autonomous adds `browser:submit`. | Per attempt, 2 minutes per browser step, 30 minutes per run. A pause holds for 30 minutes, then ends `manual_required` with nothing submitted. | Navigation and form detection retry once, before anything is filled. Model steps follow router policy. **Submission is never retried.** | After form detection, after each page filled, and before submission. A live page cannot resume: a resumed run reopens the form and the user reviews again. | Close the browser and delete its profile at once. Nothing further is sent. A submission already sent stays sent and is recorded. |
| Tracking (FR-11.7) | 0.3 | Status edits by the user, from 0.6 on confirmation events from the browser application agent | Application status history (FR-2.12, FR-9.15) | Application store only. No model, no network, no mail reading. | `read:applications`, `write:applications` | 30 seconds per update | One retry on a busy database. | Each update is one transaction | Stop between transactions. |
| Research (FR-11.8) | 0.5 | Company name, stored job text, data from `allowed` connectors, text the user pastes | Company notes marked untrusted, with a source for each fact and gaps left blank (FR-6.9) | Stored-text lookup, `allowed` connector reads, `summarize_company` through the router. No crawler, no `web_search`. | `read:job_text`, `read:company_text`, `net:connector`, `model:summarize_company`, `write:company_notes` | Per attempt, 10 minutes per run | Router policy. With no backend, shows stored text without a summary. | After each source is read, and after the summary validates | Cancel the task, kill any child. Keep stored text. |
| Analytics (FR-11.9) | 0.3 | Application history, match results from 0.5 on | Local report figures (FR-10.11) | Aggregate queries in core. No model, no network. | `read:applications`, `read:matches`, `write:reports` | 60 seconds per run | One retry on a busy database. | After each report section | Drop partial figures. |

Notes that bind the table:

- **Discovery.** Today's `allowed` read rows are Lever, RemoteOK, and We Work Remotely (RSS only). Every other source enters through user-assisted import. A site read row that is `unknown` behaves as `blocked`, and the agent cannot run a connector for it.
- **Matching.** Stated authorization values are compared deterministically and only per country. A missing value is reported as "not stated", never as eligible or ineligible (FR-4.10).
- **Resume and cover letter.** Only user-entered or user-approved material counts as evidence. A draft does not become evidence by being saved. Neither agent holds `read:sensitive`. `read:resume_text` withholds user-marked documents and lines. Unmarked text can still state visa status, citizenship, or salary ([threat model](security/threat-model.md#11-residual-risks)).
- **Career memory.** Only the user approves an answer or lowers a class (FR-8.4). Sensitive answers are not embedded. Work-authorization answers are found by exact country key, not by similarity.
- **Browser application.** `map_form_field` sees form text and candidate answer ids, never answer values. The model returns an id, and Rust fills the value. This keeps Sensitive-class values out of a prompt that holds untrusted text. `classify_answer` in the career memory agent likewise sees question text only. Both tasks declare `job_text`, in every release and whatever the question's source, so stored form questions cannot escape the no-tools rule.
- **Research.** An automated read of an arbitrary site is `unknown` in the register, so this agent reads no company site directly. FR-6.9 today means stored text, connector data, and text the user pastes. A site joins only with its own row.
- **Every model task** carrying `job_text` or `company_text` requires a provider declaring `no_tools` ([threat model](security/threat-model.md#7-cli-adapters-and-untrusted-text)). Every task above is `reviewed` except each `map_form_field` step in an Autonomous run, which is `unreviewed`.

## 5. Automation modes

The browser application agent runs in one of three modes. The mode must be earned by a register row, or under owner decision D2 for Copilot, by a row recording "no clause found". A user cannot override a site row that is `blocked`.

| Mode | Needs | Agent does | Today |
|---|---|---|---|
| Manual | A `user-assisted` apply row | Opens the link in the user's default browser, supplies files and text, records the outcome. No request to the site and no browser driven there. The user fills and submits. | Available on every `user-assisted` apply row in the register. |
| Copilot | The site's own `allowed` Copilot row, or a Copilot row recording "no clause found" (D2) | Drives an isolated browser, fills, uploads, stops. The user reviews and clicks submit. A human click does not substitute for the row. Eligible on Greenhouse, Lever, and Ashby hosted forms, recorded as "no clause found". Workday (a clause may reach it) and LinkedIn (not read) stay refused, and Indeed is `blocked`. Pauses for CAPTCHA, MFA, identity verification, and legal declarations always apply. |
| Autonomous | The site's own `allowed` row, verified connector permission, and explicit user authorization (FR-9.6) | Also submits, in one attempt. Stops at every mandatory pause. | **Default off. Zero eligible connectors.** Every hosted-form row for this step is `unknown` or `blocked`. |

Autonomous steps are `unreviewed`, so the router's [mode check](ai-providers.md#routing) refuses every OpenAI-origin model on any step of a submission, and any provider that cannot leave human confirmation in place. A site's row applies whichever model runs. Decision D2 (2026-11-01) keeps Autonomous default-off.

## 6. Mandatory pauses

From spec section 9: "Always pause for legal declarations, unverified sensitive information, identity verification, CAPTCHA, MFA, and other actions requiring human intervention."

| Pause | Requirement | Trigger | Resolved by |
|---|---|---|---|
| Legal declarations | FR-9.16 | Attestation, consent, signature, or truthfulness statement. Any question about AI use or authorship, per the [register](policy-register.md#ai-use-rules-that-bind-the-product-across-providers). | The user reads and answers it. JobPilot never answers it. |
| Unverified sensitive information | FR-9.17, FR-8.8 | A sensitive field with no approved answer, a stale answer, a pre-filled value, or any sensitive or legally consequential answer not yet confirmed for this use. | The user confirms or enters the value. |
| Identity verification | FR-9.18 | Any request to prove identity. | The user does it, or ends the run. |
| CAPTCHA | FR-9.19, FR-5.16 | Any CAPTCHA or bot challenge. | The user solves it in the visible browser, or ends the run. No solver, no bypass. |
| MFA | FR-9.20, FR-5.17 | Any second-factor prompt. | The user does it, or ends the run. JobPilot never sees or stores the code. |
| Other human intervention | FR-9.21 | A required field with no mapping, a page that differs from what the user reviewed, a redirect off the permitted origin, an unexpected dialog, an uncertain submission result, or any doubt. | The user decides. |

Rules:

- A pause applies in Copilot and Autonomous, and a model or a setting can never answer it. A run cannot be configured to skip one.
- Page text and model output can start a pause and can never end one. Only a user action through the GUI or CLI ends it.
- Doubt resolves to a pause. Nothing submits with an unknown field.
- In Manual, the user is already filling the form. The sensitive rule still applies: JobPilot supplies a sensitive answer only after confirmation (FR-8.8).
- After a pause, the run goes back through the checks in section 7 before continuing.

## 7. A run

```mermaid
stateDiagram-v2
    [*] --> Queued
    Queued --> Checking: runner starts the run
    Queued --> Cancelled: user cancels
    Checking --> Refused: permission or policy check fails
    Checking --> Running: all checks pass
    Running --> Checkpointed: step finished
    Checkpointed --> Checking: next step
    Checkpointed --> Completed: last step finished
    Running --> Retrying: retryable failure
    Retrying --> Running: attempts left
    Retrying --> Failed: attempts used up
    Running --> Failed: timeout or non-retryable failure
    Running --> ManualRequired: no backend and no deterministic path
    Running --> WaitingForHuman: mandatory pause
    WaitingForHuman --> Checking: user resolves the pause
    WaitingForHuman --> ManualRequired: pause times out or user takes over
    WaitingForHuman --> Cancelled: user cancels
    Running --> Cancelled: user cancels
    Retrying --> Cancelled: user cancels
    Checkpointed --> Cancelled: user cancels
    Running --> Halted: emergency pause
    Retrying --> Halted: emergency pause
    Checkpointed --> Halted: emergency pause
    WaitingForHuman --> Halted: emergency pause
    Queued --> Halted: emergency pause
    Checking --> Halted: emergency pause
    Halted --> Checking: user resumes from last checkpoint
    Halted --> Cancelled: user cancels
    Completed --> [*]
    ManualRequired --> [*]
    Refused --> [*]
    Failed --> [*]
    Cancelled --> [*]
```

| State | Meaning |
|---|---|
| Queued | Accepted, waiting for the runner. |
| Checking | Runner checks the agent's permission set and whether emergency pause is off. From 0.6, the permissions engine also checks the action. Re-run before every step, so a revoked authorization stops the run. The router makes its own policy and consent checks on every task. |
| Running | One step in progress under its timeout. |
| Checkpointed | Step result stored. The run can resume from here. |
| Retrying | Waiting to try again under the agent's retry policy. |
| WaitingForHuman | A mandatory pause. The run timeout is stopped. |
| Halted | Stopped by emergency pause. Stays here until a person acts. |
| Completed | Finished with validated results. |
| ManualRequired | No path remains without a person. Matches the router's `manual_required`. |
| Refused | A check failed. Nothing ran for that step. |
| Failed | Timeout or typed failure. Matches the router's `failed`. |
| Cancelled | Ended by the user. Matches the router's `cancelled`. |

A step's model task is handled by the router, including provider fallback. A router `failed` result reaches this diagram as a non-retryable failure, so model attempts are not counted twice.

### Execution logs

Every state change writes an execution log row (FR-11.12): run id, agent, step, state, duration, provider if any, data categories, and outcome. Logs never contain prompts, outputs, secrets, or decrypted sensitive values. Page text and job text are also excluded. The `agent_run` table comes with the 0.3 migration ([schema](database-schema.md#4-proposed-later-entities-not-decided)). The CLI and GUI read the same rows (FR-10.12).

Audit rows (FR-12.7) use actor `system` for a run's own actions, with the agent name in `detail`, and `gui` or `cli` for what a person does: external provider calls, submissions, emergency pause, resuming, and Autonomous authorization or revocation.

### Checkpoints

A checkpoint (FR-11.15) stores the step reached and references to stored results by id. It holds no prompt, no output text, no secret, and no decrypted sensitive value. A resumed run re-runs Checking and reads its data again, so it never acts on a stale permission.

### Cancellation

Cancellation (FR-11.16) takes effect at once: the in-flight router task is cancelled, any child process is killed, and any browser is closed with its profile deleted. A finished step's stored result stays unless the table in section 4 says otherwise. A cancelled run cannot resume.

### Submission safety

`browser:submit` runs at most once per application. If the app cannot tell whether a submission went through (crash, timeout, emergency pause mid-request), the run does not retry. It takes the "other human intervention" pause, and the user checks the site. Resume from a checkpoint never crosses submission.

## 8. Emergency pause

Emergency pause (FR-12.8) lands in 0.3, and from 0.6 it also halts browsers.

- Reachable from a native menu and from the CLI, so a hung webview cannot block it. Command names are left to the 0.3 plan.
- It cancels every in-flight task, kills child processes, closes browsers, and moves each run to Halted.
- The flag is stored in the database, so the desktop app and CLI both see it, and it survives a restart. While set, no run starts and queued runs do not proceed.
- Runs never resume on their own. The user clears the flag, then resumes or cancels each run. A resumed run passes through Checking. A browser run reopens the form, and the user reviews again.
- Setting and clearing the pause each write an audit row.

## 9. Spec section 11 coverage

| Spec requirement | Requirement ID | Where |
|---|---|---|
| Nine agents | FR-11.1 to FR-11.9 | Section 4 |
| Deterministic workflow first | FR-11.10 | Section 1 |
| Restricted permissions | FR-11.11 | Sections 3 and 4 |
| Execution logs | FR-11.12 | Section 7 |
| Timeouts | FR-11.13 | Section 4 |
| Retries | FR-11.14 | Section 4 |
| Checkpoints | FR-11.15 | Sections 4 and 7 |
| Cancellation | FR-11.16 | Sections 4 and 7 |
