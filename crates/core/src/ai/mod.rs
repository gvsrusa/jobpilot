//! Provider-neutral AI tasks. `router` picks a provider, `validate` checks what came back,
//! `consent` records what the user allowed, `noai` is the path that always exists.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;

pub mod claude_code;
pub mod consent;
pub mod noai;
pub mod ollama;
pub mod router;
pub mod validate;

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
static EMBED: TaskSpec = TaskSpec {
    task: TaskType::Embed,
    schema: "",
    required: &[Capability::Embeddings, Capability::LocalExecution],
    categories: &[DataCategory::ResumeText, DataCategory::CareerMemory],
    attempts: 2,
};

/// Timeouts are not here: they come from `Declaration.timeout`, 60 s remote, 300 s local.
pub fn spec(task: TaskType) -> &'static TaskSpec {
    match task {
        TaskType::ParseJob => &PARSE_JOB,
        TaskType::DraftResume => &DRAFT_RESUME,
        TaskType::DraftCoverLetter => &DRAFT_COVER_LETTER,
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
