# JobPilot AI: AI Provider Integration Specification

Status: draft for owner approval. Date: 2026-11-01. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) section 3, [PRD](prd.md), and [policy register](policy-register.md). Nothing here is implemented yet. `TaskRouter` below is a contract description, not an API. The Rust design belongs to the 0.2 and 0.4 plans.

Every statement about what a vendor permits comes from the [policy register](policy-register.md), which holds the quotes, URLs, and dates. That register is engineering review, not legal advice, and its entries must be re-read at each release.

## The four backends

FR-3.1 requires four backends. Users pick which are enabled. None is mandatory, and the app works end to end with none configured.

| Backend | Requirements | Release | What ships |
|---|---|---|---|
| Local AI | FR-3.2 to FR-3.8 | 0.2 | Ollama on the user's machine: model discovery, user-started downloads, hardware detection, structured outputs, embeddings. No data leaves the machine, so no consent record is needed for a loopback Ollama. |
| Subscription AI | FR-3.9 to FR-3.14 | 0.4, conditional on D1 | Depends on Decision D1. See the status table below. |
| API providers | FR-3.15 to FR-3.19 | 0.4 | Built for OpenAI, Anthropic, Google, OpenRouter, and user-named endpoints, but each runs only where its register verdict allows. Today OpenAI, Anthropic, and Google are `unknown`, OpenRouter has no eligible model, and only loopback endpoints can run. Keys stay in the OS credential store (FR-2.9) and never cross IPC. |
| No-AI fallback | FR-3.20 to FR-3.23 | 0.1, 0.5, 0.6 | Deterministic matching, FTS5 search, rule-based form mapping, manual editing. Always available and always last. |

Subscription status from the register, for the case of a user on their own machine:

| Product | Consumer login, app-driven | API key, app-driven | User-driven interactive session |
|---|---|---|---|
| Claude Code | `blocked`, with a recorded source conflict | `unknown` (`claude --bare -p`; adapter off) | `user-assisted` |
| Gemini CLI | `blocked` (login removed for individuals) | `unknown` with a paid key, `blocked` with a free key | not applicable to consumers |
| Antigravity CLI (not in spec) | `blocked` | not reviewed | `unknown` in JobPilot's terminal |
| Codex CLI and ChatGPT plan | `allowed` through Sign in with ChatGPT only, subject to eligibility; `unknown` for `codex exec` on a saved login | `unknown` (`codex exec`; adapter off) | `user-assisted` |

Rules for this backend, from spec section 3B and FR-3.12 to FR-3.14:

