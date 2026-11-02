//! Picks an eligible provider, bounds each attempt, validates what comes back, and logs
//! every step. The contract is `docs/ai-providers.md`, "TaskRouter contract".

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use super::api::ApiProvider;
use super::claude_code::ClaudeCode;
use super::consent::ConsentState;
use super::net::MAX_RETRY_WAIT;
use super::noai::NoAi;
use super::ollama::{DEFAULT_URL, Ollama};
use super::validate::validate;
use super::{
    Availability, CancellationToken, Capability, DataCategory, Declaration, Outcome, Prompt,
    Provider, ProviderId, RUN_REASONS, TaskSpec, TaskType, consent, provider_settings, spec,
};
use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::pause;
use crate::settings;

const CANCEL_POLL: Duration = Duration::from_millis(100);
const NOT_LOGGED_IN: &str = "not_logged_in";
const RATE_LIMITED: &str = "rate_limited";
const AUTH_FAILED: &str = "auth_failed";
const NO_KEY: &str = "no_key";
const NO_API_MODEL: &str = "no_api_model";
const ORIGIN_REFUSED: &str = "origin_refused";
const QUOTA_EXHAUSTED: &str = "quota_exhausted";
const PROMPT_TOO_LARGE: &str = "prompt_too_large";
const CONSENT: &str = "consent";
const CONSENT_NEEDED: &str = "consent_needed";
const CONSENT_SUSPENDED: &str = "consent_suspended";
const PROVIDER_UNAVAILABLE: &str = "provider_unavailable";
const NO_MODEL_SET: &str = "no_model_set";
const MODEL_NOT_INSTALLED: &str = "model_not_installed";
const OLLAMA: &str = "ollama";
const CLAUDE_CODE: &str = "claude_code";
const OPENAI: &str = "openai";
const OPENROUTER: &str = "openrouter";
const ENDPOINT: &str = "endpoint";

/// Providers in user order. No-AI is not in the list: it runs last, outside the checks.
/// Holds no `Db`, so build one per call and nothing blocks a restore.
pub struct Router {
    providers: Vec<Box<dyn Provider>>,
    timeout: Option<Duration>,
}

/// One task's fixed facts, passed down the steps of a run.
struct Task<'a> {
    db: &'a Db,
    actor: &'a str,
    spec: &'a TaskSpec,
    cancel: &'a CancellationToken,
    /// [`Prompt::estimated_tokens`]; 0 for a task with no prompt.
    tokens: usize,
}

/// One `task_runs` row. `reason` is a code, never a message: rows hold no prompts,
/// outputs, or secrets, and messages can quote any of them.
struct Row<'a> {
    provider: &'a str,
    started_at: String,
    duration_ms: i64,
    attempt: u32,
    outcome: &'a str,
    reason: Option<&'a str>,
}

enum Step<T> {
    Done {
        value: T,
        attempts: u32,
    },
    Next,
    /// As `Next`, with a reason the user can act on, so the task end says it, not `no_ai`.
    Manual(&'static str),
    Cancelled,
}

impl Router {
    /// Settings: `ai.order` (`["ollama","claude_code"]`, default `["ollama"]`) and `ai.enabled`
    /// (`{ollama: bool, claude_code: bool}`), as [`provider_settings`] reads them, then
    /// `ai.ollama_url`, `ai.drafting_model`, `ai.embedding_model`, `ai.model.<name>`,
    /// `ai.endpoint.url`.
    /// Spawns nothing and opens no socket: availability is checked when a task runs.
    pub async fn from_settings(db: &Db) -> Result<Router> {
        let (order, enabled) = provider_settings(db).await?;
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();
        for name in order.iter().filter(|name| enabled[*name] == true) {
            match name.as_str() {
                OLLAMA => {
                    // A bad setting costs Ollama, not every task: No-AI must still run.
                    match ollama_from_settings(db).await? {
                        Some(ollama) => providers.push(Box::new(ollama)),
                        None => eprintln!("ollama: ai.ollama_url refused, provider left out"),
                    }
                }
                CLAUDE_CODE => {
                    // No binary stays listed, so the run log shows it as unavailable.
                    let claude = ClaudeCode::locate_in(db, None)
                        .await
                        .unwrap_or_else(|_| ClaudeCode::with_binary(PathBuf::new(), None));
                    providers.push(Box::new(claude));
                }
                OPENAI | OPENROUTER | ENDPOINT => {
                    match ApiProvider::from_settings(db, name).await? {
                        Some(provider) => providers.push(Box::new(provider)),
                        None => eprintln!("{name}: no usable address, provider left out"),
                    }
                }
                _ => {}
            }
        }
        Ok(Router::with_providers(providers))
    }

