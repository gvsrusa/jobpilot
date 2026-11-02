//! Provider-neutral AI tasks. `router` picks a provider, `validate` checks what came back,
//! `consent` records what the user allowed, `noai` is the path that always exists.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{Error, Result};
use crate::hardware::{self, Hardware};
use crate::{secrets, settings};
use api::ApiProvider;
use claude_code::ClaudeCode;
use consent::ConsentState;
use gemini_cli::GeminiCli;
use ollama::OllamaStatus;
use register::ProviderKind;

pub mod api;
pub mod child;
pub mod claude_code;
pub mod codex_cli;
pub mod consent;
pub mod gemini_cli;
pub mod net;
pub mod noai;
pub mod ollama;
pub mod register;
pub mod router;
pub mod validate;

/// What a failed attempt may write to `task_runs` beside the error code. Errors can carry a
/// provider's own text, so only these exact words leave it.
pub const RUN_REASONS: &[&str] = &[
    "not_logged_in",
    "permission_denied",
    "bad_reply",
    "cli_error",
    "prompt_too_large",
    "error_max_turns",
    "error_during_execution",
    "error_max_structured_output_retries",
    "consent_needed",
    "provider_unavailable",
    "no_model_set",
    "model_not_installed",
    "rate_limited",
    "auth_failed",
    "quota_exhausted",
    "model_refused",
    "model_not_found",
    "output_truncated",
    "no_key",
    "no_api_model",
    "consent_suspended",
    "cli_too_old",
    "origin_refused",
    "model_has_tools",
    "endpoint_changed",
    "cli_account_refused",
    "unusable_reply",
];

/// Absent capabilities are false: a declaration lists only what the provider proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    StructuredOutput,
    Embeddings,
    LocalExecution,
    FileInput,
    Streaming,
    WebSearch,
    Offline,
    NoTools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataCategory {
    JobText,
    CompanyText,
    ProfileBasic,
    ResumeText,
    CareerMemory,
    Sensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    ParseJob,
    DraftResume,
    DraftCoverLetter,
    ClassifyAnswer,
    Embed,
}

#[derive(Debug)]
pub struct TaskSpec {
    pub task: TaskType,
    /// JSON text; "" for `Embed`.
    pub schema: &'static str,
    pub required: &'static [Capability],
    pub categories: &'static [DataCategory],
    pub attempts: u32,
}

const DRAFTING: &[Capability] = &[Capability::StructuredOutput, Capability::NoTools];
const DRAFT_DATA: &[DataCategory] = &[
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::ResumeText,
];

static PARSE_JOB: TaskSpec = TaskSpec {
    task: TaskType::ParseJob,
    schema: include_str!("../../schemas/parse_job.json"),
    required: DRAFTING,
    categories: &[DataCategory::JobText],
    attempts: 2,
};
static DRAFT_RESUME: TaskSpec = TaskSpec {
    task: TaskType::DraftResume,
    schema: include_str!("../../schemas/draft_resume.json"),
    required: DRAFTING,
    categories: DRAFT_DATA,
    attempts: 2,
};
static DRAFT_COVER_LETTER: TaskSpec = TaskSpec {
    task: TaskType::DraftCoverLetter,
    schema: include_str!("../../schemas/draft_cover_letter.json"),
    required: DRAFTING,
    categories: DRAFT_DATA,
    attempts: 2,
};
/// One attempt: a validation failure must not spend a second paid call past the cap.
static CLASSIFY_ANSWER: TaskSpec = TaskSpec {
    task: TaskType::ClassifyAnswer,
    schema: include_str!("../../schemas/classify_answer.json"),
    required: DRAFTING,
    categories: &[DataCategory::JobText],
    attempts: 1,
};
static EMBED: TaskSpec = TaskSpec {
    task: TaskType::Embed,
    schema: "",
    required: &[Capability::Embeddings, Capability::LocalExecution],
    categories: &[DataCategory::ResumeText, DataCategory::CareerMemory],
    attempts: 2,
};

/// Timeouts are not here: they come from `Declaration.timeout`, per provider.
pub fn spec(task: TaskType) -> &'static TaskSpec {
    match task {
        TaskType::ParseJob => &PARSE_JOB,
        TaskType::DraftResume => &DRAFT_RESUME,
        TaskType::DraftCoverLetter => &DRAFT_COVER_LETTER,
        TaskType::ClassifyAnswer => &CLASSIFY_ANSWER,
        TaskType::Embed => &EMBED,
    }
}

