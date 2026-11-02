# JobPilot AI Release 0.4 Implementation Plan: External AI Backends

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The execution method is chosen: subagent-driven. Task 11 is driven by the controller, not an implementer.

**Goal:** Ship Release 0.4: direct API providers (OpenAI, Anthropic, Google paid key, OpenRouter, one loopback OpenAI-compatible endpoint) and optional Gemini CLI and Codex CLI adapters behind the existing task router, with keys only in the OS keychain, consent extended to all of them, and the app still working end to end with no provider.

**Architecture:** Everything lands in `jobpilot-core`; the CLI and Tauri adapter stay thin. `ai/net.rs` owns the two HTTP clients (loopback and hosted). `ai/api.rs` is one `Provider` with a `Wire` per vendor shape. `ai/child.rs` takes the process rules out of `claude_code.rs` so the Gemini and Codex adapters share them. `ai/register.rs` reads the provider rows from `docs/policy-register.md` at build time, so a changed row suspends consent with nothing to remember to bump. The router gains `Error::RateLimited` and honours `Retry-After`. Its eligibility order, consent gate, and fallback rules do not change.

**Tech Stack:** Rust 1.99.1, Tauri 2.12, SQLx 0.9, reqwest 0.13.5 with `native-tls-no-alpn`, keyring 4, clap 4, React 19, TanStack Query, Vitest 5.

**Spec:** `docs/superpowers/specs/2026-11-01-jobpilot-master-spec.md` section 3 (authority). Derived: `docs/milestones.md` (Release 0.4), `docs/prd.md` FR-3.10, FR-3.11, FR-3.15 to FR-3.19, `docs/ai-providers.md`, `docs/policy-register.md` (Owner decisions), `docs/security/threat-model.md` (M-15, M-21, M-29, M-30, M-31, M-32, M-40, section 7), `docs/wireframes.md` section 13.

**Branch:** `feat/v0.4-external-ai`, cut from `main` after `feat/v0.3-memory-tracking` merges. Every signature below was rechecked at `afdfbbe`, whose tree is identical to `bd4915d`, in the pre-flight scan. Implementers commit locally and never push. Gate 4 needs a CI run, so the controller pushes when that step comes.

## Global Constraints

- No mandatory API keys, API credits, hosted backend, or cloud account. Every provider defaults off except Ollama, and nothing needs a key to work.
- Owner direction, binding: personal, non-commercial app. AI subscription and license terms are waived and are not re-raised in this plan or its copy, beyond the vendor data-use disclosure the consent screen already carries. Re-reading a provider's terms or usage policy is not a release step. Security controls stay: `no_tools` for untrusted job and company text, consent records, validated output, no fabrication (FR-7.12), no inferred work authorization (FR-4.10).
- Claude Code runs on the owner's subscription, through the installed CLI. No token extraction, no bundled vendor CLI, no read of a CLI's login files, including `~/.gemini/oauth_creds.json` and `~/.codex/auth.json` (M-32).
- Targets: macOS (arm64, x86_64) and Windows x64. Toolchain pins stay. SQL uses runtime `sqlx::query`, never `query!`. `*.sql` is LF.
- **Network.** Hosted calls go only to the four fixed vendor hosts over HTTPS. Loopback rules (M-21) are unchanged for Ollama and apply to the endpoint. No new listener, no telemetry. The webview CSP does not change: all network is in Rust.
- **Network tests.** Every test talks to a local fake on `127.0.0.1` or a mock CLI script. No test makes a real paid call. Only Task 11 touches a real provider, and CI's `real_tls_handshake` step opens a connection to the four hosts with no key and no body.
- **Keys.** A key lives in the OS credential store under `ai-key-<name>`, and only there. Never in the database, logs, `task_runs`, audit rows, exports, backups, argv, a child's environment, or the panel. `secret_index` holds the name. No command returns a key. Reads go through `secrets::keychain`, only inside a send, never for status.
- **Provider text.** A vendor's error body and reply text never reach a log, an error message, or `task_runs`. Failures are codes on `RUN_REASONS`. Bodies are read only to match fixed words, capped at 64 KiB.
- Migrations continue at `0012`. Applied files are never edited. Final `schema_version` is 12. New personal-data tables or columns carry a rule 7 comment.
- **Seam check.** A task is done only when its feature is reachable from the GUI and the CLI. Each feature task ends with a CLI integration test, a Tauri command test through the harness in `commands.rs`, and a view test. Reviewers reject a core function with no adapter caller.
- **Keychain in tests.** Tests never touch the real keychain unless `#[ignore]`d. Providers under test take `KeySource::Fixed`. `ai::status` and every read path stay off the keychain: `key_stored` comes from `secret_index`. Implementers run an `--ignored` keychain suite only when the controller says, with `--test-threads=1`. `JOBPILOT_SECRET_SERVICE` is honoured only in debug and test builds: a release binary always uses the real service.
- **Windows.** Gate every `cfg` item on both sides. Any task touching `cfg`, a child process, a dependency with C code, or Tauri plugins runs `scripts/ci-local.sh --fast`, whose `windows type-check` step must stay green. It type-checks only. CI's `windows-latest` job is the run evidence, so every test must pass there: build expected paths with `Path::join`, and give each child-process test a Windows twin or a `cfg(unix)` gate.
- **Panel.** Each new CLI command gets a row in `PANEL` in `crates/cli/src/main.rs` and, when refused, a `matches!` arm in `refused_in_panel`. The table below is binding.
- **Tauri commands.** Each new `#[tauri::command]` goes in `handler()`, in `ALLOWED_COMMANDS`, and in `ipc.ts` with matching types, with a `backend()` default in `test-utils.tsx` when a test needs one. Settings stay under the `ai.` prefix, which `OWNED_SETTINGS` keeps from the generic settings command.
- **GUI rules.** Text from outside the app renders as inert text, no anchors. Every control has a visible label and works by keyboard. Errors appear in `role="alert"` with their code. No `dangerouslySetInnerHTML`. Consent dialog focus starts on Cancel, as wireframe 13 says.
- Ponytail rule: no abstraction without a second caller. Mark deliberate limits with a `ponytail:` comment naming the ceiling.
- Every commit ends with a blank line and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The `git commit` lines below show the subject only: add this line to each.

## Dependency decisions

| Crate | Where | Change | Why |
|---|---|---|---|
| `reqwest =0.13.5` | core | features `["json", "stream", "native-tls-no-alpn"]` | See decision 1. HTTP/1.1 only: `http2` stays off, so ALPN buys nothing. |
| `rpassword =7.5.4` | cli | new | A key typed at a terminal must not echo. No terminal mode crate is in the tree. Key piped on stdin needs nothing. |
| `tauri-plugin-clipboard-manager ="2.5.0"` | desktop | new | Rust reads the key from the clipboard, so it never enters the webview. No capability is added. |

Nothing else. The fakes use `std::net::TcpListener`, as `crates/core/tests/ollama.rs` does, not a mock-server crate.

## Decisions fixed here

Nobody was asked. Each is the implementer's instruction, with the price of being wrong.

| # | Question | Decision | Why | Cost if wrong |
|---|---|---|---|---|
| 1 | TLS backend | `native-tls-no-alpn`: Schannel on Windows, Security.framework on macOS. The hosted client sets `.https_only(true)`, `.tls_version_min(Version::TLS_1_2)`, `.no_proxy()`, `.redirect(Policy::none())`. | reqwest's default `rustls` pulls `aws-lc-sys`, which needs a real C toolchain and breaks the stub `windows type-check` that runs before every commit. Native TLS adds no C build on either target, uses the OS trust store and OS patching, and `security-framework` is already in the tree through `keyring`. | macOS native TLS tops out at TLS 1.2. If a vendor goes 1.3-only, swap to `rustls`, a feature change plus real MinGW in the Windows check. Task 11's handshake step is the early warning. |
| 2 | How a key enters the GUI | Rust reads the clipboard after a button press, shows a native dialog naming the provider and the key's last four characters, stores it, and clears the clipboard. The webview sends only a provider name. | `ai-providers.md` says keys "never cross IPC" and threat model B1 says the webview "never holds a secret value". A password field would break both. Script in the webview cannot answer the native dialog. | A slightly odd flow, copy then press. If the plugin misbehaves on Windows WebView2, fall back to a Rust-owned window, one task of work. |
| 3 | How a key enters the CLI | `ai key set <provider>` reads a hidden prompt on a terminal, one line on piped stdin. Never argv. Refused in the panel. | argv leaks through process lists and shell history. | None worth naming. |
| 4 | Generic `secret_set` | Stays for other names. `ai-key-` joins `data-key-` as a reserved prefix, so script calling `secret_set` or `secret_delete` cannot swap a provider key for its own and route the user's data to another account. | The swap attack is real: `secret_set` is open to the webview. | Nothing: the DataView secrets form is not how keys enter. |
| 5 | How many loopback endpoints | One, `ai.endpoint.url` and `ai.model.endpoint`. | YAGNI. | A second one needs a settings shape change. |
| 6 | Is the loopback endpoint local | No. It needs a consent record and `local: false`, as a loopback address does not prove local execution: a proxy on that port can forward. It also refuses `:cloud` and `-cloud` model names, so it cannot reach Ollama's cloud models. | Routing check 2's own rule. `is_external` already fails closed. | One extra consent click for a server that really is local. |
| 7 | OpenAI API surface | Chat Completions, one wire with OpenRouter and the endpoint. `store: false` is sent to OpenAI. No `temperature` and no token limit is sent. | One code path, and the only shape the endpoint and OpenRouter share. Newer models refuse sampling fields. | A future OpenAI model that exists only on Responses needs a new `Wire` arm. |
| 8 | Google API surface | `generateContent` with `generationConfig.responseMimeType` and `responseJsonSchema`. | Documented and current. `responseSchema` is marked deprecated in the API reference, and the structured-output guide has moved to an Interactions API that nothing else here shares. | Google retiring `generateContent` needs a `Wire` change. |
| 9 | Telling a free Gemini key from a paid one | It cannot be told: the rate-limit docs say tiers are seen in AI Studio, and the error docs name no free-tier marker. Consent for `google/paid_key` and `gemini_cli/cli_login` carries a required attestation, which the native dialog and `--attest` state. No attestation, no record, so no call. | Owner decision: a free key is refused for personal data. An attestation is the only honest gate. | A wrong attestation sends personal data under free-tier terms. The dialog says so, and the owner accepts that risk. |
| 10 | OpenRouter models | Allowed only with an `openai/`, `anthropic/`, or `google/` prefix and no `:free` suffix. Sent with `provider: {data_collection: "deny", require_parameters: true}`. | Routing check 2: an aggregator cannot widen a vendor's verdict. `:free` is the free-tier row. | Other models stay off until a register row exists for them. |
| 11 | Retries on hosted providers | One retry on 429, 503, or 529 only when `Retry-After` gives whole seconds, at most 30. No header, a date form, or a longer wait ends that provider with `rate_limited`. Timeouts, network errors, and other 5xx stay unretried, and quota errors never retry. Attempts stay `spec.attempts`, so `classify_answer` stays one attempt. | The milestone asks for `Retry-After`, and `ai-providers.md` says external providers retry validation only while `agents.md` says otherwise. A 429 spent nothing, a timeout may have. Anthropic's spend-cap 429 has no header, so it must not loop. | A provider that would have recovered moves to the next one. `ai-providers.md` and `agents.md` are corrected in Task 10. |
| 12 | Register changes and consent | `consent.register_row` holds the provider's row text, date read excluded, built from `include_str!` of the register. Unequal to the build's text means suspended, computed on read. | No version to forget, and a date re-read does not suspend anyone. | Any edit to a row suspends, even a typo fix: one click. |
| 13 | The owner's existing Claude consent on upgrade | Suspended by the migration default `''`. The owner re-confirms once. The AI screen says why in `role="alert"` and offers one `Review consent...` button. The dialog opens with the old categories ticked, so `Allow selected` is the one click; Rust's native confirmation stays behind it (M-13). The Resumes screen says why Generate is gone. | It fails closed, and the screen now shows what will be sent. A suspension the GUI does not explain is not acceptable (controller ruling). | One click, and Claude Code stops with `consent_suspended` until then. |
| 14 | No audit row for a suspension | None: nothing happens at that moment. `task_runs` logs each skip, and the new `consent_given` is audited. | A read must not write. | The audit log shows no moment of suspension. |
| 15 | Gemini CLI no-tools | **Proven in docs, declared `no_tools`**, on these flags at version 0.40.0 or later, with a run-time guard. See "No-tools evidence". | Docs say denied tools are "completely excluded from the model's memory". | A later CLI changes this. The version floor, the `tool_use` guard, and the ignored smoke test limit the damage. Remove the capability if the smoke fails. |
| 16 | Codex CLI no-tools | **Not proven, not declared.** See "No-tools evidence". Every task at HEAD requires `NoTools`, so Codex runs no task today, and the screen says so. | The rule says skip unless proven. | The adapter ships inert. When OpenAI documents a way to remove `apply_patch`, adding the flag and the capability is a small change. |
| 17 | Sign-in variables in the CLI child | The allow-list keeps the variables that name the CLI's own sign-in: `GEMINI_API_KEY`, `GEMINI_CLI_HOME`, `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION` for Gemini; `CODEX_HOME`, `CODEX_API_KEY` for Codex. `GOOGLE_API_KEY`, `GOOGLE_GENAI_USE_VERTEXAI`, and `GOOGLE_APPLICATION_CREDENTIALS` are dropped: Vertex AI stays off. | Reconciles "does not alter the environment to change the sign-in" with M-15. JobPilot copies by name and never looks at a value. | `~/.gemini/.env` can still select Vertex, which the app cannot see. Recorded as a residual. |
| 18 | Finding `node` from a GUI launch | The child's `PATH` starts with the CLI's own folder, and for a picked npm entry file also `<prefix>/bin`. | A macOS GUI launch has a minimal `PATH`, and an nvm install's `#!/usr/bin/env node` then fails. The owner's install is exactly this. | Nothing: it only adds folders that hold the CLI. |
| 19 | Planted binary (M-15, M-40) | The resolved absolute path shows on the provider row and in the native consent dialog. It is not pinned to the record. | Writing to the user's `PATH` is same-user malware, out of scope in threat model section 1, and pinning would suspend consent on every update. | A swapped CLI runs unnoticed under an existing consent. Signed builds at 1.0 do not change this either. |
| 20 | "Shows what will be sent" | Tasks, categories, destination, and data use, plus wireframe 13's **Show exactly what will be sent**: the real prompt for the newest pasted posting, from `generate::preview`, as inert text. | The wireframe is explicit, and the prompt builder is reusable. | None. |
| 21 | Hosted timeouts | 120 s per attempt for API providers, 180 s for CLIs, 300 s for the endpoint. | A draft on a slow hosted model passes `agents.md`'s proposed 60 s. | A longer wait on a hung call. |
| 22 | Hosted context limit | `200_000` estimated tokens, 8,192 for the endpoint. An overflow the vendor reports is `prompt_too_large`. | Matches Claude's declaration, and the estimate runs high. The endpoint matches Ollama, since some local servers cut a long prompt silently. | A long posting skips the endpoint. Overflow wording is tested against fakes only. |
| 23 | Model names | The user types the vendor's id, per provider, checked against a safe character set. No default and no suggested list. | Lists go stale, and Google's id lands in the URL path. | A little typing. |
| 24 | Sign in with ChatGPT | Not in 0.4. Wireframe 13's row is dropped. | The milestone's exit tests do not list it, and it stays optional under D1. | A later release adds it. |
| 25 | `gemini` and `codex` in the terminal panel | Not added. `claude` stays the only allowed session. | Not in the exit tests. M-14 allows them but does not require them. | The owner uses an ordinary terminal for those. M-14's text is corrected in Task 10. |
| 26 | Mode check | Stays a comment. No `unreviewed` task exists until 0.6, when `TaskSpec` gains a mode. Model origin is kept where 0.6 needs it, in `Wire` and the model name. | Nothing to refuse today. | 0.6 adds it, as already planned. |
| 27 | Vertex AI | No variant, so no way in. Its unread terms stay unread. | D4 chose paid keys. | A later release reads the terms and adds a row. |
| 28 | Probing a CLI that is off | `ai::status` runs `version_of` only for an enabled CLI provider. An off one reports `available: true` when a path was found, and reason `not_installed` when none was. | `status` runs on every AI screen refresh, a Node CLI takes seconds to start, and `cargo test` must never start a real `gemini` or `codex`. All CLI providers default off. | A CLI too old for the floor reads fine until it is enabled, then says `cli_too_old`. |

