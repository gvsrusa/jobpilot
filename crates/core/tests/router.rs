//! Task router. Every provider here is a mock: no test reaches a real Ollama or Claude.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::validate::{MAX_OUTPUT_BYTES, validate};
use jobpilot_core::ai::{
    Availability, CancellationToken, Capability, DataCategory, Declaration, Outcome, Prompt,
    Provider, ProviderId, TaskSpec, TaskType, consent, spec,
};
use jobpilot_core::render::ResumeDoc;
use jobpilot_core::{AppPaths, Db, Error, Result, audit, settings, transfer};
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;

const CLAUDE: &str = "claude_code/cli_login";
const JOB: &str = r#"{"title":"Engineer","company":"Acme","location":"Oslo","remote":"hybrid",
    "skills":["Rust"],"seniority":"senior","compensation":""}"#;
const SENTINEL: &str = "ZXQ-POSTING-SENTINEL";

enum Reply {
    Text(String),
    Timeout,
    Io,
    Refused,
    TooLarge,
    Unavailable,
    Hang,
    Vectors(Vec<Vec<f32>>),
}

#[derive(Default)]
struct Counters {
    calls: AtomicU32,
    probes: AtomicU32,
    dropped: AtomicU32,
    schema: Mutex<String>,
}

struct DropCount(Arc<Counters>);

impl Drop for DropCount {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Mock {
    declaration: Declaration,
    replies: Mutex<VecDeque<Reply>>,
    availability: Availability,
    hang_in_probe: bool,
    /// Revokes this provider's consent during its first call.
    revoke_during_call: Option<SqlitePool>,
    /// Cancels the task during each call, so the reply races the cancel.
    cancel_during_call: Option<CancellationToken>,
    counters: Arc<Counters>,
}

impl Mock {
    fn new(id: ProviderId, local: bool, capabilities: &[Capability], replies: Vec<Reply>) -> Mock {
        Mock {
            declaration: Declaration {
                id,
                capabilities: capabilities.to_vec(),
                local,
                context_limit: 8192,
                timeout: Duration::from_secs(5),
                sign_in: "test".into(),
            },
            replies: Mutex::new(replies.into()),
            availability: Availability::Available,
            hang_in_probe: false,
            revoke_during_call: None,
            cancel_during_call: None,
            counters: Arc::default(),
        }
    }

    fn ollama(replies: Vec<Reply>) -> Mock {
        let capabilities = [
            Capability::StructuredOutput,
            Capability::NoTools,
            Capability::Embeddings,
            Capability::LocalExecution,
        ];
        Mock::new(ProviderId::Ollama, true, &capabilities, replies)
    }

    fn claude(replies: Vec<Reply>) -> Mock {
        let capabilities = [Capability::StructuredOutput, Capability::NoTools];
        Mock::new(ProviderId::ClaudeCode, false, &capabilities, replies)
    }

    fn unavailable(mut self) -> Mock {
        self.availability = Availability::Unavailable("not installed at /secret/path".into());
        self
    }

    fn counters(&self) -> Arc<Counters> {
        self.counters.clone()
    }

    fn boxed(self) -> Box<dyn Provider> {
        Box::new(self)
    }

    fn next(&self) -> Option<Reply> {
        self.counters.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel_during_call {
            cancel.cancel();
        }
        self.replies.lock().unwrap().pop_front()
    }
}

impl Provider for Mock {
    fn declaration(&self) -> Declaration {
        self.declaration.clone()
    }

    fn available(&self) -> Pin<Box<dyn Future<Output = Availability> + Send + '_>> {
        self.counters.probes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            if self.hang_in_probe {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            self.availability.clone()
        })
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        *self.counters.schema.lock().unwrap() = prompt.schema.to_string();
        Box::pin(async {
            let reply = self.next();
            if let Some(pool) = &self.revoke_during_call {
                sqlx::query("UPDATE consent SET revoked_at = 'now'")
                    .execute(pool)
                    .await
                    .unwrap();
            }
            match reply {
                Some(Reply::Text(text)) => Ok(text),
                Some(Reply::Timeout) => Err(Error::Timeout),
                Some(Reply::Io) => Err(Error::Io(std::io::Error::other("reset"))),
                Some(Reply::Refused) => Err(Error::Refused("not_allowed".into())),
                Some(Reply::TooLarge) => Err(Error::Refused("prompt_too_large".into())),
                Some(Reply::Unavailable) => Err(Error::Unavailable),
                Some(Reply::Hang) => {
                    let _guard = DropCount(self.counters.clone());
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(JOB.to_string())
                }
                Some(Reply::Vectors(_)) | None => Err(Error::Failed("script ended".into())),
            }
        })
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        _inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send + 'a>> {
        Box::pin(async {
            match self.next() {
                Some(Reply::Vectors(vectors)) => Ok(vectors),
                Some(Reply::Timeout) => Err(Error::Timeout),
                Some(Reply::Hang) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(Vec::new())
                }
                _ => Err(Error::Failed("script ended".into())),
            }
        })
    }
}

fn text(reply: &str) -> Reply {
    Reply::Text(reply.to_string())
}

fn prompt() -> Prompt {
    Prompt {
        system: "system rules".into(),
        user: format!("<<<UNTRUSTED_POSTING>>>{SENTINEL}<<<END_UNTRUSTED_POSTING>>>"),
        schema: spec(TaskType::ParseJob).schema,
    }
}

fn draft() -> String {
    json!({
        "doc": {"name": "", "contact": [], "summary": [], "sections": []},
        "claims": [{"text": "Built parsers", "evidence_ids": ["a"]}],
    })
    .to_string()
}

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    (tmp, db)
}

async fn parse_job(router: &Router, db: &Db) -> Outcome {
    let cancel = CancellationToken::new();
    router
        .run(db, "gui", TaskType::ParseJob, prompt(), cancel)
        .await
        .unwrap()
}