    /// No-AI is always the real one, so anything else claiming that id is dropped.
    pub fn with_providers(mut providers: Vec<Box<dyn Provider>>) -> Router {
        providers.retain(|provider| provider.declaration().id != ProviderId::NoAi);
        Router {
            providers,
            timeout: None,
        }
    }

    /// Overrides every `Declaration.timeout`.
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Router {
            timeout: Some(timeout),
            ..self
        }
    }

    /// In routing order, ending `NoAi`.
    pub fn provider_ids(&self) -> Vec<ProviderId> {
        let ids = self.providers.iter().map(|p| p.declaration().id);
        ids.chain([ProviderId::NoAi]).collect()
    }

    pub async fn run(
        &self,
        db: &Db,
        actor: &str,
        task: TaskType,
        prompt: Prompt,
        cancel: CancellationToken,
    ) -> Result<Outcome> {
        self.run_spec(db, actor, spec(task), prompt, cancel).await
    }

    /// `run` with a chosen spec, so tests can prove a rule holds for a spec that does not
    /// ship. `run` is the only production caller.
    #[doc(hidden)]
    pub async fn run_spec(
        &self,
        db: &Db,
        actor: &str,
        spec: &TaskSpec,
        mut prompt: Prompt,
        cancel: CancellationToken,
    ) -> Result<Outcome> {
        prompt.schema = spec.schema;
        let task = Task {
            db,
            actor,
            spec,
            cancel: &cancel,
            tokens: prompt.estimated_tokens(),
        };
        if let Some(reason) = refusal(spec) {
            log(&task, row("router", 0, "refused", Some(reason), db).await?).await?;
            return Ok(Outcome::Failed {
                reason: reason.into(),
            });
        }
        pause::ensure_off(db).await?;
        // What the user can act on, when every provider has passed.
        let mut manual = None;

        for provider in &self.providers {
            let provider = provider.as_ref();
            let call = || async {
                let raw = provider.complete(&prompt, cancel.clone()).await?;
                validate(spec.schema, &raw)
            };
            match self.try_provider(&task, provider, call).await? {
                Step::Done {
                    value: output,
                    attempts,
                } => {
                    return Ok(Outcome::Ok {
                        provider: provider.declaration().id,
                        attempts,
                        output,
                    });
                }
                Step::Next => {}
                Step::Manual(reason) => {
                    if manual.is_none_or(|held| weight(reason) >= weight(held)) {
                        manual = Some(reason);
                    }
                }
                Step::Cancelled => return Ok(Outcome::Cancelled),
            }
        }

        if cancel.is_cancelled() {
            log_no_ai(&task, "cancelled", None).await?;
            return Ok(Outcome::Cancelled);
        }
        Ok(match NoAi.complete(&prompt, cancel.clone()).await {
            Err(Error::ManualRequired(reason)) => {
                let reason = manual.map_or(reason, str::to_string);
                log_no_ai(&task, "manual_required", Some(&reason)).await?;
                Outcome::ManualRequired { reason }
            }
            result => {
                let reason = result.err().map_or("no_ai", |error| error.code());
                log_no_ai(&task, "failed", Some(reason)).await?;
                Outcome::Failed {
                    reason: reason.into(),
                }
            }
        })
    }