/// The owner-decision allow-list: a provider the decisions leave out has no variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Ollama,
    ClaudeCode,
    NoAi,
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Google,
    #[serde(rename = "openrouter")]
    OpenRouter,
    Endpoint,
    GeminiCli,
    CodexCli,
}

impl ProviderId {
    pub const ALL: [ProviderId; 10] = [
        ProviderId::Ollama,
        ProviderId::ClaudeCode,
        ProviderId::NoAi,
        ProviderId::OpenAi,
        ProviderId::Anthropic,
        ProviderId::Google,
        ProviderId::OpenRouter,
        ProviderId::Endpoint,
        ProviderId::GeminiCli,
        ProviderId::CodexCli,
    ];
}

/// This text is the `consent.provider` value, so it names the sign-in method too.
impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProviderId::Ollama => "ollama",
            ProviderId::ClaudeCode => "claude_code/cli_login",
            ProviderId::NoAi => "no_ai",
            ProviderId::OpenAi => "openai/api_key",
            ProviderId::Anthropic => "anthropic/api_key",
            ProviderId::Google => "google/paid_key",
            ProviderId::OpenRouter => "openrouter/api_key",
            ProviderId::Endpoint => "endpoint/loopback",
            ProviderId::GeminiCli => "gemini_cli/cli_login",
            ProviderId::CodexCli => "codex_cli/cli_login",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Declaration {
    pub id: ProviderId,
    pub capabilities: Vec<Capability>,
    /// Data stays on this machine. Only a confirmed local model may claim it.
    pub local: bool,
    pub context_limit: usize,
    pub timeout: Duration,
    pub sign_in: String,
}

pub struct Prompt {
    pub system: String,
    pub user: String,
    /// The router sets this to the task's schema before any provider sees it.
    pub schema: &'static str,
}

/// Token estimates assume one token per this many UTF-8 bytes. Counting bytes, not characters,
/// keeps it high enough for CJK, where a three-byte character can be a token or more.
pub const BYTES_PER_TOKEN: usize = 2;