/// (provider, attempt, outcome, reason), oldest first.
async fn rows(db: &Db) -> Vec<(String, i64, String, Option<String>)> {
    sqlx::query_as("SELECT provider, attempt, outcome, reason FROM task_runs ORDER BY rowid")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn skip_reasons(db: &Db, provider: &str) -> Vec<String> {
    rows(db)
        .await
        .into_iter()
        .filter(|row| row.0 == provider && row.2 == "skipped")
        .filter_map(|row| row.3)
        .collect()
}

fn calls(counters: &Counters) -> u32 {
    counters.calls.load(Ordering::SeqCst)
}

fn manual(reason: &str) -> Outcome {
    Outcome::ManualRequired {
        reason: reason.into(),
    }
}

fn is_ok(outcome: &Outcome, id: ProviderId, count: u32) -> bool {
    matches!(outcome, Outcome::Ok { provider, attempts, .. } if *provider == id && *attempts == count)
}

async fn run_with_unavailable(why: &str) -> Outcome {
    let (_tmp, db) = setup().await;
    let mut ollama = Mock::ollama(vec![]);
    ollama.availability = Availability::Unavailable(why.into());
    let router = Router::with_providers(vec![ollama.boxed()]);
    parse_job(&router, &db).await
}

#[tokio::test]
async fn ollama_with_no_model_set_says_so() {
    assert_eq!(
        run_with_unavailable("no_model").await,
        manual("no_model_set")
    );
}

#[tokio::test]
async fn ollama_missing_the_model_says_so() {
    assert_eq!(
        run_with_unavailable("model_missing").await,
        manual("model_not_installed")
    );
}

#[tokio::test]
async fn an_unknown_unavailable_stays_provider_unavailable() {
    assert_eq!(
        run_with_unavailable("not_running").await,
        manual("provider_unavailable")
    );
}

#[tokio::test]
async fn falls_back_in_order_and_ends_no_ai_manual_required() {
    let (_tmp, db) = setup().await;
    let first = Mock::ollama(vec![Reply::Timeout, Reply::Timeout]);
    let second = Mock::ollama(vec![]).unavailable();
    let second_counters = second.counters();
    let router = Router::with_providers(vec![first.boxed(), second.boxed()]);

    assert_eq!(
        parse_job(&router, &db).await,
        manual("provider_unavailable")
    );

    assert_eq!(calls(&second_counters), 0);
    assert_eq!(
        rows(&db).await,
        vec![
            ("ollama".into(), 1, "failed".into(), Some("timeout".into())),
            ("ollama".into(), 2, "failed".into(), Some("timeout".into())),
            (
                "ollama".into(),
                0,
                "skipped".into(),
                Some("unavailable".into())
            ),
            (
                "no_ai".into(),
                1,
                "manual_required".into(),
                Some("provider_unavailable".into())
            ),
        ]
    );
}

#[tokio::test]
async fn truncated_output_fails_validation_then_falls_back() {
    let (_tmp, db) = setup().await;
    let cancel = CancellationToken::new;

    let retried = Mock::ollama(vec![text("{\"doc\":{"), text(&draft())]);
    let router = Router::with_providers(vec![retried.boxed()]);
    let outcome = router
        .run(&db, "gui", TaskType::DraftResume, prompt(), cancel())
        .await
        .unwrap();
    assert!(is_ok(&outcome, ProviderId::Ollama, 2), "{outcome:?}");

    let first = Mock::ollama(vec![text("{\"doc\":{"), text("{\"doc\":{")]);
    let second = Mock::ollama(vec![text(&draft())]);
    let (first_counters, second_counters) = (first.counters(), second.counters());
    let router = Router::with_providers(vec![first.boxed(), second.boxed()]);
    let outcome = router
        .run(&db, "gui", TaskType::DraftResume, prompt(), cancel())
        .await
        .unwrap();
    assert!(is_ok(&outcome, ProviderId::Ollama, 1), "{outcome:?}");
    assert_eq!(calls(&first_counters), 2);
    assert_eq!(calls(&second_counters), 1);
}

#[tokio::test]
async fn job_text_task_skips_provider_without_no_tools() {
    let (_tmp, db) = setup().await;
    let mock = Mock::new(
        ProviderId::Ollama,
        true,
        &[Capability::StructuredOutput],
        vec![text(JOB)],
    );
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("no_ai"));

    assert_eq!(calls(&counters), 0);
    assert_eq!(skip_reasons(&db, "ollama").await, ["no_tools"]);
}

/// The rule follows the data, not each spec's `required` list, so a spec that forgets
/// `NoTools` still cannot send untrusted text to a provider with tools.
#[tokio::test]
async fn untrusted_categories_demand_no_tools_even_if_the_spec_omits_it() {
    for category in [DataCategory::JobText, DataCategory::CompanyText] {
        let (_tmp, db) = setup().await;
        let categories: &'static [DataCategory] = match category {
            DataCategory::JobText => &[DataCategory::JobText],
            _ => &[DataCategory::ProfileBasic, DataCategory::CompanyText],
        };
        let forgetful = TaskSpec {
            task: TaskType::ParseJob,
            schema: spec(TaskType::ParseJob).schema,
            required: &[Capability::StructuredOutput],
            categories,
            attempts: 2,
        };
        let mock = Mock::new(
            ProviderId::Ollama,
            true,
            &[Capability::StructuredOutput],
            vec![text(JOB)],
        );
        let counters = mock.counters();
        let outcome = Router::with_providers(vec![mock.boxed()])
            .run_spec(&db, "gui", &forgetful, prompt(), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(outcome, manual("no_ai"));
        assert_eq!(calls(&counters), 0);
        assert_eq!(skip_reasons(&db, "ollama").await, ["no_tools"]);
    }
}

#[tokio::test]
async fn task_without_untrusted_text_does_not_demand_no_tools() {
    let (_tmp, db) = setup().await;
    let plain = TaskSpec {
        task: TaskType::ParseJob,
        schema: spec(TaskType::ParseJob).schema,
        required: &[Capability::StructuredOutput],
        categories: &[DataCategory::ResumeText],
        attempts: 2,
    };
    let mock = Mock::new(
        ProviderId::Ollama,
        true,
        &[Capability::StructuredOutput],
        vec![text(JOB)],
    );
    let outcome = Router::with_providers(vec![mock.boxed()])
        .run_spec(&db, "gui", &plain, prompt(), CancellationToken::new())
        .await
        .unwrap();
    assert!(is_ok(&outcome, ProviderId::Ollama, 1), "{outcome:?}");
}