    /// Runs the same checks as `run`. Returns the model the vectors belong to.
    /// The `actor` of an audit row here is `"system"`: indexing runs in the background.
    pub async fn embed(
        &self,
        db: &Db,
        inputs: &[String],
        cancel: CancellationToken,
    ) -> Result<(String, Vec<Vec<f32>>)> {
        let task = Task {
            db,
            actor: "system",
            spec: spec(TaskType::Embed),
            cancel: &cancel,
            tokens: 0,
        };
        // Manual, not a refusal, so a search still answers by words while the pause is on.
        if pause::is_set(db).await? {
            return Err(Error::ManualRequired("paused".into()));
        }
        let model = settings::get(db, "ai.embedding_model")
            .await?
            .and_then(|value| value.as_str().map(str::to_string))
            .filter(|model| !model.is_empty());

        if let Some(model) = &model {
            for provider in &self.providers {
                let provider = provider.as_ref();
                let call = || async {
                    let vectors = provider.embed(model, inputs, cancel.clone()).await?;
                    check_vectors(&vectors, inputs.len())?;
                    Ok(vectors)
                };
                match self.try_provider(&task, provider, call).await? {
                    Step::Done { value, .. } => return Ok((model.clone(), value)),
                    Step::Next | Step::Manual(_) => {}
                    Step::Cancelled => return Err(Error::Cancelled),
                }
            }
        }

        if cancel.is_cancelled() {
            log_no_ai(&task, "cancelled", None).await?;
            return Err(Error::Cancelled);
        }
        let error = match NoAi.embed("", inputs, cancel.clone()).await {
            Ok(_) => Error::ManualRequired("no_embedding_model".into()),
            Err(error) => error,
        };
        if let Error::ManualRequired(reason) = &error {
            log_no_ai(&task, "manual_required", Some(reason)).await?;
        }
        Err(error)
    }

    /// Checks one provider, then runs its attempts. `call` does one attempt, validation
    /// included; dropping its future ends the attempt and kills any child.
    async fn try_provider<T, F, Fut>(
        &self,
        task: &Task<'_>,
        provider: &dyn Provider,
        call: F,
    ) -> Result<Step<T>>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let declaration = provider.declaration();
        let name = declaration.id.to_string();
        let limit = self.timeout.unwrap_or(declaration.timeout);

        if let Some(reason) = ineligible(task, &declaration).await? {
            log(
                task,
                row(&name, 0, "skipped", Some(&reason), task.db).await?,
            )
            .await?;
            return Ok(after_skip(task, &declaration, &reason));
        }

        // Check 5, last on purpose: an ineligible provider is not even probed.
        let probe = async { Ok(provider.available().await) };
        match bounded(probe, limit, task.cancel).await {
            Ok(Availability::Available) => {}
            // The provider's text is not logged: it can hold a path.
            Ok(Availability::Unavailable(why)) => {
                let skipped = row(&name, 0, "skipped", Some("unavailable"), task.db).await?;
                log(task, skipped).await?;
                return Ok(Step::Manual(unavailable_reason(&why)));
            }
            Err(Error::Timeout) => {
                let skipped = row(&name, 0, "skipped", Some("unavailable"), task.db).await?;
                log(task, skipped).await?;
                return Ok(Step::Manual(PROVIDER_UNAVAILABLE));
            }
            Err(_) => {
                log(task, row(&name, 0, "cancelled", None, task.db).await?).await?;
                return Ok(Step::Cancelled);
            }
        }