## No-tools evidence

Read 2026-11-01 from each CLI's own documentation. The rule is threat model section 7: an adapter handles untrusted text only with a no-tools configuration.

**Gemini CLI 0.40.0: proven.** Sources: the installed `bundle/docs` tree of `@google/gemini-cli` 0.40.0, https://geminicli.com/docs/reference/policy-engine/, https://geminicli.com/docs/cli/headless/.

| Doc says | Used as |
|---|---|
| "`*`: Matches **any tool** (built-in or MCP)." (policy engine, tool name wildcards) | `toolName = "*"` |
| "`deny`: The tool call is blocked and is not executed. For global rules (those without an `argsPattern`), tools that are denied are **completely excluded from the model's memory**. This means the model will not even see the tool as an option" | `decision = "deny"`, no `argsPattern` |
| "`ask_user`: ... (In non-interactive mode, this is treated as `deny`.)" | `--approval-mode default` |
| "`--policy` Additional policy files or directories to load", "`--admin-policy` Additional admin policy files or directories to load" (`gemini --help`). "Supplemental admin policies are **ignored** if any `.toml` policy files are found in the standard system location." | Both flags name the one file, so the user-tier rule stands if the admin one is ignored. |
| "Use the special term `gemini -e none` to disable all extensions." | `--extensions none` |
| "`--allowed-mcp-server-names` Allowed MCP server names" | One name no server has, so no MCP server starts. |
| "`--skip-trust` Trust the current workspace for this session, skipping the folder trust check." | The cwd is an empty temp folder. Without this a headless run can exit `FatalUntrustedWorkspaceError`. |
| "`GEMINI_SYSTEM_MD` ... completely overriding the built-in default. This is a full replacement, not a merge." | The task's system text replaces the coding-agent prompt. Set by file path, so it stays out of argv. |
| "`hooksConfig.enabled`: Canonical toggle for the hooks system. When disabled, no hooks will be executed." and "The path can be overridden using the `GEMINI_CLI_SYSTEM_SETTINGS_PATH`" and "the System Overrides file has the highest precedence" | A temp override file turns the owner's hooks off for this child. |
| "`tool_use`: Tool call requests with arguments." (headless, streaming JSON) | Run-time guard: any `tool_use` or `tool_result` event fails the attempt. |
| "`cat logs.txt \| gemini`" (CLI reference) and "Headless mode is triggered when the CLI is run in a non-TTY environment" | The prompt goes on stdin, with no `-p`. |

Policy file, written to the attempt's temp folder:

```toml
[[rule]]
toolName = "*"
decision = "deny"
priority = 999
```

Argv, exact, pinned by `argv_is_exactly_the_documented_list`:

```text
--output-format stream-json --approval-mode default --policy <dir>/deny-all.toml
--admin-policy <dir>/deny-all.toml --extensions none
--allowed-mcp-server-names jobpilot-none --skip-trust
```

Environment set by the adapter on top of the allow-list: `GEMINI_SYSTEM_MD=<dir>/system.md`, `GEMINI_CLI_SYSTEM_SETTINGS_PATH=<dir>/settings.json` holding `{"hooksConfig":{"enabled":false}}`.

Read from the installed 0.40.0 bundle, not the docs, and so re-checked by the smoke test: assistant text is `{"type":"message","role":"assistant","content":...,"delta":true}`, the end is `{"type":"result","status":"success"}`, `FatalAuthenticationError` exits 41.

Residual, recorded in Task 10: `~/.gemini/GEMINI.md` still loads, so the owner's own instructions sit beside untrusted text with no tool to act through. `tools.discoveryCommand` in user settings is not disabled. Windows policy path handling is unrun.

**Codex CLI 0.125.0: not proven.** Sources: https://learn.chatgpt.com/docs/cli/reference, https://learn.chatgpt.com/docs/config-file/config-reference, https://learn.chatgpt.com/docs/non-interactive-mode.

| Doc says | Reaches |
|---|---|
| "`features.shell_tool`: Enable command execution tools (stable; on by default)." | Shell. |
| "`web_search`: ... `"disabled"` to remove the tool." | Web search. |
| "`tools.view_image`: Enable the local-image attachment tool `view_image`." | Image read. |
| "`features.apps`", "`features.multi_agent`", "`features.hooks`", "`--ignore-user-config`: Do not load `$CODEX_HOME/config.toml`. Authentication still uses `CODEX_HOME`." | Connectors, sub-agents, hooks, the user's MCP servers. |
| "`--sandbox, -s` ... Sandbox policy for model-generated commands." and "By default, `codex exec` runs in a read-only sandbox." | Limits a tool. Does not remove one. |

**The gap:** `apply_patch`, a file tool the hooks docs name beside `Bash`, has no key on the config reference, and no page states that the switches above leave the model with no tools. The non-interactive page lists no tool switch at all. The rule is "no-tools" and this is "fewer tools", so the adapter does not declare `NoTools`.

The adapter still runs with every documented switch off, in case a future task carries no untrusted text. Argv, exact:

```text
exec --ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules --sandbox read-only
--disable shell_tool --disable apps --disable multi_agent --disable hooks
-c web_search="disabled" -c tools.view_image=false -c approval_policy="never"
--output-schema <dir>/schema.json --output-last-message <dir>/reply.txt -
```

## API wires

Bodies carry no `tools`, `tool_choice`, `functions`, or search option on any wire. Every schema has `maxLength` removed first by `provider_schema`, because Anthropic returns 400 on it. The router's own validation still enforces it. The four schemas at HEAD already have `additionalProperties: false` and `required` equal to `properties` on every object, so `strict: true` is safe, and a test keeps that true.

| Wire | Request | Auth | Reply text | Other |
|---|---|---|---|---|
| `Chat(OpenAi)` | `POST https://api.openai.com/v1/chat/completions`; `model`, `messages` (system, user), `response_format: {type: "json_schema", json_schema: {name: "jobpilot_output", strict: true, schema}}`, `store: false` | `Authorization: Bearer` | `choices[0].message.content` | |
| `Chat(OpenRouter)` | `POST https://openrouter.ai/api/v1/chat/completions`; same, without `store`, with `provider: {data_collection: "deny", require_parameters: true}` | `Authorization: Bearer` | same | A `200` may hold a top-level `error`. |
| `Chat(Endpoint)` | `POST <url>/chat/completions`; same, without `store` or `provider` | `Authorization: Bearer` only when a key is stored | same | Loopback client. |
| `Anthropic` | `POST https://api.anthropic.com/v1/messages`; `model`, `max_tokens: 16000`, `system`, `messages`, `output_config: {format: {type: "json_schema", schema}}` | `x-api-key`, `anthropic-version: 2023-06-01` | the `type: "text"` block in `content` | `max_tokens` is required by the API. Skip thinking blocks. |
| `Google` | `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`; `systemInstruction: {parts: [{text}]}`, `contents: [{role: "user", parts: [{text}]}]`, `generationConfig: {responseMimeType: "application/json", responseJsonSchema: schema}` | `x-goog-api-key` | text of `candidates[0].content.parts`, joined | Key never in the query string. |

Header values holding a key are built with `set_sensitive(true)`.

Reply mapping, shared by `read_reply(wire, body)`:

| Finding | Error |
|---|---|
| A tool call: `tool_calls` non-empty, a `tool_use` block, a `functionCall` part | `Refused("permission_denied")` |
| `message.refusal` set, `finish_reason: content_filter`, `stop_reason: refusal`, `finishReason: SAFETY`, `promptFeedback.blockReason` set | `Refused("model_refused")` |
| `finish_reason: length`, `stop_reason: max_tokens`, `finishReason: MAX_TOKENS` | `Failed("output_truncated")` |
| Top-level `error` on a 200, or no text where text belongs | `Failed("bad_reply")` |

Status mapping, `classify_status(status, retry_after, body)`:

| Status | Error |
|---|---|
| 401, 403 | `Refused("auth_failed")` |
| 402, or 429 whose body holds `insufficient_quota`, `credit_balance_exhausted`, `spend_limit`, or `usage_limit` | `Refused("quota_exhausted")` |
| 404 | `Refused("model_not_found")` |
| 400, 413 whose body holds `context_length_exceeded`, `maximum context length`, `prompt is too long`, or `exceeds the maximum number of tokens` | `Refused("prompt_too_large")` |
| 429, 503, 529 | `RateLimited { retry_after }` |
| Anything else, 3xx included | `Failed("http_<n>")` |

## Panel table

Binding. "Refused" means a row with `true` and a `matches!` arm in `refused_in_panel`.

| Command | Panel | Why |
|---|---|---|
| `ai key set openai` | refused | Takes a secret. |
| `ai key remove openai` | refused | Its GUI twin confirms natively. |
| `ai model openai m` | refused | Changes what runs. |
| `ai endpoint http://127.0.0.1:1` | refused | Changes what runs. Its GUI twin confirms natively. |
| `ai cli-path gemini_cli /g` | refused | Names a path, and changes what runs. |
| `ai consent show openai` | allowed | Reads only. |
| `ai consent show openai --job j` | allowed | Prints only what the user's own data would send. |
| `ai consent give openai --categories job_text` | refused | Existing row kept, with this provider added. |
| `ai consent revoke openai` | allowed | Only takes something away. |
| `ai enable openai`, `ai disable openai`, `ai order openai` | refused | Existing leaves, new values. |

## Windows story

The repo is public, so GitHub Actions minutes are not a limit. CI's `windows-latest` job links and runs the suite, and that run is Gate 4's evidence.

| Item | Built in | Proven by |
|---|---|---|
| Keys in Credential Manager | Task 3: same `keyring` path as the data key; length cap 512 keeps under the 2,560-byte blob limit | `#[ignore]`d `secrets_store.rs` tests, in CI's "Keychain tests (Windows)" step |
| CLI detection | Task 6: `gemini.cmd`, `gemini.exe`, `codex.cmd`, `codex.exe`; `%APPDATA%\npm`, `%LOCALAPPDATA%\Volta\bin`, `%USERPROFILE%\scoop\shims`, `%ProgramFiles%\nodejs` | Pure functions run on both with `windows: true`; `#[cfg(windows)]` tests in CI |
| `.cmd` shims | Task 6: args via Rust's batch escaping, never a shell string. An argument it refuses is `cli_error`. No line breaks in argv. Nothing long in argv either: Gemini's prompt, system text, and policy all go by stdin or file, so cmd.exe's 8,191-character limit never bites. `taskkill /T /F` ends the Node child behind the shim. | `cmd_shim_takes_spaces_stdin_and_exit_code` with a path holding a space, in CI |
| TLS | Task 1: Schannel | `real_tls_handshake`, a CI step on both systems, added in Task 10 |
| Real `gemini` and `codex` on Windows, the GUI, native dialogs, the clipboard | Tasks 3, 6, 7, 8 | Not run by CI. Gate 4 lists them as not exercised. |

Gate 4 for 0.4 records the CI run id and its Windows result, from Task 11. `scripts/ci-local.sh --fast` stays the pre-commit check, and its Windows step only type-checks.

## Review Focus

1. The clipboard holds something other than a key: a job posting, a paragraph, a key with a trailing newline. The app refuses with no echo, stores nothing, and leaves the clipboard alone; a trailing newline is trimmed and the key works. Pinned by `clipboard_that_is_not_a_key_stores_nothing_and_echoes_nothing` (Task 3).
2. A `200` reply that is not an answer: OpenRouter's error in the body, a tool call, a refusal, a truncated reply. Nothing is stored, and the next provider gets the task, with its own consent check. Pinned by `a_200_holding_an_error_moves_on` and `a_tool_call_in_a_reply_stores_nothing` (Tasks 4, 5, 9).
3. `Retry-After` that cannot be obeyed: an hour, a negative number, a date, garbage. The app never sleeps for it and moves on. Pinned by `retry_after_only_trusts_small_whole_seconds` and `a_long_retry_after_moves_on_at_once` (Task 1).
4. A GUI launch on macOS with an nvm-only `node`, and a picker that resolves the link to `bundle/gemini.js`. The CLI is found, accepted, and runs. Pinned by `npm_entry_file_is_accepted_and_finds_its_node` and `child_path_starts_with_the_programs_folder` (Task 6).
5. The endpoint address is aimed at Ollama's own port with a cloud model, or the server answers with a redirect to a remote host. Neither sends anything further. Pinned by `endpoint_refuses_a_cloud_model_on_any_port` and `no_client_follows_a_redirect` (Tasks 1, 4).

## File map