#[tokio::test]
async fn sensitive_with_untrusted_text_is_refused_before_any_provider() {
    let mixes: [&'static [DataCategory]; 3] = [
        &[DataCategory::Sensitive, DataCategory::JobText],
        &[DataCategory::CompanyText, DataCategory::Sensitive],
        &[
            DataCategory::JobText,
            DataCategory::CompanyText,
            DataCategory::Sensitive,
        ],
    ];
    for categories in mixes {
        let (_tmp, db) = setup().await;
        let mixed = TaskSpec {
            task: TaskType::ParseJob,
            schema: spec(TaskType::ParseJob).schema,
            required: spec(TaskType::ParseJob).required,
            categories,
            attempts: 2,
        };
        // Even a provider consented for everything never sees this task.
        let all = [
            DataCategory::JobText,
            DataCategory::CompanyText,
            DataCategory::Sensitive,
        ];
        consent::give(&db, CLAUDE, &all, "2026-11-01", "gui")
            .await
            .unwrap();
        let (ollama, claude) = (Mock::ollama(vec![text(JOB)]), Mock::claude(vec![text(JOB)]));
        let counters = [ollama.counters(), claude.counters()];
        let router = Router::with_providers(vec![ollama.boxed(), claude.boxed()]);

        let outcome = router
            .run_spec(&db, "gui", &mixed, prompt(), CancellationToken::new())
            .await
            .unwrap();

        let reason = "sensitive_with_untrusted_text";
        assert_eq!(
            outcome,
            Outcome::Failed {
                reason: reason.into()
            }
        );
        for counters in counters {
            assert_eq!(calls(&counters), 0);
            assert_eq!(counters.probes.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            rows(&db).await,
            vec![(
                "router".into(),
                0,
                "refused".into(),
                Some(reason.to_string())
            )]
        );
    }
}

#[tokio::test]
async fn sensitive_alone_still_runs() {
    let (_tmp, db) = setup().await;
    let sensitive = TaskSpec {
        task: TaskType::ParseJob,
        schema: spec(TaskType::ParseJob).schema,
        required: &[Capability::StructuredOutput],
        categories: &[DataCategory::Sensitive],
        attempts: 2,
    };
    let router = Router::with_providers(vec![Mock::ollama(vec![text(JOB)]).boxed()]);
    let outcome = router
        .run_spec(&db, "gui", &sensitive, prompt(), CancellationToken::new())
        .await
        .unwrap();
    assert!(is_ok(&outcome, ProviderId::Ollama, 1), "{outcome:?}");
}

#[test]
fn no_shipped_task_mixes_sensitive_with_untrusted_text() {
    for task in [
        TaskType::ParseJob,
        TaskType::DraftResume,
        TaskType::DraftCoverLetter,
        TaskType::Embed,
    ] {
        let categories = spec(task).categories;
        assert!(!categories.contains(&DataCategory::Sensitive), "{task:?}");
    }
}

#[tokio::test]
async fn external_provider_without_consent_is_skipped_and_consent_unlocks_it() {
    let (_tmp, db) = setup().await;
    let build = || Router::with_providers(vec![Mock::claude(vec![text(JOB)]).boxed()]);

    assert_eq!(parse_job(&build(), &db).await, manual("consent_needed"));
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent"]);

    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let outcome = parse_job(&build(), &db).await;
    assert!(is_ok(&outcome, ProviderId::ClaudeCode, 1), "{outcome:?}");

    consent::revoke(&db, CLAUDE, "gui").await.unwrap();
    assert_eq!(parse_job(&build(), &db).await, manual("consent_needed"));
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent", "consent"]);
}

#[tokio::test]
async fn fallback_never_widens_consent() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();

    let claude = Mock::claude(vec![text(&draft())]);
    let counters = claude.counters();
    let router = Router::with_providers(vec![claude.boxed()]);
    let outcome = router
        .run(
            &db,
            "gui",
            TaskType::DraftResume,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, manual("consent_needed"));
    assert_eq!(calls(&counters), 0);

    consent::revoke(&db, CLAUDE, "gui").await.unwrap();
    let ollama = Mock::ollama(vec![Reply::Refused]);
    let claude = Mock::claude(vec![text(JOB)]);
    let counters = claude.counters();
    let router = Router::with_providers(vec![ollama.boxed(), claude.boxed()]);
    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));
    assert_eq!(calls(&counters), 0);
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent", "consent"]);
}

#[tokio::test]
async fn consent_is_keyed_by_provider_and_sign_in_method() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, "claude_code", &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    consent::give(&db, "ollama", &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let router = Router::with_providers(vec![Mock::claude(vec![text(JOB)]).boxed()]);
    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent"]);
}

#[tokio::test]
async fn revoking_between_attempts_stops_the_retry() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let mut claude = Mock::claude(vec![text("not json"), text(JOB)]);
    claude.revoke_during_call = Some(db.pool.clone());
    let counters = claude.counters();
    let router = Router::with_providers(vec![claude.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));

    assert_eq!(calls(&counters), 1);
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent"]);
}

/// The end of a task says what would change it: consent, whichever way round the providers
/// are, and not "no provider is on" while one is waiting for it.
#[tokio::test]
async fn consent_needed_outranks_an_unavailable_provider_in_either_order() {
    let (_tmp, db) = setup().await;
    let down = || Mock::ollama(vec![]).unavailable().boxed();
    let waiting = || Mock::claude(vec![text(JOB)]).boxed();

    let router = Router::with_providers(vec![down(), waiting()]);
    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));
    let router = Router::with_providers(vec![waiting(), down()]);
    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));
}

/// A reason from a real attempt is the most specific there is, so a later skip keeps it.
#[tokio::test]
async fn a_failed_attempt_keeps_its_reason_past_a_later_skip() {
    let (_tmp, db) = setup().await;
    let too_big = {
        let mut mock = Mock::ollama(vec![text(JOB)]);
        mock.declaration.context_limit = 1;
        mock
    };
    let waiting = Mock::claude(vec![text(JOB)]);
    let router = Router::with_providers(vec![too_big.boxed(), waiting.boxed()]);
    assert_eq!(parse_job(&router, &db).await, manual("prompt_too_large"));
}

/// Consent would not let this one run, so asking for it would send the user the wrong way.
#[tokio::test]
async fn a_provider_short_of_more_than_consent_does_not_ask_for_consent() {
    let (_tmp, db) = setup().await;
    let mut mock = Mock::claude(vec![text(JOB)]);
    mock.declaration.capabilities.clear();
    let router = Router::with_providers(vec![mock.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("no_ai"));
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent"]);
}

/// With no provider listed at all, `no_ai` stays the honest reason.
#[tokio::test]
async fn no_provider_at_all_is_still_no_ai() {
    let (_tmp, db) = setup().await;
    let router = Router::with_providers(vec![]);
    assert_eq!(parse_job(&router, &db).await, manual("no_ai"));
}

/// Consent cannot rescue a model that is not local: it is ineligible outright.
#[tokio::test]
async fn ollama_model_not_confirmed_local_is_skipped_whatever_the_consent() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, "ollama", &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let mut mock = Mock::ollama(vec![text(JOB)]);
    mock.declaration.local = false;
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("no_ai"));

    assert_eq!(calls(&counters), 0);
    assert_eq!(skip_reasons(&db, "ollama").await, ["model_not_local"]);
}

/// Only Ollama's detection confirms a local model. Claude Code claiming it changes nothing.
#[tokio::test]
async fn claude_code_declaring_local_still_needs_consent() {
    let (_tmp, db) = setup().await;
    let mut mock = Mock::claude(vec![text(JOB)]);
    mock.declaration.local = true;
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));

    assert_eq!(calls(&counters), 0);
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent"]);
}

#[tokio::test]
async fn missing_required_capability_is_skipped_by_name() {
    let (_tmp, db) = setup().await;
    let mock = Mock::new(
        ProviderId::Ollama,
        true,
        &[Capability::NoTools],
        vec![text(JOB)],
    );
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("no_ai"));

    assert_eq!(calls(&counters), 0);
    assert_eq!(skip_reasons(&db, "ollama").await, ["structured_output"]);
}