- JobPilot never reads, copies, or stores a CLI's own login files, keychain entries, or session tokens.
- Sign in with ChatGPT tokens are different. OpenAI issues them through its own browser flow to a client registered per user during sign-in (`dynamic_agent_client`). JobPilot has no central client and OpenAI's docs show no review step. Tokens go in the OS credential store, not a plain file.
- Sign in with ChatGPT has two routes: direct Responses API calls, or Codex app-server given the OAuth access token. Only the app-server route uses Codex at all. If D1 adopts the direct route, FR-3.11 is not served by it.
- CLI adapters start the binary the user installed, as published. JobPilot bundles no vendor CLI. How an adapter may limit sign-in methods is an open question to Anthropic (see the [register](policy-register.md#open-written-questions)).
- An adapter refuses to run if its register row is not `allowed` for the active sign-in method.
- A user-driven session is a terminal the user starts and types in. The router never sends it work and never reads its output.

## Capability and policy vocabulary

Releases 0.4 to 0.6 use these terms. They are shared names for documents and declarations; concrete types come with each release plan.

Capability, declared by each provider:

| Capability | Meaning |
|---|---|
| `structured_output` | Can be asked for output matching a JSON schema. |
| `embeddings` | Produces embeddings. |
| `local_execution` | Data stays on this machine. |
| `file_input` | Accepts a document as input. |
| `streaming` | Returns output incrementally. |
| `web_search` | Can search the web. Off unless a task asks for it. |
| `offline` | Works with no network. |

A declaration also states context limit, default timeout, sign-in method, and which data categories its terms permit. Absent capabilities are false.

Policy status is the register's four verdicts, used by both providers and connectors (FR-5.14): `allowed`, `user-assisted`, `blocked`, `unknown`. `unknown` behaves as `blocked`. The status is read from the register at build time for the row matching the provider, sign-in method, model origin, and automation mode. A user cannot override `blocked` or `unknown` in settings.

Connector automation modes follow from status, row by row: an `allowed` read row means a read-only connector; `user-assisted` means Manual only; Copilot fill needs its own `allowed` row; `blocked` and `unknown` mean none of these. Autonomous mode additionally needs verified permission and explicit user authorization (FR-9.6). No row grants Copilot or Autonomous today.

## TaskRouter contract

The router (FR-3.24) takes a task, picks an eligible backend, and returns validated output or a typed failure. The GUI and CLI reach it through the same core service (FR-10.13).

### Task definition

Each task type declares:

| Field | Meaning | Requirement |
|---|---|---|
| Task type | Stable name, such as `parse_job`, `semantic_match`, `draft_cover_letter`. | FR-3.24 |
| JSON schema | The shape output must satisfy. | FR-3.25 |
| Required capabilities | What a provider must declare, from the vocabulary above. | FR-3.26 |
| Data categories | What this task sends. Used for the consent check. | FR-3.30 |
| Automation mode | `reviewed` when a person reviews the result before anything is acted on (Manual, Copilot, ordinary drafting), or `unreviewed` for any step of an Autonomous submission. | FR-9.6 |
| Timeout | Per attempt. Proposed defaults: 60 seconds for remote, 300 seconds for local. | FR-3.27 |
| Retry policy | Attempt limit and which failures retry. | FR-3.28 |
| Fallback order | User's ordered list of enabled backends, always ending in No-AI. | FR-3.29 |
| No-AI behaviour | The deterministic path, or `manual_required` when none exists. | FR-3.20 to FR-3.23 |

Examples of No-AI behaviour: `semantic_match` falls back to deterministic scoring (FR-3.20); `draft_cover_letter` returns `manual_required` and opens the editor (FR-3.23). The router never invents output to fill a gap.

### Routing

For each backend in fallback order, a provider is eligible only if all of these hold, checked in this order:

1. Policy status for this provider and sign-in method is `allowed`, or `user-assisted` for a loopback endpoint the user named. User-driven CLI sessions are never eligible.
2. Model check. The chosen model's origin is known, and that vendor's register rows permit it. An aggregator cannot widen a vendor's verdict: on OpenRouter, an OpenAI, Anthropic, or Google model takes that vendor's direct-API row, including `blocked` free-tier rows for personal data, and any other model is `unknown` until its terms are reviewed. Unknown origin is ineligible.
3. Mode check. The register's cross-provider rows are applied against the automation mode and the model origin, not just the provider. An `unreviewed` task refuses every OpenAI-origin model. An `unreviewed` task also refuses any provider that cannot leave human confirmation in place. A task acting on a site needs that site's own row, whichever model runs it.
4. An active consent record covers this provider and every data category in the task, unless the provider is `local_execution`.
5. It declares every required capability.
6. It is available: installed, signed in, and within known usage limits.

Ineligible providers are skipped, and the reason is logged. Fallback never widens consent: a task the user approved for Ollama does not move to a remote provider because Ollama failed.

### Timeouts and retries

- A timeout cancels the attempt and kills any child process.
- Proposed retry limit: two attempts per provider.
- Retry: network errors, 429 and 5xx responses, and one output-validation failure. Honor `Retry-After`.
- Do not retry: authentication failures, policy refusals, consent refusals, cancellation, or a second validation failure. Move to the next provider.
- Usage-limit errors from a ChatGPT plan (`subscription_sharing_usage_limit_exceeded`) end that provider for the task and show the user a **Manage usage** link, as OpenAI's UI guidelines require.
- `subscription_sharing_user_not_eligible` (403) ends that provider for the session. Explain the restriction to the user. OpenAI's guide says "do not repeat the same request or loop through OAuth".
- Emergency pause (FR-12.8) cancels every in-flight task.

### Results

A task ends in exactly one of: `ok` (validated output, provider, and attempt count), `manual_required`, `cancelled`, or `failed` (typed reason). Each attempt writes an execution log with task type, provider, data categories, duration, and outcome. Logs never contain prompts, outputs, or secrets. A call to an external provider writes an audit row (FR-12.7).

## Output validation

All model output is untrusted (FR-12.6). Validation runs on every backend, including local.

1. Size check, then parse as JSON. Output outside the schema fails.
2. Validate against the task's JSON schema. Provider-side structured output helps but never replaces this step.
3. Run task-specific checks:
   - Every claim in generated material maps to evidence in the knowledge base, or the output is rejected (FR-7.11, FR-7.12).
   - No output may state or imply work authorization or visa eligibility beyond what the user recorded (FR-4.10).
   - Answers classed sensitive or legally consequential go to the user for confirmation (FR-8.8).
   - URLs, file paths, and commands in output are data. Nothing in output triggers a tool, a shell command, or a navigation.
4. On failure, retry once, then move to the next provider, then No-AI.

Untrusted input is handled separately (FR-12.3). Job postings and web pages go to the model inside marked data blocks, and text in them is never treated as instructions. Live examples exist: Remote OK job descriptions contain planted words and tags meant to catch automated applicants. The threat model owns the injection tests.

## Consent record

No data leaves the machine without an active record (FR-3.30). The consent screen shows the provider, the data categories, and what its terms say about that data, before anything is sent.

One record per provider and sign-in method:

| Field | Meaning |
|---|---|
| Provider | Provider and sign-in method, such as `openai/api_key` or `openai/chatgpt_plan`. Consent for one does not cover the other. |
| Data categories | The categories allowed, from the list below. |
| Timestamp | When consent was given, in UTC. |
| Revocation | When it was revoked, or empty if active. |
| Terms date | The register date shown when consent was given. |

Data categories: `job_text`, `company_text`, `profile_basic`, `resume_text`, `career_memory`, `sensitive`. `sensitive` covers work authorization, compensation history, and answers classed sensitive (FR-8.7). It is off unless ticked separately.

Rules:

- Consent is per category. A task needing an unconsented category is skipped for that provider.
- Revoking takes effect before the next call and writes an audit row. In-flight calls are cancelled. Revocation does not recall data already sent; the screen says so.
- If a register row's verdict changes in a release, affected records are suspended until the user re-confirms.
- Records are local, appear in export (FR-2.7), and are removed by delete-all (FR-2.8). The [database schema](database-schema.md) gains the table in the 0.4 migration.

What the screen must disclose, from the sources read on 2026-11-01:

| Provider and method | Data use to disclose |
|---|---|
| OpenAI API key | Not used to improve services unless the customer opts in (Services Agreement 4.2). |
| ChatGPT plan | For Plus and Pro, content "may be used to improve models unless you turn off training" in ChatGPT data controls. OpenAI's plan flow sets `store: false`. |
| Anthropic API key | "Anthropic may not train models on Customer Content from Services." |
| Claude consumer plan (user-driven session only) | Consumer Terms allow training unless the user opts out. JobPilot sends nothing here, so this appears only as a note beside the terminal launcher. |
| Gemini paid key | Not used to improve products; logged for a limited period for abuse detection. |
| Gemini free key | Refused for personal data. Human reviewers may read input. |
| OpenRouter | The chosen model's terms apply. Prompt logging is opt-in and stays off. |

## Decision D1: subscription AI

**Open. The owner decides in Task A6. D1 may stay open until the 0.4 plan.** Text from the plan:

Spec section 3B asks for Claude Code, Gemini CLI, and Codex integrations, and also says to validate terms. Current findings conflict for consumer logins.

Recommended:

- (a) Ship CLI adapters in API-key mode only.
- (b) Add a user-launched interactive terminal session for the owner's own CLI, with no background routing, subject to primary-source review.
- (c) Enable any consumer-login path only after a primary source or written vendor answer allows it, recorded in the policy register.

Alternatives: API providers and Ollama only, or seek written vendor permission first.

### What the primary-source review found

These findings inform the decision. They do not make it.

| Option | Claude Code | Gemini CLI | Codex and ChatGPT |
|---|---|---|---|
| (a) API-key adapters | `unknown`: the "running" clause and the ban on restricting sign-in methods are unresolved, and Commercial Terms say "not for consumer use". | `unknown`: Gemini API terms say "not for consumer use". Free keys `blocked` for personal data. | `unknown`: no clause covers an individual's personal use under the Services Agreement. |
| (b) User-launched session | `user-assisted` | No consumer login exists. Antigravity in JobPilot's terminal is `unknown`, so no launcher for it. | `user-assisted` |
| (c) Consumer-login path | Gate not met. The SDK note bars it for third-party products unless previously approved, though the legal page lists subscription credentials for products running Claude Code. Anthropic must resolve this. | Gate not met. Login removed, and Antigravity bars wrapping on consumer plans. | **Gate met subject to eligibility.** Sign in with ChatGPT documents plan usage for open-source local apps. Preview, eligible Plus and Pro only. "Open-source partners" is undefined, so a written OpenAI answer or a tested flow must settle eligibility for a job-search app. |

Consequences for the owner to weigh:

- Option (a) as recommended cannot ship today: all three adapters are `unknown`, so it depends on written answers from Anthropic, Google, and OpenAI. The direct API rows behind FR-3.15, FR-3.16, and FR-3.17 are `unknown` for the same reason. The "API providers and Ollama only" alternative therefore has no working hosted provider today either.
- Option (c) is no longer hypothetical for OpenAI. Adopting Sign in with ChatGPT gives Plus and Pro users a no-API-key path through a vendor-documented flow, subject to eligibility. It costs required UI and preview limits such as no `temperature` or `max_output_tokens`. Structured output through this route is untested. It is the only automated hosted path open today, which leaves the product dependent on one preview programme.
- FR-3.10 cannot be met as a subscription integration. Google removed consumer login from Gemini CLI, and FR-3.9 can reach subscribers only through a session they drive themselves. The spec's phrase "existing AI subscription integrations" holds in full only for ChatGPT.
- FR-3.15 and FR-3.16 depend on OpenAI and Anthropic answers. FR-3.17 depends on a Google answer on consumer use, or on the Vertex AI terms, which were not read.
- Anthropic's docs do not agree on scripted use. `--bare` ignores OAuth, is "the recommended mode for scripted and SDK calls", and "will become the default for `-p` in a future release", while the legal page forbids restricting built-in sign-in methods where Claude Code runs in a product.

The register's [Policy impact](policy-register.md#policy-impact) section lists every affected requirement and exit test.