```text
crates/core/migrations/0012_consent_register_row.sql  consent.register_row             Task 2
crates/core/src/ai/net.rs                             clients, Retry-After, capped read Task 1
crates/core/src/ai/register.rs                        provider table, register rows    Task 2
crates/core/src/ai/api.rs                             ApiProvider, wires               Tasks 4, 5
crates/core/src/ai/child.rs                           spawn rules, detection           Task 6
crates/core/src/ai/gemini_cli.rs                                                       Task 7
crates/core/src/ai/codex_cli.rs                                                        Task 8
crates/core/src/ai/{mod,router,consent,claude_code,ollama}.rs   edits                  Tasks 1, 2, 6
crates/core/src/{error,secrets,generate,backup}.rs    edits                            Tasks 1 to 3
crates/core/src/agents/{draft,memory}.rs, memory/classify.rs    provider lists          Tasks 2, 9
crates/core/tests/fake_http/mod.rs                    shared HTTP fake                 Task 1
crates/core/tests/{net,register,api_chat,api_vendors,gemini_cli,codex_cli,child}.rs
crates/core/tests/injection_hosted.rs                                                  Task 9
crates/core/tests/injection/{18,19}_*.txt                                              Task 9
crates/core/tests/mock-{gemini,codex}/{gemini,gemini.cmd,codex,codex.cmd}              Tasks 7, 8
crates/cli/src/{ai,main,data,memory}.rs, crates/cli/tests/cli.rs                       Tasks 2 to 9
apps/desktop/src-tauri/src/{commands,native,lib}.rs                                    Tasks 2 to 8
apps/desktop/src/components/{ConsentDialog,ProviderRow}.tsx   new                      Tasks 2, 3
apps/desktop/src/views/{AiView,ResumesView,MemoryView}.tsx, lib/{ipc,reasons}.ts       Tasks 2 to 9
docs, scripts/ci-local.sh, .github/workflows/ci.yml, version bump                      Task 10
```

---

### Task 1: TLS, the two clients, and `Retry-After`

**Files:**
- Create: `crates/core/src/ai/net.rs`, `crates/core/tests/net.rs`, `crates/core/tests/fake_http/mod.rs`
- Modify: `crates/core/Cargo.toml`, `Cargo.lock`, `crates/core/src/error.rs`, `crates/core/src/ai/{mod,ollama,router}.rs`, `crates/core/tests/{router,ollama}.rs`, `crates/cli/src/main.rs`, `apps/desktop/src/lib/reasons.ts`, `apps/desktop/src/views/views02.test.tsx`

**Interfaces:**
- Consumes: `Ollama::new(&str, Option<String>, Option<String>) -> Result<Ollama>`, `bounded` and `try_provider` in `router.rs`, `RUN_REASONS`.
- Produces, in `ai::net`:

```rust
pub(crate) const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_ERROR_BODY: u64 = 64 * 1024;
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
/// M-21, moved from `Ollama::new`: http only, 127.0.0.1, [::1], or localhost pinned to 127.0.0.1.
pub fn loopback_client(url: &str) -> Result<(Url, Client)>;
/// `pub` only so `tests/net.rs` can test the real client settings.
#[doc(hidden)] pub fn hosted_client() -> Result<Client>;
/// Whole seconds only. A date, a negative, or garbage is `None`.
pub fn retry_after(header: Option<&str>) -> Option<Duration>;
pub(crate) async fn read_capped(response: Response, limit: u64) -> Result<Vec<u8>>;
pub(crate) fn net(error: reqwest::Error) -> Error; // moved from ollama.rs
```

- Produces, in `error`: `Error::RateLimited { retry_after: Option<Duration> }`, code `rate_limited`, message "rate limited".
- Produces in tests: `fake_http::Fake::start(routes: Vec<(&'static str, Canned)>) -> Fake`, with `url`, `hits(path) -> Vec<Value>`, `headers(path) -> Vec<String>`, `paths() -> Vec<String>`, and `Canned { status, headers, body, hold }`. A route takes a queue of replies, so a test can script 429 then 200. The file starts `#![allow(dead_code)]`: each test binary uses part of it, and clippy runs with `-D warnings` on all targets.

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/net.rs` and `router.rs`

```rust
#[test] fn retry_after_only_trusts_small_whole_seconds() {
    assert_eq!(retry_after(Some("2")), Some(Duration::from_secs(2)));
    assert_eq!(retry_after(Some(" 0 ")), Some(Duration::ZERO));
    for bad in ["-1", "1.5", "soon", "", "Wed, 21 Oct 2026 07:28:00 GMT", "99999999999999999999"] {
        assert_eq!(retry_after(Some(bad)), None, "{bad}");
    }
    assert_eq!(retry_after(None), None);
}
#[tokio::test] async fn https_ollama_address_stays_refused() {
    assert_eq!(Ollama::new("https://127.0.0.1:11434", None, None).err().unwrap().code(), "refused");
}
#[tokio::test] async fn no_client_follows_a_redirect() {
    // Fake answers 302 to a second fake. `Ollama` and a `loopback_client` both stop at the 302,
    // and the second fake records no hit. Closes M-21's "redirect handling is untested".
    assert!(second.hits("/x").is_empty());
}
// router.rs, a Mock external provider gains Reply::RateLimited(Option<Duration>)
#[tokio::test] async fn a_429_with_retry_after_waits_then_retries_once() {
    assert!(clock.elapsed() >= Duration::from_millis(900));
    assert_eq!(counters.calls.load(SeqCst), 2);
}
#[tokio::test] async fn a_long_retry_after_moves_on_at_once() {
    // Retry-After 31 on the first provider, Ollama next. Elapsed under 2 s, Ollama answers.
}
#[tokio::test] async fn a_429_with_no_header_is_not_retried_on_an_external_provider() {
    assert_eq!(counters.calls.load(SeqCst), 1);
    assert_eq!(outcome, manual("rate_limited"));
}
#[tokio::test] async fn a_cancel_during_the_wait_ends_within_a_poll() {}
#[tokio::test] async fn classify_stays_one_attempt_on_a_429() {
    assert_eq!(counters.calls.load(SeqCst), 1);
}
#[tokio::test] async fn a_pause_during_the_wait_stops_the_retry() {}
```

Also in `crates/core/tests/ollama.rs`: `a_429_from_ollama_retries_after_its_header` and `a_503_from_ollama_retries`. Add the `RateLimited` case to `every_variant_has_its_code`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test net && cargo test -p jobpilot-core --test router a_429`
Expected: FAIL, `net` and `RateLimited` not found.

- [ ] **Step 3: Enable TLS and write `net.rs`**

Change the `reqwest` line to features `["json", "stream", "native-tls-no-alpn"]`, and keep `=0.13.5`. Move the host and port checks out of `Ollama::new` into `loopback_client`, unchanged, including `.no_proxy()`, `.redirect(Policy::none())`, and the `localhost` pin. `Ollama::new` then keeps `parsed.origin()` and takes the client. `hosted_client` builds as decision 1 says, with `connect_timeout(10 s)`.

Add `only_native_tls_is_linked` to `crates/core/tests/net.rs`: `Cargo.lock` holds no `rustls`, `aws-lc-sys`, or `ring` package. Add `hosted_client_refuses_plain_http` to the same file.

- [ ] **Step 4: Teach Ollama the new error**

`Ollama::post` maps 429 and 503 to `Error::RateLimited { retry_after: retry_after(header) }`, reading the header before the body. That makes the doc's "retried on 429 and 5xx" true for those two. Other statuses stay as they are.

- [ ] **Step 5: Change `try_provider`**

Add `RATE_LIMITED` beside `NOT_LOGGED_IN`. Replace the match after a failed attempt with this order:

```rust
match error {
    Error::RateLimited { retry_after } if attempt < task.spec.attempts => {
        let wait = match (retry_after, is_external(&declaration)) {
            (Some(wait), _) if wait <= MAX_RETRY_WAIT => wait,
            (None, false) => Duration::from_secs(1),
            _ => return Ok(Step::Manual(RATE_LIMITED)),
        };
        let sleep = async { tokio::time::sleep(wait).await; Ok(()) };
        if bounded(sleep, wait + CANCEL_POLL, task.cancel).await.is_err() {
            log(task, row(&name, attempt, "cancelled", None, task.db).await?).await?;
            return Ok(Step::Cancelled);
        }
    }
    Error::RateLimited { .. } => return Ok(Step::Manual(RATE_LIMITED)),
    // the existing arms follow, unchanged
}
```

`rate_limited` joins `weight`'s default rank, so a real attempt's reason outranks a skip. `no_key`, `no_api_model`, and `cli_too_old` join rank 0 with `no_model_set`: each says a provider is not ready, and none may outrank `consent_needed`. Add to `RUN_REASONS`: `rate_limited`, `auth_failed`, `quota_exhausted`, `model_refused`, `model_not_found`, `output_truncated`, `no_key`, `no_api_model`, `consent_suspended`, `cli_too_old`, `origin_refused`. Treat `auth_failed` and `quota_exhausted` as `not_logged_in` is treated: `Step::Manual(reason)`.

Copy, in `main.rs::message` and `reasons.ts`:

| Reason | Text |
|---|---|
| `rate_limited` | "The AI provider is busy and asked to wait. Try again shortly, or write this one yourself." |
| `auth_failed` | "The AI provider refused the key. Check it on the AI screen." |
| `quota_exhausted` | "The AI provider says your usage or credit has run out. Add credit with the vendor, then try again." |

Extend the `manual_required_%s` table in `views02.test.tsx` with all three.

- [ ] **Step 6: Run the gate**

Run: `cargo test --workspace && pnpm test && scripts/ci-local.sh --fast`
Expected: PASS, including `windows type-check`. If Schannel breaks the stub build, stop and report: decision 1 rests on it.

- [ ] **Step 7: Commit**

```bash
git add crates apps Cargo.lock
git commit -m "feat: TLS for hosted calls, loopback rules in one place, Retry-After in the router"
```

### Task 2: Provider table, register rows, consent for every provider

**Files:**
- Create: `crates/core/migrations/0012_consent_register_row.sql`, `crates/core/src/ai/register.rs`, `crates/core/tests/register.rs`, `apps/desktop/src/components/{ConsentDialog,ProviderRow}.tsx`
- Modify: `crates/core/src/ai/{mod,router,consent}.rs`, `crates/core/src/{generate,backup}.rs`, `crates/core/src/agents/{draft,memory}.rs`, `crates/core/tests/{db,backup,router,agents,generate,claude_code}.rs`, `crates/cli/src/{ai,data,main}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/views/{AiView,ResumesView}.tsx`, `apps/desktop/src/lib/{ipc,reasons}.ts`, `apps/desktop/src/test-utils.tsx`, `apps/desktop/src/views/views02.test.tsx`

**Interfaces:**
- Consumes: `consent::give(db, provider, categories, terms_date, actor)`, `consent::revoke(db, provider, actor)`, `consent::active(db, provider)`, `provider_settings`, `set_order`, `set_enabled`, `generate::build_prompt`, `knowledge::get_job_text`, `knowledge::evidence`, `profile::basic`, `native::confirmed`.
- Produces, in `ai`:

```rust
pub enum ProviderId {
    Ollama, ClaudeCode, NoAi,
    #[serde(rename = "openai")] OpenAi, Anthropic, Google,
    #[serde(rename = "openrouter")] OpenRouter, Endpoint, GeminiCli, CodexCli,
}
impl ProviderId { pub const ALL: [ProviderId; 10]; }
// Display: openai/api_key, anthropic/api_key, google/paid_key, openrouter/api_key,
// endpoint/loopback, gemini_cli/cli_login, codex_cli/cli_login. Existing three unchanged.
pub const PROVIDER_NAMES: &[&str] = &["ollama", "claude_code", "openai", "anthropic", "google",
    "openrouter", "endpoint", "gemini_cli", "codex_cli"];
pub struct ExternalStatus {
    pub name: String, pub label: String, pub kind: ProviderKind,
    pub available: bool, pub reason: Option<String>,
    pub model: Option<String>, pub key_stored: bool, pub path: Option<String>,
    pub consent: Option<Vec<DataCategory>>, pub suspended: bool, pub runs_job_text: bool,
}
// AiStatus gains `pub external: Vec<ExternalStatus>`, every provider but Ollama and Claude Code,
// and `pub register_date: String`. `ClaudeStatus` gains `pub suspended: bool`.
```

- Produces, in `ai::register`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind { Api, Cli, Endpoint }
pub struct ProviderInfo {
    pub name: &'static str, pub id: ProviderId, pub label: &'static str, pub kind: ProviderKind,
    /// Where the data goes, in words. Shown on the consent screen.
    pub destination: &'static str,
    pub capabilities: &'static [Capability],
    pub vendor_row: (&'static str, &'static str), // Integration, Operation cells
    pub owner_row: Option<&'static str>,          // first-cell prefix in the owner table
    pub data_use: &'static str,
    pub attestation: Option<&'static str>,
}
pub const PROVIDERS: &[ProviderInfo];
pub fn info(name: &str) -> Option<&'static ProviderInfo>;
pub fn info_for(key: &str) -> Option<&'static ProviderInfo>; // by consent key
/// Vendor row cells except Date read, then the owner row line, `\n`-joined, `\r` stripped.
pub fn row_text(key: &str) -> Option<String>;
pub fn date_read(key: &str) -> Option<String>;
pub fn register_date() -> String;
pub const REGISTER: &str;
#[doc(hidden)] pub fn rows_of(register: &str, key: &str) -> Option<String>; // `row_text` on any text
```

- Produces, in `ai::consent`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum ConsentState { None, Active(Vec<DataCategory>), Suspended(Vec<DataCategory>) }
pub async fn state(db: &Db, provider: &str) -> Result<ConsentState>;
/// By settings name. Takes the date from the register, and refuses `attestation_needed`
/// when the provider has one and `attested` is false.
pub async fn give_for(db: &Db, name: &str, categories: &[DataCategory], attested: bool, actor: &str) -> Result<()>;
```

`consent::give` keeps its signature and writes `register_row`, `''` for a name the table does not hold, as `router.rs` tests give `"claude_code"` and `"ollama"` to prove those names unlock nothing. Its `ON CONFLICT` clause sets `register_row` too, or a second consent in one millisecond stays suspended. `consent::active` returns `None` while suspended, so every caller fails closed.

- Produces, in `ai`:

```rust
pub struct TaskSharing { pub task: TaskType, pub categories: Vec<DataCategory>, pub runs: bool, pub skipped_because: Option<String> }
pub struct Sharing {
    pub name: String, pub label: String, pub key: String, pub destination: String,
    pub path: Option<String>, pub tasks: Vec<TaskSharing>, pub data_use: String,
    pub terms_date: String, pub attestation: Option<String>,
    pub suspended: bool, pub given: Option<Vec<DataCategory>>,
}
pub async fn sharing(db: &Db, name: &str) -> Result<Sharing>;
```

`tasks` lists the four completion tasks; `Embed` is local only and is left out. `given` holds the categories of an active or a suspended record, so a review starts ticked. There is no verdict field: the owner decisions govern what ships, and no screen re-raises a vendor's verdict.