/// Availability is check 5: an ineligible provider is not even probed.
#[tokio::test]
async fn ineligible_provider_is_never_probed() {
    let (_tmp, db) = setup().await;
    let claude = Mock::claude(vec![]);
    let counters = claude.counters();
    let router = Router::with_providers(vec![claude.boxed()]);
    parse_job(&router, &db).await;
    assert_eq!(counters.probes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn retry_policy_follows_the_error() {
    // (reply, calls, why)
    let cases: Vec<(Vec<Reply>, u32, &str)> = vec![
        (vec![Reply::Io, Reply::Io], 2, "io retries once"),
        (vec![Reply::Refused, Reply::Refused], 1, "never on refused"),
        (
            vec![Reply::Unavailable, Reply::Unavailable],
            1,
            "never on unavailable",
        ),
        (
            vec![text(&"x".repeat(MAX_OUTPUT_BYTES + 1)), text(JOB)],
            1,
            "never on too large",
        ),
    ];
    for (replies, expected, why) in cases {
        let (_tmp, db) = setup().await;
        let mock = Mock::ollama(replies);
        let counters = mock.counters();
        let router = Router::with_providers(vec![mock.boxed()]);
        assert_eq!(parse_job(&router, &db).await, manual("no_ai"), "{why}");
        assert_eq!(calls(&counters), expected, "{why}");
    }
}

#[tokio::test]
async fn io_then_success_counts_both_attempts() {
    let (_tmp, db) = setup().await;
    let router = Router::with_providers(vec![Mock::ollama(vec![Reply::Io, text(JOB)]).boxed()]);
    let outcome = parse_job(&router, &db).await;
    assert!(is_ok(&outcome, ProviderId::Ollama, 2), "{outcome:?}");
}

/// A generous attempt limit does not buy a third look at bad output.
#[tokio::test]
async fn second_validation_failure_ends_the_provider_with_attempts_left() {
    let (_tmp, db) = setup().await;
    let generous = TaskSpec {
        task: TaskType::ParseJob,
        schema: spec(TaskType::ParseJob).schema,
        required: spec(TaskType::ParseJob).required,
        categories: spec(TaskType::ParseJob).categories,
        attempts: 3,
    };
    let mock = Mock::ollama(vec![text("nope"), text("nope"), text(JOB)]);
    let counters = mock.counters();
    let outcome = Router::with_providers(vec![mock.boxed()])
        .run_spec(&db, "gui", &generous, prompt(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(calls(&counters), 2);
}

/// The caller's `Prompt.schema` cannot weaken validation.
#[tokio::test]
async fn output_is_checked_against_the_task_schema_not_the_prompt() {
    let (_tmp, db) = setup().await;
    let mock = Mock::ollama(vec![text("{}"), text("{}")]);
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);
    let loose = Prompt {
        schema: "{}",
        ..prompt()
    };
    let outcome = router
        .run(
            &db,
            "gui",
            TaskType::ParseJob,
            loose,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(
        *counters.schema.lock().unwrap(),
        spec(TaskType::ParseJob).schema
    );
}

#[tokio::test]
async fn timeout_kills_attempt_and_moves_on() {
    let (_tmp, db) = setup().await;
    let slow = Mock::ollama(vec![Reply::Hang, Reply::Hang]);
    let slow_counters = slow.counters();
    let next = Mock::ollama(vec![text(JOB)]);
    let router = Router::with_providers(vec![slow.boxed(), next.boxed()])
        .with_timeout(Duration::from_millis(100));

    let started = Instant::now();
    let outcome = parse_job(&router, &db).await;

    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(is_ok(&outcome, ProviderId::Ollama, 1), "{outcome:?}");
    assert_eq!(slow_counters.dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn declared_timeout_bounds_an_attempt_without_an_override() {
    let (_tmp, db) = setup().await;
    let mut slow = Mock::ollama(vec![Reply::Hang, Reply::Hang]);
    slow.declaration.timeout = Duration::from_millis(100);
    let router = Router::with_providers(vec![slow.boxed()]);

    let started = Instant::now();
    assert_eq!(parse_job(&router, &db).await, manual("no_ai"));

    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn attempt_rows_record_how_long_the_attempt_took() {
    let (_tmp, db) = setup().await;
    let slow = Mock::ollama(vec![Reply::Hang, Reply::Hang]);
    let router =
        Router::with_providers(vec![slow.boxed()]).with_timeout(Duration::from_millis(100));

    parse_job(&router, &db).await;

    let durations: Vec<i64> =
        sqlx::query_scalar("SELECT duration_ms FROM task_runs WHERE provider = 'ollama'")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(durations.len(), 2);
    assert!(durations.iter().all(|ms| *ms >= 50), "{durations:?}");
}

#[tokio::test]
async fn hanging_availability_probe_cannot_outlive_the_timeout() {
    let (_tmp, db) = setup().await;
    let mut slow = Mock::ollama(vec![text(JOB)]);
    slow.hang_in_probe = true;
    let counters = slow.counters();
    let router =
        Router::with_providers(vec![slow.boxed()]).with_timeout(Duration::from_millis(100));

    let started = Instant::now();
    assert_eq!(
        parse_job(&router, &db).await,
        manual("provider_unavailable")
    );

    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(calls(&counters), 0);
    assert_eq!(skip_reasons(&db, "ollama").await, ["unavailable"]);
}

#[tokio::test]
async fn cancel_ends_with_cancelled_and_no_further_attempts() {
    let (_tmp, db) = setup().await;
    let slow = Mock::ollama(vec![Reply::Hang, text(JOB)]);
    let next = Mock::ollama(vec![text(JOB)]);
    let (slow_counters, next_counters) = (slow.counters(), next.counters());
    let router = Router::with_providers(vec![slow.boxed(), next.boxed()]);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();

    let started = Instant::now();
    let (outcome, ()) = tokio::join!(
        router.run(&db, "gui", TaskType::ParseJob, prompt(), cancel),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        }
    );

    assert_eq!(outcome.unwrap(), Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(calls(&slow_counters), 1);
    assert_eq!(slow_counters.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(calls(&next_counters), 0);
    let last = rows(&db).await.pop().unwrap();
    assert_eq!(last, ("ollama".into(), 1, "cancelled".into(), None));
}

#[tokio::test]
async fn already_cancelled_task_sends_nothing() {
    let (_tmp, db) = setup().await;
    let mock = Mock::ollama(vec![text(JOB)]);
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = router
        .run(&db, "gui", TaskType::ParseJob, prompt(), cancel)
        .await
        .unwrap();

    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(calls(&counters), 0);
}

/// A valid reply finishing in the same poll as the cancel still ends `Cancelled`.
#[tokio::test]
async fn reply_racing_a_cancel_is_not_returned() {
    let (_tmp, db) = setup().await;
    let cancel = CancellationToken::new();
    let mut mock = Mock::ollama(vec![text(JOB)]);
    mock.cancel_during_call = Some(cancel.clone());
    let router = Router::with_providers(vec![mock.boxed()]);

    let outcome = router
        .run(&db, "gui", TaskType::ParseJob, prompt(), cancel)
        .await
        .unwrap();

    assert_eq!(outcome, Outcome::Cancelled);
    let last = rows(&db).await.pop().unwrap();
    assert_eq!(last, ("ollama".into(), 1, "cancelled".into(), None));
}

/// Nothing left to try, but the user cancelled: that is not a request for manual work.
#[tokio::test]
async fn cancel_wins_over_the_no_ai_fallback() {
    let (_tmp, db) = setup().await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = Router::with_providers(Vec::new())
        .run(&db, "gui", TaskType::ParseJob, prompt(), cancel)
        .await
        .unwrap();

    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(
        rows(&db).await,
        vec![("no_ai".into(), 0, "cancelled".into(), None)]
    );
}

#[tokio::test]
async fn embed_cancel_wins_over_the_no_ai_fallback() {
    let (_tmp, db) = setup().await;
    with_embedding_model(&db).await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = Router::with_providers(Vec::new())
        .embed(&db, &["a".to_string()], cancel)
        .await;

    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert_eq!(
        rows(&db).await,
        vec![("no_ai".into(), 0, "cancelled".into(), None)]
    );
}

#[tokio::test]
async fn external_call_writes_an_audit_row_without_prompt() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let router = Router::with_providers(vec![Mock::claude(vec![text(JOB)]).boxed()]);

    parse_job(&router, &db).await;

    let log = audit::recent(&db, 10).await.unwrap();
    let calls: Vec<_> = log
        .iter()
        .filter(|entry| entry.action == "ai_external_call")
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].actor, "gui");
    assert_eq!(
        calls[0].detail,
        json!({"provider": CLAUDE, "categories": ["job_text"]})
    );
    assert!(!calls[0].detail.to_string().contains(SENTINEL));
}

/// The audit row comes first: a call that cannot be audited does not happen.
#[tokio::test]
async fn failed_audit_write_stops_the_send() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    sqlx::query("DROP TABLE audit_log")
        .execute(&db.pool)
        .await
        .unwrap();
    let claude = Mock::claude(vec![text(JOB)]);
    let counters = claude.counters();
    let router = Router::with_providers(vec![claude.boxed()]);

    let result = router
        .run(
            &db,
            "gui",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await;

    assert!(matches!(result, Err(Error::Db(_))), "{result:?}");
    assert_eq!(calls(&counters), 0);
}

#[tokio::test]
async fn each_external_attempt_is_audited_and_local_ones_are_not() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "cli")
        .await
        .unwrap();
    let ollama = Mock::ollama(vec![Reply::Refused]);
    let claude = Mock::claude(vec![text("not json"), text(JOB)]);
    let router = Router::with_providers(vec![ollama.boxed(), claude.boxed()]);

    parse_job(&router, &db).await;

    let log = audit::recent(&db, 10).await.unwrap();
    let count = |action: &str| log.iter().filter(|e| e.action == action).count();
    assert_eq!(count("ai_external_call"), 2);
    assert!(log.iter().all(|e| e.detail["provider"] != "ollama"));
}

#[tokio::test]
async fn logs_hold_codes_never_prompts_outputs_or_probe_text() {
    let (_tmp, db) = setup().await;
    let bad = format!("{{\"title\":\"{SENTINEL}\"}}");
    let first = Mock::ollama(vec![text(&bad), text(&bad)]);
    let second = Mock::ollama(vec![]).unavailable();
    let router = Router::with_providers(vec![first.boxed(), second.boxed()]);

    parse_job(&router, &db).await;

    let dump: Vec<(String,)> = sqlx::query_as(
        "SELECT task_type || data_categories || started_at || outcome || coalesce(reason, '') \
         FROM task_runs",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(dump.len(), 4);
    for (row,) in &dump {
        assert!(row.starts_with("parse_job[\"job_text\"]"), "{row}");
        assert!(!row.contains(SENTINEL), "{row}");
        assert!(!row.contains("/secret/path"), "{row}");
        assert!(!row.contains("system rules"), "{row}");
    }
    let validation = rows(&db).await[0].clone();
    assert_eq!(validation.3.as_deref(), Some("validation"));
}

#[test]
fn validate_rejects_oversize_and_unknown_fields() {
    let schema = spec(TaskType::ParseJob).schema;
    let oversize = format!("{JOB}{}", " ".repeat(2 * 1024 * 1024));
    assert!(matches!(
        validate(schema, &oversize),
        Err(Error::TooLarge { limit }) if limit == MAX_OUTPUT_BYTES as u64
    ));

    let mut extra: serde_json::Value = serde_json::from_str(JOB).unwrap();
    extra["ignore_previous"] = json!("x");
    assert!(matches!(
        validate(schema, &extra.to_string()),
        Err(Error::Validation(_))
    ));
}

/// Library messages quote the rejected value, and messages can reach a log or a screen.
#[test]
fn validation_errors_never_quote_the_output() {
    let schema = spec(TaskType::ParseJob).schema;
    for raw in [
        SENTINEL.to_string(),
        format!("{{\"title\":\"{SENTINEL}\"}}"),
    ] {
        let error = validate(schema, &raw).unwrap_err();
        assert!(!error.to_string().contains(SENTINEL), "{error}");
    }
}

#[test]
fn validate_tolerates_exactly_one_json_fence() {
    let schema = spec(TaskType::ParseJob).schema;
    let fenced = format!("  ```json\n{JOB}\n```\n");
    assert_eq!(validate(schema, &fenced).unwrap()["company"], "Acme");
    assert!(validate(schema, JOB).is_ok());

    let twice = format!("```json\n{fenced}\n```");
    assert!(matches!(
        validate(schema, &twice),
        Err(Error::Validation(_))
    ));
    let prose = format!("Here you go: {JOB}");
    assert!(matches!(
        validate(schema, &prose),
        Err(Error::Validation(_))
    ));
}

#[test]
fn a_malformed_schema_is_an_error_not_a_panic() {
    assert!(validate("", JOB).is_err());
    assert!(validate(r#"{"type": 7}"#, JOB).is_err());
}

#[test]
fn output_at_the_size_limit_is_not_too_large() {
    let padded = format!("{JOB}{}", " ".repeat(MAX_OUTPUT_BYTES - JOB.len()));
    assert_eq!(padded.len(), MAX_OUTPUT_BYTES);
    assert!(validate(spec(TaskType::ParseJob).schema, &padded).is_ok());
}

#[test]
fn parse_job_schema_enforces_its_bounds() {
    let schema = spec(TaskType::ParseJob).schema;
    let mut job: serde_json::Value = serde_json::from_str(JOB).unwrap();
    job["title"] = json!("x".repeat(500));
    assert!(validate(schema, &job.to_string()).is_ok());

    for (field, value) in [
        ("title", json!("x".repeat(501))),
        ("remote", json!("sometimes")),
        ("skills", json!(["x".repeat(501)])),
    ] {
        let mut bad: serde_json::Value = serde_json::from_str(JOB).unwrap();
        bad[field] = value;
        assert!(validate(schema, &bad.to_string()).is_err(), "{field}");
    }
    let mut missing: serde_json::Value = serde_json::from_str(JOB).unwrap();
    missing.as_object_mut().unwrap().remove("seniority");
    assert!(validate(schema, &missing.to_string()).is_err());
}

/// Each level of `ResumeDoc` is `deny_unknown_fields`, so the schema must refuse the same
/// extras, or a reply could pass the router and fail to deserialize later.
#[test]
fn draft_schemas_refuse_extras_at_every_level() {
    let resume = spec(TaskType::DraftResume).schema;
    let valid = json!({
        "doc": {"name": "A", "contact": ["c"], "summary": ["s"], "sections": [
            {"heading": "H", "entries": [
                {"title": "T", "subtitle": "S", "dates": "D", "bullets": ["B"]}
            ]}
        ]},
        "claims": [{"text": "B", "evidence_ids": ["id"]}],
    });
    validate(resume, &valid.to_string()).unwrap();
    let doc: ResumeDoc = serde_json::from_value(valid["doc"].clone()).unwrap();
    let round_trip = json!({"doc": doc, "claims": []});
    validate(resume, &round_trip.to_string()).unwrap();

    for pointer in [
        "",
        "/doc",
        "/doc/sections/0",
        "/doc/sections/0/entries/0",
        "/claims/0",
    ] {
        let mut extra = valid.clone();
        extra.pointer_mut(pointer).unwrap()["x"] = json!(1);
        assert!(
            matches!(
                validate(resume, &extra.to_string()),
                Err(Error::Validation(_))
            ),
            "{pointer:?}"
        );
    }

    let mut long = valid.clone();
    long["claims"][0]["text"] = json!("x".repeat(601));
    assert!(validate(resume, &long.to_string()).is_err());

    let letter = spec(TaskType::DraftCoverLetter).schema;
    let valid = json!({"paragraphs": [{"text": "T", "evidence_ids": []}]});
    validate(letter, &valid.to_string()).unwrap();
    for pointer in ["", "/paragraphs/0"] {
        let mut extra = valid.clone();
        extra.pointer_mut(pointer).unwrap()["x"] = json!(1);
        assert!(validate(letter, &extra.to_string()).is_err(), "{pointer:?}");
    }
}

#[test]
fn task_specs_match_the_design() {
    for task in [
        TaskType::ParseJob,
        TaskType::DraftResume,
        TaskType::DraftCoverLetter,
    ] {
        let spec = spec(task);
        assert_eq!(spec.task, task);
        assert_eq!(
            spec.required,
            [Capability::StructuredOutput, Capability::NoTools]
        );
        assert_eq!(spec.attempts, 2);
    }
    assert_eq!(spec(TaskType::ParseJob).categories, [DataCategory::JobText]);
    let drafting = [
        DataCategory::JobText,
        DataCategory::ProfileBasic,
        DataCategory::ResumeText,
    ];
    assert_eq!(spec(TaskType::DraftResume).categories, drafting);
    assert_eq!(spec(TaskType::DraftCoverLetter).categories, drafting);
    let embed = spec(TaskType::Embed);
    assert_eq!(embed.schema, "");
    assert_eq!(
        embed.required,
        [Capability::Embeddings, Capability::LocalExecution]
    );
    assert_eq!(
        embed.categories,
        [DataCategory::ResumeText, DataCategory::CareerMemory]
    );
}

/// Task 17's documented `consent give` must be enough for both drafting tasks.
#[tokio::test]
async fn documented_consent_unlocks_claude_drafting() {
    let (_tmp, db) = setup().await;
    let documented = [
        DataCategory::JobText,
        DataCategory::ResumeText,
        DataCategory::ProfileBasic,
    ];
    consent::give(&db, CLAUDE, &documented, "2026-11-01", "cli")
        .await
        .unwrap();
    let letter = json!({"paragraphs": []}).to_string();
    for (task, reply) in [
        (TaskType::DraftResume, draft()),
        (TaskType::DraftCoverLetter, letter),
    ] {
        let router = Router::with_providers(vec![Mock::claude(vec![text(&reply)]).boxed()]);
        let outcome = router
            .run(&db, "cli", task, prompt(), CancellationToken::new())
            .await
            .unwrap();
        assert!(is_ok(&outcome, ProviderId::ClaudeCode, 1), "{outcome:?}");
    }
}

/// Embedding has no schema and takes no prompt: only `Router::embed` may run it.
#[tokio::test]
async fn run_refuses_the_embed_task_up_front() {
    let (_tmp, db) = setup().await;
    let mock = Mock::ollama(vec![text("{}")]);
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);

    let outcome = router
        .run(
            &db,
            "gui",
            TaskType::Embed,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let reason = "embed_is_not_a_completion";
    assert_eq!(
        outcome,
        Outcome::Failed {
            reason: reason.into()
        }
    );
    assert_eq!(calls(&counters), 0);
    assert_eq!(counters.probes.load(Ordering::SeqCst), 0);
    assert_eq!(
        rows(&db).await,
        vec![(
            "router".into(),
            0,
            "refused".into(),
            Some(reason.to_string())
        )]
    );
}

#[test]
fn provider_ids_display_as_the_consent_key() {
    assert_eq!(ProviderId::Ollama.to_string(), "ollama");
    assert_eq!(ProviderId::ClaudeCode.to_string(), CLAUDE);
    assert_eq!(ProviderId::NoAi.to_string(), "no_ai");
}

#[tokio::test]
async fn works_with_ollama_alone_claude_alone_and_no_ai() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();

    let ollama = Router::with_providers(vec![Mock::ollama(vec![text(JOB)]).boxed()]);
    let outcome = parse_job(&ollama, &db).await;
    assert!(is_ok(&outcome, ProviderId::Ollama, 1), "{outcome:?}");

    let claude = Router::with_providers(vec![Mock::claude(vec![text(JOB)]).boxed()]);
    let outcome = parse_job(&claude, &db).await;
    assert!(is_ok(&outcome, ProviderId::ClaudeCode, 1), "{outcome:?}");

    let none = Router::with_providers(Vec::new());
    assert_eq!(parse_job(&none, &db).await, manual("no_ai"));
}

#[tokio::test]
async fn router_keeps_user_order_and_always_ends_with_the_real_no_ai() {
    let (_tmp, db) = setup().await;
    let impostor = Mock::new(ProviderId::NoAi, true, &[], vec![text(JOB)]);
    let counters = impostor.counters();
    let router = Router::with_providers(vec![
        impostor.boxed(),
        Mock::claude(vec![]).boxed(),
        Mock::ollama(vec![]).boxed(),
    ]);
    assert_eq!(
        router.provider_ids(),
        [ProviderId::ClaudeCode, ProviderId::Ollama, ProviderId::NoAi]
    );
    assert_eq!(parse_job(&router, &db).await, manual("consent_needed"));
    assert_eq!(calls(&counters), 0);

    let from_settings = Router::from_settings(&db).await.unwrap();
    assert_eq!(
        from_settings.provider_ids(),
        [ProviderId::Ollama, ProviderId::NoAi]
    );
}

fn vectors(count: usize) -> Reply {
    Reply::Vectors(vec![vec![0.5, 0.5]; count])
}

async fn with_embedding_model(db: &Db) {
    settings::set(db, "ai.embedding_model", &json!("nomic-embed-text"))
        .await
        .unwrap();
}

#[tokio::test]
async fn embed_returns_model_and_vectors() {
    let (_tmp, db) = setup().await;
    with_embedding_model(&db).await;
    let inputs = vec!["a".to_string(), "b".to_string()];
    let router = Router::with_providers(vec![Mock::ollama(vec![vectors(2)]).boxed()]);

    let (model, output) = router
        .embed(&db, &inputs, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(model, "nomic-embed-text");
    assert_eq!(output.len(), 2);
    let row = rows(&db).await.pop().unwrap();
    assert_eq!(row, ("ollama".into(), 1, "ok".into(), None));
}

#[tokio::test]
async fn embed_without_a_model_or_provider_is_manual_required() {
    let (_tmp, db) = setup().await;
    let inputs = vec!["a".to_string()];
    let is_manual = |result: Result<(String, Vec<Vec<f32>>)>| matches!(result, Err(Error::ManualRequired(reason)) if reason == "no_embedding_model");

    let mock = Mock::ollama(vec![vectors(1)]);
    let counters = mock.counters();
    let router = Router::with_providers(vec![mock.boxed()]);
    let cancel = CancellationToken::new;
    assert!(is_manual(router.embed(&db, &inputs, cancel()).await));
    assert_eq!(calls(&counters), 0);

    with_embedding_model(&db).await;
    let none = Router::with_providers(Vec::new());
    assert!(is_manual(none.embed(&db, &inputs, cancel()).await));
    let last = rows(&db).await.pop().unwrap();
    assert_eq!(last.0, "no_ai");
    assert_eq!(last.2, "manual_required");
}

/// Embedding runs the same checks: `LocalExecution` is required and consent still applies.
#[tokio::test]
async fn embed_runs_the_routing_checks() {
    let (_tmp, db) = setup().await;
    with_embedding_model(&db).await;
    let inputs = vec!["a".to_string()];

    let drafting_only = Mock::claude(vec![vectors(1)]);
    let lacking = drafting_only.counters();
    let claiming = Mock::new(
        ProviderId::ClaudeCode,
        true,
        &[Capability::Embeddings, Capability::LocalExecution],
        vec![vectors(1)],
    );
    let claimed = claiming.counters();
    let router = Router::with_providers(vec![drafting_only.boxed(), claiming.boxed()]);

    let result = router.embed(&db, &inputs, CancellationToken::new()).await;

    assert!(
        matches!(result, Err(Error::ManualRequired(_))),
        "{result:?}"
    );
    assert_eq!(calls(&lacking), 0);
    assert_eq!(calls(&claimed), 0);
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent", "consent"]);
}

#[tokio::test]
async fn embed_rejects_a_reply_of_the_wrong_shape_and_falls_back() {
    let (_tmp, db) = setup().await;
    with_embedding_model(&db).await;
    let inputs = vec!["a".to_string(), "b".to_string()];
    let uneven = Reply::Vectors(vec![vec![1.0], vec![1.0, 2.0]]);
    let first = Mock::ollama(vec![vectors(1), uneven]);
    let second = Mock::ollama(vec![vectors(2)]);
    let first_counters = first.counters();
    let router = Router::with_providers(vec![first.boxed(), second.boxed()]);

    let (_, output) = router
        .embed(&db, &inputs, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(output.len(), 2);
    assert_eq!(calls(&first_counters), 2);
}

#[tokio::test]
async fn embed_refuses_empty_or_non_finite_vectors() {
    let (_tmp, db) = setup().await;
    with_embedding_model(&db).await;
    let inputs = vec!["a".to_string()];
    for bad in [vec![Vec::new()], vec![vec![f32::NAN, 1.0]]] {
        let mock = Mock::ollama(vec![Reply::Vectors(bad.clone()), Reply::Vectors(bad)]);
        let result = Router::with_providers(vec![mock.boxed()])
            .embed(&db, &inputs, CancellationToken::new())
            .await;
        assert!(
            matches!(result, Err(Error::ManualRequired(_))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn embed_honours_timeout_and_cancel() {
    let (_tmp, db) = setup().await;
    with_embedding_model(&db).await;
    let inputs = vec!["a".to_string()];

    let router = Router::with_providers(vec![Mock::ollama(vec![Reply::Hang, Reply::Hang]).boxed()])
        .with_timeout(Duration::from_millis(50));
    let started = Instant::now();
    let result = router.embed(&db, &inputs, CancellationToken::new()).await;
    assert!(
        matches!(result, Err(Error::ManualRequired(_))),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(500));

    let router = Router::with_providers(vec![Mock::ollama(vec![Reply::Hang]).boxed()]);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let (result, ()) = tokio::join!(router.embed(&db, &inputs, cancel), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
}

#[tokio::test]
async fn consent_gives_revokes_and_audits() {
    let (_tmp, db) = setup().await;
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);

    let both = [DataCategory::JobText, DataCategory::ResumeText];
    consent::give(&db, CLAUDE, &both, "2026-11-01", "gui")
        .await
        .unwrap();
    assert_eq!(
        consent::active(&db, CLAUDE).await.unwrap(),
        Some(both.to_vec())
    );

    consent::revoke(&db, CLAUDE, "cli").await.unwrap();
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);

    let log = audit::recent(&db, 10).await.unwrap();
    let actions: Vec<(&str, &str, Option<&str>)> = log
        .iter()
        .map(|e| (e.action.as_str(), e.actor.as_str(), e.target.as_deref()))
        .collect();
    assert_eq!(
        actions,
        [
            ("consent_revoked", "cli", Some(CLAUDE)),
            ("consent_given", "gui", Some(CLAUDE)),
        ]
    );
    assert_eq!(
        log[1].detail,
        json!({"categories": ["job_text", "resume_text"], "terms_date": "2026-11-01"})
    );

    // The record stays, with its revocation, for export.
    let kept: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT terms_date, revoked_at FROM consent")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].1.is_some());
}

/// One active record per provider: narrowing consent must really narrow it.
#[tokio::test]
async fn a_new_consent_replaces_the_active_one() {
    let (_tmp, db) = setup().await;
    let wide = [DataCategory::JobText, DataCategory::ResumeText];
    let narrow = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &wide, "2026-11-01", "gui")
        .await
        .unwrap();
    consent::give(&db, CLAUDE, &narrow, "2026-11-02", "gui")
        .await
        .unwrap();

    assert_eq!(
        consent::active(&db, CLAUDE).await.unwrap(),
        Some(narrow.to_vec())
    );
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM consent WHERE revoked_at IS NULL")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(open, 1);
    consent::revoke(&db, CLAUDE, "gui").await.unwrap();
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
}

#[tokio::test]
async fn consent_for_one_provider_leaves_another_alone() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    consent::give(&db, "openai/api_key", &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    consent::revoke(&db, "openai/api_key", "gui").await.unwrap();
    assert!(consent::active(&db, CLAUDE).await.unwrap().is_some());
}

#[tokio::test]
async fn empty_consent_is_refused_and_revoking_nothing_is_not_audited() {
    let (_tmp, db) = setup().await;
    let empty = consent::give(&db, CLAUDE, &[], "2026-11-01", "gui").await;
    assert!(matches!(empty, Err(Error::Validation(_))), "{empty:?}");
    consent::revoke(&db, CLAUDE, "gui").await.unwrap();
    assert!(audit::recent(&db, 10).await.unwrap().is_empty());
}

/// An unreadable record blocks the send rather than being read as consent.
#[tokio::test]
async fn a_corrupt_consent_record_sends_nothing() {
    let (_tmp, db) = setup().await;
    sqlx::query(
        "INSERT INTO consent (provider, categories, given_at, terms_date) \
         VALUES (?, '[\"job_text\",\"from_the_future\"]', 'now', '2026-11-01')",
    )
    .bind(CLAUDE)
    .execute(&db.pool)
    .await
    .unwrap();
    let claude = Mock::claude(vec![text(JOB)]);
    let counters = claude.counters();
    let router = Router::with_providers(vec![claude.boxed()]);

    let result = router
        .run(
            &db,
            "gui",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err(), "{result:?}");
    assert_eq!(calls(&counters), 0);
}

#[tokio::test]
async fn runs_and_consent_reach_the_export() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    parse_job(&Router::with_providers(Vec::new()), &db).await;

    let path = transfer::export_all(&db, "gui").await.unwrap();
    let text = std::fs::read_to_string(path.join("data.json")).unwrap();
    let data: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(data["tables"]["consent"][0]["provider"], CLAUDE);
    assert_eq!(data["tables"]["task_runs"][0]["outcome"], "manual_required");
}

/// Tauri commands need `Send` futures, and an embedded `dyn Provider` must not break that.
#[test]
fn router_futures_are_send() {
    fn is_send<T: Send>(_: &T) {}
    fn check(router: &Router, db: &Db) {
        is_send(&router.run(
            db,
            "gui",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        ));
        is_send(&router.embed(db, &[], CancellationToken::new()));
    }
    let _ = check;
}

// ---- prompt size and external retries -----------------------------------------------------

/// A prompt as the router will measure it, with this task's schema in it.
fn sized_prompt(user: &str) -> Prompt {
    Prompt {
        system: "sys".into(),
        user: user.into(),
        schema: spec(TaskType::ParseJob).schema,
    }
}

async fn run_prompt(router: &Router, db: &Db, prompt: Prompt) -> Outcome {
    router
        .run(
            db,
            "gui",
            TaskType::ParseJob,
            prompt,
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[test]
fn the_estimate_counts_every_part_in_bytes_and_rounds_up() {
    let prompt = |system: &str, user: &str, schema: &'static str| Prompt {
        system: system.into(),
        user: user.into(),
        schema,
    };
    assert_eq!(prompt("", "", "").estimated_tokens(), 0);
    assert_eq!(prompt("a", "", "").estimated_tokens(), 1);
    assert_eq!(prompt("ab", "cd", "ef").estimated_tokens(), 3);
    assert_eq!(prompt("ab", "cd", "e").estimated_tokens(), 3);
    assert_eq!(prompt("", "\u{6f22}\u{5b57}", "").estimated_tokens(), 3);
}

#[tokio::test]
async fn a_prompt_at_the_limit_runs_and_one_past_it_is_skipped() {
    let (_tmp, db) = setup().await;
    let tokens = sized_prompt("usr").estimated_tokens();

    let mut fits = Mock::ollama(vec![text(JOB)]);
    fits.declaration.context_limit = tokens;
    let router = Router::with_providers(vec![fits.boxed()]);
    let outcome = run_prompt(&router, &db, sized_prompt("usr")).await;
    assert!(is_ok(&outcome, ProviderId::Ollama, 1), "{outcome:?}");

    let mut over = Mock::ollama(vec![text(JOB)]);
    over.declaration.context_limit = tokens - 1;
    let counters = over.counters();
    let router = Router::with_providers(vec![over.boxed()]);
    let outcome = run_prompt(&router, &db, sized_prompt("usr")).await;

    assert_eq!(outcome, manual("prompt_too_large"));
    assert_eq!(calls(&counters), 0);
    assert_eq!(counters.probes.load(Ordering::SeqCst), 0);
    assert_eq!(skip_reasons(&db, "ollama").await, ["prompt_too_large"]);
    let last = rows(&db).await.pop().unwrap();
    assert_eq!(last.0, "no_ai");
    assert_eq!(last.3.as_deref(), Some("prompt_too_large"));
}

#[tokio::test]
async fn the_schema_counts_toward_the_size() {
    let (_tmp, db) = setup().await;
    let mut small = Mock::ollama(vec![text(JOB)]);
    // Room for the text alone, not for the schema the router adds.
    small.declaration.context_limit = 16;
    let router = Router::with_providers(vec![small.boxed()]);
    let bare = Prompt {
        system: "sys".into(),
        user: "usr".into(),
        schema: "",
    };

    assert_eq!(
        run_prompt(&router, &db, bare).await,
        manual("prompt_too_large")
    );
}

#[tokio::test]
async fn cjk_text_is_measured_high_enough() {
    let (_tmp, db) = setup().await;
    let user = "\u{6f22}".repeat(4_000);
    let schema_tokens = sized_prompt("").estimated_tokens();
    let mut small = Mock::ollama(vec![text(JOB)]);
    // Enough at two characters a token, too little for the bytes this text really takes.
    small.declaration.context_limit = schema_tokens + 3_000;
    let counters = small.counters();
    let router = Router::with_providers(vec![small.boxed()]);

    let outcome = run_prompt(&router, &db, sized_prompt(&user)).await;

    assert_eq!(outcome, manual("prompt_too_large"));
    assert_eq!(calls(&counters), 0);
}

#[tokio::test]
async fn a_small_provider_falls_through_to_a_larger_one() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let mut small = Mock::ollama(vec![text(JOB)]);
    small.declaration.context_limit = 10;
    let first = small.counters();
    let large = Mock::claude(vec![text(JOB)]);
    let router = Router::with_providers(vec![small.boxed(), large.boxed()]);

    let outcome = run_prompt(&router, &db, sized_prompt("usr")).await;

    assert!(is_ok(&outcome, ProviderId::ClaudeCode, 1), "{outcome:?}");
    assert_eq!(calls(&first), 0);
}

#[tokio::test]
async fn an_ineligible_provider_reports_that_and_not_the_size() {
    let (_tmp, db) = setup().await;
    let mut claude = Mock::claude(vec![text(JOB)]);
    claude.declaration.context_limit = 10;
    let router = Router::with_providers(vec![claude.boxed()]);

    let outcome = run_prompt(&router, &db, sized_prompt("usr")).await;

    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(skip_reasons(&db, CLAUDE).await, ["consent"]);
}

#[tokio::test]
async fn an_overflow_the_provider_reports_is_not_retried_and_reaches_the_user() {
    let (_tmp, db) = setup().await;
    let ollama = Mock::ollama(vec![Reply::TooLarge, text(JOB)]);
    let counters = ollama.counters();
    let router = Router::with_providers(vec![ollama.boxed()]);

    assert_eq!(parse_job(&router, &db).await, manual("prompt_too_large"));

    assert_eq!(calls(&counters), 1);
    let first = rows(&db).await.remove(0);
    assert_eq!(first.2, "failed");
    assert_eq!(first.3.as_deref(), Some("prompt_too_large"));
}

#[tokio::test]
async fn claude_is_never_sent_again_after_a_timeout_or_broken_call() {
    for failure in [Reply::Timeout, Reply::Io] {
        let (_tmp, db) = setup().await;
        let job_text = [DataCategory::JobText];
        consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
            .await
            .unwrap();
        let claude = Mock::claude(vec![failure, text(JOB)]);
        let counters = claude.counters();
        let router = Router::with_providers(vec![claude.boxed()]);

        assert_eq!(parse_job(&router, &db).await, manual("no_ai"));

        assert_eq!(calls(&counters), 1);
    }
}

#[tokio::test]
async fn claude_is_retried_once_after_a_validation_failure() {
    let (_tmp, db) = setup().await;
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let claude = Mock::claude(vec![text("not json"), text(JOB)]);
    let router = Router::with_providers(vec![claude.boxed()]);

    let outcome = parse_job(&router, &db).await;

    assert!(is_ok(&outcome, ProviderId::ClaudeCode, 2), "{outcome:?}");
}

#[tokio::test]
async fn ollama_is_still_retried_after_a_timeout() {
    let (_tmp, db) = setup().await;
    let ollama = Mock::ollama(vec![Reply::Timeout, text(JOB)]);
    let router = Router::with_providers(vec![ollama.boxed()]);

    let outcome = parse_job(&router, &db).await;

    assert!(is_ok(&outcome, ProviderId::Ollama, 2), "{outcome:?}");
}