        let mut validation_failed = false;
        for attempt in 1..=task.spec.attempts {
            // Again each attempt: "revoking takes effect before the next call".
            if let Some(reason) = ineligible(task, &declaration).await? {
                log(
                    task,
                    row(&name, 0, "skipped", Some(&reason), task.db).await?,
                )
                .await?;
                return Ok(after_skip(task, &declaration, &reason));
            }
            // Each attempt: a pause set during the last one stops the next.
            if task.cancel.is_cancelled() || pause::is_set(task.db).await? {
                log(task, row(&name, attempt, "cancelled", None, task.db).await?).await?;
                return Ok(Step::Cancelled);
            }
            if is_external(&declaration) {
                audit_external(task, &name).await?;
            }

            let mut record = row(&name, attempt, "ok", None, task.db).await?;
            let clock = Instant::now();
            let result = bounded(call(), limit, task.cancel).await;
            record.duration_ms = i64::try_from(clock.elapsed().as_millis()).unwrap_or(i64::MAX);
            // A reply racing the cancel loses: a cancelled task never returns output.
            let result = if task.cancel.is_cancelled() {
                Err(Error::Cancelled)
            } else {
                result
            };

            let error = match result {
                Ok(value) => {
                    log(task, record).await?;
                    return Ok(Step::Done {
                        value,
                        attempts: attempt,
                    });
                }
                Err(error) => error,
            };
            if matches!(error, Error::Cancelled) {
                record.outcome = "cancelled";
                log(task, record).await?;
                return Ok(Step::Cancelled);
            }
            record.outcome = "failed";
            let reason = run_reason(&error);
            record.reason = Some(reason);
            log(task, record).await?;

            // A timed-out or broken external attempt may already have spent the user's usage.
            // A 429 spent nothing, so it may wait and retry, within the attempts the task has.
            match error {
                Error::RateLimited { retry_after } if attempt < task.spec.attempts => {
                    let wait = match (retry_after, is_external(&declaration)) {
                        (Some(wait), _) if wait <= MAX_RETRY_WAIT => wait,
                        (None, false) => Duration::from_secs(1),
                        _ => return Ok(Step::Manual(RATE_LIMITED)),
                    };
                    let sleep = async {
                        tokio::time::sleep(wait).await;
                        Ok(())
                    };
                    if bounded(sleep, wait + CANCEL_POLL, task.cancel)
                        .await
                        .is_err()
                    {
                        log(task, row(&name, attempt, "cancelled", None, task.db).await?).await?;
                        return Ok(Step::Cancelled);
                    }
                }
                Error::RateLimited { .. } => return Ok(Step::Manual(RATE_LIMITED)),
                Error::Timeout | Error::Io(_) if !is_external(&declaration) => {}
                Error::Validation(_) if !validation_failed => validation_failed = true,
                _ if [
                    NOT_LOGGED_IN,
                    PROMPT_TOO_LARGE,
                    AUTH_FAILED,
                    QUOTA_EXHAUSTED,
                    NO_KEY,
                    ORIGIN_REFUSED,
                ]
                .contains(&reason) =>
                {
                    return Ok(Step::Manual(reason));
                }
                _ => return Ok(Step::Next),
            }
        }
        Ok(Step::Next)
    }
}

/// `None` when the address is refused.
pub async fn ollama_from_settings(db: &Db) -> Result<Option<Ollama>> {
    let text = async |key: &str| -> Result<Option<String>> {
        let value = settings::get(db, key).await?;
        Ok(value.and_then(|value| value.as_str().map(str::to_string)))
    };
    let url = text("ai.ollama_url").await?;
    let url = url.as_deref().unwrap_or(DEFAULT_URL);
    let drafting = text("ai.drafting_model").await?;
    let embedding = text("ai.embedding_model").await?;
    Ok(Ollama::new(url, drafting, embedding).ok())
}