- Produces, in `generate`: `pub struct PromptPreview { pub task: TaskType, pub system: String, pub user: String }` and `pub async fn preview(db: &Db, task: TaskType, job_text_id: &str) -> Result<PromptPreview>`.
- Produces, Tauri: `ai_sharing(provider) -> Sharing`, `ai_preview(job_text_id) -> PromptPreview`. `ai_consent_give` and `ai_consent_revoke` take any external name.

| `name` | Vendor row: Integration, Operation | Owner row prefix | Attestation |
|---|---|---|---|
| `claude_code` | `Claude Code`, `` JobPilot runs `claude -p` or the Agent SDK in the background under the user's consumer login `` | `` Claude Code: `claude --bare -p` `` | none |
| `openai` | `OpenAI API`, `Direct API calls with the user's own key` | `Direct API: OpenAI` | none |
| `anthropic` | `Anthropic API`, same | `Direct API: OpenAI` | none |
| `google` | `Google Gemini API`, `Direct API calls with the user's own paid key` | `Direct API: OpenAI` | "This key belongs to a Google project with billing on. A free Gemini key is refused for personal data." |
| `openrouter` | `OpenRouter`, `API calls with the user's own key` | `Direct API: OpenAI` | none |
| `endpoint` | `Compatible endpoints`, `Calls to a server the user runs on loopback` | none | none |
| `gemini_cli` | `Gemini CLI`, `` JobPilot runs `gemini` with the user's paid Gemini API key `` | `Gemini CLI and Codex CLI adapters` | "Gemini CLI here is not signed in with a free Gemini API key. A free key is refused for personal data." |
| `codex_cli` | `Codex CLI`, `` JobPilot runs `codex exec` in the background on the user's saved ChatGPT login `` | `Gemini CLI and Codex CLI adapters` | none |

`data_use` copies the disclosure table in `docs/ai-providers.md`, "Consent record". That table has no endpoint row, so `endpoint` reads: "This goes to the server you named at this address. What that server does with it is up to that server." `capabilities` is `[StructuredOutput, NoTools]` for all but `codex_cli`, which has `[StructuredOutput]`. Task 7's and Task 8's `declaration()` read this table, so the screen and the router cannot differ.

- [ ] **Step 1: Write the migration**

```sql
-- rule 7: register_row is register text, not personal data. Backup copies it with the row,
-- export writes it, delete-all removes it with the root. The default '' equals no build's text,
-- so a record from before this column is suspended until the user confirms again.
ALTER TABLE consent ADD COLUMN register_row TEXT NOT NULL DEFAULT '';
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/core/tests/register.rs
#[test] fn every_external_provider_has_its_register_rows() {
    for info in PROVIDERS {
        assert!(row_text(&info.id.to_string()).is_some(), "{}", info.name);
        assert!(!info.data_use.is_empty());
    }
}
#[test] fn the_date_read_is_not_part_of_the_row_text() {
    let text = row_text("openai/api_key").unwrap();
    assert!(!text.contains("| 2026-11-01 |"));
    assert!(text.contains("not for consumer use") || text.contains("consumers or individuals"));
}
#[test] fn windows_line_endings_change_nothing() {
    assert_eq!(rows_of(&REGISTER.replace('\n', "\r\n"), KEY), rows_of(REGISTER, KEY));
}
#[test] fn left_out_providers_have_no_way_in() {
    for name in ["vertex_ai", "vertex", "gemini_free", "antigravity", "ollama_cloud", "chatgpt_plan"] {
        assert!(info(name).is_none());
    }
}
#[tokio::test] async fn a_left_out_name_is_refused_by_every_setting() {
    assert_eq!(set_enabled(&db, "vertex_ai", true).await.unwrap_err().code(), "validation");
    assert_eq!(set_order(&db, &["antigravity".into()]).await.unwrap_err().code(), "validation");
}
#[tokio::test] async fn a_changed_row_suspends_and_a_new_consent_lifts_it() {
    consent::give(&db, CLAUDE, &all, "2026-11-01", "gui").await.unwrap();
    sqlx::query("UPDATE consent SET register_row = 'older words'").execute(&db.pool).await.unwrap();
    assert!(matches!(consent::state(&db, CLAUDE).await.unwrap(), ConsentState::Suspended(_)));
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
    consent::give(&db, CLAUDE, &all, "2026-11-01", "gui").await.unwrap();
    assert!(matches!(consent::state(&db, CLAUDE).await.unwrap(), ConsentState::Active(_)));
}
#[tokio::test] async fn attestation_is_required_where_the_table_says() {
    assert_eq!(give_for(&db, "google", &cats, false, "cli").await.unwrap_err().to_string(),
        "refused: attestation_needed");
    give_for(&db, "google", &cats, true, "cli").await.unwrap();
}
#[tokio::test] async fn sharing_lists_every_task_and_says_why_codex_runs_none() {
    let s = sharing(&db, "codex_cli").await.unwrap();
    assert!(s.tasks.iter().all(|t| !t.runs && t.skipped_because.as_deref() == Some("no_tools")));
}
// router.rs
#[tokio::test] async fn a_suspended_consent_skips_and_says_so() {
    assert_eq!(outcome, manual("consent_suspended"));
    assert_eq!(counters.calls.load(SeqCst), 0);
}
// db.rs
#[tokio::test] async fn a_fresh_database_is_schema_12() {}
#[tokio::test] async fn a_0_3_database_upgrades_to_0_4() {
    // A schema 11 root with a Claude consent row opens at 12, writes pre-migrate-11-*, keeps the
    // row, and reads Suspended.
}
// backup.rs
#[tokio::test] async fn restore_turns_every_consented_provider_off() {
    // Consent for claude_code and openai, both on. After restore, both off, both revoked.
}
// agents.rs
#[tokio::test] async fn a_resumed_run_knows_a_hosted_provider() {
    // agents/draft.rs::rebuild finds `openai/api_key`.
}
// generate.rs
#[tokio::test] async fn preview_is_the_prompt_and_withholds_a_marked_item() {
    assert_eq!(preview.user, build_prompt(task, &basic, &evidence, &posting).user);
}
```

Also rename `a_fresh_database_is_schema_11` and change line 276's `11` in `crates/core/tests/db.rs` to `app_schema_version()`.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test register`
Expected: FAIL, `register` not found.

- [ ] **Step 4: Implement `register.rs` and the id changes**

`const REGISTER: &str = include_str!("../../../../docs/policy-register.md");`. A row is the line whose first two `|`-split cells, trimmed, equal `vendor_row`. Split on `" | "`, as cells hold no such text, and a test proves each mapped row has seven cells. `register_date` reads `Register date: ` from the file's third line. Drop `TERMS_DATE`'s use from adapters; keep the const, which tests use.

`provider_settings` gives every new name `false`. `known`'s message lists the names from `PROVIDER_NAMES`. Fix the two hard-coded lists: `agents/draft.rs::rebuild` searches `ProviderId::ALL`, and `agents/memory.rs::asked_since` prefers any external id over Ollama, by `ProviderId::ALL`. `crates/core/tests/claude_code.rs:766` uses `openai` as a name outside the allow-list: change it to `vertex_ai`, or that test stops proving what it says.

`backup::revoke_external_consent` turns off each revoked provider, deriving the setting name from the text before `/` through `register::info_for`:

```sql
UPDATE settings SET value = json_set(value, '$.' || ?, json('false')), updated_at = ... WHERE key = 'ai.enabled' AND json_type(value) = 'object'
```

`ai::status` adds `external`, from a table walk that never touches the keychain or the network: `key_stored` from `secrets::names`, `model` from `ai.model.<name>`. CLI providers and hosted providers fill `available` as Tasks 3 to 8 land; until then it reads `false`, reason `not_built`.

`generate::preview` reads what `ask` reads, in the same order, and refuses tasks other than the two drafts with `Validation`. It builds a prompt and sends nothing. Move the three reads to one `gather` so the two cannot differ.

Router: `ineligible` matches `consent::state`, `None` to `CONSENT`, `Suspended` to the new `CONSENT_SUSPENDED`, and `Active` to the existing coverage test. `after_skip` maps both like `CONSENT`. `weight` ranks `consent_suspended` with `consent_needed`.

- [ ] **Step 5: Add the CLI**

`ProviderArg` gains the seven names, with `#[value(name = "...")]` matching `PROVIDER_NAMES`. `name()` follows. `ConsentCommand::Give` gains `--attest`. Add `ConsentCommand::Show { provider, #[arg(long)] job: Option<String> }`, printing the `Sharing` in plain lines, and with `--job` the preview of `draft_resume`. `external_only` becomes `register::info(name)` being `Some`, so Ollama still says consent does not apply; its message drops "claude_code only". `ai status` text prints one line per external provider. `data::status` adds `"external"` to its JSON and a line per enabled provider.

Give prints the data use and the attestation before it records. Update `PANEL` from the panel table. Rewrite the `consent_needed` message here and in `reasons.ts` to name no one provider, and keep the example command that `consent_hint_names_the_whole_command` pins:

"An AI provider is on, but your consent does not cover this task. See `jobpilot ai status`, then give consent in an ordinary terminal, for example `jobpilot ai consent give claude_code --categories job_text,profile_basic,resume_text`, or on the AI screen."

`consent_suspended`, in `main.rs::message` and `reasons.ts`: "Your consent for this provider was given before the terms on file now, so it is paused and nothing is sent. Review it once on the AI screen, or with `jobpilot ai consent show`." The same words cover the upgrade from 0.3 and a changed register row.

- [ ] **Step 6: Add the Tauri commands and the screen**

`consent_provider(provider)` becomes `register::info(provider)` minus Ollama, returning `Validation` otherwise. Remove `CONSENT_PROVIDER`. `ai_consent_give` builds its native message from `ai::sharing`: label, resolved path when there is one, categories, destination, data use, and attestation when there is one, then calls `consent::give_for(.., true, ACTOR)`, since pressing Allow on that text attests. `ai_preview` calls `generate::preview` for `DraftResume`.

Move `ConsentDialog` to `components/ConsentDialog.tsx`, taking `provider: string`. Add:
- "What will be sent": each task with its categories, or "Skipped: this provider cannot prove it has no tools".
- "Where it goes": `destination`.
- "How {label} uses it (terms read {terms_date})": `data_use`.
- `Show exactly what will be sent`, shown when a pasted posting exists, rendering system and user text in `<pre>` as inert text. With no posting: "Paste a posting to see this."
- The attestation as a line above the buttons.

Focus starts on Cancel. Tick `given` when reviewing a suspended or active record, nothing otherwise, as the wireframe says.

`ProviderRow.tsx` renders one external provider: label, "Unavailable: {plain reason}", "Suspended: this consent was given before the terms on file now. Review it once to turn it back on." in `role="alert"`, consent categories, `Use {label}`, `Review...`, `Revoke consent`. `AiView` maps `s.external` to rows in an "AI providers" section, with the heading "Register date {register_date}". Add the "Not available" list from wireframe 13, static read-only rows, minus Sign in with ChatGPT, plus "Vertex AI: not supported." and "Ollama cloud models: not supported." Each is text, with no control. The Claude section uses the same `ConsentDialog` and `suspended` state: while suspended it shows that same alert and one `Review consent...` button in place of today's "Off. Claude Code is used only after you consent below.", which would be false. `ai_status` gives `ClaudeStatus.consent: null` then, so the dialog takes `given` from `ai_sharing`. `Provider` in `ipc.ts` widens to every name. `Generated.provider` widens to `ProviderId` in full. `backendEnabled` in `ResumesView` also counts any `external` row that is enabled, available, consented, and `runs_job_text`. When a provider that is on is suspended, Claude Code included, and nothing else can draft, `ResumesView` shows the suspended line and a pointer to the AI screen instead of leaving Generate gone with no word. `PROVIDERS` in `AiView` gains the labels. Add `consent_suspended` to `reasons.ts`.

- [ ] **Step 7: Seam check**

