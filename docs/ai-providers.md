# JobPilot AI: AI Provider Integration Specification

Status: draft for owner approval, with the 0.2 implementation noted inline. Date: 2026-11-01. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) section 3, [PRD](prd.md), and [policy register](policy-register.md). Release 0.2 implements the router for Ollama, Claude Code, and No-AI; `Router` in `crates/core/src/ai/router.rs` is the Rust form of `TaskRouter` below. Release 0.4 adds OpenAI, Anthropic, Google (paid key), OpenRouter, a loopback endpoint, and the optional Gemini CLI and Codex CLI adapters; see [As built in 0.4](#as-built-in-04). Codex ships inert.

Every statement about what a vendor permits comes from the [policy register](policy-register.md), which holds the quotes, URLs, and dates. That register is engineering review, not legal advice, and its entries were to be re-read at each release. The owner waived that re-read for the AI provider rows (it stays for the site rows 0.5 and 0.6 rely on), so no release step re-reads a provider's terms. The owner decided on 2026-11-01 that JobPilot is a personal, non-commercial app and waived AI subscription policy concerns, accepting any account risk (decisions D1 and D4). The register's [owner decisions](policy-register.md#owner-decisions-2026-11-01) set what the product does with AI provider rows. The waiver covers vendor terms only: `no_tools`, the mode check, and consent all still apply.

## The four backends

FR-3.1 requires four backends. Users pick which are enabled. None is mandatory, and the app works end to end with none configured.

| Backend | Requirements | Release | What ships |
|---|---|---|---|
| Local AI | FR-3.2 to FR-3.8 | 0.2 | Ollama on the user's machine: model discovery, user-started downloads, hardware detection, structured outputs, embeddings. No data leaves the machine, so no consent record is needed for a model that runs locally under loopback Ollama. Ollama cloud models do not qualify, see routing check 2. |
| Subscription AI | FR-3.9 to FR-3.14 | 0.2 (Claude Code), 0.4 (optional Gemini CLI, Codex CLI) | Claude Code through the official `claude` CLI and its own login: a user-driven session in the terminal, and a headless backend (`claude -p` style) the router may call. Gemini CLI and Codex CLI adapters are optional and use whatever sign-in the installed CLI supports. A CLI that is not installed shows as unavailable. |
| API providers | FR-3.15 to FR-3.19 | 0.4 (D4 decided) | OpenAI, Anthropic, Google (paid key), OpenRouter, and user-named endpoints. Under standard BYOK the first four are eligible subject to consent and capability checks; the consent screen shows the vendor's terms. Free Gemini keys, non-loopback endpoints, Vertex AI, and Ollama cloud models stay ineligible. Keys stay in the OS credential store (FR-2.9) and never cross IPC. |
| No-AI fallback | FR-3.20 to FR-3.23 | 0.1, 0.5, 0.6 | Deterministic matching, FTS5 search, rule-based form mapping, manual editing. Always available and always last. |

Subscription status from the register, for the case of a user on their own machine. These are the vendor verdicts. The last column is what the product does under the owner decisions.

| Product | Consumer login, app-driven | API key, app-driven | User-driven interactive session | Product (owner decisions) |
|---|---|---|---|---|
| Claude Code | `blocked`, with a recorded source conflict | `unknown` (`claude --bare -p`) | `user-assisted` | Headless adapter and terminal session ship (D1), on the CLI's own login. |
| Gemini CLI | `blocked` (login removed for individuals) | `unknown` with a paid key, `blocked` with a free key | not applicable to consumers | Optional adapter on the installed CLI's sign-in (D1). Free key refused for personal data. |
| Antigravity CLI (not in spec) | `blocked` | not reviewed | `unknown` in JobPilot's terminal | No adapter or launcher; not covered by D1. |
| Codex CLI and ChatGPT plan | `allowed` through Sign in with ChatGPT only, subject to eligibility; `unknown` for `codex exec` on a saved login | `unknown` (`codex exec`) | `user-assisted` | Optional `codex exec` adapter on the installed CLI's sign-in (D1). Sign in with ChatGPT stays optional. |

Rules for this backend, from spec section 3B and FR-3.12 to FR-3.14:

- JobPilot never reads, copies, or stores a CLI's own login files, keychain entries, or session tokens.
- Sign in with ChatGPT tokens are different. OpenAI issues them through its own browser flow to a client registered per user during sign-in (`dynamic_agent_client`). JobPilot has no central client and OpenAI's docs show no review step. Tokens go in the OS credential store, not a plain file.
- Sign in with ChatGPT has two routes: direct Responses API calls, or Codex app-server given the OAuth access token. Only the app-server route uses Codex at all. If the direct route is used, FR-3.11 is not served by it.
- CLI adapters start the binary the user installed, as published, and the CLI uses its own login. JobPilot bundles no vendor CLI, does not extract tokens, and does not reverse engineer a CLI. An adapter does not alter the child environment to change which sign-in method the CLI uses.
- The Claude Code headless adapter passes job text only with tools disabled (`no_tools`). Other adapters meet the same rule or are ineligible for tasks with `job_text` or `company_text`.
- An adapter whose CLI is not installed or not signed in shows as unavailable, and routing skips it (check 6).
- A user-driven session is a terminal the user starts and types in. The router never sends it work and never reads its output.

## Capability and policy vocabulary

Releases 0.2 to 0.6 use these terms. They are shared names for documents and declarations; concrete types come with each release plan.

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
| `no_tools` | The model gets no shell, file, browser, or search tool on this provider. Proven by an adapter test where a CLI is involved ([threat model](security/threat-model.md#7-cli-adapters-and-untrusted-text)). |

A declaration also states context limit, default timeout, sign-in method, and which data categories its terms permit. Absent capabilities are false.

Tasks with `job_text` or `company_text` require `no_tools`. Ollama declares it from 0.2, because no tool is offered through its API call.

Policy status is the register's four verdicts, used by both providers and connectors (FR-5.14): `allowed`, `user-assisted`, `blocked`, `unknown`. For connectors and sites, `unknown` behaves as `blocked`, except Copilot fill on a row recording "no clause found" (D2). For AI providers, eligibility follows the register's [owner decisions](policy-register.md#owner-decisions-2026-11-01). The status is read from the register at build time for the row matching the provider, sign-in method, model origin, and automation mode. A user cannot override a site row that is `blocked`, or a provider the owner decisions leave ineligible, in settings.

Connector automation modes follow from status, row by row: an `allowed` read row means a read-only connector; `user-assisted` means Manual only; Copilot fill needs its own `allowed` row or, under D2, a row recording "no clause found"; `blocked` means none of these, and `unknown` means none except that Copilot case. Autonomous mode additionally needs verified permission and explicit user authorization (FR-9.6), is default off, and no row grants it today.

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
| Timeout | Per attempt. Defaults: 180 seconds for Claude Code, 300 seconds for local. | FR-3.27 |
| Retry policy | Attempt limit and which failures retry. | FR-3.28 |
| Fallback order | User's ordered list of enabled backends, always ending in No-AI. | FR-3.29 |
| No-AI behaviour | The deterministic path, or `manual_required` when none exists. | FR-3.20 to FR-3.23 |

Examples of No-AI behaviour: `semantic_match` falls back to deterministic scoring (FR-3.20); `draft_cover_letter` returns `manual_required` and opens the editor (FR-3.23). The router never invents output to fill a gap.

### Routing

For each backend in fallback order, a provider is eligible only if all of these hold, checked in this order:

1. Owner-decision check. The provider and sign-in method are eligible under the register's [owner decisions](policy-register.md#owner-decisions-2026-11-01): the Claude Code headless adapter, the optional Gemini CLI and Codex CLI adapters, direct OpenAI, Anthropic, and Google (paid key), OpenRouter, Sign in with ChatGPT, and a loopback endpoint the user named. Ollama running a local model on loopback has no third party and needs no register row. A provider the decisions leave out (free Gemini key, Antigravity, non-loopback endpoints, Vertex AI, Ollama cloud models) is ineligible, and a setting cannot change that. User-driven CLI sessions are never eligible.
2. Model check. `local_execution` is decided per model, not per provider. Ollama can serve cloud models through the same loopback address after sign-in, so a loopback address does not prove local execution. A model counts as local only when the 0.2 plan's detection confirms it; otherwise it is ineligible, and it skips no check. A confirmed local model skips this check: it has no vendor row, and its licence is the user's concern. For every other provider, the chosen model's origin is known, and that vendor's rows, read with the owner decisions, permit it. An aggregator cannot widen a vendor's verdict: on OpenRouter, an OpenAI, Anthropic, or Google model takes that vendor's direct-API row, including `blocked` free-tier rows for personal data, and any other model is `unknown` until its terms are reviewed. Unknown origin is ineligible.
3. Mode check. The register's cross-provider rows are applied against the automation mode and the model origin, not just the provider. An `unreviewed` task refuses every OpenAI-origin model. An `unreviewed` task also refuses any provider that cannot leave human confirmation in place. A task acting on a site needs that site's own row, whichever model runs it.
4. An active consent record covers this provider and every data category in the task, unless the chosen model is confirmed local.
5. It declares every required capability.
6. It is available: installed, signed in, and within known usage limits.

Ineligible providers are skipped, and the reason is logged. Fallback never widens consent: a task the user approved for Ollama does not move to a remote provider because Ollama failed.

Providers run in the stored `ai.order`, then every enabled provider it leaves out, so enabling a provider is enough to route it and the AI screen shows the same order the router uses. When every provider passes, `manual_required` names what would change the result: the reason from a real attempt (`not_logged_in`, `prompt_too_large`, `unusable_reply`, `output_truncated`), else `consent_needed` when consent alone held a provider back, else `no_model_set` or `model_not_installed` when Ollama is on but lacks a drafting model or the chosen one, else `provider_unavailable` when one is on but cannot be reached, else `no_ai`.

### Timeouts and retries

- A timeout cancels the attempt and kills any child process.
- Retry limit: the task's `attempts` per provider (two for parsing and drafting, one for `classify_answer`).
- Retry, local provider (Ollama): network errors, 429 and 503 (after the `Retry-After` wait, else one second, at most 30 seconds), and one output-validation failure. Other 5xx responses move to the next provider. At temperature 0 a retry usually repeats the reply, so this rarely rescues a local model.
- Retry, external provider (decision 11): one retry on 429, 503, or 529, only when `Retry-After` gives whole seconds and at most 30. No header, a date form, or a longer wait ends that provider with `rate_limited`. Quota and spend-cap errors never retry, and neither do timeouts, network errors, or other 5xx responses, because the attempt may already have spent usage. One output-validation failure is retried. A task with one attempt (`classify_answer`) never retries.
- Do not retry: a timeout or network error on an external provider (the attempt may already have spent usage), authentication failures, policy refusals, consent refusals, cancellation, or a second validation failure. Move to the next provider.
- A prompt whose estimate (UTF-8 bytes / 2, with system text and schema) passes a provider's context limit skips that provider, as does an overflow the provider reports. If nothing else takes the task, it ends `manual_required` with reason `prompt_too_large`. Ollama calls send `num_ctx`, `truncate: false`, and `shift: false`, so an overflow is an error and not a silent cut.
- Usage-limit errors from a ChatGPT plan (`subscription_sharing_usage_limit_exceeded`) end that provider for the task and show the user a **Manage usage** link, as OpenAI's UI guidelines require.
- `subscription_sharing_user_not_eligible` (403) ends that provider for the session. Explain the restriction to the user. OpenAI's guide says "do not repeat the same request or loop through OAuth".
- Emergency pause (FR-12.8) cancels every in-flight task.

### Results

A task ends in exactly one of: `ok` (validated output, provider, and attempt count), `manual_required`, `cancelled`, or `failed` (typed reason). Each attempt writes an execution log with task type, provider, data categories, duration, and outcome. Logs never contain prompts, outputs, or secrets. A call to an external provider writes an audit row (FR-12.7).

## As built in 0.2

The names below are the Rust names in `crates/core/src/ai/`. The vocabulary above is the contract; this is what the code does.

| Contract term | 0.2 code |
|---|---|
| `TaskRouter` | `Router` (`router.rs`). `Router::from_settings(&Db)` builds it from the user's order and enabled flags; `run(db, task, prompt, cancel)` and `embed(...)` take the `Db`, so the router holds none. |
| Task types | `TaskType::{ParseJob, DraftResume, DraftCoverLetter, ClassifyAnswer, Embed}` with one `TaskSpec` each (`mod.rs`). `ClassifyAnswer` arrived in 0.3. `semantic_match` and `map_form_field` have no task yet. |
| Provider | trait `Provider` (`declaration`, `available`, `complete`, `embed`). Implementations: `Ollama`, `ClaudeCode`, `NoAi`. `ProviderId` has exactly those three variants, so a provider the owner decisions leave out has no way in. |
| Results | `Outcome::{Ok, ManualRequired, Cancelled, Failed}`. |
| Cancellation | one `CancellationToken` type, polled every 100 ms. Revoking consent, Ctrl-C, and app exit fire it, and a dropped attempt kills its child. |
| Consent key | `ProviderId::ClaudeCode` displays as `claude_code/cli_login`, and that text is the `consent.provider` value. |

**`classify_answer` as built in 0.3.** The task sees a question's text only, never an answer, declares `job_text` so it needs a `no_tools` provider and consent, and may only raise a class: a lower class from the model is ignored, and a question whose class a person set (`class_source = user`) is skipped before any call. It is one attempt with no retry (`schemas/classify_answer.json`), and a refresh makes at most 20 calls (`CLASSIFY_BATCH`). Only a person starts a refresh, and the GUI and CLI say how many Claude calls it will make first. Two reasons were added to the allow-list that the log and the messages use: `no_model_set` (Ollama is enabled with no drafting model chosen) and `model_not_installed` (the chosen model is not among the installed ones). The router also refuses every task while the emergency pause is on, between attempts too.

**Ollama local-model detection.** A model counts as local only when its `/api/tags` row has no `remote_model` and no `remote_host` (absent or empty), `size > 0`, and a name that neither ends in `:cloud` nor contains `-cloud`. Any other row is remote: hidden from the screen and ineligible in routing check 2. The same rule gates the embedding model, so a `:cloud` embedding model cannot pass as local. A fixture test pins both cases. Evidence: https://docs.ollama.com/api/tags and https://docs.ollama.com/cloud, read 2026-11-01. Re-check at each release.

**Claude Code argv.** The adapter runs the `claude` the user installed, found, in order, through `JOBPILOT_CLAUDE_BIN`, the `ai.claude_path` setting, the `PATH`, the usual install folders, and a login-shell lookup (a GUI launch does not inherit the shell's `PATH`). The job text goes to standard input and never to argv. The argument list is fixed:

```text
-p --output-format json --json-schema <schema> --tools "" --allowedTools StructuredOutput
--strict-mcp-config --disable-slash-commands --setting-sources "" --no-session-persistence
--permission-mode dontAsk --system-prompt <system> [--model <model>]
```

`--tools ""` removes every built-in tool. `--allowedTools StructuredOutput` keeps the one tool `--json-schema` needs to hand the reply back; `--disallowedTools "*"` was tried and turns the reply into free text (real run, 2.1.316). `--bare` is never used, because it ignores the OAuth login. The child runs in an empty temporary directory with an allow-listed environment, 180 seconds per attempt. A test pins the list (`argv_is_exactly_the_verified_list`), and an ignored smoke test (`real_claude_has_no_tools_smoke`, which also needs `JOBPILOT_REAL_CLAUDE_SMOKE=1`) checks a real `init` event for the tool list. The plugins the owner installed still appear in `init`; see the [threat model](security/threat-model.md#7-cli-adapters-and-untrusted-text).

**Thinking and truncation on Ollama.** Calls send `think: false`. Left on, thinking counts against `num_ctx`: `qwen3:4b` spent all 8,192 tokens on it in about 90 s and returned no content. `done_reason: length` is `Failed("output_truncated")`, as on the hosted wires, and is not retried: at temperature 0 a retry repeats the same cut. The drafting prompt also caps a resume at 3 summary items, 5 entries in a section, and 4 bullets in an entry, because `gemma4` otherwise wrote out a twelve-job knowledge base and ran out of window. The caps hold for every provider, so a tailored draft can leave out older jobs. These were measured on Ollama 0.33.1 with `gemma4:latest` and `qwen3:4b`, on 2026-11-02.

**Retries as built.** Ollama is retried on network errors, 429, 503, and one validation failure. Claude Code is retried once after a validation failure only, never after a timeout or a broken call, because the attempt may already have spent usage. A prompt over a provider's context limit skips it with `prompt_too_large`; Ollama reports an overflow as an error because the call sends `num_ctx`, `truncate: false`, and `shift: false`. The overflow wording was tested against a fake server only.

## Output validation

All model output is untrusted (FR-12.6). Validation runs on every backend, including local.

1. Size check, then parse as JSON. Output outside the schema fails.
2. Validate against the task's JSON schema. Provider-side structured output helps but never replaces this step.
3. Run task-specific checks:
   - Every line of generated material is a claim that cites evidence ids from the list the prompt supplied (FR-7.11, FR-7.12). A model writes each line once with what it cites (`schemas/draft_resume.json`: `summary`, and `sections` of entries whose title, subtitle, and dates share the entry's `evidence_ids` and whose bullets cite their own), and Rust builds the document and its claims from that one tree, so the claims are the lines by construction. The name and contact are the profile's: the schema has no field for them. A section heading comes from the schema's `enum`, compared after ASCII lower-casing, so "Experience" from a provider that only reads the schema as text passes and an invented heading does not. Before any check, the assembly cuts evidence ids out of every line of a resume or letter: bracketed tokens that are a listed id or shaped like one (32 hex digits), and listed ids standing alone. Other brackets stay. Only `evidence_ids` cites, so a line whose only id was in its prose is still flagged. A reply outside this shape, or with no text, stores nothing and fails validation inside the attempt, so the router retries it once and then ends with `unusable_reply`. `create_version` still compares claims to lines for every generated or edited version. A claim that cites no usable evidence (an id outside the list counts as none), or that states work authorization, is **stored flagged** (`no_evidence`, `authorization_statement`) and blocks export until the user edits or removes it. It is not rejected and not retried. Matching is by id and by word, so a paraphrase can pass; the user's review is the final control.
   - No output may state or imply work authorization or visa eligibility beyond what the user recorded (FR-4.10).
   - Answers classed sensitive or legally consequential go to the user for confirmation (FR-8.8).
   - URLs, file paths, and commands in output are data. Nothing in output triggers a tool, a shell command, or a navigation.
4. On a failed size, JSON, or schema check, retry once, then move to the next provider, then No-AI. A flagged claim is not a failure of this kind: the draft is stored and the user resolves the flag.

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
- A restore revokes every external consent record, and audits it as `consent_revoked_by_restore`. Consent is a decision made now, so a backup cannot bring one back.
- Revoking takes effect before the next call and writes an audit row. In-flight calls are cancelled. Revocation does not recall data already sent; the screen says so.
- If a register row's verdict changes in a release, affected records are suspended until the user re-confirms.
- Records are local, appear in export (FR-2.7), and are removed by delete-all (FR-2.8). The [database schema](database-schema.md) gains the table in the 0.4 migration.

What the screen must disclose, from the sources read on 2026-11-01:

| Provider and method | Data use to disclose |
|---|---|
| OpenAI API key | Not used to improve services unless the customer opts in (Services Agreement 4.2). |
| ChatGPT plan | For Plus and Pro, content "may be used to improve models unless you turn off training" in ChatGPT data controls. OpenAI's plan flow sets `store: false`. |
| Anthropic API key | "Anthropic may not train models on Customer Content from Services." |
| Claude Code on the user's own plan (headless adapter and terminal session) | Consumer Terms allow training unless the user opts out. The consent screen and the terminal launcher both show this. |
| Gemini paid key | Not used to improve products; logged for a limited period for abuse detection. |
| Gemini free key | Refused for personal data. Human reviewers may read input. |
| Gemini CLI or Codex CLI (optional adapters) | The terms of whatever sign-in the installed CLI uses, shown by name on the consent screen. |
| Any API provider under standard BYOK | The vendor's terms, including any "not for consumer use" wording, are shown on the consent screen so the user sees them before consenting. |
| OpenRouter | The chosen model's terms apply. Prompt logging is opt-in and stays off. |

## Decision D1: subscription AI

**Decided (2026-11-01).** Claude Code is included on the owner's own subscription through the official `claude` CLI and its own login, in two uses: a user-driven session in the app's terminal, and a headless backend (`claude -p` style) the task router may call. No token extraction, no reverse engineering. FR-3.9 is met this way. The headless adapter passes job text only with tools disabled. Gemini CLI and Codex CLI adapters are optional and may use whatever sign-in the installed CLI supports; absent CLIs show as unavailable. The owner waived subscription policy concerns for this personal, non-commercial app and accepts any account risk. The options and evidence below are kept as the record. Text from the plan:

Spec section 3B asks for Claude Code, Gemini CLI, and Codex integrations, and also says to validate terms. Current findings conflict for consumer logins.

Recommended:

- (a) Ship CLI adapters in API-key mode only.
- (b) Add a user-launched interactive terminal session for the owner's own CLI, with no background routing, subject to primary-source review.
- (c) Enable any consumer-login path only after a primary source or written vendor answer allows it, recorded in the policy register.

Alternatives: API providers and Ollama only, or seek written vendor permission first.

Option (a) cannot ship today unless D4 selects standard BYOK or a vendor answers in writing, because all three adapters are `unknown`. FR-3.10 cannot be met as a subscription integration. Sign in with ChatGPT is the only vendor-documented automated hosted path, and it is a preview.

### What the primary-source review found

These findings inform the decision. They do not make it.

| Option | Claude Code | Gemini CLI | Codex and ChatGPT |
|---|---|---|---|
| (a) API-key adapters | `unknown`: the "running" clause and the ban on restricting sign-in methods are unresolved, and Commercial Terms say "not for consumer use". | `unknown`: Gemini API terms say "not for consumer use". Free keys `blocked` for personal data. | `unknown`: no clause covers an individual's personal use under the Services Agreement. |
| (b) User-launched session | `user-assisted` | No consumer login exists. Antigravity in JobPilot's terminal is `unknown`, so no launcher for it. | `user-assisted` |
| (c) Consumer-login path | Gate not met. The SDK note bars it for third-party products unless previously approved, though the legal page lists subscription credentials for products running Claude Code. Anthropic must resolve this. | Gate not met. Login removed, and Antigravity bars wrapping on consumer plans. | **Gate met subject to eligibility.** Sign in with ChatGPT documents plan usage for open-source local apps. Preview, eligible Plus and Pro only. "Open-source partners" is undefined, so a written OpenAI answer or a tested flow must settle eligibility for a job-search app. |

Consequences for the owner to weigh:

- Option (a) as recommended cannot ship today: all three adapters are `unknown`, so it depends on [Decision D4](#decision-d4-user-held-api-keys) or on written answers from Anthropic, Google, and OpenAI. D4 alone does not open the Claude Code adapter, whose row turns on the "running" clause. The direct API rows behind FR-3.15, FR-3.16, and FR-3.17 are `unknown` for the same reason. Under strict, the "API providers and Ollama only" alternative therefore has no working hosted provider today either.
- Option (c) is no longer hypothetical for OpenAI. Adopting Sign in with ChatGPT gives Plus and Pro users a no-API-key path through a vendor-documented flow, subject to eligibility. It costs required UI and preview limits such as no `temperature` or `max_output_tokens`. Structured output through this route is untested. It is the only automated hosted path open today, which leaves the product dependent on one preview programme.
- FR-3.10 cannot be met as a subscription integration. Google removed consumer login from Gemini CLI, and FR-3.9 can reach subscribers only through a session they drive themselves. The spec's phrase "existing AI subscription integrations" holds in full only for ChatGPT.
- FR-3.15 and FR-3.16 depend on OpenAI and Anthropic answers. FR-3.17 depends on a Google answer on consumer use, or on the Vertex AI terms, which were not read.
- Anthropic's docs do not agree on scripted use. `--bare` ignores OAuth, is "the recommended mode for scripted and SDK calls", and "will become the default for `-p` in a future release", while the legal page forbids restricting built-in sign-in methods where Claude Code runs in a product.

The register's [Policy impact](policy-register.md#policy-impact) section lists every affected requirement and exit test.

## Decision D4: user-held API keys

**Decided (2026-11-01): standard BYOK.** A user-held key in this local client is the user's own use, with the vendor's terms shown on the consent screen. FR-3.15, FR-3.16, and FR-3.17 can ship. The register's verdicts stay as the record of what vendors say; the options and evidence below are kept as the record.

Spec section 3C offers optional API providers. The register's [one rule](policy-register.md#optional-api-providers-fr-315-to-fr-319) allows a key only where a primary clause covers an individual key owner using third-party client software. That rule came from a project ruling, not from the owner, and it decides the outcome: OpenAI, Anthropic, and Google direct APIs are `unknown`, OpenRouter has zero eligible models, and only loopback endpoints run.

The case for reading it differently: with a user's own key, the user, not JobPilot, is the vendor's contracting party. The register quotes Anthropic saying "This does not restrict how customers provision and manage their own API keys" and warning users about third-party tools, which acknowledges that keys are used in them. OpenAI's Usage Policies, in the text effective 2026-11-20, say the intermediary approval rule "does not apply solely because an application integrates OpenAI models". None of this reaches personal use: "not for consumer use" still stands in the Anthropic and Google terms, and OpenAI's agreement covers an organization.

| Option | Meaning | Consequence |
|---|---|---|
| Strict (current) | A key opens only where a primary clause covers this case. | Nothing hosted runs on a user's key until a vendor answers in writing, and FR-3.15 to FR-3.17 wait on answers that may never come. No user carries a terms risk from a key. |
| Standard BYOK | A user-held key in a local client is the user's own use. JobPilot shows the vendor's terms on the consent screen, and the user decides. | OpenAI, Anthropic, and paid Google direct calls can ship in 0.4, and OpenRouter's OpenAI, Anthropic, and Google models inherit those rows. The register's rule and rows must be re-decided first. The vendor text stays unresolved, so a vendor could act against the user's own key or account. |

**What the product is today under strict:** Ollama, loopback endpoints, and No-AI, with three read connectors (Lever, RemoteOK, We Work Remotely), Manual apply only, and Sign in with ChatGPT if D1(c) is adopted.

Unchanged by either option: free Gemini keys stay `blocked`, non-loopback endpoints and OpenRouter's other models stay `unknown`, Vertex AI stays unread, and consent (FR-3.30), the mode check, and `no_tools` all still apply. The CLI adapters are settled separately by [Decision D1](#decision-d1-subscription-ai), and the [threat model](security/threat-model.md#7-cli-adapters-and-untrusted-text) keeps `codex exec` away from job text.

Recommended, and chosen: standard BYOK for direct API calls, with the [written questions](policy-register.md#open-written-questions) optional. A key the user created and pays for is the ordinary way these APIs are used, the spec asks for these providers, and the consent screen puts the terms in front of the person who carries the risk. Choosing strict is a reasonable answer if the owner prefers no terms exposure for users.
## As built in 0.4
Names are the Rust names in `crates/core/src/ai/`. `ProviderId` now has ten variants: `Ollama`, `ClaudeCode`, `NoAi`, `OpenAi`, `Anthropic`, `Google`, `OpenRouter`, `Endpoint`, `GeminiCli`, and `CodexCli`. A provider the owner decisions leave out (free Gemini key, Vertex AI, a non-loopback endpoint, Ollama cloud models) has no variant, so a setting cannot name it. Every new provider defaults off. The AI screen is headed "AI providers", with a section of the same name.

**Hosted client** (`net.rs`, `hosted_client`). Native TLS (Schannel on Windows, Security.framework on macOS), HTTPS only, TLS 1.2 or later, no environment proxy, no redirect, and a response read capped at 64 KiB for error bodies. macOS native TLS tops out at TLS 1.2, a residual if a vendor goes 1.3-only. The loopback client (`loopback_client`) serves Ollama and the endpoint, follows no redirect, and refuses every address that is not loopback over `http`. `real_tls_handshake` (ignored, `JOBPILOT_REAL_TLS=1`) sends an unauthenticated `GET` with no body to the four vendor hosts and accepts any HTTP status; CI runs it as the `TLS handshake` step on both systems, and it is the Schannel proof.

**Wires** (`api.rs`). No body carries `tools`, `tool_choice`, `functions`, or a search option, and every schema loses `maxLength` first (`provider_schema`) because Anthropic returns 400 on it. The router still enforces it.

| Provider | Request | Auth | Reply text |
|---|---|---|---|
| OpenAI | `POST https://api.openai.com/v1/chat/completions`, `response_format` `json_schema` with `strict: true`, `store: false`, no `temperature`, no token limit | `Authorization: Bearer` | `choices[0].message.content` |
| OpenRouter | `POST https://openrouter.ai/api/v1/chat/completions`, same body without `store`, with `provider: {data_collection: "deny", require_parameters: true}` | `Authorization: Bearer` | same; a `200` may hold a top-level `error` |
| Endpoint | `POST <url>/chat/completions`, same body without `store` or `provider`; the path is empty or `/v<1-2 digits>` | `Authorization: Bearer` only when a key is stored | same |
| Anthropic | `POST https://api.anthropic.com/v1/messages`, `max_tokens: 16000`, `output_config.format` `json_schema` | `x-api-key`, `anthropic-version: 2023-06-01` | the `text` block; thinking blocks are skipped |
| Google | `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`, `generationConfig` with `responseMimeType` and `responseJsonSchema` | `x-goog-api-key` | the `text` parts, joined; thought parts are skipped |

Header values holding a key are `set_sensitive`, and a key is never in a URL. A reply is a clean answer only when it ends normally: a tool call (`tool_calls` non-empty, a `tool_use` block, a `functionCall` or other tool part) is `permission_denied`; a refusal or safety block is `model_refused`; a length stop is `output_truncated`; a missing finish on the Anthropic and Google wires, or an error beside valid text, is `bad_reply`. The chat wire reads a missing finish as clean, because some local servers omit it. These shapes are checked against local fakes only; the real APIs have not accepted them (see the [0.4 release note](releases/0.4.0.md)).

**Statuses** (`classify_status`, which takes the wire):

| Status | Reason |
|---|---|
| 401, 403 | `auth_failed` |
| 402, or 429 holding `insufficient_quota`, `credit_balance_exhausted`, or `usage_limit` | `quota_exhausted` |
| 400 or 429 on the Anthropic wire holding `enforced_spend_limit_reached` or "you have reached your specified api usage limits" | `quota_exhausted` |
| 400 on the Google wire holding `API_KEY_INVALID` | `auth_failed`; any other Google 400 stays `http_400` |
| 404 | `model_not_found` |
| 400 or 413 holding `context_length_exceeded`, `maximum context length`, `prompt is too long`, or `exceeds the maximum number of tokens` | `prompt_too_large` |
| 429, 503, 529 | `rate_limited` after the retry rule above |
| anything else, 3xx included | `http_<n>` (task row: `failed`) |

`spend_limit` is no longer a quota word: the plan listed it, and it matched by accident. The spend-cap words are matched on the Anthropic wire alone and `API_KEY_INVALID` on the Google wire alone, so a local server's echoed text cannot steer routing there. `QUOTA_WORDS` and the overflow words still match on the endpoint wire: a hostile local server can pick `quota_exhausted` or `prompt_too_large`, which changes the reason shown and moves no data. `usage_limit` has no vendor source I can name. Google's bad-key body (a 400 with `API_KEY_INVALID`) is known only from forum sources, not from Google's documents, and no real key has produced it. `model_not_found` and `model_refused` end the task with their own reason on screen, not `no_ai`, because model ids are typed by hand. Provider error text and reply text never reach a log, a message, or `task_runs`; bodies are read only to match these fixed words.

**Models** (decision 23). The user types the vendor's id, per provider, from a safe character set. There is no default and no list. OpenRouter accepts an `openai/`, `anthropic/`, or `google/` prefix, no `:free`, `:online`, or other suffix except `:nitro` and `:floor`. Ids naming search, deep research, or computer use are refused on every provider but the endpoint (`model_has_tools`). The endpoint refuses `:cloud` and `-cloud` names.

**Timeouts and limits.** 120 s per attempt for hosted API providers, 180 s for CLIs, 300 s for the endpoint and Ollama. Context limit 200,000 estimated tokens for hosted providers, 8,192 for the endpoint.

**Keys** (decisions 2 and 3). A key lives in the OS credential store as `ai-key-<name>` and nowhere else: not in the database (`secret_index` holds the name), logs, `task_runs`, audit rows, exports, backups, argv, or a child's environment. Reads happen inside a send, never for status. In the app, a button reads the clipboard in Rust, shows a native dialog naming the provider and the key's last four characters, stores the key, and clears the clipboard; the webview sends only a provider name. In the terminal, `jobpilot ai key set <provider>` reads a hidden prompt, or one line on piped stdin; it is refused in the panel and takes no key in argv. The generic `secret_set` and `secret_delete` refuse the `ai-key-` prefix, so script cannot swap a key. A key is length-capped at 512 bytes, under the Windows blob limit. Deleting everything removes provider keys for every root on the OS account.

**The endpoint** (decisions 5 and 6). One address, `ai.endpoint.url` with `ai.model.endpoint`. A loopback address does not prove local execution (a proxy on that port can forward), so it needs consent and counts as external. A username, password, query, or fragment is refused and a trailing `/` is trimmed. Changing or clearing the address suspends its consent, drops its key, and reads `endpoint_changed`; returning to the exact consented address reactivates the consent but not the key.

**CLI discovery** (`child.rs`). `JOBPILOT_GEMINI_BIN` or `JOBPILOT_CODEX_BIN`, the `ai.gemini_path` or `ai.codex_path` setting, `PATH`, the usual install folders (`gemini.cmd`, `gemini.exe`, `codex.cmd`, `codex.exe`, `%APPDATA%\npm`, Volta, scoop, nodejs, nvm and fnm newest first), then one login-shell lookup. The resolved absolute path shows on the row and in the native consent dialog. It is not pinned to the record. A CLI that is off is not started: status reports its path and probes only an enabled one, so an old CLI reads fine until it is turned on. A wrong stored path reads `wrong_path`. The child's `PATH` starts with the CLI's own folder (and `<prefix>/bin` for a picked npm entry file) so an nvm install finds `node` from a GUI launch.

**Child environment** (decision 17). The allow-list keeps the variables that name the CLI's sign-in, copied by name; the app never looks at a value. For Gemini that is `GEMINI_API_KEY` and `GEMINI_CLI_HOME` only. `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION` were in the plan's list and were dropped, because they could point the CLI at Vertex AI. `GOOGLE_API_KEY`, `GOOGLE_GENAI_USE_VERTEXAI`, and `GOOGLE_APPLICATION_CREDENTIALS` are dropped too. For Codex it is `CODEX_HOME` and `CODEX_API_KEY`; `CODEX_API_KEY` is on the child allow-list although Codex is inert. `~/.gemini/.env` and `~/.env` can still select Vertex, which the app cannot see.

**Gemini CLI** (`gemini_cli.rs`). Declared `no_tools` at version 0.40.0 or later, on the documented flags and a deny-all policy, with a run-time guard. Per attempt the adapter writes `deny-all.toml` (`toolName = "*"`, `decision = "deny"`, `priority = 999`), `system.md`, and `settings.json` to a temporary folder, runs in an empty `work/` folder inside it, and sends the prompt on stdin with no `-p`. Argv, exact, pinned by `argv_is_exactly_the_documented_list`:

```text
--output-format stream-json --approval-mode default --policy <dir>/deny-all.toml
--admin-policy <dir>/deny-all.toml --extensions none
--allowed-mcp-server-names jobpilot-none --skip-trust
```

Set on top of the allow-list: `GEMINI_SYSTEM_MD=<dir>/system.md`, `GEMINI_CLI_SYSTEM_SETTINGS_PATH=<dir>/settings.json` holding `{"hooksConfig":{"enabled":false},"tools":{"sandbox":false}}`, and `GEMINI_SANDBOX=false`. A `GEMINI_SANDBOX` already in the child's environment fails the attempt (`refuse_sandbox`). No `-m` is passed, so the CLI picks the model. Rules the adapter holds:
- The stream is read as an allow-list. Any non-blank line that is not a parsed known event in the expected shape fails the attempt with `bad_reply`, a `tool_use` or `tool_result` event fails it first and outranks every other outcome, and an `error` event fails with its mapped code. Empty text is `bad_reply`. If the real CLI prints anything else on stdout, every run ends `bad_reply`; it fails closed.
- Every `@` in the prompt is sent as `\@` (the CLI's own literal form), by ruling: a headless `@path` reads a file with no policy check, so job text could pull in a local file. A draft may repeat the backslash; validation or an edit catches it. The consent screen's "Show exactly what will be sent" shows this wire text, `\@` included.
- A prompt that opens with `/` never starts the child; both prompt builders open with fixed text.
- A temporary folder path with a comma or non-UTF-8 fails closed, because `--policy` splits on commas.
- A version below 0.40.0 (a pre-release of it counts as below) is `cli_too_old`. A version past 0.40.0 is allowed and the row says it is newer than checked.
- An account the CLI refuses (`IneligibleTierError` and its kin) gives `cli_account_refused`, with copy that names a possible cause as a hint: Google's docs now carry a banner that Gemini CLI was replaced by Antigravity CLI for unpaid-tier and Google One users on June 18th, 2026. The error class is unverified, so the copy stays a hint. Exit 41 is `not_logged_in`.

Not proven by a real run: `GEMINI_SANDBOX=false` and the `tools.sandbox` override, the stream shape and exit 41 (read from the 0.40.0 bundle, not the docs), the `\@` escape, and the deny rule's real effect. `real_gemini_has_no_tools_smoke` re-checks them once, with `JOBPILOT_REAL_GEMINI_SMOKE=1` and `--ignored`. Windows policy path handling is unrun.

**Gemini CLI keeps a transcript.** The CLI records the full prompt under `~/.gemini/tmp/<id>/chats` for 30 days by default. It has no off switch (checked in 0.40.0 and on main). Moving it needs `GEMINI_CLI_HOME`, which moves the sign-in with it. So the consent text for `gemini_cli/cli_login` says so, in the register row's Notes cell, and a record given before that text is suspended.

**The Code Assist licence path.** The register's row for a Gemini CLI sign-in with a Code Assist Standard or Enterprise licence is `unknown` and not built: employer-licensed seats are not the target user, and its schedule page and terms were not read. Dropping `GOOGLE_CLOUD_PROJECT` from the child (decision 17) means a licence sign-in that needs the project fails inside the CLI with `ProjectIdRequiredError`, which reads as `cli_account_refused`; the hint copy names plans, not the project. The path is accepted as unsupported. The consent for `gemini_cli/cli_login` carries the required attestation either way.

**Codex CLI** (`codex_cli.rs`). Ships inert. It declares `StructuredOutput` only, because no OpenAI page documents a way to remove `apply_patch` (a file tool the hooks docs name beside `Bash`), and the non-interactive page lists no tool switch at all. Every task that ships needs `no_tools`, so the router skips it with no child started (`every_task_that_ships_skips_it_and_starts_no_child`), and the AI screen says "No task can use this yet: it cannot prove it has no tools." It still runs, for a future task with no untrusted text, with every documented switch off. Argv, exact:

```text
exec --ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules --sandbox read-only
--disable shell_tool --disable apps --disable multi_agent --disable hooks
-c web_search="disabled" -c tools.view_image=false -c approval_policy="never"
--output-schema <dir>/schema.json --output-last-message <dir>/reply.txt -
```

The run path and these flags are unverified: nothing reaches Codex while every task needs no tools, and the adapter was written from OpenAI's reference pages (Codex CLI 0.125.0) and tested against a mock only. They need checking before the first task that qualifies.

**Consent keys.** The key is the provider name and sign-in method, and it is the `consent.provider` value. A record carries the register row's text (`consent.register_row`, migration 0012, decision 12); a build whose row text differs reads the record as suspended, with no write. The date read is not part of that text.

| Provider | Consent key | Attestation | Terms disclosed |
|---|---|---|---|
| Claude Code | `claude_code/cli_login` | none | Consumer Terms allow training unless the user opts out |
| OpenAI | `openai/api_key` | none | Not used to improve services unless the customer opts in |
| Anthropic | `anthropic/api_key` | none | "Anthropic may not train models on Customer Content from Services." |
| Google | `google/paid_key` | required: the key belongs to a project with billing on | Not used to improve products; logged for a limited period |
| OpenRouter | `openrouter/api_key` | none | The chosen model's terms apply; prompt logging stays off |
| Endpoint | `endpoint/loopback` | none | Goes to the server you named; what it does with it is its business |
| Gemini CLI | `gemini_cli/cli_login` | required | The sign-in's terms; the CLI keeps a transcript with no off switch |
| Codex CLI | `codex_cli/cli_login` | none | The sign-in's terms; no task can use it yet |

A free Gemini key cannot be told from a paid one (the docs name no marker), so the attestation is the only honest gate; a wrong one sends personal data under free-tier terms, and the owner accepts that. Fallback never widens consent. A restore revokes every external consent. A suspended record skips the provider with `consent_suspended`, and the AI screen explains it in an alert with one `Review consent...` button.

**Refresh ceiling.** `memory::refresh_plan` returns `pending` (at most 20 questions, `CLASSIFY_BATCH`) and `external_calls`, which is `pending` times the external providers that declare `no_tools` and hold an active consent covering `job_text`. A reply that fails goes on to the next provider, so each can be asked once per question. The GUI dialog and the CLI text state `external_calls`, with the 20-question limit given separately.