/// What a skip tells the user at the end of the task. Consent is named only when nothing else
/// holds this provider back: a provider it would not unlock must not send them to that screen.
fn after_skip<T>(task: &Task<'_>, declaration: &Declaration, reason: &str) -> Step<T> {
    match reason {
        PROMPT_TOO_LARGE => Step::Manual(PROMPT_TOO_LARGE),
        CONSENT if unfit(task, declaration).is_none() => Step::Manual(CONSENT_NEEDED),
        CONSENT_SUSPENDED if unfit(task, declaration).is_none() => Step::Manual(CONSENT_SUSPENDED),
        _ => Step::Next,
    }
}

/// Which reason ends the task when providers gave several: what a real attempt found, then
/// what the user can grant here, then a provider that is simply not there right now.
fn weight(reason: &str) -> u8 {
    match reason {
        PROVIDER_UNAVAILABLE | NO_MODEL_SET | MODEL_NOT_INSTALLED | NO_KEY | NO_API_MODEL
        | "cli_too_old" => 0,
        CONSENT_NEEDED | CONSENT_SUSPENDED => 1,
        _ => 2,
    }
}

/// The words a probe gives for what is missing, in the user's terms. Anything else is the
/// generic reason, so provider text never leaves.
fn unavailable_reason(why: &str) -> &'static str {
    match why {
        "no_model" => NO_MODEL_SET,
        "model_missing" => MODEL_NOT_INSTALLED,
        NO_KEY => NO_KEY,
        NO_API_MODEL => NO_API_MODEL,
        _ => PROVIDER_UNAVAILABLE,
    }
}

/// A task no provider may run, whatever it declares. Sensitive values never travel beside
/// untrusted text, because that text can steer the model that reads them.
fn refusal(spec: &TaskSpec) -> Option<&'static str> {
    if spec.task == TaskType::Embed {
        return Some("embed_is_not_a_completion");
    }
    (spec.categories.contains(&DataCategory::Sensitive) && carries_untrusted_text(spec))
        .then_some("sensitive_with_untrusted_text")
}

fn carries_untrusted_text(spec: &TaskSpec) -> bool {
    let untrusted = [DataCategory::JobText, DataCategory::CompanyText];
    spec.categories.iter().any(|c| untrusted.contains(c))
}

/// The sub-reason when it is on the allow-list, else the error code: never provider text.
pub(crate) fn run_reason(error: &Error) -> &'static str {
    let (Error::Refused(why) | Error::Failed(why)) = error else {
        return error.code();
    };
    let known = RUN_REASONS.iter().find(|reason| *reason == why);
    known.copied().unwrap_or(error.code())
}

/// Data leaves the machine. Fails closed: anything but Ollama counts, whatever it declares.
fn is_external(declaration: &Declaration) -> bool {
    declaration.id != ProviderId::Ollama
}

/// The routing checks, in order. Returns the reason code to skip, or `None`.
///
/// 1. Owner decisions: `ProviderId` is the allow-list, so nothing ineligible constructs.
/// 2. Ollama needs a confirmed local model, and consent cannot stand in for one. Everything
///    else needs a consent record, given under this build's register text, covering every
///    category of the task.
/// 3. Mode: every task in 0.2 is `reviewed`, so there is nothing to refuse. `TaskSpec` gains
///    a mode with the first `unreviewed` task.
/// 4. Every required capability is declared. Absent means false, so this fails closed, and
///    untrusted text demands `no_tools` whatever the spec lists. The prompt must also fit
///    the declared context: a provider that cuts a prompt can lose the rules and the markers
///    and still answer.
/// 5. Availability, in `try_provider`.
async fn ineligible(task: &Task<'_>, declaration: &Declaration) -> Result<Option<String>> {
    if is_external(declaration) {
        match consent::state(task.db, &declaration.id.to_string()).await? {
            ConsentState::None => return Ok(Some(CONSENT.into())),
            ConsentState::Suspended(_) => return Ok(Some(CONSENT_SUSPENDED.into())),
            ConsentState::Active(granted) => {
                if !task.spec.categories.iter().all(|c| granted.contains(c)) {
                    return Ok(Some(CONSENT.into()));
                }
            }
        }
    } else if !declaration.local {
        return Ok(Some("model_not_local".into()));
    }
    Ok(unfit(task, declaration))
}