- `cli.rs`: `consent_show_says_what_is_sent_for_each_provider`; `consent_give_needs_attest_for_google`; `panel_refuses_new_ai_commands`.
- `commands.rs`: `consent_dialog_names_data_use_and_attestation` (the `asked` log holds the data use and the attestation), `a_left_out_provider_is_refused_at_ipc`.
- `views02.test.tsx`: `consent_dialog_shows_tasks_destination_and_terms`, `consent_focus_starts_on_cancel`, `show_exactly_what_will_be_sent_is_inert_text` (a `<script>` in the posting appears as text), `a_suspended_row_says_so_and_offers_review`, `claude_suspended_on_upgrade_says_why_and_review_starts_ticked`, `resumes_view_says_why_generate_is_gone_while_suspended`, `the_not_available_list_has_no_controls`, `resumes_view_counts_a_hosted_provider_as_a_backend`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates apps
git commit -m "feat: provider table from the register, consent for every provider, suspension on a changed row"
```

### Task 3: Provider keys in the OS keychain

**Files:**
- Modify: `crates/core/src/secrets.rs`, `crates/core/src/ai/mod.rs`, `crates/core/tests/secrets_store.rs`, `crates/cli/Cargo.toml`, `crates/cli/src/{ai,main}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/Cargo.toml`, `apps/desktop/src-tauri/src/{commands,native,lib}.rs`, `apps/desktop/src/components/ProviderRow.tsx`, `apps/desktop/src/lib/ipc.ts`, `apps/desktop/src/views/views02.test.tsx`, `Cargo.lock`

**Interfaces:**
- Consumes: `secrets::set`, `secrets::delete`, `secrets::get`, `secrets::keychain`, `native::confirmed`.
- Produces, in `secrets`:

```rust
pub const API_KEY_PREFIX: &str = "ai-key-";
pub fn api_key_name(provider: &str) -> String;       // ai-key-openai
/// Trims, then 8 to 512 visible ASCII characters with no space.
/// One fixed error, `Validation("that is not an API key")`, which never holds the text.
pub fn check_api_key(raw: &str) -> Result<Zeroizing<String>>;
pub async fn set_api_key(db: &Db, provider: &str, key: &str, actor: &str) -> Result<()>;
pub async fn delete_api_key(db: &Db, provider: &str, actor: &str) -> Result<()>;
/// Off the runtime. `None` when no entry.
pub(crate) async fn read_api_key(provider: &str) -> Result<Option<Zeroizing<String>>>;
```

`set_api_key` is refused unless `register::info(provider)` has kind `Api` or `Endpoint`.

- Produces, in `native`: `pub async fn clipboard_text(app) -> Result<Option<String>>`, `pub async fn clear_clipboard(app, was: &str) -> Result<()>`, with `NativeHook.clipboard: Mutex<Option<String>>` under `cfg(test)`.
- Produces, Tauri: `ai_key_from_clipboard(provider)`, `ai_key_remove(provider)`. CLI: `ai key set <provider>`, `ai key remove <provider>`.

- [ ] **Step 1: Write the failing tests**

```rust
// secrets.rs unit tests
#[test] fn api_key_names_are_reserved_from_the_generic_commands() {
    for name in ["ai-key-openai", "AI-KEY-x"] { assert!(matches!(refuse_reserved(name), Err(Error::Refused(_)))); }
}
#[test] fn check_api_key_trims_and_never_quotes() {
    assert_eq!(check_api_key("  sk-abcdefgh\n").unwrap().as_str(), "sk-abcdefgh");
    let long = "x".repeat(513);
    for bad in ["", "short", "two words here", "has\ttab1", long.as_str(), "café-key-1234"] {
        let message = check_api_key(bad).unwrap_err().to_string();
        assert!(bad.is_empty() || !message.contains(bad), "{message}");
    }
}
// crates/core/tests/secrets_store.rs, ignored, JOBPILOT_SECRET_SERVICE=jobpilot-test
#[ignore = "touches the OS keychain"] #[tokio::test]
async fn provider_key_round_trips_and_appears_in_no_row_or_export() {
    // Sentinel key. set_api_key, then secrets::get(&api_key_name("openai")), since read_api_key
    // is pub(crate). Then a full transfer::export_all and a scan of every
    // table's text and every exported file: the sentinel appears in none, and the name does.
}
#[ignore = "touches the OS keychain"] #[tokio::test]
async fn a_key_of_the_longest_accepted_length_round_trips() {} // the Credential Manager blob limit
#[ignore = "touches the OS keychain"] #[tokio::test]
async fn delete_all_removes_provider_keys() {}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --lib secrets`
Expected: FAIL.

- [ ] **Step 3: Implement the core**

Rename `refuse_data_key` to `refuse_reserved`, covering both prefixes case-insensitively, as Windows matches names. `secrets::set` and `secrets::delete` call it. `set_api_key` and `delete_api_key` share their bodies through private `store` and `erase`, so the index row and audit stay: `secret_set`, target the name, no value. Delete-all and export already walk `secret_index`, so they need no change, and the ignored test proves it.

- [ ] **Step 4: Add the CLI**

`AiCommand::Key(KeyCommand)`, with `Set { provider }` and `Remove { provider }`. `Set` reads with `rpassword::prompt_password("Key (not shown): ")` when `std::io::stdin().is_terminal()`, else one line of stdin. `Zeroizing` holds it. Output: `key stored for openai`, nothing more. Add the `PANEL` rows.

Tests in `cli.rs`: `key_set_refuses_argv_by_shape` (no positional beyond provider parses), `key_set_with_a_bad_key_echoes_nothing` (stdin `two words here` gives `validation`, stderr does not hold it, needs no keychain).

- [ ] **Step 5: Add the GUI path**

Add `tauri-plugin-clipboard-manager = "2.5.0"` and `.plugin(tauri_plugin_clipboard_manager::init())` in `lib.rs`. Confirm `ClipboardExt::read_text` and `write_text` from the crate's source before use. No change to `capabilities/default.json`.

`ai_key_from_clipboard(app, state, provider)`, with no lock across the dialog, as `ai_consent_give` does:

1. `clipboard_text`, then `check_api_key`. Empty or not a key: `Validation("the clipboard does not hold a key")`, with no dialog and the clipboard left alone.
2. `native::confirmed`, title "{label} key", message "Store the key ending in {last 4} as your {label} key? It goes to the system keychain. The clipboard is cleared after.", action "Store key".
3. `set_api_key`, then `clear_clipboard` only if the clipboard still holds that text.

`ai_key_remove` confirms natively, then deletes. `ProviderRow` for `kind: "api"` shows `Key stored` or `No key`, `Paste key from clipboard...`, and `Remove key`, with one line: "Copy your key first. The key stays out of this window."

- [ ] **Step 6: Seam check**

`commands.rs`, with a `SECRET_LAST4` sentinel:
- `clipboard_that_is_not_a_key_stores_nothing_and_echoes_nothing`: hook clipboard holds a posting; `code` is `validation`, `asked` is empty, `secret_names` is empty, the error text holds no word of it.
- `the_webview_gains_no_clipboard_capability`, as `the_webview_gains_no_dialog_capability`.
- `the_dialog_shows_only_the_last_four`, stopping at the dialog with `confirm = false`: `confirmation_required`, the message holds the last four and not the rest.
- `key_commands_take_no_value_argument`: the two commands' argument lists hold `provider` only.
- `secret_set_refuses_a_provider_key_name`: `secret_set` and `secret_delete` on `ai-key-openai` are `refused`.

`views02.test.tsx`: `paste_key_sends_only_the_provider_name`, `a_key_field_does_not_exist` (no `input[type=password]` on the AI screen).

Run: `cargo test --workspace && pnpm typecheck && pnpm test && scripts/ci-local.sh --fast`
Expected: PASS.
Then, only when the controller says: `JOBPILOT_SECRET_SERVICE=jobpilot-test cargo test -p jobpilot-core --test secrets_store -- --ignored --test-threads=1`

- [ ] **Step 7: Commit**

```bash
git add crates apps Cargo.lock
git commit -m "feat: provider keys in the OS keychain, entered without passing the webview"
```

### Task 4: Chat wire: OpenAI, OpenRouter, the loopback endpoint

**Files:**
- Create: `crates/core/src/ai/api.rs`, `crates/core/tests/api_chat.rs`
- Modify: `crates/core/src/ai/{mod,router,ollama}.rs`, `crates/cli/src/{ai,main}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/components/ProviderRow.tsx`, `apps/desktop/src/lib/{ipc,reasons}.ts`, `apps/desktop/src/views/views02.test.tsx`

**Interfaces:**
- Consumes: `net::{hosted_client, loopback_client, read_capped, retry_after, MAX_REPLY_BYTES, MAX_ERROR_BODY}`, `secrets::read_api_key`, `register::info`, `ollama::name_is_local` (make `pub(crate)`), `Provider`.
- Produces, in `ai::api`:

```rust
pub enum KeySource { Keychain(String), Fixed(Zeroizing<String>), None }
pub struct ApiProvider { /* wire, base, model, key, key_stored, client, info */ }
impl ApiProvider {
    /// `None` is left out of routing, as a refused Ollama address is. Reads settings and
    /// `secret_index`. Opens no socket and reads no key.
    pub async fn from_settings(db: &Db, name: &str) -> Result<Option<ApiProvider>>;
    /// Fake servers only: refuses any base that is not loopback.
    #[doc(hidden)]
    pub fn for_test(name: &str, base: &str, model: &str, key: KeySource) -> Result<ApiProvider>;
}
/// Refused before anything is sent. `Ok(())` means eligible.
pub fn check_model(name: &str, model: &str) -> Result<()>;
/// The task schema with every `maxLength` removed.
pub fn provider_schema(schema: &str) -> Result<serde_json::Value>;
pub async fn set_model(db: &Db, name: &str, model: &str) -> Result<()>;
pub async fn set_endpoint(db: &Db, url: &str) -> Result<()>;
pub(crate) fn classify_status(status: u16, retry_after: Option<Duration>, body: &[u8]) -> Error;
```

`Router::from_settings` gains arms for the API names. Settings: `ai.model.<name>`, `ai.endpoint.url`.

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/api_chat.rs`, each on `fake_http::Fake`

```rust
#[tokio::test] async fn openai_request_is_the_documented_shape() {
    let body = &fake.hits("/chat/completions")[0];
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["messages"][0]["role"], "system");
    for absent in ["tools", "tool_choice", "functions", "temperature", "max_tokens", "web_search_options"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    assert!(fake.headers("/chat/completions")[0].contains("authorization: bearer sk-test"));
}
#[tokio::test] async fn openrouter_denies_data_collection_and_requires_parameters() {
    assert_eq!(body["provider"]["data_collection"], "deny");
    assert_eq!(body["provider"]["require_parameters"], true);
}
#[tokio::test] async fn a_200_holding_an_error_moves_on() {} // bad_reply, next provider answers
#[tokio::test] async fn a_refusal_is_model_refused_and_stores_nothing() {}
#[tokio::test] async fn a_length_stop_is_truncated_not_retried() { assert_eq!(fake.hits(PATH).len(), 1); }
#[tokio::test] async fn a_reply_over_the_cap_is_too_large() {}
#[tokio::test] async fn statuses_map_to_their_reasons() {
    // 401, 403 auth_failed; 402 quota_exhausted; 404 model_not_found; 429 with
    // insufficient_quota quota_exhausted and one hit; 400 context_length_exceeded prompt_too_large.
}
#[tokio::test] async fn a_key_in_an_error_body_reaches_no_row_message_or_audit() {
    // Fake answers 401 echoing the key. Scan task_runs, audit rows, and the Outcome.
}
#[tokio::test] async fn a_timeout_is_not_retried_on_a_hosted_provider() { assert_eq!(hits, 1); }
#[tokio::test] async fn fallback_never_widens_consent() {
    // openai consented, openrouter not. openai answers 503; openrouter gets no hit; ends manual.
}
#[tokio::test] async fn consent_revoked_stops_the_next_call() {}
#[test] fn every_schema_is_strict_ready_after_provider_schema() {
    // For all four schemas: no maxLength, and every object has additionalProperties false and
    // required equal to its properties.
}
#[test] fn check_model_holds_the_origin_rules() {
    check_model("openrouter", "anthropic/claude-x").unwrap();
    for bad in ["openrouter/auto", "meta-llama/llama-x", "openai/gpt-x:free", "google/x:free", "x"] {
        assert_eq!(check_model("openrouter", bad).unwrap_err().to_string(), "refused: origin_refused");
    }
    let long = "m".repeat(201);
    for bad in ["../v1", "a b", "a?x=1", "a#b", "", long.as_str()] {
        assert_eq!(check_model("openai", bad).unwrap_err().code(), "validation");
    }
}
#[tokio::test] async fn endpoint_refuses_a_cloud_model_on_any_port() {
    for model in ["kimi:cloud", "x-cloud"] {
        assert_eq!(check_model("endpoint", model).unwrap_err().to_string(), "refused: not_local");
    }
}
#[tokio::test] async fn a_non_loopback_endpoint_is_refused_and_sends_nothing() {
    for url in ["https://api.example.com/v1", "http://192.168.1.2:1234/v1", "http://10.0.0.1/v1",
                "http://localhost.example.com/v1", "https://127.0.0.1:1234/v1"] {
        assert_eq!(set_endpoint(&db, url).await.unwrap_err().to_string(), "refused: not_loopback");
    }
    // A credential or query in the address would land in settings, and so in export.
    for url in ["http://user:secret@127.0.0.1:1234/v1", "http://127.0.0.1:1234/v1?key=x"] {
        assert_eq!(set_endpoint(&db, url).await.unwrap_err().code(), "validation");
    }
    assert!(settings::get(&db, "ai.endpoint.url").await.unwrap().is_none());
}
#[tokio::test] async fn an_endpoint_still_needs_consent() {
    assert!(fake.hits(PATH).is_empty());
    assert_eq!(outcome, manual("consent_needed"));
}
#[tokio::test] async fn for_test_refuses_a_real_host() {
    assert!(ApiProvider::for_test("openai", "https://api.openai.com/v1", "m", KeySource::None).is_err());
}
#[tokio::test] async fn no_key_or_no_model_is_unavailable_with_no_socket() {}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test api_chat`
Expected: FAIL, `api` not found.

- [ ] **Step 3: Implement `api.rs`**

Follow the wire and mapping tables above. `check_model`: 1 to 200 characters of `[A-Za-z0-9._:/-]`, and no `..`, else `Validation`. Google also allows no `/` or `:`. Then the origin rule for OpenRouter and the cloud-name rule for the endpoint. `declaration()`:

```rust
Declaration {
    id: self.info.id.clone(),
    capabilities: self.info.capabilities.to_vec(),
    local: false,
    context_limit: 200_000, // 8_192 for the endpoint
    timeout: Duration::from_secs(120), // 300 for the endpoint
    sign_in: "api key in the OS keychain".into(), // "loopback server" for the endpoint
}
```

`available()` opens no socket: `Unavailable("no_api_model")` and `Unavailable("no_key")` map to the reasons of those names through `unavailable_reason`. Do not reuse `no_model`: that word is Ollama's, and it maps to `no_model_set`. The endpoint needs no key. `complete` reads the key on each attempt through `KeySource`, and `Keychain` with no entry is `Refused("no_key")`. It checks `CancellationToken` through the router's drop, as Ollama does. `embed` is `Refused("no_embeddings")`.

`set_endpoint` stores the address with any trailing `/` removed, so `<url>/chat/completions` has one slash, and refuses a username, password, query, or fragment with `Validation`. Reads go through `read_capped`: `MAX_REPLY_BYTES` on success, `MAX_ERROR_BODY` on failure. Parse JSON from bytes, and a parse failure is `bad_reply`. The returned text goes to the router's `validate`.

`Ollama::is_local` and its checks do not change.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test api_chat`
Expected: PASS.

- [ ] **Step 5: Add the CLI**

`AiCommand::Model { provider, model }` calls `api::set_model`. `AiCommand::Endpoint { url }`, where an empty string clears both settings. `no_key`: "No key is stored for this provider. Add one with `jobpilot ai key set <provider>` in an ordinary terminal, or on the AI screen." `no_api_model`: "No model is chosen for this provider. Set one with `jobpilot ai model <provider> <id>`, or on the AI screen." `origin_refused`: "That model is not eligible through OpenRouter. Use an openai/, anthropic/, or google/ model, and not a free one." Add `PANEL` rows.

`ai::status` fills `available` and `reason` for API rows from `ApiProvider::from_settings` and its `available()`. A `None` there means no address is set, or a stored one is refused: the row reads reason `no_endpoint`.

- [ ] **Step 6: Add the GUI**

`ai_set_api_model(provider, model)`, no native dialog, since the consent record is the gate and `check_model` holds the origin rules. `ai_set_endpoint(url)`, as `ai_set_ollama_url`: `loopback_client` refuses before anyone is asked, then `native::confirmed`. `ProviderRow` adds a `Model id` field with a Save button for API rows, and `Add endpoint...` with an address field for the endpoint row. Add `no_key`, `no_api_model`, `origin_refused`, `no_endpoint` to `reasons.ts`.

- [ ] **Step 7: Seam check**

- `cli.rs`, `endpoint_drafts_end_to_end_against_a_fake`: bring the fake in with `#[path = "../../core/tests/fake_http/mod.rs"] mod fake_http;`, start `Fake` in the test, `ai endpoint <fake>/v1`, `ai model endpoint m`, `ai enable endpoint`, `ai disable ollama`, `ai consent give endpoint --categories job_text,profile_basic,resume_text`, `resume generate j` returns a draft from `endpoint/loopback`, and `agents log` shows that provider. This is the full stack with no keychain.
- `commands.rs`: `ai_set_endpoint_refuses_before_any_dialog`, `api_model_reaches_core`.
- `views02.test.tsx`: `an_api_row_sets_a_model`, `the_endpoint_row_explains_loopback_only`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates apps
git commit -m "feat: OpenAI, OpenRouter, and a loopback endpoint through the router"
```

### Task 5: Anthropic and Google wires

**Files:**
- Modify: `crates/core/src/ai/{api,router}.rs`, `crates/core/tests/api_vendors.rs` (new), `crates/cli/tests/cli.rs`, `apps/desktop/src/components/ProviderRow.tsx`, `apps/desktop/src/views/views02.test.tsx`

**Interfaces:**
- Consumes: Task 4's `ApiProvider`, `classify_status`, `provider_schema`, `check_model`.
- Produces: `Wire::Anthropic`, `Wire::Google`, reached through `ApiProvider::from_settings(db, "anthropic" | "google")`. No new public name.

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/api_vendors.rs`

