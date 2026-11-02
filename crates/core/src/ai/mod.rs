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
use crate::settings;
use claude_code::ClaudeCode;
use ollama::OllamaStatus;

pub mod claude_code;
pub mod consent;
pub mod net;
pub mod noai;
pub mod ollama;
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
/// One attempt: a validation failure must not spend a second Claude call past the cap.
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

/// Timeouts are not here: they come from `Declaration.timeout`, 180 s Claude, 300 s local.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Ollama,
    ClaudeCode,
    NoAi,
}

/// This text is the `consent.provider` value, so it names the sign-in method too.
impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProviderId::Ollama => "ollama",
            ProviderId::ClaudeCode => "claude_code/cli_login",
            ProviderId::NoAi => "no_ai",
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
}

#[derive(Debug, Clone, Serialize)]
pub struct AiStatus {
    pub ollama: OllamaStatus,
    pub claude_code: ClaudeStatus,
    pub hardware: Hardware,
    pub order: Vec<String>,
    pub enabled: serde_json::Value,
    pub drafting_model: Option<String>,
    pub embedding_model: Option<String>,
}

/// The provider names the settings accept.
pub const PROVIDER_NAMES: &[&str] = &["ollama", "claude_code"];

async fn text_setting(db: &Db, key: &str) -> Result<Option<String>> {
    let value = settings::get(db, key).await?;
    Ok(value.and_then(|v| v.as_str().map(str::to_string)))
}

/// `ai.order` and `ai.enabled` as they take effect, read by the router and the screen alike so
/// the two cannot differ. Each name is known and appears once: the stored order first, then
/// every enabled provider it leaves out, because `ai.order` is only written when the user
/// reorders and enabling a provider must be enough to route it. The consent check still
/// decides whether an external provider runs. A provider that is off stays listed if the
/// order names it; the router passes over it. Ollama defaults on, Claude off, because data
/// leaves the machine.
pub(crate) async fn provider_settings(db: &Db) -> Result<(Vec<String>, serde_json::Value)> {
    let stored = settings::get(db, "ai.enabled").await?.unwrap_or_default();
    let enabled = serde_json::json!({
        "ollama": stored["ollama"].as_bool().unwrap_or(true),
        "claude_code": stored["claude_code"].as_bool().unwrap_or(false),
    });
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

/// What the screen and the CLI show: availability of both providers, hardware, and the
/// settings the router reads. Probes loopback Ollama and runs `claude --version`; sends no
/// text and needs no consent.
pub async fn status(db: &Db) -> Result<AiStatus> {
    let ollama = match router::ollama_from_settings(db).await? {
        Some(ollama) => ollama.status().await,
        None => OllamaStatus::Refused {
            reason: "not_loopback".into(),
        },
    };
    let consent = consent::active(db, &ProviderId::ClaudeCode.to_string()).await?;
    let claude_code = match ClaudeCode::locate_in(db, None).await {
        Ok(claude) => ClaudeStatus {
            available: matches!(claude.available().await, Availability::Available),
            path: Some(claude.path().display().to_string()),
            consent,
        },
        Err(_) => ClaudeStatus {
            available: false,
            path: None,
            consent,
        },
    };
    let (order, enabled) = provider_settings(db).await?;
    Ok(AiStatus {
        ollama,
        claude_code,
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
    Err(Error::Validation(
        "a provider is ollama or claude_code".into(),
    ))
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