impl Prompt {
    /// An upper estimate for routing. The real count depends on a model's tokenizer.
    pub fn estimated_tokens(&self) -> usize {
        let bytes = self.system.len() + self.user.len() + self.schema.len();
        bytes.div_ceil(BYTES_PER_TOKEN)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Ok {
        provider: ProviderId,
        attempts: u32,
        output: serde_json::Value,
    },
    ManualRequired {
        reason: String,
    },
    Cancelled,
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Availability {
    Available,
    Unavailable(String),
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed futures, no async-trait crate: the trait is object-safe for `Vec<Box<dyn Provider>>`.
pub trait Provider: Send + Sync {
    fn declaration(&self) -> Declaration;
    fn available(&self) -> BoxFuture<'_, Availability>;
    /// Raw text. Validation is the router's.
    fn complete<'a>(
        &'a self,
        prompt: &'a Prompt,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>>;
    fn embed<'a>(
        &'a self,
        model: &'a str,
        inputs: &'a [String],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>>>;
}

// ponytail: no tokio-util; one flag per task. The router polls it every 100 ms and drops
// the attempt, which kills any child (kill_on_drop).
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaudeStatus {
    pub available: bool,
    pub path: Option<String>,
    /// The categories the active consent record allows, if there is one.
    pub consent: Option<Vec<DataCategory>>,
    /// A record exists, given under other register text, so `consent` is `None`.
    pub suspended: bool,
}

/// One external provider on the AI screen. Built by a table walk that never touches the
/// keychain or the network.
#[derive(Debug, Clone, Serialize)]
pub struct ExternalStatus {
    pub name: String,
    pub label: String,
    pub kind: ProviderKind,
    pub available: bool,
    pub reason: Option<String>,
    pub model: Option<String>,
    pub key_stored: bool,
    /// The program a CLI row runs, or the address an endpoint row sends to.
    pub path: Option<String>,
    pub consent: Option<Vec<DataCategory>>,
    pub suspended: bool,
    /// It can carry a posting: it declares what the drafting tasks require.
    pub runs_job_text: bool,
    /// The installed CLI passes the last version the no-tools evidence was read from. A note,
    /// not a stop: the deny rule, the guard, and the smoke test still stand.
    pub newer_than_checked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiStatus {
    pub ollama: OllamaStatus,
    pub claude_code: ClaudeStatus,
    /// Every provider but Ollama and Claude Code, which have their own sections.
    pub external: Vec<ExternalStatus>,
    pub register_date: String,
    pub hardware: Hardware,
    pub order: Vec<String>,
    pub enabled: serde_json::Value,
    pub drafting_model: Option<String>,
    pub embedding_model: Option<String>,
}

/// The provider names the settings accept.
pub const PROVIDER_NAMES: &[&str] = &[
    "ollama",
    "claude_code",
    "openai",
    "anthropic",
    "google",
    "openrouter",
    "endpoint",
    "gemini_cli",
    "codex_cli",
];

/// What one completion task sends to a provider, or why it never runs there.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSharing {
    pub task: TaskType,
    pub categories: Vec<DataCategory>,
    pub runs: bool,
    pub skipped_because: Option<String>,
}

/// What the consent screen shows for one provider. There is no verdict: the owner decisions
/// govern what ships, and no screen re-raises a vendor's.
#[derive(Debug, Clone, Serialize)]
pub struct Sharing {
    pub name: String,
    pub label: String,
    pub key: String,
    pub destination: String,
    /// The program a CLI runs, or the address the endpoint sends to.
    pub path: Option<String>,
    pub tasks: Vec<TaskSharing>,
    pub data_use: String,
    pub terms_date: String,
    pub attestation: Option<String>,
    pub suspended: bool,
    /// The categories of an active or a suspended record, so a review starts ticked.
    pub given: Option<Vec<DataCategory>>,
}

/// `Embed` is local only, so it never reaches a consent screen.
const COMPLETION_TASKS: [TaskType; 4] = [
    TaskType::ParseJob,
    TaskType::DraftResume,
    TaskType::DraftCoverLetter,
    TaskType::ClassifyAnswer,
];

pub async fn sharing(db: &Db, name: &str) -> Result<Sharing> {
    let info = register::info(name)
        .ok_or_else(|| Error::Validation("consent does not apply to this provider".into()))?;
    let key = info.id.to_string();
    let state = consent::state(db, &key).await?;
    let suspended = matches!(state, ConsentState::Suspended(_));
    let given = match state {
        ConsentState::None => None,
        ConsentState::Active(categories) | ConsentState::Suspended(categories) => Some(categories),
    };
    let tasks = COMPLETION_TASKS.map(|task| {
        let spec = spec(task);
        let skipped_because = router::lacking(spec, info.capabilities);
        TaskSharing {
            task,
            categories: spec.categories.to_vec(),
            runs: skipped_because.is_none(),
            skipped_because,
        }
    });
    let path = match info.id {
        ProviderId::ClaudeCode => ClaudeCode::locate_in(db, None)
            .await
            .ok()
            .map(|claude| claude.path().display().to_string()),
        ProviderId::Endpoint => api::endpoint_address(db).await?,
        ProviderId::GeminiCli | ProviderId::CodexCli => cli_path(db, info.name).await,
        _ => None,
    };
    Ok(Sharing {
        name: info.name.into(),
        label: info.label.into(),
        destination: info.destination.into(),
        path,
        tasks: tasks.into(),
        data_use: info.data_use.into(),
        terms_date: register::date_read(&key).unwrap_or_else(register::register_date),
        attestation: info.attestation.map(str::to_string),
        suspended,
        given,
        key,
    })
}

async fn text_setting(db: &Db, key: &str) -> Result<Option<String>> {
    let value = settings::get(db, key).await?;
    Ok(value.and_then(|v| v.as_str().map(str::to_string)))
}

/// `ai.order` and `ai.enabled` as they take effect, read by the router and the screen alike so
/// the two cannot differ. Each name is known and appears once: the stored order first, then
/// every enabled provider it leaves out, because `ai.order` is only written when the user
/// reorders and enabling a provider must be enough to route it. The consent check still
/// decides whether an external provider runs. A provider that is off stays listed if the
/// order names it; the router passes over it. Ollama defaults on, every other provider off,
/// because data leaves the machine.
pub(crate) async fn provider_settings(db: &Db) -> Result<(Vec<String>, serde_json::Value)> {
    let stored = settings::get(db, "ai.enabled").await?.unwrap_or_default();
    let enabled: serde_json::Map<String, serde_json::Value> = PROVIDER_NAMES
        .iter()
        .map(|name| {
            let on = stored[*name].as_bool().unwrap_or(*name == "ollama");
            ((*name).to_string(), serde_json::Value::Bool(on))
        })
        .collect();
    let enabled = serde_json::Value::Object(enabled);
    let stored: Option<Vec<String>> = settings::get(db, "ai.order")
        .await?
        .and_then(|value| serde_json::from_value(value).ok());
    let stored = stored.unwrap_or_else(|| vec!["ollama".into()]);
    let on = PROVIDER_NAMES.iter().filter(|name| enabled[**name] == true);
    let mut order: Vec<String> = Vec::new();
    for name in stored.iter().map(String::as_str).chain(on.copied()) {
        if PROVIDER_NAMES.contains(&name) && !order.iter().any(|seen| seen == name) {
            order.push(name.to_string());
        }
    }
    Ok((order, enabled))
}

/// Where a CLI provider's program is, by the shared discovery (the consent dialog's line).
async fn cli_path(db: &Db, name: &str) -> Option<String> {
    let spec = child::spec_for(name)?;
    let found = child::find_cli(spec, Some(db)).await.ok()?;
    Some(found.display().to_string())
}

/// Past this, a CLI row reads `no_answer`: its own probe is 10 s, and a login shell lookup
/// before it is 5 s, so no row holds the screen longer than one of each.
const CLI_ROW_BOUND: Duration = Duration::from_secs(16);

/// What a CLI row shows: available, the reason it is not, where its program is, and whether
/// it is newer than the version checked.
type CliRow = (bool, Option<String>, Option<String>, bool);

/// Decision 28: a CLI that is off is found, by the folders alone, and never started, because
/// `status` runs on every refresh of the AI screen and a Node CLI takes seconds to start. Only
/// an enabled one gets the login shell and the `--version` probe, which for Gemini includes its
/// version floor. One discovery per row. An off CLI the folders do not show may still be one
/// only the login shell finds, an nvm install under a GUI launch: it reads `not_searched`, and
/// turning it on is what looks. A stored path that names something else reads `wrong_path`, on
/// or off: looking again cannot help until it changes.
async fn cli_row(db: &Db, name: &str, enabled: bool) -> CliRow {
    let Some(spec) = child::spec_for(name) else {
        return (false, Some("not_built".into()), None, false);
    };
    let found = if enabled {
        child::find_cli(spec, Some(db)).await
    } else {
        child::find_cli_quick(spec, Some(db)).await
    };
    let Ok(bin) = found else {
        let unsearched = !enabled && child::login_shell_may_find(spec);
        let why = if child::stored_path_is_wrong(spec, db).await {
            "wrong_path"
        } else if unsearched {
            "not_searched"
        } else {
            "not_installed"
        };
        return (false, Some(why.into()), None, false);
    };
    let path = Some(bin.display().to_string());
    if !enabled {
        return (true, None, path, false);
    }
    let probe = if name == "gemini_cli" {
        GeminiCli::with_binary(bin).check().await
    } else {
        let env = child::program_env(&bin, spec);
        child::version_of(&bin, env)
            .await
            .map(|_| false)
            .map_err(str::to_string)
    };
    match probe {
        Ok(newer) => (true, None, path, newer),
        Err(why) => (false, Some(why), path, false),
    }
}

async fn bounded_cli_row(db: &Db, name: &str, enabled: bool) -> CliRow {
    tokio::time::timeout(CLI_ROW_BOUND, cli_row(db, name, enabled))
        .await
        .unwrap_or((false, Some("no_answer".into()), None, false))
}

/// A provider row's availability, by the same checks routing uses. Opens no socket.
async fn api_availability(
    db: &Db,
    info: &register::ProviderInfo,
) -> Result<(bool, Option<String>)> {
    Ok(match ApiProvider::from_settings(db, info.name).await? {
        Some(provider) => match provider.available().await {
            Availability::Available => (true, None),
            Availability::Unavailable(why) => (false, Some(why)),
        },
        None if info.kind == ProviderKind::Endpoint => (false, Some("no_endpoint".into())),
        None => (false, Some("not_built".into())),
    })
}

/// Every external provider but Claude Code, from the table.
async fn external_status(db: &Db) -> Result<Vec<ExternalStatus>> {
    let stored = secrets::names(db).await?;
    let (_, enabled) = provider_settings(db).await?;
    // Together, so `status` waits for one probe timeout at most.
    let (gemini, codex) = tokio::join!(
        bounded_cli_row(db, "gemini_cli", enabled["gemini_cli"] == true),
        bounded_cli_row(db, "codex_cli", enabled["codex_cli"] == true),
    );
    let mut rows = Vec::new();
    for info in register::PROVIDERS {
        if info.id == ProviderId::ClaudeCode {
            continue;
        }
        let state = consent::state(db, &info.id.to_string()).await?;
        let suspended = matches!(state, ConsentState::Suspended(_));
        let (available, reason, cli_path, newer_than_checked) = match info.name {
            "gemini_cli" => gemini.clone(),
            "codex_cli" => codex.clone(),
            _ => {
                let (available, reason) = api_availability(db, info).await?;
                (available, reason, None, false)
            }
        };
        let model = text_setting(db, &format!("ai.model.{}", info.name)).await?;
        let path = match info.kind {
            ProviderKind::Endpoint => api::endpoint_address(db).await?,
            ProviderKind::Cli => cli_path,
            _ => None,
        };
        rows.push(ExternalStatus {
            name: info.name.into(),
            label: info.label.into(),
            kind: info.kind,
            available,
            reason,
            model: model.filter(|model| !model.is_empty()),
            key_stored: stored.contains(&secrets::api_key_name(info.name)),
            path,
            consent: state.allowed(),
            suspended,
            runs_job_text: router::lacking(spec(TaskType::DraftResume), info.capabilities)
                .is_none(),
            newer_than_checked,
        });
    }
    Ok(rows)
}

/// What the screen and the CLI show: availability of every provider, hardware, and the
/// settings the router reads. Probes loopback Ollama and runs `claude --version`; sends no
/// text and needs no consent.
pub async fn status(db: &Db) -> Result<AiStatus> {
    let ollama = match router::ollama_from_settings(db).await? {
        Some(ollama) => ollama.status().await,
        None => OllamaStatus::Refused {
            reason: "not_loopback".into(),
        },
    };
    let state = consent::state(db, &ProviderId::ClaudeCode.to_string()).await?;
    let suspended = matches!(state, ConsentState::Suspended(_));
    let consent = state.allowed();
    let claude_code = async {
        match ClaudeCode::locate_in(db, None).await {
            Ok(claude) => ClaudeStatus {
                available: matches!(claude.available().await, Availability::Available),
                path: Some(claude.path().display().to_string()),
                consent,
                suspended,
            },
            Err(_) => ClaudeStatus {
                available: false,
                path: None,
                consent,
                suspended,
            },
        }
    };
    let (claude_code, external) = tokio::join!(claude_code, external_status(db));
    let (order, enabled) = provider_settings(db).await?;
    Ok(AiStatus {
        ollama,
        claude_code,
        external: external?,
        register_date: register::register_date(),
        hardware: hardware::detect(),
        order,
        enabled,
        drafting_model: text_setting(db, "ai.drafting_model").await?,
        embedding_model: text_setting(db, "ai.embedding_model").await?,
    })
}

fn known(name: &str) -> Result<()> {
    if PROVIDER_NAMES.contains(&name) {
        return Ok(());
    }
    Err(Error::Validation(format!(
        "a provider is one of {}",
        PROVIDER_NAMES.join(", ")
    )))
}

/// Providers in the order the router tries them. Each name once.
pub async fn set_order(db: &Db, order: &[String]) -> Result<()> {
    order.iter().try_for_each(|name| known(name))?;
    let mut seen: Vec<&String> = Vec::new();
    for name in order {
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    settings::set(db, "ai.order", &serde_json::json!(seen)).await
}

pub async fn set_enabled(db: &Db, provider: &str, on: bool) -> Result<()> {
    known(provider)?;
    let mut enabled = settings::get(db, "ai.enabled").await?.unwrap_or_default();
    if !enabled.is_object() {
        enabled = serde_json::json!({});
    }
    enabled[provider] = serde_json::Value::Bool(on);
    settings::set(db, "ai.enabled", &enabled).await
}

/// `which` is `drafting` or `embedding`.
pub async fn set_model(db: &Db, which: &str, model: &str) -> Result<()> {
    let key = match which {
        "drafting" => "ai.drafting_model",
        "embedding" => "ai.embedding_model",
        _ => return Err(Error::Validation("a model is drafting or embedding".into())),
    };
    settings::set(db, key, &serde_json::json!(model.trim())).await
}