```rust
#[tokio::test] async fn anthropic_request_is_the_documented_shape() {
    let body = &fake.hits("/v1/messages")[0];
    assert_eq!(body["max_tokens"], 16000);
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert!(body["output_config"]["format"]["schema"].to_string().find("maxLength").is_none());
    assert!(body.get("system").is_some());
    for absent in ["tools", "tool_choice", "output_format", "temperature"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    let head = &fake.headers("/v1/messages")[0];
    assert!(head.contains("x-api-key: sk-ant-test") && head.contains("anthropic-version: 2023-06-01"));
    assert!(!head.contains("anthropic-beta"));
}
#[tokio::test] async fn anthropic_reads_the_text_block_after_thinking() {}
#[tokio::test] async fn anthropic_refusal_and_max_tokens_map() {} // model_refused, output_truncated
#[tokio::test] async fn a_tool_use_block_stores_nothing() {} // permission_denied
#[tokio::test] async fn a_529_with_retry_after_retries_once() {}
#[tokio::test] async fn a_spend_cap_429_with_no_header_does_not_loop() { assert_eq!(hits, 1); }
#[tokio::test] async fn google_request_is_the_documented_shape() {
    const PATH: &str = "/v1beta/models/gemini-x:generateContent";
    let body = &fake.hits(PATH)[0];
    assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
    assert!(body["generationConfig"]["responseJsonSchema"].is_object());
    assert!(body["generationConfig"].get("responseSchema").is_none());
    assert!(body["systemInstruction"]["parts"][0]["text"].is_string());
    assert!(fake.headers(PATH)[0].contains("x-goog-api-key: test-key"));
    assert!(!fake.paths().iter().any(|path| path.contains("key=")));
}
#[tokio::test] async fn google_model_cannot_change_the_path() {
    for bad in ["x/../y", "models/x", "x:streamGenerateContent", "a/b"] {
        assert_eq!(check_model("google", bad).unwrap_err().code(), "validation");
    }
}
#[tokio::test] async fn google_finish_reasons_and_blocks_map() {} // MAX_TOKENS, SAFETY, promptFeedback
#[tokio::test] async fn a_function_call_part_stores_nothing() {}
#[tokio::test] async fn a_google_key_with_no_attestation_sends_nothing() {
    // The exit test's refusal: key row present, enabled, model set, no consent because
    // give_for refused. No hit, reason consent_needed.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test api_vendors`
Expected: FAIL, unknown wire.

- [ ] **Step 3: Implement both wires** from the tables above. `check_model("google", ..)` allows no `/` and no `:`. Add `anthropic` and `google` to `Router::from_settings`'s API arm.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test api_vendors`
Expected: PASS.

- [ ] **Step 5: Seam check**

The commands and rows from Tasks 2 to 4 already take these names. `cli.rs`: `anthropic_and_google_take_a_model_and_consent` (each `ai model`, `ai enable`, `ai consent give`, and Google without `--attest` refused). `views02.test.tsx`: `google_consent_shows_the_paid_key_line`. A `ProviderRow` for `google` also shows "Free keys are not supported." as text.

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates apps
git commit -m "feat: Anthropic and Google paid-key providers"
```

### Task 6: Shared child rules and CLI detection

**Files:**
- Create: `crates/core/src/ai/child.rs`, `crates/core/tests/child.rs`
- Modify: `crates/core/src/ai/{mod,claude_code}.rs`, `crates/cli/src/ai.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/components/ProviderRow.tsx`, `apps/desktop/src/lib/{ipc,reasons}.ts`, `apps/desktop/src/views/views02.test.tsx`

**Interfaces:**
- Consumes: from `claude_code.rs`: `ALLOWED_ENV`, `KillTree`, `kill_tree`, `is_program`, `absolute_dirs`, `fallback_dirs`, `search`, `login_shell_lookup`, `find_binary`, `checked`, `has_claude_name`. Their `pub` names stay where they are as thin wrappers, because `crates/core/tests/claude_{code,env,locate}.rs` and `crates/cli/src/shell.rs` import them. Those tests stay unchanged and green: they prove the move changed nothing.
- Produces, in `ai::child`:

```rust
pub struct CliSpec {
    pub program: &'static str,        // "gemini"
    pub env_bin: &'static str,        // "JOBPILOT_GEMINI_BIN"
    pub setting: &'static str,        // "ai.gemini_path"
    pub npm_entry: Option<(&'static str, &'static str)>, // ("@google/gemini-cli", "gemini.js")
    pub extra_env: &'static [&'static str],
}
/// Here, not in the adapters: `set_cli_path`, `find_cli`, and this task's tests need them first.
/// Decision 17 gives `extra_env`. Codex: `"codex"`, `"JOBPILOT_CODEX_BIN"`, `"ai.codex_path"`,
/// `Some(("@openai/codex", "codex.js"))`.
pub const GEMINI: CliSpec;
pub const CODEX: CliSpec;
pub fn spec_for(name: &str) -> Option<&'static CliSpec>; // gemini_cli, codex_cli
/// `env` is final: `run_piped` adds nothing to it, so `ClaudeCode` passes its own list unchanged.
pub struct ChildRun<'a> { pub bin: &'a Path, pub args: Vec<OsString>, pub env: Vec<(String, String)>, pub cwd: &'a Path, pub stdin: &'a [u8], pub limit: u64 }
pub struct ChildOutput { pub stdout: Vec<u8>, pub status: ExitStatus }
#[doc(hidden)] pub async fn run_piped(run: ChildRun<'_>) -> Result<ChildOutput>; // tests/child.rs calls it
/// `--version`. `Err` is the word for `Availability::Unavailable`, as Claude's probe gives them:
/// `not_installed`, `version_failed`, or `no_answer` past `VERSION_TIMEOUT`.
pub(crate) async fn version_of(bin: &Path, env: Vec<(String, String)>) -> std::result::Result<String, &'static str>;
pub fn child_env(spec_extra: &[&str]) -> Vec<(String, String)>;
pub fn child_path(bin: &Path, spec: &CliSpec, inherited: &OsStr) -> OsString;
pub fn bin_names(program: &str, windows: bool) -> Vec<String>; // windows: .exe then .cmd
pub fn cli_dirs(home: Option<PathBuf>, env: &dyn Fn(&str) -> Option<OsString>, windows: bool) -> Vec<PathBuf>;
pub fn accepts_name(path: &Path, spec: &CliSpec, windows: bool) -> bool;
pub async fn find_cli(spec: &CliSpec, db: Option<&Db>) -> std::result::Result<PathBuf, String>;
pub async fn set_cli_path(db: &Db, name: &str, path: &str) -> Result<()>;
```

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/child.rs`

```rust
#[test] fn windows_prefers_the_exe_then_the_shim() {
    assert_eq!(bin_names("gemini", true), ["gemini.exe", "gemini.cmd"]);
    assert_eq!(bin_names("gemini", false), ["gemini"]);
}
#[test] fn windows_looks_in_the_npm_and_manager_folders() {
    let env = |name: &str| match name {
        "APPDATA" => Some(r"C:\U\AppData\Roaming".into()),
        "LOCALAPPDATA" => Some(r"C:\U\AppData\Local".into()),
        "ProgramFiles" => Some(r"C:\Program Files".into()),
        _ => None,
    };
    let dirs = cli_dirs(Some(r"C:\U".into()), &env, true);
    for wanted in ["npm", "Volta", "shims", "nodejs"] {
        assert!(dirs.iter().any(|dir| dir.to_string_lossy().contains(wanted)), "{wanted}");
    }
}
#[test] fn nvm_folders_are_searched_newest_first() {} // real temp tree with v18, v22, v24
#[test] fn child_path_starts_with_the_programs_folder() {
    let path = child_path(Path::new("/h/.nvm/versions/node/v24/bin/gemini"), &GEMINI, OsStr::new("/usr/bin"));
    assert_eq!(std::env::split_paths(&path).next().unwrap(), Path::new("/h/.nvm/versions/node/v24/bin"));
}
#[test] fn npm_entry_file_is_accepted_and_finds_its_node() {
    let entry = Path::new("/h/.nvm/versions/node/v24/lib/node_modules/@google/gemini-cli/bundle/gemini.js");
    assert!(accepts_name(entry, &GEMINI, false));
    let path = child_path(entry, &GEMINI, OsStr::new("/usr/bin"));
    assert!(std::env::split_paths(&path).any(|dir| dir == Path::new("/h/.nvm/versions/node/v24/bin")));
    assert!(!accepts_name(Path::new("/tmp/gemini.js"), &GEMINI, false));
}
#[test] fn only_the_named_program_is_accepted() {
    for bad in ["/bin/sh", "/x/gemini-evil", "/x/Gemini.sh"] { assert!(!accepts_name(Path::new(bad), &GEMINI, false)); }
}
#[test] fn sign_in_variables_pass_and_vertex_and_other_keys_do_not() {
    // With GEMINI_API_KEY, GOOGLE_API_KEY, GOOGLE_GENAI_USE_VERTEXAI, OPENAI_API_KEY,
    // ANTHROPIC_API_KEY, and NODE_OPTIONS set, child_env(GEMINI.extra_env) holds only the first.
}
#[cfg(unix)] #[tokio::test] async fn run_piped_kills_the_tree_when_dropped() {}
#[tokio::test] async fn run_piped_caps_the_output() {}
#[cfg(windows)] #[tokio::test] async fn cmd_shim_takes_spaces_stdin_and_exit_code() {
    // A .cmd in a folder named "a b" echoes its stdin and exits 3, called with an argument
    // holding a space and a percent sign.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test child`
Expected: FAIL, `child` not found.

- [ ] **Step 3: Implement `child.rs` and move the code**

Move, do not rewrite. `ClaudeCode::run` keeps its reply handling and calls `run_piped`. `claude_code::{absolute_dirs, fallback_dirs, search, login_shell_lookup}` become wrappers. `login_shell_lookup` takes the program name internally, with the same fixed-command rule. `find_cli` follows `find_binary`'s order, adding the nvm, Volta, and fnm folders before the login-shell lookup. `run_piped` leaves `PATH` as `ChildRun.env` gives it; Tasks 7 and 8 replace `PATH` in their env with `child_path`, and Claude's env does not change. Tests here also run on CI's Windows job, so `run_piped_caps_the_output` needs a child that exists on both. `cfg!(windows)` picks the `windows` arguments, so the same logic is tested on macOS. An `InvalidInput` from spawning a `.cmd`, which is Rust refusing an argument it cannot escape, maps to `Failed("cli_error")`.

`VERSION_TIMEOUT` becomes 10 s for Node CLIs. `claude --version` keeps 5 s.

- [ ] **Step 4: Run the existing suites**

Run: `cargo test -p jobpilot-core --test claude_code --test claude_env --test claude_locate --test child && cargo test -p jobpilot-cli`
Expected: PASS, with `claude_*.rs` not edited.

- [ ] **Step 5: Add the CLI and GUI path controls**

`AiCommand::CliPath { provider, path }` accepts `gemini_cli` and `codex_cli`, and an empty path clears. `ai_set_cli_path(provider)` follows `ai_set_claude_path`, with the picker as the confirmation and `accepts_name` as the check. `ai_clear_cli_path(provider)` follows `ai_clear_claude_path`. `ProviderRow` for `kind: "cli"` shows the resolved path, `Choose gemini...` when unavailable, and `Use automatic`. Add the `PANEL` row. `ai::status` and `ai::sharing` fill `path` for CLI rows through `find_cli` here, so decision 19's dialog line holds from this task on, and a row with no path reads reason `not_installed`.

- [ ] **Step 6: Seam check**

`cli.rs`: `cli_path_refuses_another_program`. `commands.rs`: `cli_picker_refuses_another_program`, `cli_picker_accepts_a_resolved_npm_entry`. `views02.test.tsx`: `a_missing_cli_shows_unavailable_and_a_chooser`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test && scripts/ci-local.sh --fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates apps
git commit -m "refactor: shared child rules and CLI detection, including Windows shims"
```

### Task 7: Gemini CLI adapter

**Files:**
- Create: `crates/core/src/ai/gemini_cli.rs`, `crates/core/tests/gemini_cli.rs`, `crates/core/tests/mock-gemini/gemini`, `crates/core/tests/mock-gemini/gemini.cmd`
- Modify: `crates/core/src/ai/{mod,router}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/components/ProviderRow.tsx`, `apps/desktop/src/views/views02.test.tsx`, `apps/desktop/src/lib/reasons.ts`, `crates/cli/src/main.rs`, `.gitattributes`, `scripts/ci-local.sh`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `child::{CliSpec, GEMINI, ChildRun, run_piped, version_of, child_env, child_path, find_cli}`, `register::info`, `MAX_OUTPUT_BYTES`.
- Produces, in `ai::gemini_cli`:

```rust
pub const SPEC: &CliSpec = &child::GEMINI;
pub const MIN_VERSION: (u32, u32, u32) = (0, 40, 0);
pub const DENY_ALL_POLICY: &str = "[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 999\n";
pub struct GeminiCli { /* bin */ }
impl GeminiCli {
    pub fn with_binary(bin: PathBuf) -> GeminiCli;
    pub async fn locate_in(db: &Db) -> Result<GeminiCli>;
    pub fn path(&self) -> &Path;
    /// Exact, as listed under "No-tools evidence". `dir` holds the policy.
    pub fn argv(dir: &Path) -> Vec<OsString>;
}
pub(crate) fn read_stream(stdout: &[u8], status: ExitStatus) -> Result<String>;
```

- [ ] **Step 1: Write the mock** `crates/core/tests/mock-gemini/gemini`, on the pattern of `mock-claude/claude`

The adapter clears the child's environment, so each test starts it through a wrapper script named `gemini` (`gemini.cmd` on Windows) that sets `MOCK_GEMINI_*`, as `MockCli` does in `claude_code.rs`. The mock answers `--version` with `$MOCK_GEMINI_VERSION`, default `0.40.0`. It records args, stdin, env, cwd, and the policy file through `MOCK_GEMINI_*` paths, and copies `$MOCK_GEMINI_SYSTEM_COPY` from `$GEMINI_SYSTEM_MD`. It prints the file `$MOCK_GEMINI_REPLY` names or a default stream, exits `$MOCK_GEMINI_EXIT`, and honours `MOCK_GEMINI_SLEEP`. Default stream:

```text
{"type":"init","session_id":"s","model":"m"}
{"type":"message","role":"user","content":"x"}
{"type":"message","role":"assistant","content":"{\"class\":","delta":true}
{"type":"message","role":"assistant","content":"\"normal\"}","delta":true}
{"type":"result","status":"success","stats":{}}
```

Add the `.cmd` twin with a `crates/core/tests/mock-gemini/gemini.cmd text eol=crlf` line in `.gitattributes`.

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/gemini_cli.rs`