/// Check 4: what the provider cannot do for this task, whatever the user allows.
fn unfit(task: &Task<'_>, declaration: &Declaration) -> Option<String> {
    let missing = lacking(task.spec, &declaration.capabilities);
    if missing.is_none() && task.tokens > declaration.context_limit {
        return Some(PROMPT_TOO_LARGE.into());
    }
    missing
}

/// The capability a spec needs that `capabilities` leaves out, as a reason code. The consent
/// screen reads this too, so it and the router cannot differ on what a provider runs.
pub(super) fn lacking(spec: &TaskSpec, capabilities: &[Capability]) -> Option<String> {
    if carries_untrusted_text(spec) && !capabilities.contains(&Capability::NoTools) {
        return Some(code(&Capability::NoTools));
    }
    let missing = spec.required.iter().find(|c| !capabilities.contains(c));
    missing.map(code)
}

/// Ends `work` at the timeout or within one poll of the cancel, whichever is first.
async fn bounded<T>(
    work: impl Future<Output = Result<T>>,
    limit: Duration,
    cancel: &CancellationToken,
) -> Result<T> {
    let cancelled = async {
        while !cancel.is_cancelled() {
            tokio::time::sleep(CANCEL_POLL).await;
        }
    };
    tokio::select! {
        biased;
        () = cancelled => Err(Error::Cancelled),
        result = tokio::time::timeout(limit, work) => result.unwrap_or(Err(Error::Timeout)),
    }
}

fn check_vectors(vectors: &[Vec<f32>], inputs: usize) -> Result<()> {
    let dims = vectors.first().map_or(0, Vec::len);
    let sound = vectors.len() == inputs
        && (inputs == 0 || dims > 0)
        && vectors
            .iter()
            .all(|v| v.len() == dims && v.iter().all(|x| x.is_finite()));
    if !sound {
        return Err(Error::Validation(
            "embedding reply has the wrong shape".into(),
        ));
    }
    Ok(())
}

/// Written before the send, so a call that cannot be audited does not happen.
async fn audit_external(task: &Task<'_>, provider: &str) -> Result<()> {
    let detail = json!({"provider": provider, "categories": task.spec.categories});
    let target = code(&task.spec.task);
    audit::record(
        task.db,
        task.actor,
        "ai_external_call",
        Some(&target),
        detail,
    )
    .await
}

/// The snake_case name of a serde enum value.
fn code<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn row<'a>(
    provider: &'a str,
    attempt: u32,
    outcome: &'a str,
    reason: Option<&'a str>,
    db: &Db,
) -> Result<Row<'a>> {
    let started_at = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')")
        .fetch_one(&db.pool)
        .await?;
    Ok(Row {
        provider,
        started_at,
        duration_ms: 0,
        attempt,
        outcome,
        reason,
    })
}

async fn log_no_ai(task: &Task<'_>, outcome: &str, reason: Option<&str>) -> Result<()> {
    let attempt = u32::from(outcome != "cancelled");
    log(task, row("no_ai", attempt, outcome, reason, task.db).await?).await
}

// ponytail: task_runs is never pruned. A task writes a few rows, and export reads them all
// into memory, so add retention if the table passes 100,000 rows.
async fn log(task: &Task<'_>, row: Row<'_>) -> Result<()> {
    sqlx::query(
        "INSERT INTO task_runs (id, task_type, provider, data_categories, started_at, \
         duration_ms, attempt, outcome, reason) \
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(code(&task.spec.task))
    .bind(row.provider)
    .bind(json!(task.spec.categories).to_string())
    .bind(row.started_at)
    .bind(row.duration_ms)
    .bind(row.attempt)
    .bind(row.outcome)
    .bind(row.reason)
    .execute(&task.db.pool)
    .await?;
    Ok(())
}