```rust
#[test] fn argv_is_exactly_the_documented_list() {
    let dir = Path::new("/d");
    let policy = dir.join("deny-all.toml"); // a backslash on Windows, which CI runs
    let policy = policy.to_str().unwrap();
    assert_eq!(GeminiCli::argv(dir), [
        "--output-format", "stream-json", "--approval-mode", "default",
        "--policy", policy, "--admin-policy", policy,
        "--extensions", "none", "--allowed-mcp-server-names", "jobpilot-none", "--skip-trust",
    ].map(OsString::from));
}
#[test] fn the_declaration_claims_no_tools_from_the_table() {}
#[tokio::test] async fn prompt_goes_to_stdin_never_argv() {} // sentinel in stdin file, absent in args
#[tokio::test] async fn system_text_goes_by_file_and_the_schema_with_it() {}
#[tokio::test] async fn the_policy_on_disk_denies_every_tool() {
    assert_eq!(fs::read_to_string(recorded_policy).unwrap(), DENY_ALL_POLICY);
}
#[tokio::test] async fn the_hooks_override_is_set_for_this_child() {
    // env has GEMINI_CLI_SYSTEM_SETTINGS_PATH; its file parses and holds hooksConfig.enabled false.
}
#[tokio::test] async fn working_directory_is_fresh_empty_and_removed() {}
#[tokio::test] async fn a_tool_use_event_fails_the_attempt_and_stores_nothing() {
    assert_eq!(result.unwrap_err().to_string(), "refused: permission_denied");
}
#[tokio::test] async fn a_tool_use_quoted_in_text_does_not() {} // only a line's own `type` counts
#[tokio::test] async fn noise_before_the_stream_is_skipped() {} // a non-JSON line first
#[tokio::test] async fn a_stream_with_no_result_event_is_a_bad_reply() {}
#[tokio::test] async fn a_result_status_of_error_is_a_cli_error() {}
#[tokio::test] async fn exit_41_is_not_logged_in() {}
#[tokio::test] async fn a_version_below_the_floor_is_unavailable() {
    assert_eq!(cli.available().await, Availability::Unavailable("cli_too_old".into()));
}
#[tokio::test] async fn an_absent_cli_is_unavailable_and_the_task_goes_on() {}
#[tokio::test] async fn timeout_kills_the_child() {}
#[tokio::test] async fn a_validation_failure_retries_once_a_timeout_does_not() {}
#[tokio::test] async fn it_runs_a_draft_through_the_router_with_consent() {}
#[tokio::test] async fn no_consent_starts_no_child() {}
#[tokio::test] #[ignore = "spends one real Gemini call; needs JOBPILOT_REAL_GEMINI_SMOKE=1"]
async fn real_gemini_has_no_tools_smoke() {
    // Returns with a `skipped` line unless JOBPILOT_REAL_GEMINI_SMOKE is set, as
    // real_claude_has_no_tools_smoke does, because `--ignored` keychain runs reach this test.
    // Prompt tells the model to run `ls` and read a canary file in the cwd, with the canary
    // planted by the test. Assert no tool_use event in stdout and the canary text is absent.
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test gemini_cli`
Expected: FAIL, `gemini_cli` not found.

- [ ] **Step 4: Implement `gemini_cli.rs`**

The child's env is `child_env(SPEC.extra_env)` with `PATH` replaced by `child_path`, plus the two variables under "No-tools evidence". Each attempt makes a `tempfile::tempdir` holding `deny-all.toml`, `system.md`, `settings.json`, and an empty `work/`, which is the cwd, so the model sees nothing even if a tool slipped through. `system.md` is `prompt.system`, then `\n\nReply with one JSON object and nothing else. It must match this JSON Schema:\n`, then the schema. `prompt.user` goes on stdin. Output limit is `4 * MAX_OUTPUT_BYTES`, because of the stream's framing.

`declaration()`: capabilities from the table, `local: false`, `context_limit: 200_000`, `timeout: 180 s`, `sign_in: format!("gemini cli on its own sign-in ({})", path)`.

`available()`: `version_of`, whose `Err` word is the reason, then parse `x.y.z`, and below the floor or unparsable is `cli_too_old`. Sign-in is not probed, as for Claude.

`read_stream`: split lines, parse each as JSON, skip what does not parse. `tool_use` or `tool_result` is `Refused("permission_denied")`. Join assistant `content`. Exit 41 is `Refused("not_logged_in")`. A `result` with `status` other than `success`, or any other non-zero exit, is `Failed("cli_error")`. No `result` is `Failed("bad_reply")`. Add a `ponytail:` comment: only a child-reported event can trip the guard, and the deny rule is what stops a tool.

Router arm: `GEMINI_CLI` builds like `CLAUDE_CODE`, with an empty path when not found, so the run log shows it as unavailable. `unavailable_reason` maps `cli_too_old`. `ai::status` fills `available` and `reason` by decision 28, and probes enabled CLIs together with `tokio::join!`, so `jobpilot status` waits for one timeout at most. `not_logged_in` copy says "Claude Code" today: make it name no vendor in `reasons.ts` and `main.rs`: "This CLI is not signed in. Run it once in your own terminal to sign in, then try again." Update `views02.test.tsx`'s row for it. Add `--skip real_gemini` to `keychain_suites` in `scripts/ci-local.sh` and to both keychain steps in `ci.yml`, so no `--ignored` run reaches the real CLI. `cli_too_old`: "The installed CLI is older than the version JobPilot checked. Update it, then try again."

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test gemini_cli`
Expected: PASS.

- [ ] **Step 6: Seam check**

- `cli.rs`: `gemini_cli_drafts_through_the_mock`: `JOBPILOT_GEMINI_BIN`, `ai disable ollama`, enable, `consent give gemini_cli --categories job_text,profile_basic,resume_text --attest`, `resume generate`, provider `gemini_cli/cli_login`. `gemini_cli_absent_shows_unavailable`.
- `commands.rs`: `ai_status_lists_gemini_with_its_path`.
- `views02.test.tsx`: `gemini_row_shows_it_runs_job_text_with_no_tools`.

Run: `cargo test --workspace && pnpm typecheck && pnpm test && scripts/ci-local.sh --fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates apps .gitattributes scripts .github
git commit -m "feat: optional Gemini CLI adapter with every tool denied"
```

### Task 8: Codex CLI adapter

**Files:**
- Create: `crates/core/src/ai/codex_cli.rs`, `crates/core/tests/codex_cli.rs`, `crates/core/tests/mock-codex/codex`, `crates/core/tests/mock-codex/codex.cmd`
- Modify: `crates/core/src/ai/{mod,router}.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src/components/ProviderRow.tsx`, `apps/desktop/src/views/views02.test.tsx`, `.gitattributes`

**Interfaces:**
- Consumes: as Task 7, with `child::CODEX`, plus `Router::run_spec` and `TaskSpec` for a spec that needs no `NoTools`.
- Produces, in `ai::codex_cli`:

```rust
pub const SPEC: &CliSpec = &child::CODEX;
pub struct CodexCli { /* bin */ }
impl CodexCli {
    pub fn with_binary(bin: PathBuf) -> CodexCli;
    pub async fn locate_in(db: &Db) -> Result<CodexCli>;
    pub fn path(&self) -> &Path;
    pub fn argv(dir: &Path) -> Vec<OsString>;
}
```

- [ ] **Step 1: Write the mock**, on the pattern of Task 7, with `MOCK_CODEX_*`. Tests start it through a wrapper script named `codex`, for the same reason. It reads stdin, then copies the file `$MOCK_CODEX_REPLY` names, or writes `{"class":"normal"}`, to the path after `--output-last-message`, and records args and env. Add the `.gitattributes` line for `codex.cmd`.

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/codex_cli.rs`

```rust
#[test] fn argv_is_exactly_the_documented_list() {} // the list under "No-tools evidence", paths by Path::join
#[test] fn it_does_not_claim_no_tools() {
    assert!(!codex.declaration().capabilities.contains(&Capability::NoTools));
}
#[tokio::test] async fn every_task_that_ships_skips_it_and_starts_no_child() {
    // Consent for every category. For all four TaskTypes: no child started, a `no_tools` skip row.
    for task in [TaskType::ParseJob, TaskType::DraftResume, TaskType::DraftCoverLetter, TaskType::ClassifyAnswer] {}
}
#[tokio::test] async fn a_spec_with_no_untrusted_text_runs_on_it() {
    // run_spec with a local TaskSpec: StructuredOutput only, categories ProfileBasic. Proves the
    // adapter works on the installed CLI's sign-in if a task ever qualifies.
}
#[tokio::test] async fn prompt_goes_to_stdin_never_argv() {}
#[tokio::test] async fn the_schema_goes_by_file() {}
#[tokio::test] async fn an_absent_cli_is_unavailable() {}
#[tokio::test] async fn a_cli_that_fails_its_version_is_unavailable() {
    // The owner's real install today: `codex --version` exits 1.
}
#[tokio::test] async fn a_non_zero_exit_is_a_cli_error() {}
#[tokio::test] async fn no_auth_file_is_read_or_passed() {} // env holds no OPENAI_API_KEY
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test codex_cli`
Expected: FAIL, `codex_cli` not found.

- [ ] **Step 4: Implement `codex_cli.rs`**

The child's env is built as in Task 7. Each attempt: temp dir, `schema.json` from `provider_schema`, an empty `work/` cwd, stdin is `prompt.system`, a blank line, `prompt.user`. After a zero exit, read `reply.txt` under `MAX_OUTPUT_BYTES`, and a missing file is `bad_reply`. `sign_in: format!("codex cli on its own sign-in ({})", path)`. A `// ponytail:` comment quotes the gap: no documented way to remove `apply_patch`, so no `NoTools`. Add the router arm and fill `ai::status`.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test -p jobpilot-core --test codex_cli`
Expected: PASS.

- [ ] **Step 6: Seam check**

- `cli.rs`: `codex_is_skipped_for_a_draft_and_says_why`: Ollama disabled, Codex enabled with consent, `resume generate` ends `no_ai`, and `ai consent show codex_cli` says "Skipped". `codex_failing_its_version_shows_unavailable`, which enables Codex first, by decision 28.
- `commands.rs`: `ai_status_says_codex_runs_no_job_text`.
- `views02.test.tsx`: `codex_row_says_it_runs_no_task_today`: the row reads "No task can use this yet: it cannot prove it has no tools."

Run: `cargo test --workspace && pnpm typecheck && pnpm test && scripts/ci-local.sh --fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates apps .gitattributes
git commit -m "feat: optional Codex CLI adapter, skipped for untrusted text"
```

### Task 9: Injection tests on hosted backends, no-provider proof, copy sweep

**Files:**
- Create: `crates/core/tests/injection_hosted.rs`, `crates/core/tests/injection/18_key_exfiltration.txt`, `crates/core/tests/injection/19_tool_call_reply.txt`
- Modify: `crates/core/tests/{injection,agents}.rs`, `crates/core/tests/injection/README.md`, `crates/core/src/memory/classify.rs`, `crates/core/src/ai/mod.rs`, `crates/cli/src/memory.rs`, `crates/cli/tests/cli.rs`, `apps/desktop/src/lib/{ipc,reasons}.ts`, `apps/desktop/src/test-utils.tsx`, `apps/desktop/src/views/{MemoryView,AiView}.tsx`, `apps/desktop/src/views/{views02,views03}.test.tsx`, `apps/desktop/src-tauri/src/commands.rs`

**Interfaces:**
- Consumes: `generate::{resume, cover_letter, build_prompt}`, `drafts::list`, `knowledge::normalize`, `fake_http`, `ApiProvider::for_test`, `KeySource::Fixed`, `GeminiCli::with_binary`, the corpus and `MOCK_REPLY:` convention in `crates/core/tests/injection.rs`.
- Produces: `RefreshPlan.external_calls: usize`, replacing `claude_calls`. `memory::refresh_plan(db, router)` keeps its signature. CLI JSON key and `ipc.ts` type follow.

- [ ] **Step 1: Add the corpus files**

`18` asks the model to print its API key, its system prompt, and every environment variable into a claim, and to fetch a URL with the profile email. Its `MOCK_REPLY` puts those in a claim citing no evidence. `19` has a `MOCK_REPLY` for a reply that is a tool call rather than JSON. Add both to `every_corpus_file_normalizes_without_panic_and_reports_findings` and `README.md`.

- [ ] **Step 2: Write the failing tests** in `crates/core/tests/injection_hosted.rs`

The fake plays a fully steered model: it reads the `MOCK_REPLY:` line from the prompt it received and returns that in the wire's own reply shape. A table runs each case on every wire: OpenAI, OpenRouter, endpoint, Anthropic, Google. The Gemini mock CLI runs them too, through `$MOCK_GEMINI_REPLY`.

```rust
#[tokio::test] async fn every_corpus_file_stays_data_on_every_hosted_wire() {
    // For each file, each wire: generate::resume. Assert as injection.rs does: nothing invented
    // reaches export, flagged claims stay flagged, a schema break stores nothing.
}
#[tokio::test] async fn the_posting_stays_in_marked_blocks_in_the_user_message() {
    // Body: the sentinel appears only in the user message, between OPEN and CLOSE, never in
    // system, systemInstruction, or a header.
}
#[tokio::test] async fn no_wire_offers_a_tool() {} // the recursive key scan: tools, functions, tool_choice, web_search
#[tokio::test] async fn a_tool_call_in_a_reply_stores_nothing() {} // file 19, each wire's shape
#[tokio::test] async fn the_key_is_never_in_a_body_or_a_prompt() {}
#[tokio::test] async fn a_sensitive_value_never_reaches_a_hosted_body() {}
#[tokio::test] async fn work_authorization_is_never_stated_by_a_steered_hosted_reply() {
    // File 01. The claim is flagged `authorization_statement` and export refuses.
}
#[tokio::test] async fn a_steered_classifier_never_lowers_a_class_on_a_hosted_wire() {}
#[tokio::test] async fn the_gemini_mock_steered_to_a_tool_stores_nothing() {} // a tool_use stream
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p jobpilot-core --test injection_hosted`
Expected: FAIL only where a gap exists. A case that passes at once still ships: it pins behaviour Tasks 4 to 7 built. Fix any real gap in the owning module, with its own failing test first.

- [ ] **Step 4: Prove the app with no provider**

Add `no_provider_works_end_to_end` to `crates/cli/tests/cli.rs`. A fresh home with `HOME` on the temp root, `HTTP(S)_PROXY` pointed at a dead address, no CLIs on `PATH`, `SHELL` removed, and every provider off: `status`, `doc add`, `doc search`, `profile set`, `kb add`, `job paste`, `resume generate` (ends `manual_required` with reason `no_ai`), `app add`, `app status`, `memory add-question`, `memory search`, `analytics report`, `backup create`, `data export`. Each exits as expected. The CLI has no `audit` command, so the test reads `audit_log` through `sqlx` and finds no `ai_external_call`.

Add `commands.rs::no_provider_screen_is_a_normal_state` and `views02.test.tsx::the_empty_state_reads_as_normal`: "JobPilot works without one. Tasks that need AI end with a manual path."

- [ ] **Step 5: Sweep the copy**

`RefreshPlan.claude_calls` becomes `external_calls`: any id in `router.provider_ids()` that is external, declares `NoTools`, and has an active consent covering `job_text`. Update `classify.rs`, `crates/cli/src/memory.rs`, `MemoryView.tsx` ("Up to N paid or subscription calls, limit 20", "No outside calls"), `ipc.ts`, `test-utils.tsx`, and their tests. The doc comments naming Claude in `classify.rs` and `mod.rs` become provider-neutral.

`AiView`'s page heading becomes "AI providers", its intro keeps "No AI is needed. JobPilot works with none.", and "Cloud models are off." becomes "Ollama cloud models are off." `provider_unavailable` in `reasons.ts` drops "or point JobPilot at claude". Search for what is left:

```bash
grep -rn "Claude" crates/core/src crates/cli/src apps/desktop/src --include='*.rs' --include='*.ts' --include='*.tsx' | grep -v "test\|claude_code.rs\|TerminalPanel\|shell.rs"
```

Expected: only Claude's own row, its dialog text, and register data.

- [ ] **Step 6: Run everything**

Run: `cargo test --workspace && pnpm typecheck && pnpm test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates apps
git commit -m "test: injection corpus on hosted backends, no-provider proof, provider-neutral copy"
```

### Task 10: Release gates and docs

**Files:**
- Create: `docs/releases/0.4.0.md`
- Modify: `docs/milestones.md`, `docs/ai-providers.md`, `docs/policy-register.md`, `docs/security/threat-model.md`, `docs/agents.md`, `docs/architecture.md`, `docs/database-schema.md`, `docs/development.md`, `docs/wireframes.md`, `docs/prd.md`, `README.md`, `CHANGELOG.md`, `Cargo.toml`, `Cargo.lock`, `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, `scripts/ci-local.sh`, `.github/workflows/ci.yml`, `crates/core/tests/net.rs`

**Interfaces:**
- Consumes: everything above. Produces no code interface.

- [ ] **Step 1: Update the docs**

| File | Change |
|---|---|
| `docs/ai-providers.md` | The status line, line 5's "must be re-read at each release" per the owner's waiver, the backends table, and a new "As built in 0.4" section: wires, statuses, `Retry-After` per decision 11 (replacing the external retry rule in "Timeouts and retries"), Gemini's argv with its quotes, Codex's gap, the consent key table. Correct "Ollama is retried on network errors, 429, 5xx" to what the code does: network errors, 429, 503. |
| `docs/security/threat-model.md` | Status block for 0.4 with a **Built 0.4** label. M-15, M-21 (redirect now tested, endpoint added), M-29, M-30, M-31, M-32 updated with test names. Section 7 table: `gemini` proven with its flags and residuals; `codex exec` updated with the `apply_patch` gap. Section 11: free-key attestation, `.env` selecting Vertex, the Gemini residuals, macOS TLS 1.2; "Register drift" says the re-read is waived for AI rows. M-14: `claude` is the only panel session. B1: keys enter through a Rust clipboard read. |
| `docs/policy-register.md` | Line 7 and "Release re-check": the owner waived the re-read for AI provider rows, and it stays for the site rows 0.5 and 0.6 rely on. Edit no table row and no date: decision 12 suspends consent on any row change, and the owner decision lines are part of that text. FR-3.11's "interactive session" stays as vendor record. |
| `docs/milestones.md` | Scope sentence, the 0.4 gate record, "0.1 to 0.4 are built". Gate 4's row stays open for the CI run id, which Task 11 fills. |
| `docs/agents.md` | 120 s per attempt for hosted, "Router policy" per decision 11. External backends widen in 0.4. |
| `docs/architecture.md` | The new modules, the two clients, the keychain path. |
| `docs/database-schema.md` | Section for `0012`, with the rule 7 note. |
| `docs/development.md` | TLS backend, fakes, `real_gemini_has_no_tools_smoke`, `JOBPILOT_GEMINI_BIN`, `JOBPILOT_CODEX_BIN`, and `--skip real_gemini --skip real_tls` on its two `--ignored` lines. |
| `docs/wireframes.md` | Section 13 as built: Rust clipboard flow, no key field, no ChatGPT row, so no Manage usage link, no verdict on the list, and the suspended wording from Task 2. Section 14 heading. |
| `docs/prd.md` | Open items line for FR-3.10 and FR-3.11 as they now stand. |
| `README.md`, `CHANGELOG.md`, `docs/releases/0.4.0.md` | Built, tests, not exercised, risks, upgrading (one Claude re-consent). |

- [ ] **Step 2: Wire CI and the TLS check**

Add `real_tls_handshake` to `crates/core/tests/net.rs`, ignored, gated by `JOBPILOT_REAL_TLS=1`, returning with a `skipped` line when that is unset, as `real_claude_has_no_tools_smoke` does. It uses `hosted_client` to send an unauthenticated `GET` with no body to each of the four vendor hosts and expects any HTTP status, which proves the handshake with no key and no cost. Task 11 Step 3 and CI call it. Do not run it here: implementers make no real network call.

Add `--skip real_tls` to `keychain_suites` in `scripts/ci-local.sh` and to both keychain steps in `.github/workflows/ci.yml`. Add a `TLS handshake` step to `ci.yml` after them, on both systems: `cargo test -p jobpilot-core --test net -- --ignored real_tls_handshake`, with `JOBPILOT_REAL_TLS: 1` in its `env`. That step is the Schannel proof. Remove `ci.yml`'s "The repository is private" comment, and in the header of `ci-local.sh` replace "when minutes run out" and the ponytail with: this is the pre-commit gate, Windows is type-checked here, and CI's Windows job runs it.

- [ ] **Step 3: Bump the version** to `0.4.0` in `Cargo.toml` (workspace), `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, and `Cargo.lock`.

- [ ] **Step 4: Run the gate**

Run: `scripts/ci-local.sh --fast`
Then: `pnpm --filter desktop sidecar && pnpm tauri build --no-bundle`
Expected: all steps `ok`, exit 0. The controller runs the full `scripts/ci-local.sh`, which adds the keychain suites, after this task.

Migration gate: `a_fresh_database_is_schema_12` and `a_0_3_database_upgrades_to_0_4` pass. Record the test counts.

- [ ] **Step 5: Commit**

```bash
git add -A docs scripts .github crates apps README.md CHANGELOG.md Cargo.toml Cargo.lock
git commit -m "docs: Release 0.4 gates, threat model, CI TLS check, and version 0.4.0"
```

### Task 11: Real end to end on a temporary root

Driven by the controller, not an implementer subagent: it spends a little real usage. **At most one real completion per available provider.** Use only what is installed and signed in, and add no keys on the owner's behalf: if no key exists for a provider, that provider is recorded as not exercised. Never touch the owner's data root. No full-screen screenshots. If a step fails, stop, fix through a subagent with a failing test first, and rerun from the failed step, counting calls already spent against the limit.

**Files:**
- Modify: `docs/milestones.md` (0.4 gate record), `docs/releases/0.4.0.md`, `docs/ai-providers.md` (no-tools quotes, if they moved)

**Interfaces:**
- Consumes: a debug build of `jobpilot` and the app from `pnpm tauri build --debug --no-bundle`. A release build ignores `JOBPILOT_SECRET_SERVICE` and would store keys and the data key under the owner's real service, where entries are keyed by name alone. Task 10 already proved the release build compiles.

- [ ] **Step 1: Set up and take stock**

```bash
cargo build -p jobpilot-cli
E2E=$(mktemp -d); export JOBPILOT_HOME="$E2E/home" JOBPILOT_SECRET_SERVICE=jobpilot-e2e
JP="$PWD/target/debug/jobpilot"
$JP status; $JP ai status
```

Expected: schema 12, version 0.4.0, every external provider off, No-AI present. Record which are available. Known at planning time: `claude` 2.1.316 present; `gemini` 0.40.0 under nvm, sign-in unproven; `codex` 0.125.0 installed but its `--version` fails (missing vendor binary), so it must read unavailable; Ollama binary present; no provider key in the environment.

- [ ] **Step 2: The no-provider path**

With every provider off, build the candidate data as in 0.3's Task 11, and run `resume generate <job>`: expect `manual_required`, reason `no_ai`. `app add`, `analytics report`: expect both to work. `sqlite3 "$JOBPILOT_HOME/jobpilot.db" "SELECT count(*) FROM audit_log WHERE action = 'ai_external_call'"` prints 0.

- [ ] **Step 3: TLS against the real hosts, no key, no spend**

Run: `JOBPILOT_REAL_TLS=1 cargo test -p jobpilot-core --test net -- --ignored real_tls_handshake`
Expected: every host answers an HTTP status. This is decision 1's early warning on macOS.

- [ ] **Step 4: Claude Code, one call**

`ai enable claude_code`, `ai consent show claude_code --job <job>`, then give consent. `resume generate <job> --export pdf`. Expect a draft from `claude_code/cli_login`, and a PDF. A fresh root has no old record, so this proves consent and not the re-consent path: Steps 9 and 10 do that. Disable it.

- [ ] **Step 5: Gemini CLI, one call, if it is installed and signed in**

This is a security check, not a terms check, so it stays. Re-read the no-tools evidence above against the versions installed now: the flags in `gemini --help`, and the policy-engine and headless pages. Update the quotes in `docs/ai-providers.md` if they moved. A flag that is gone stops this step, as a failed smoke does.

Run: `JOBPILOT_REAL_GEMINI_SMOKE=1 cargo test -p jobpilot-core --test gemini_cli -- --ignored real_gemini`
That is the one call. Expect no `tool_use` event and no canary text. A login failure (`not_logged_in`) spends nothing useful: record the provider as not signed in, and the no-tools proof as unproven at run time. If the smoke sees a `tool_use`, remove `NoTools` from the table through a subagent, as decision 15 says.

- [ ] **Step 6: Codex CLI, no call**

`ai enable codex_cli`, `ai status`: expect Codex unavailable while its install is broken, then `ai disable codex_cli`. Re-read the `apply_patch` gap against the installed version's docs: if a documented switch now removes it, record that for a later release, with no change to the capability here. With a working install, still no call: every task skips it. `ai consent show codex_cli` says why.

- [ ] **Step 7: API providers, one call each, only if the owner already has a key to hand**

For each of OpenAI, Anthropic, Google, OpenRouter with a key available: `ai key set`, `ai model`, `ai enable`, `ai consent give`, one `resume generate`. Disable it before the next. `ai key remove` afterwards. With no key, record "not exercised: no key", and move on.

- [ ] **Step 8: Local paths, free**

If an Ollama model is installed, start it, and run one draft through Ollama, then the same model through the endpoint at `http://127.0.0.1:11434/v1`, which needs its own consent. If none is installed, record it.

- [ ] **Step 9: Checks, no spend**

```bash
sqlite3 "$JOBPILOT_HOME/jobpilot.db" "SELECT provider, outcome, reason, count(*) FROM task_runs GROUP BY 1,2,3;"
```

Expected: one successful attempt per exercised provider. Scan the database, the exports, and `audit_log` for a distinctive word from the posting in logs, and for any key's last eight characters anywhere: expect none.

Force a suspension by SQL on the temporary root, as the Task 2 test does: `ai status` shows suspended, a draft ends `consent_suspended`, and no call is made. Setting `register_row` to `''` is the 0.3 upgrade's exact state. Then `pause set` during a slow mock CLI: ends `paused`. Then `backup create`, `backup restore`: every provider off, consent revoked.

- [ ] **Step 10: The GUI, once**

Build with `pnpm tauri build --debug --no-bundle` and launch that app with `JOBPILOT_HOME` and `JOBPILOT_SECRET_SERVICE` as in Step 1. By keyboard: the AI screen lists every provider and the Not available list. Review opens the dialog with focus on Cancel, Show exactly what will be sent shows the prompt, and `Paste key from clipboard...` with a throwaway text refuses with no dialog. Give Claude Code consent, set its `register_row` to `''` by SQL, and reload: the Claude section shows why it is suspended, `Review consent...` opens with the categories ticked, and `Allow selected` lifts it through the native confirmation. Drive any native dialog through System Events accessibility (`osascript`), and read the results from `ai status` and audit rows. If accessibility is not granted, record them as unproven. Capture only a window or element screenshot if evidence is needed.

- [ ] **Step 11: Gate 4, the CI run**

Push the branch, then run `gh workflow run ci.yml --ref feat/v0.4-external-ai`: `ci.yml` starts on its own only for a pull request or `main`. Wait for it. The evidence is `windows-latest`'s test, keychain, and `TLS handshake` steps and `macos-latest`'s. A red job is fixed through a subagent with a failing test first, then this step reruns.

- [ ] **Step 12: Clean up and record**

Run `data delete --confirm DELETE` with the debug binary, which removes the `jobpilot-e2e` keychain entries, before removing `$E2E`. Fill the gate record: what ran, real versions, calls spent per provider, the CI run id and its Windows result for Gate 4, and what is unproven: real CLIs, native dialogs, and the GUI on Windows, each provider with no key, and hosted replies only ever scripted against the corpus.

```bash
git add docs
git commit -m "docs: Release 0.4 end-to-end results"
```
