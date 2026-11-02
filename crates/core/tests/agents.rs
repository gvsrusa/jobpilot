//! Agents as the adapters reach them.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jobpilot_core::agents::draft::DraftWorkflow;
use jobpilot_core::agents::memory::MemoryWorkflow;
use jobpilot_core::agents::{self, tracking};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    Availability, BoxFuture, CancellationToken, Capability, DataCategory, Declaration, Prompt,
    Provider, ProviderId, TaskType, consent,
};
use jobpilot_core::applications::{self, Application, ApplicationInput};
use jobpilot_core::crypto::{self, DataKey};
use jobpilot_core::drafts::ExportFormat;
use jobpilot_core::knowledge::{self, JobText, KnowledgeItem};
use jobpilot_core::memory::classify::{
    CLASSIFY_BATCH, classify_pending, classify_prompt, refresh_plan,
};
use jobpilot_core::memory::{self, Class};
use jobpilot_core::profile::{self, ProfileBasic};
use jobpilot_core::render::Template;
use jobpilot_core::runner::{
    self, Agent, Ctx, ExportChoice, Permission, RunInput, RunState, StepDef, StepOutput, Workflow,
};
use jobpilot_core::{AppPaths, Db, Error, Result, backup, settings, transfer};
use serde_json::json;
use sqlx::{Column, Row};
use tempfile::TempDir;

async fn setup() -> (TempDir, Db, Application) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        ..ApplicationInput::default()
    };
    let app = applications::add(&db, "cli", input).await.unwrap();
    (tmp, db, app)
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

#[tokio::test]
async fn tracking_status_change_runs_on_the_runner() {
    let (_tmp, db, app) = setup().await;
    let moved = agents::set_status(
        &db,
        "cli",
        &app.id,
        "applied",
        Some("2026-10-02"),
        "sent",
        token(),
    )
    .await
    .unwrap();
    assert_eq!(moved.status, "applied");
    assert_eq!(moved.applied_on.as_deref(), Some("2026-10-02"));
    let history = applications::history(&db, &app.id).await.unwrap();
    assert_eq!(history.last().unwrap().note, "sent");
    assert_eq!(history.last().unwrap().actor, "cli");

    let runs = runner::list(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].agent, Agent::Tracking);
    assert_eq!(runs[0].state, RunState::Completed);
    let detail = runner::get(&db, &runs[0].id).await.unwrap();
    for row in &detail.log {
        assert_eq!(row.provider, None);
        assert!(row.data_categories.is_empty());
    }
}

#[tokio::test]
async fn tracking_stores_no_note() {
    let (_tmp, db, app) = setup().await;
    agents::set_status(
        &db,
        "cli",
        &app.id,
        "applied",
        None,
        "my private note",
        token(),
    )
    .await
    .unwrap();
    let (input,): (String,) = sqlx::query_as("SELECT input FROM agent_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!input.contains("private"), "{input}");
}

#[tokio::test]
async fn tracking_validates_before_a_run_exists() {
    let (_tmp, db, app) = setup().await;
    let cases = [
        ("missing", "applied", None, "", "not_found"),
        (app.id.as_str(), "ghosted", None, "", "validation"),
        (app.id.as_str(), "applied", Some("soon"), "", "validation"),
        (
            app.id.as_str(),
            "applied",
            None,
            &*"x".repeat(20_001),
            "validation",
        ),
    ];
    for (id, status, on, note, code) in cases {
        let error = agents::set_status(&db, "cli", id, status, on, note, token())
            .await
            .unwrap_err();
        assert_eq!(error.code(), code, "{status}");
    }
    assert!(runner::list(&db, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_halted_tracking_run_resumes_from_its_stored_input() {
    let (_tmp, db, app) = setup().await;
    agents::set_status(&db, "cli", &app.id, "applied", None, "", token())
        .await
        .unwrap();
    let id = runner::list(&db, 1).await.unwrap().remove(0).id;
    // As a process that died before its update left things.
    sqlx::query("UPDATE agent_runs SET state = 'halted', step = 0, finished_at = NULL")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE applications SET status = 'saved', applied_on = NULL")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM application_events WHERE kind = 'status'")
        .execute(&db.pool)
        .await
        .unwrap();

    let router = Router::from_settings(&db).await.unwrap();
    let done = agents::resume_run(&db, &router, "cli", &id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    let app = applications::get(&db, &app.id).await.unwrap();
    assert_eq!(app.status, "applied");
    let last = applications::history(&db, &app.id).await.unwrap();
    assert_eq!(last.last().unwrap().note, "", "the note is not stored");
    assert_eq!(
        agents::resume_run(&db, &router, "cli", "missing", token())
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test]
async fn a_cancelled_token_changes_no_status() {
    let (_tmp, db, app) = setup().await;
    let cancel = token();
    cancel.cancel();
    let error = agents::set_status(&db, "cli", &app.id, "applied", None, "", cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cancelled");
    assert_eq!(
        applications::get(&db, &app.id).await.unwrap().status,
        "saved"
    );
}

/// Rule 7, as the 0009 comment states it.
#[tokio::test]
async fn run_rows_go_through_backup_export_and_delete_all() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        ..ApplicationInput::default()
    };
    let app = applications::add(&db, "cli", input).await.unwrap();
    agents::set_status(&db, "cli", &app.id, "applied", None, "", token())
        .await
        .unwrap();
    let snapshot = backup::create(&db, "cli").await.unwrap();
    agents::set_status(&db, "cli", &app.id, "offer", None, "", token())
        .await
        .unwrap();

    let db = backup::restore(db, &snapshot.path, "cli").await.unwrap().db;
    assert_eq!(runner::list(&db, 10).await.unwrap().len(), 1);

    let dir = transfer::export_all(&db, "cli").await.unwrap();
    let data = std::fs::read_to_string(dir.join("data.json")).unwrap();
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(data["tables"]["agent_runs"].as_array().unwrap().len(), 1);
    assert!(
        !data["tables"]["agent_run_log"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    transfer::delete_all(db, "DELETE", true).await.unwrap();
    let db = Db::open(&paths).await.unwrap();
    assert!(runner::list(&db, 10).await.unwrap().is_empty());
    db.close().await;
}

async fn halt(db: &Db, id: &str) {
    sqlx::query(
        "UPDATE agent_runs SET state = 'halted', step = 0, finished_at = NULL WHERE id = ?",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_resume_never_rewinds_a_newer_status() {
    let (_tmp, db, app) = setup().await;
    agents::set_status(&db, "cli", &app.id, "applied", None, "", token())
        .await
        .unwrap();
    let id = runner::list(&db, 1).await.unwrap().remove(0).id;
    halt(&db, &id).await;
    agents::set_status(&db, "cli", &app.id, "interviewing", None, "", token())
        .await
        .unwrap();

    let router = Router::from_settings(&db).await.unwrap();
    let error = agents::resume_run(&db, &router, "cli", &id, token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "validation");
    let current = applications::get(&db, &app.id).await.unwrap();
    assert_eq!(current.status, "interviewing");
    let run = runner::get(&db, &id).await.unwrap().run;
    assert_eq!(run.state, RunState::Halted, "still the person's to cancel");
    agents::cancel_run(&db, "cli", &id).await.unwrap();
}

/// The update landed and its process died before the checkpoint: nothing to rewind.
#[tokio::test]
async fn a_resume_of_an_update_that_landed_is_quiet() {
    let (_tmp, db, app) = setup().await;
    agents::set_status(&db, "cli", &app.id, "applied", None, "", token())
        .await
        .unwrap();
    let id = runner::list(&db, 1).await.unwrap().remove(0).id;
    halt(&db, &id).await;
    let events = applications::history(&db, &app.id).await.unwrap().len();
    let router = Router::from_settings(&db).await.unwrap();
    let done = agents::resume_run(&db, &router, "cli", &id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(
        applications::history(&db, &app.id).await.unwrap().len(),
        events
    );
}

/// Tracking's step, run as another agent or with a fired token, so its own checks are what
/// stops it. The runner's would stop it first otherwise.
struct Wrapped {
    inner: tracking::Tracking,
    agent: Agent,
    fire: bool,
}

static WRAPPED: &[StepDef] = &[StepDef {
    name: "update",
    needs: &[Permission::ReadApplications],
    attempts: 1,
}];

impl Workflow for Wrapped {
    fn agent(&self) -> Agent {
        self.agent
    }

    fn input(&self) -> RunInput {
        self.inner.input()
    }

    fn steps(&self) -> &'static [StepDef] {
        WRAPPED
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        if self.fire {
            ctx.cancel.cancel();
        }
        self.inner.step(ctx, index)
    }
}

fn wrapped(app: &Application, agent: Agent, fire: bool) -> Wrapped {
    Wrapped {
        inner: tracking::Tracking {
            application_id: app.id.clone(),
            status: "applied".into(),
            on: None,
            note: String::new(),
        },
        agent,
        fire,
    }
}

#[tokio::test]
async fn tracking_checks_its_own_permissions() {
    let (_tmp, db, app) = setup().await;
    let workflow = wrapped(&app, Agent::Analytics, false);
    let done = runner::start(&db, "cli", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Refused);
    assert_eq!(done.reason.as_deref(), Some("permission_denied"));
    assert_eq!(
        applications::get(&db, &app.id).await.unwrap().status,
        "saved"
    );
}

#[tokio::test]
async fn tracking_stops_before_the_update_once_the_token_fired() {
    let (_tmp, db, app) = setup().await;
    let workflow = wrapped(&app, Agent::Tracking, true);
    let done = runner::start(&db, "cli", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(
        applications::get(&db, &app.id).await.unwrap().status,
        "saved"
    );
}

// ---- scripted providers -----------------------------------------------------------------

const CLAUDE: &str = "claude_code/cli_login";
const BULLET: &str = "Shipped the Rust compiler";

/// A provider that answers from a list and counts what it is asked. `Ollama` is local and
/// needs no consent; `ClaudeCode` is external.
struct Script {
    id: ProviderId,
    no_tools: bool,
    embeds: bool,
    replies: Mutex<VecDeque<String>>,
    calls: Arc<AtomicUsize>,
    embed_calls: Arc<AtomicUsize>,
    /// Fired during the first call, as a press on Stop while the model works.
    cancel_in_call: Option<CancellationToken>,
}

struct Handles {
    calls: Arc<AtomicUsize>,
    embed_calls: Arc<AtomicUsize>,
}

impl Handles {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn script(id: ProviderId, replies: Vec<String>) -> (Script, Handles) {
    let calls = Arc::new(AtomicUsize::new(0));
    let embed_calls = Arc::new(AtomicUsize::new(0));
    let provider = Script {
        id,
        no_tools: true,
        embeds: false,
        replies: Mutex::new(replies.into()),
        calls: calls.clone(),
        embed_calls: embed_calls.clone(),
        cancel_in_call: None,
    };
    (provider, Handles { calls, embed_calls })
}

fn router_of(provider: Script) -> Router {
    Router::with_providers(vec![Box::new(provider)])
}

impl Provider for Script {
    fn declaration(&self) -> Declaration {
        let local = self.id == ProviderId::Ollama;
        let mut capabilities = vec![Capability::StructuredOutput];
        if self.no_tools {
            capabilities.push(Capability::NoTools);
        }
        if self.embeds {
            capabilities.extend([Capability::Embeddings, Capability::LocalExecution]);
        }
        Declaration {
            id: self.id.clone(),
            capabilities,
            local,
            context_limit: 100_000,
            timeout: Duration::from_secs(5),
            sign_in: "test".into(),
        }
    }

    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async { Availability::Available })
    }

    fn complete<'a>(
        &'a self,
        _prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel_in_call {
            cancel.cancel();
        }
        let reply = self.replies.lock().unwrap().pop_front();
        Box::pin(async move { reply.ok_or_else(|| Error::Failed("no reply left".into())) })
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
        self.embed_calls.fetch_add(1, Ordering::SeqCst);
        let vectors = inputs.iter().map(|_| vec![0.5, 0.25, 0.125]).collect();
        Box::pin(async move { Ok(vectors) })
    }
}

async fn consent_to(db: &Db, categories: &[DataCategory]) {
    consent::give(db, CLAUDE, categories, "2026-01-01", "cli")
        .await
        .unwrap();
}

async fn count(db: &Db, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// Every text column of a table, joined.
async fn text_of(db: &Db, table: &str) -> String {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {table}")))
        .fetch_all(&db.pool)
        .await
        .unwrap();
    let mut all = String::new();
    for row in &rows {
        for column in row.columns() {
            if let Ok(Some(text)) = row.try_get::<Option<String>, _>(column.ordinal()) {
                all.push_str(&text);
                all.push('\n');
            }
        }
    }
    all
}

// ---- resume and cover letter ------------------------------------------------------------

struct World {
    _tmp: TempDir,
    db: Db,
    item: KnowledgeItem,
    job: JobText,
}

async fn world_with(bullet: &str, posting: &str, name: &str) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    profile::set_basic(
        &db,
        ProfileBasic {
            name: name.into(),
            email: "ada@example.com".into(),
            ..ProfileBasic::default()
        },
    )
    .await
    .unwrap();
    let item = knowledge::add(&db, "experience", bullet, "user", None)
        .await
        .unwrap();
    let job = knowledge::import_job_text(&db, "Compiler Engineer", posting)
        .await
        .unwrap();
    World {
        _tmp: tmp,
        db,
        item,
        job,
    }
}

async fn world() -> World {
    world_with(BULLET, "We need a Rust engineer.", "Ada Lovelace").await
}

fn resume_reply(bullet: &str, ids: &[&str]) -> String {
    json!({
        "doc": {
            "name": "Mallory Model",
            "contact": [],
            "summary": [],
            "sections": [{
                "heading": "Experience",
                "entries": [{"title": "", "subtitle": "", "dates": "", "bullets": [bullet]}],
            }],
        },
        "claims": [{"text": bullet, "evidence_ids": ids}],
    })
    .to_string()
}

fn pdf() -> Option<ExportChoice> {
    Some(ExportChoice {
        format: ExportFormat::Pdf,
        template: Template::Classic,
    })
}

async fn claude_world() -> (World, Router, Handles) {
    let w = world().await;
    consent_to(
        &w.db,
        &[
            DataCategory::JobText,
            DataCategory::ProfileBasic,
            DataCategory::ResumeText,
        ],
    )
    .await;
    let reply = resume_reply(BULLET, &[&w.item.id]);
    let (provider, handles) = script(ProviderId::ClaudeCode, vec![reply]);
    (w, router_of(provider), handles)
}

#[tokio::test]
async fn resume_agent_stores_a_draft_and_logs_provider_and_categories() {
    let (w, router, handles) = claude_world().await;
    let made = agents::draft_resume(&w.db, &router, "cli", &w.job.id, None, token())
        .await
        .unwrap();
    assert_eq!(made.blocked, 0);
    assert_eq!(made.exported, None);
    assert_eq!(handles.calls(), 1);

    let runs = runner::list(&w.db, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].agent, Agent::Resume);
    assert_eq!(runs[0].state, RunState::Completed);
    let detail = runner::get(&w.db, &runs[0].id).await.unwrap();
    let logged = detail
        .log
        .iter()
        .find(|row| row.provider.as_deref() == Some(CLAUDE))
        .expect("a row names the provider");
    assert_eq!(
        logged.data_categories,
        [
            DataCategory::JobText,
            DataCategory::ProfileBasic,
            DataCategory::ResumeText
        ]
    );
}

#[tokio::test]
async fn no_ai_ends_manual_required_and_keeps_the_reason() {
    let w = world().await;
    let router = Router::with_providers(vec![]);
    let error = agents::draft_resume(&w.db, &router, "cli", &w.job.id, None, token())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::ManualRequired(why) if why == "no_ai"),
        "{error:?}"
    );
    let run = runner::list(&w.db, 1).await.unwrap().remove(0);
    assert_eq!(run.state, RunState::ManualRequired);
    assert_eq!(run.reason.as_deref(), Some("no_ai"));
}

#[tokio::test]
async fn no_evidence_never_calls_a_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let job = knowledge::import_job_text(&db, "Engineer", "A posting.")
        .await
        .unwrap();
    let (provider, handles) = script(ProviderId::Ollama, vec![resume_reply(BULLET, &[])]);
    let router = router_of(provider);
    let error = agents::draft_cover_letter(&db, &router, "cli", &job.id, None, token())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::ManualRequired(why) if why == "no_evidence"),
        "{error:?}"
    );
    assert_eq!(handles.calls(), 0);
    let run = runner::list(&db, 1).await.unwrap().remove(0);
    assert_eq!(run.agent, Agent::CoverLetter);
    assert_eq!(run.state, RunState::ManualRequired);
    assert_eq!(run.reason.as_deref(), Some("no_evidence"));
}

#[tokio::test]
async fn a_missing_posting_gets_its_plain_error_and_no_run() {
    let w = world().await;
    let router = Router::with_providers(vec![]);
    let error = agents::draft_resume(&w.db, &router, "cli", "missing", None, token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "not_found");
    assert!(runner::list(&w.db, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn resume_export_renders_into_the_exports_folder_only() {
    let w = world().await;
    let reply = resume_reply(BULLET, &[&w.item.id]);
    let (provider, _) = script(ProviderId::Ollama, vec![reply]);
    let router = router_of(provider);
    let made = agents::draft_resume(&w.db, &router, "cli", &w.job.id, pdf(), token())
        .await
        .unwrap();
    let path = std::path::PathBuf::from(made.exported.expect("exported"));
    assert!(path.starts_with(w.db.paths().exports()), "{path:?}");
    assert!(path.is_file());
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("pdf"));
}

#[tokio::test]
async fn a_blocked_draft_is_stored_and_not_rendered() {
    let w = world().await;
    let (provider, _) = script(ProviderId::Ollama, vec![resume_reply(BULLET, &[])]);
    let router = router_of(provider);
    let made = agents::draft_resume(&w.db, &router, "cli", &w.job.id, pdf(), token())
        .await
        .unwrap();
    assert!(made.blocked > 0);
    assert_eq!(made.exported, None);
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 1);
    assert!(
        !w.db.paths().exports().exists()
            || std::fs::read_dir(w.db.paths().exports()).unwrap().count() == 0
    );
    let run = runner::list(&w.db, 1).await.unwrap().remove(0);
    assert_eq!(run.state, RunState::Completed);
}

/// Draft, then a halt at render the first time it is asked.
struct HaltAtRender<'r> {
    inner: DraftWorkflow<'r>,
    tripped: AtomicBool,
}

impl Workflow for HaltAtRender<'_> {
    fn agent(&self) -> Agent {
        self.inner.agent()
    }

    fn input(&self) -> RunInput {
        self.inner.input()
    }

    fn steps(&self) -> &'static [StepDef] {
        self.inner.steps()
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        if index == 1 && !self.tripped.swap(true, Ordering::SeqCst) {
            return Box::pin(async { Err(Error::Paused) });
        }
        self.inner.step(ctx, index)
    }
}

#[tokio::test]
async fn a_halted_run_resumes_at_render_with_no_second_model_call() {
    let w = world().await;
    let reply = resume_reply(BULLET, &[&w.item.id]);
    let (provider, handles) = script(ProviderId::Ollama, vec![reply]);
    let router = router_of(provider);
    let workflow = HaltAtRender {
        inner: DraftWorkflow::new(TaskType::DraftResume, &w.job.id, pdf(), &router),
        tripped: AtomicBool::new(false),
    };
    let halted = runner::start(&w.db, "cli", &workflow, token())
        .await
        .unwrap();
    assert_eq!(halted.state, RunState::Halted);
    assert_eq!(handles.calls(), 1);
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 1);

    let done = agents::resume_run(&w.db, &router, "cli", &halted.run_id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(handles.calls(), 1, "no second model call");
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 1);
    let files: Vec<_> = std::fs::read_dir(w.db.paths().exports()).unwrap().collect();
    assert_eq!(files.len(), 1, "the file exists");
}

#[tokio::test]
async fn a_halted_run_can_be_cancelled_and_leaves_its_draft() {
    let w = world().await;
    let reply = resume_reply(BULLET, &[&w.item.id]);
    let (provider, _) = script(ProviderId::Ollama, vec![reply]);
    let router = router_of(provider);
    let workflow = HaltAtRender {
        inner: DraftWorkflow::new(TaskType::DraftResume, &w.job.id, pdf(), &router),
        tripped: AtomicBool::new(false),
    };
    let halted = runner::start(&w.db, "cli", &workflow, token())
        .await
        .unwrap();
    agents::cancel_run(&w.db, "cli", &halted.run_id)
        .await
        .unwrap();
    let run = runner::get(&w.db, &halted.run_id).await.unwrap().run;
    assert_eq!(run.state, RunState::Cancelled);
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 1);
}

#[tokio::test]
async fn cancelling_a_draft_stores_nothing() {
    let w = world().await;
    let first = resume_reply(BULLET, &[&w.item.id]);
    let (provider, _) = script(ProviderId::Ollama, vec![first]);
    agents::draft_resume(&w.db, &router_of(provider), "cli", &w.job.id, None, token())
        .await
        .unwrap();

    let cancel = token();
    let second = resume_reply(BULLET, &[&w.item.id]);
    let (mut provider, _) = script(ProviderId::Ollama, vec![second]);
    provider.cancel_in_call = Some(cancel.clone());
    let error = agents::draft_resume(&w.db, &router_of(provider), "cli", &w.job.id, None, cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cancelled");
    assert_eq!(
        count(&w.db, "SELECT count(*) FROM drafts").await,
        1,
        "earlier versions stay"
    );
    let newest = runner::list(&w.db, 1).await.unwrap().remove(0);
    assert_eq!(newest.state, RunState::Cancelled);
}

#[tokio::test]
async fn a_run_holds_no_posting_prompt_or_answer_text() {
    let w = world_with(
        "ZXQ-KNOWLEDGE-CANARY experience",
        "ZXQ-POSTING-CANARY wanted",
        "ZXQ-PROFILE-CANARY",
    )
    .await;
    let reply = resume_reply("ZXQ-REPLY-CANARY", &[&w.item.id]);
    let (provider, _) = script(ProviderId::Ollama, vec![reply]);
    memory::add_question(&w.db, "ZXQ-ANSWER-CANARY question?", None)
        .await
        .unwrap();
    agents::draft_resume(&w.db, &router_of(provider), "cli", &w.job.id, None, token())
        .await
        .unwrap();
    let (provider, _) = script(
        ProviderId::Ollama,
        vec![json!({"class": "normal"}).to_string()],
    );
    agents::memory_refresh(&w.db, &router_of(provider), "cli", token())
        .await
        .unwrap();

    for table in ["agent_runs", "agent_run_log"] {
        let text = text_of(&w.db, table).await;
        assert!(!text.is_empty(), "{table}");
        assert!(!text.contains("CANARY"), "{table}: {text}");
    }
    assert!(count(&w.db, "SELECT count(*) FROM agent_run_log").await > 0);
}

// ---- career memory ----------------------------------------------------------------------

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn class_reply(class: &str) -> String {
    json!({ "class": class }).to_string()
}

async fn questions(db: &Db, n: usize) {
    for i in 0..n {
        memory::add_question(db, &format!("Describe project number {i}"), None)
            .await
            .unwrap();
    }
}

async fn use_model(db: &Db) {
    settings::set(db, "ai.embedding_model", &json!("test-embed"))
        .await
        .unwrap();
}

#[tokio::test]
async fn memory_refresh_classifies_then_embeds() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    let first = memory::add_question(&db, "Tell me about yourself", None)
        .await
        .unwrap();
    let second = memory::add_question(&db, "Why this company?", None)
        .await
        .unwrap();
    let (mut provider, handles) = script(
        ProviderId::Ollama,
        vec![class_reply("sensitive"), class_reply("normal")],
    );
    provider.embeds = true;
    let done = agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!(
        (
            done.classified.checked,
            done.classified.raised,
            done.classified.skipped
        ),
        (2, 1, false)
    );
    assert_eq!(done.embedded, 2);
    assert_eq!(handles.calls(), 2);

    let first = memory::get(&db, &first.id).await.unwrap().question;
    assert_eq!(first.class, Class::Sensitive);
    assert_eq!(first.class_source, "model");
    assert!(first.classified_at.is_some());
    let second = memory::get(&db, &second.id).await.unwrap().question;
    assert_eq!(
        (second.class, second.class_source.as_str()),
        (Class::Normal, "rules")
    );
    assert!(second.classified_at.is_some());

    let run = runner::list(&db, 1).await.unwrap().remove(0);
    assert_eq!(
        (run.agent, run.state),
        (Agent::CareerMemory, RunState::Completed)
    );
}

#[tokio::test]
async fn memory_refresh_with_no_ai_finishes() {
    let (_tmp, db) = open().await;
    let question = memory::add_question(&db, "Please certify this is true", None)
        .await
        .unwrap();
    assert_eq!(question.class, Class::Legal);
    let done = agents::memory_refresh(&db, &Router::with_providers(vec![]), "cli", token())
        .await
        .unwrap();
    assert!(done.classified.skipped);
    assert_eq!((done.classified.checked, done.embedded), (0, 0));
    let run = runner::list(&db, 1).await.unwrap().remove(0);
    assert_eq!(run.state, RunState::Completed);
    let stored = memory::get(&db, &question.id).await.unwrap().question;
    assert_eq!(
        (stored.class, stored.class_source.as_str()),
        (Class::Legal, "rules")
    );
    assert_eq!(stored.classified_at, None);
}

#[tokio::test]
async fn a_model_raises_and_never_lowers() {
    let (_tmp, db) = open().await;
    let pay = memory::add_question(&db, "What is your expected salary?", None)
        .await
        .unwrap();
    let plain = memory::add_question(&db, "Why this company?", None)
        .await
        .unwrap();
    assert_eq!((pay.class, plain.class), (Class::Sensitive, Class::Normal));
    let (provider, _) = script(
        ProviderId::Ollama,
        vec![class_reply("normal"), class_reply("legal")],
    );
    let done = classify_pending(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!((done.checked, done.raised), (2, 1));
    let pay = memory::get(&db, &pay.id).await.unwrap().question;
    assert_eq!(
        (pay.class, pay.class_source.as_str()),
        (Class::Sensitive, "rules")
    );
    let plain = memory::get(&db, &plain.id).await.unwrap().question;
    assert_eq!(
        (plain.class, plain.class_source.as_str()),
        (Class::Legal, "model")
    );
}

const KEY_ID: &str = "00112233aabbccdd";

/// An unapproved sealed answer under a sensitive question, under a key made of sevens, so
/// no test touches the keychain.
async fn sealed_answer(db: &Db) -> (String, String) {
    let question = memory::add_question(db, "What is your salary?", None)
        .await
        .unwrap();
    let key = DataKey::from_bytes(KEY_ID, [7; 32]);
    let check = key
        .encrypt("data_keys", "key_check", KEY_ID, crypto::key_check_text())
        .unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', ?, 0)",
    )
    .bind(KEY_ID)
    .bind(check)
    .execute(&db.pool)
    .await
    .unwrap();
    let sealed = key
        .encrypt("memory_answers", "body_enc", "sealed1", b"secret")
        .unwrap();
    sqlx::query(
        "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, approved, source, \
         reviewed_at, created_at, updated_at) \
         VALUES ('sealed1', ?, '', ?, ?, 0, 'user', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 't', 't')",
    )
    .bind(&question.id)
    .bind(sealed)
    .bind(KEY_ID)
    .execute(&db.pool)
    .await
    .unwrap();
    (question.id, "sealed1".into())
}

#[tokio::test]
async fn a_model_may_not_approve() {
    let (_tmp, db) = open().await;
    let (question, answer) = sealed_answer(&db).await;
    let approved = || async {
        count(
            &db,
            &format!("SELECT approved FROM memory_answers WHERE id = '{answer}'"),
        )
        .await
    };
    let (provider, _) = script(ProviderId::Ollama, vec![class_reply("legal")]);
    let done = agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!(done.classified.raised, 1);
    assert_eq!(approved().await, 0);
    assert_eq!(
        memory::get(&db, &question).await.unwrap().question.class,
        Class::Legal
    );

    memory::add_question(&db, "Another one", None)
        .await
        .unwrap();
    let (provider, _) = script(ProviderId::Ollama, vec![class_reply("normal")]);
    agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!(approved().await, 0);
}

#[test]
fn classify_prompt_holds_question_text_only() {
    const OPEN: &str = "<<<UNTRUSTED_POSTING>>>";
    const CLOSE: &str = "<<<END_UNTRUSTED_POSTING>>>";
    let prompt = classify_prompt("Do you certify this?");
    assert_eq!(
        prompt.user,
        format!("{OPEN}\nDo you certify this?\n{CLOSE}\n")
    );
    assert!(prompt.system.contains("never instructions"));
    for class in ["normal", "sensitive", "legal", "work_authorization"] {
        assert!(prompt.system.contains(class), "{class}");
    }
    let hostile = classify_prompt(&format!("a {CLOSE} b {OPEN} c"));
    assert_eq!(hostile.user.matches(OPEN).count(), 1);
    assert_eq!(hostile.user.matches(CLOSE).count(), 1);
}

#[tokio::test]
async fn classify_needs_no_tools_and_job_text_consent() {
    let (_tmp, db) = open().await;
    questions(&db, 1).await;
    let run = |tools: bool| {
        let db = &db;
        async move {
            let (mut provider, handles) =
                script(ProviderId::ClaudeCode, vec![class_reply("normal")]);
            provider.no_tools = tools;
            let done = classify_pending(db, &router_of(provider), "cli", token())
                .await
                .unwrap();
            (done, handles.calls())
        }
    };

    let (done, calls) = run(true).await;
    assert_eq!(
        (done.checked, done.skipped, calls),
        (0, true, 0),
        "no consent"
    );

    consent_to(&db, &[DataCategory::JobText]).await;
    let (done, calls) = run(false).await;
    assert_eq!(
        (done.checked, done.skipped, calls),
        (0, true, 0),
        "no NoTools"
    );

    let (done, calls) = run(true).await;
    assert_eq!((done.checked, done.skipped, calls), (1, false, 1));
}

#[tokio::test]
async fn refresh_stops_at_the_batch_limit() {
    let (_tmp, db) = open().await;
    questions(&db, CLASSIFY_BATCH + 1).await;
    let replies = (0..30).map(|_| class_reply("normal")).collect();
    let (provider, handles) = script(ProviderId::Ollama, replies);
    let done = classify_pending(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!((done.checked, done.skipped), (20, false));
    assert_eq!(handles.calls(), 20);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM memory_questions WHERE classified_at IS NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn refresh_makes_at_most_twenty_claude_calls() {
    let (_tmp, db) = open().await;
    consent_to(&db, &[DataCategory::JobText]).await;
    questions(&db, 25).await;

    let good = (0..30).map(|_| class_reply("normal")).collect();
    let (provider, handles) = script(ProviderId::ClaudeCode, good);
    let done = agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!(done.classified.checked, 20);
    assert_eq!(handles.calls(), 20);

    let (_tmp, db) = open().await;
    consent_to(&db, &[DataCategory::JobText]).await;
    questions(&db, 25).await;
    let bad = (0..30).map(|_| "not json".to_owned()).collect();
    let (provider, handles) = script(ProviderId::ClaudeCode, bad);
    let done = agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!(
        (done.classified.checked, done.classified.skipped),
        (0, true)
    );
    assert_eq!(handles.calls(), 1, "one attempt, no retry");
}

#[tokio::test]
async fn refresh_plan_counts_claude_calls_and_calls_no_provider() {
    let (_tmp, db) = open().await;
    questions(&db, 25).await;
    let (provider, handles) = script(ProviderId::ClaudeCode, vec![]);
    let router = router_of(provider);

    let none = refresh_plan(&db, &router).await.unwrap();
    assert_eq!((none.pending, none.claude_calls), (20, 0), "no consent");
    consent_to(&db, &[DataCategory::JobText]).await;
    let plan = refresh_plan(&db, &router).await.unwrap();
    assert_eq!((plan.pending, plan.claude_calls), (20, 20));

    consent_to(&db, &[DataCategory::ResumeText]).await;
    assert_eq!(refresh_plan(&db, &router).await.unwrap().claude_calls, 0);

    let (local, _) = script(ProviderId::Ollama, vec![]);
    consent_to(&db, &[DataCategory::JobText]).await;
    let plan = refresh_plan(&db, &router_of(local)).await.unwrap();
    assert_eq!(
        (plan.pending, plan.claude_calls),
        (20, 0),
        "no Claude in the order"
    );
    assert_eq!(
        handles.calls() + handles.embed_calls.load(Ordering::SeqCst),
        0
    );
}

/// A halt the first time classify is asked, before it does anything: a refresh stopped at
/// the start of its first step.
struct HaltBeforeClassify<'r> {
    inner: MemoryWorkflow<'r>,
    tripped: AtomicBool,
}

impl Workflow for HaltBeforeClassify<'_> {
    fn agent(&self) -> Agent {
        self.inner.agent()
    }

    fn input(&self) -> RunInput {
        self.inner.input()
    }

    fn steps(&self) -> &'static [StepDef] {
        self.inner.steps()
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            if index == 0 && !self.tripped.swap(true, Ordering::SeqCst) {
                return Err(Error::Paused);
            }
            self.inner.step(ctx, index).await
        })
    }
}

#[tokio::test]
async fn resuming_a_refresh_makes_no_claude_call() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    questions(&db, 2).await;
    let (mut provider, handles) = script(
        ProviderId::Ollama,
        vec![class_reply("normal"), class_reply("normal")],
    );
    provider.embeds = true;
    let router = router_of(provider);
    let workflow = HaltBeforeClassify {
        inner: MemoryWorkflow::new(&router, false),
        tripped: AtomicBool::new(false),
    };
    let halted = runner::start(&db, "cli", &workflow, token()).await.unwrap();
    assert_eq!(halted.state, RunState::Halted);
    assert_eq!(handles.calls(), 0);
    assert_eq!(handles.embed_calls.load(Ordering::SeqCst), 0);

    // Two questions wait and replies are ready: only the rebuilt workflow's own rule keeps
    // the resume from spending them.
    let done = agents::resume_run(&db, &router, "cli", &halted.run_id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(handles.calls(), 0, "a resume makes no model call");
    assert!(
        handles.embed_calls.load(Ordering::SeqCst) > 0,
        "embed still ran"
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        2
    );
}

#[tokio::test]
async fn adding_a_question_starts_nothing() {
    let (_tmp, db) = open().await;
    let (provider, handles) = script(ProviderId::Ollama, vec![class_reply("legal")]);
    let _router = router_of(provider);
    memory::add_question(&db, "Why this company?", None)
        .await
        .unwrap();
    assert!(runner::list(&db, 10).await.unwrap().is_empty());
    assert_eq!(count(&db, "SELECT count(*) FROM agent_runs").await, 0);
    assert_eq!(handles.calls(), 0);
}

/// A step shares its overseer's task, so a render that never yields would stall the timeout
/// and the heartbeat. A ticker on the same single thread must keep running through an export.
#[tokio::test]
async fn an_export_does_not_stall_the_overseer() {
    use jobpilot_core::drafts;
    use jobpilot_core::render::{Entry, ResumeDoc, Section};
    use std::time::Instant;

    let (_tmp, db) = open().await;
    let entries = (0..300)
        .map(|i| Entry {
            title: format!("Role {i}"),
            subtitle: "Company".into(),
            dates: "2020 - 2021".into(),
            bullets: (0..8)
                .map(|b| format!("Did thing {i}.{b} in depth"))
                .collect(),
        })
        .collect();
    let doc = ResumeDoc {
        name: "Ada".into(),
        sections: vec![Section {
            heading: "Experience".into(),
            entries,
        }],
        ..ResumeDoc::default()
    };
    let draft = drafts::save_manual(&db, "resume", None, doc).await.unwrap();

    let gaps = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (gaps, stop) = (gaps.clone(), stop.clone());
        tokio::spawn(async move {
            let mut last = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
                gaps.lock().unwrap().push(last.elapsed());
                last = Instant::now();
            }
        })
    };
    // Start only once the ticker is running.
    let clock = Instant::now();
    while gaps.lock().unwrap().len() < 5 {
        assert!(
            clock.elapsed() < Duration::from_secs(5),
            "the ticker never ran"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let started = Instant::now();
    drafts::export(
        &db,
        &draft.id,
        Template::Classic,
        ExportFormat::Pdf,
        &db.paths().exports(),
        "cli",
    )
    .await
    .unwrap();
    let took = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    ticker.await.unwrap();

    let worst = gaps.lock().unwrap().iter().max().copied().unwrap();
    eprintln!("export took {took:?}, worst tick gap {worst:?}");
    assert!(
        took > Duration::from_millis(60),
        "too quick to prove anything: {took:?}"
    );
    assert!(
        worst < took / 3,
        "the export held the thread: {worst:?} of {took:?}"
    );
}

// ---- fix round 1 ------------------------------------------------------------------------

async fn wait_for(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let clock = std::time::Instant::now();
    while !done().await {
        assert!(clock.elapsed() < Duration::from_secs(5), "never: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_persons_class_is_final() {
    let (_tmp, db) = open().await;
    let lowered = memory::add_question(&db, "Please certify this is true", None)
        .await
        .unwrap();
    assert_eq!(lowered.class, Class::Legal);
    let answer = memory::add_answer(&db, &lowered.id, "Yes", None)
        .await
        .unwrap();
    memory::set_class(&db, "cli", &lowered.id, Class::Normal)
        .await
        .unwrap();
    let answer = memory::get(&db, &lowered.id)
        .await
        .unwrap()
        .answers
        .into_iter()
        .find(|a| a.id == answer.id)
        .unwrap();
    memory::approve(&db, "cli", &answer.id, true).await.unwrap();
    let raised = memory::add_question(&db, "Why this company?", None)
        .await
        .unwrap();
    memory::set_class(&db, "cli", &raised.id, Class::Sensitive)
        .await
        .unwrap();

    let plan_router = router_of(script(ProviderId::Ollama, vec![]).0);
    assert_eq!(refresh_plan(&db, &plan_router).await.unwrap().pending, 0);
    let (provider, handles) = script(
        ProviderId::Ollama,
        vec![class_reply("legal"), class_reply("legal")],
    );
    let done = classify_pending(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!((done.checked, done.raised, done.skipped), (0, 0, false));
    assert_eq!(handles.calls(), 0);
    let kept = memory::get(&db, &lowered.id).await.unwrap();
    assert_eq!(kept.question.class, Class::Normal);
    assert!(kept.answers[0].approved, "their approval stands");
    assert_eq!(
        memory::get(&db, &raised.id).await.unwrap().question.class,
        Class::Sensitive
    );
}

#[tokio::test]
async fn work_authorization_questions_are_never_sent() {
    let (_tmp, db) = open().await;
    let q = memory::add_question(&db, "Are you authorized to work in the US?", Some("US"))
        .await
        .unwrap();
    assert_eq!(q.class, Class::WorkAuthorization);
    let (provider, handles) = script(ProviderId::Ollama, vec![class_reply("normal")]);
    let done = classify_pending(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert_eq!((done.checked, done.skipped), (0, false));
    assert_eq!(handles.calls(), 0);
}

#[tokio::test]
async fn a_cancel_is_an_error_not_a_skip() {
    let (_tmp, db) = open().await;
    questions(&db, 2).await;
    // Already fired: nothing is asked, and the router is not even entered.
    let fired = token();
    fired.cancel();
    let (provider, handles) = script(ProviderId::Ollama, vec![class_reply("normal")]);
    let error = classify_pending(&db, &router_of(provider), "cli", fired)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cancelled");
    assert_eq!(handles.calls(), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM task_runs").await, 0);

    // Fired while the model works.
    let cancel = token();
    let (mut provider, _) = script(ProviderId::Ollama, vec![class_reply("normal")]);
    provider.cancel_in_call = Some(cancel.clone());
    let error = classify_pending(&db, &router_of(provider), "cli", cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cancelled");
}

/// The row of the classify step in a run's log.
async fn classify_row(db: &Db) -> runner::LogRow {
    let run = runner::list(db, 1).await.unwrap().remove(0);
    let detail = runner::get(db, &run.id).await.unwrap();
    detail
        .log
        .into_iter()
        .find(|row| row.step == "classify" && row.state == RunState::Checkpointed)
        .expect("classify was checkpointed")
}

#[tokio::test]
async fn the_log_names_the_claude_call_that_succeeded() {
    let (_tmp, db) = open().await;
    consent_to(&db, &[DataCategory::JobText]).await;
    questions(&db, 1).await;
    let (provider, _) = script(ProviderId::ClaudeCode, vec![class_reply("normal")]);
    agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    let row = classify_row(&db).await;
    assert_eq!(row.provider.as_deref(), Some(CLAUDE));
    assert_eq!(row.data_categories, [DataCategory::JobText]);
}

#[tokio::test]
async fn the_log_names_a_claude_call_that_came_back_as_bad_json() {
    let (_tmp, db) = open().await;
    consent_to(&db, &[DataCategory::JobText]).await;
    questions(&db, 1).await;
    let (provider, handles) = script(ProviderId::ClaudeCode, vec!["not json".into()]);
    let done = agents::memory_refresh(&db, &router_of(provider), "cli", token())
        .await
        .unwrap();
    assert!(done.classified.skipped);
    assert_eq!(handles.calls(), 1);
    let row = classify_row(&db).await;
    assert_eq!(row.provider.as_deref(), Some(CLAUDE), "the text did leave");
    assert_eq!(row.data_categories, [DataCategory::JobText]);
}

#[tokio::test]
async fn the_log_names_no_provider_when_none_was_asked() {
    let (_tmp, db) = open().await;
    questions(&db, 1).await;
    agents::memory_refresh(&db, &Router::with_providers(vec![]), "cli", token())
        .await
        .unwrap();
    let row = classify_row(&db).await;
    assert_eq!(row.provider, None);
    assert!(row.data_categories.is_empty());
}

/// A plain answer under a normal question, and a key made of sevens.
async fn plain_answer(db: &Db) -> (String, String) {
    let key = DataKey::from_bytes(KEY_ID, [7; 32]);
    let check = key
        .encrypt("data_keys", "key_check", KEY_ID, crypto::key_check_text())
        .unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', ?, 0)",
    )
    .bind(KEY_ID)
    .bind(check)
    .execute(&db.pool)
    .await
    .unwrap();
    let question = memory::add_question(db, "Why this company?", None)
        .await
        .unwrap();
    let answer = memory::add_answer(db, &question.id, "Because", None)
        .await
        .unwrap();
    (question.id, answer.id)
}

#[tokio::test]
async fn a_raise_seals_in_the_same_run() {
    let (_tmp, db) = open().await;
    let (_, answer) = plain_answer(&db).await;
    let (provider, _) = script(ProviderId::Ollama, vec![class_reply("sensitive")]);
    let router = router_of(provider);
    let workflow =
        MemoryWorkflow::new(&router, false).with_key(DataKey::from_bytes(KEY_ID, [7; 32]));
    let done = runner::start(&db, "cli", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(workflow.done().sealed, 1);
    let (body, enc): (String, Option<Vec<u8>>) =
        sqlx::query_as("SELECT body, body_enc FROM memory_answers WHERE id = ?")
            .bind(&answer)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(body, "");
    assert!(enc.is_some());
}

#[test]
fn a_seal_key_error_is_noted_and_nothing_else_is() {
    use jobpilot_core::agents::memory::sealed;
    for error in [
        Error::Locked,
        Error::KeyUnavailable("x".into()),
        Error::KeyMissing { key_id: "k".into() },
        Error::KeyMismatch { key_id: "k".into() },
        Error::Secret("x".into()),
    ] {
        let code = error.code();
        assert_eq!(sealed(Err(error)).unwrap(), (0, Some(code.to_owned())));
    }
    assert_eq!(sealed(Ok(3)).unwrap(), (3, None));
    assert!(sealed(Err(Error::Validation("x".into()))).is_err());
}

fn letter_reply(text: &str, ids: &[&str]) -> String {
    json!({ "paragraphs": [{ "text": text, "evidence_ids": ids }] }).to_string()
}

#[tokio::test]
async fn a_cover_letter_is_drafted_by_its_own_agent() {
    let w = world().await;
    let (provider, _) = script(
        ProviderId::Ollama,
        vec![letter_reply(BULLET, &[&w.item.id])],
    );
    let made = agents::draft_cover_letter(
        &w.db,
        &router_of(provider),
        "cli",
        &w.job.id,
        pdf(),
        token(),
    )
    .await
    .unwrap();
    assert_eq!(made.draft.kind, "cover_letter");
    assert!(made.exported.is_some());
    let run = runner::list(&w.db, 1).await.unwrap().remove(0);
    assert_eq!(
        (run.agent, run.state),
        (Agent::CoverLetter, RunState::Completed)
    );
    let detail = runner::get(&w.db, &run.id).await.unwrap();
    assert!(detail.log.iter().any(|r| r.provider.is_some()));
}

#[tokio::test]
async fn a_halted_letter_resumes_as_a_letter() {
    let w = world().await;
    let (provider, handles) = script(
        ProviderId::Ollama,
        vec![letter_reply(BULLET, &[&w.item.id])],
    );
    let router = router_of(provider);
    let workflow = HaltAtRender {
        inner: DraftWorkflow::new(TaskType::DraftCoverLetter, &w.job.id, pdf(), &router),
        tripped: AtomicBool::new(false),
    };
    let halted = runner::start(&w.db, "cli", &workflow, token())
        .await
        .unwrap();
    assert_eq!(halted.state, RunState::Halted);
    let done = agents::resume_run(&w.db, &router, "cli", &halted.run_id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(handles.calls(), 1);
    assert_eq!(
        count(
            &w.db,
            "SELECT count(*) FROM drafts WHERE kind = 'cover_letter'"
        )
        .await,
        1
    );
    assert_eq!(
        std::fs::read_dir(w.db.paths().exports()).unwrap().count(),
        1
    );
}

/// A step run under an agent that does not hold its needs, with the step table narrowed so the
/// runner lets it in: the step's own check is what stops it.
struct Narrowed<W> {
    inner: W,
    agent: Agent,
}

static NARROW: &[StepDef] = &[
    StepDef {
        name: "a",
        needs: &[Permission::ReadApplications],
        attempts: 1,
    },
    StepDef {
        name: "b",
        needs: &[Permission::ReadApplications],
        attempts: 1,
    },
    StepDef {
        name: "c",
        needs: &[Permission::ReadApplications],
        attempts: 1,
    },
];

impl<W: Workflow> Workflow for Narrowed<W> {
    fn agent(&self) -> Agent {
        self.agent
    }

    fn input(&self) -> RunInput {
        self.inner.input()
    }

    fn steps(&self) -> &'static [StepDef] {
        &NARROW[..self.inner.steps().len()]
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        self.inner.step(ctx, index)
    }
}

#[tokio::test]
async fn the_draft_and_memory_steps_check_their_own_permissions() {
    let w = world().await;
    let (provider, handles) = script(
        ProviderId::Ollama,
        vec![resume_reply(BULLET, &[&w.item.id])],
    );
    let router = router_of(provider);
    let draft = Narrowed {
        inner: DraftWorkflow::new(TaskType::DraftResume, &w.job.id, None, &router),
        agent: Agent::Analytics,
    };
    let done = runner::start(&w.db, "cli", &draft, token()).await.unwrap();
    assert_eq!(done.state, RunState::Refused);
    assert_eq!(done.reason.as_deref(), Some("permission_denied"));
    assert_eq!(handles.calls(), 0);
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 0);

    questions(&w.db, 1).await;
    let (provider, handles) = script(ProviderId::Ollama, vec![class_reply("legal")]);
    let router = router_of(provider);
    let memory = Narrowed {
        inner: MemoryWorkflow::new(&router, false),
        agent: Agent::Analytics,
    };
    let done = runner::start(&w.db, "cli", &memory, token()).await.unwrap();
    assert_eq!(done.state, RunState::Refused);
    assert_eq!(handles.calls(), 0);
}

#[tokio::test]
async fn a_failed_checkpoint_write_does_not_draft_twice() {
    let w = world().await;
    let reply = resume_reply(BULLET, &[&w.item.id]);
    let (provider, handles) = script(ProviderId::Ollama, vec![reply.clone(), reply]);
    let router = router_of(provider);
    sqlx::query(
        "CREATE TRIGGER stop_checkpoint BEFORE UPDATE ON agent_runs \
         WHEN NEW.state = 'checkpointed' BEGIN SELECT RAISE(ABORT, 'no'); END",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    let error = agents::draft_resume(&w.db, &router, "cli", &w.job.id, pdf(), token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "db");
    assert_eq!(handles.calls(), 1);
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 1);

    sqlx::query("DROP TRIGGER stop_checkpoint")
        .execute(&w.db.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_runs SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-60 seconds')",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(runner::reap_stale(&w.db).await.unwrap(), 1);
    let run = runner::list(&w.db, 1).await.unwrap().remove(0);
    assert_eq!(run.state, RunState::Halted);
    let done = agents::resume_run(&w.db, &router, "cli", &run.id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(handles.calls(), 1, "no second model call");
    assert_eq!(count(&w.db, "SELECT count(*) FROM drafts").await, 1);
    assert_eq!(
        std::fs::read_dir(w.db.paths().exports()).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn a_name_that_states_authorization_is_stored_and_the_export_is_skipped_aloud() {
    let w = world().await;
    let (provider, _) = script(
        ProviderId::Ollama,
        vec![resume_reply(BULLET, &[&w.item.id])],
    );
    let router = router_of(provider);
    let workflow = HaltAtRender {
        inner: DraftWorkflow::new(TaskType::DraftResume, &w.job.id, pdf(), &router),
        tripped: AtomicBool::new(false),
    };
    let halted = runner::start(&w.db, "cli", &workflow, token())
        .await
        .unwrap();
    // Edited under the run, as the name of a stored draft can be: no claim is blocked, and
    // the export still refuses.
    sqlx::query(
        "UPDATE drafts SET body = json_set(body, '$.name', 'Authorized to work in the US')",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    let again = DraftWorkflow::new(TaskType::DraftResume, &w.job.id, pdf(), &router);
    let done = runner::resume(&w.db, "cli", &halted.run_id, &again, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    let made = again.into_generated(&w.db, &done.saved).await.unwrap();
    assert_eq!(made.blocked, 0);
    assert_eq!(made.exported, None);
    assert_eq!(made.export_skipped.as_deref(), Some("claims_blocked"));
    assert!(
        !w.db.paths().exports().exists()
            || std::fs::read_dir(w.db.paths().exports()).unwrap().count() == 0
    );

    // And a clean draft says nothing.
    let w = world().await;
    let (provider, _) = script(
        ProviderId::Ollama,
        vec![resume_reply(BULLET, &[&w.item.id])],
    );
    let made = agents::draft_resume(
        &w.db,
        &router_of(provider),
        "cli",
        &w.job.id,
        pdf(),
        token(),
    )
    .await
    .unwrap();
    assert_eq!(made.export_skipped, None);
}

/// The run's id is part of the insert, not a second statement after it.
#[tokio::test]
async fn a_version_is_written_with_its_run_id_in_one_statement() {
    let w = world().await;
    sqlx::query(
        "CREATE TRIGGER no_update BEFORE UPDATE ON drafts BEGIN SELECT RAISE(ABORT, 'no'); END",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    let (provider, _) = script(
        ProviderId::Ollama,
        vec![resume_reply(BULLET, &[&w.item.id])],
    );
    agents::draft_resume(&w.db, &router_of(provider), "cli", &w.job.id, None, token())
        .await
        .unwrap();
    let run = runner::list(&w.db, 1).await.unwrap().remove(0);
    let (stored,): (Option<String>,) = sqlx::query_as("SELECT run_id FROM drafts")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some(run.id.as_str()));
}

#[tokio::test]
async fn a_blocked_draft_says_why_it_was_not_exported() {
    let w = world().await;
    let (provider, _) = script(ProviderId::Ollama, vec![resume_reply(BULLET, &[])]);
    let made = agents::draft_resume(
        &w.db,
        &router_of(provider),
        "cli",
        &w.job.id,
        pdf(),
        token(),
    )
    .await
    .unwrap();
    assert_eq!(made.export_skipped.as_deref(), Some("claims_blocked"));
}

/// A cancel that drops the export past its grace leaves the blocking job running: the file
/// still lands, so it still gets its audit row. The runtime has one blocking thread and the
/// test holds it, so the render is queued, and the export is dropped while it waits: no clock
/// decides whether the export was cut short.
#[test]
fn a_file_that_lands_after_a_dropped_export_is_audited() {
    use jobpilot_core::drafts;
    use jobpilot_core::render::{Entry, ResumeDoc, Section};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_tmp, db) = open().await;
        let doc = ResumeDoc {
            name: "Ada".into(),
            sections: vec![Section {
                heading: "Experience".into(),
                entries: vec![Entry {
                    title: "Role".into(),
                    bullets: vec!["Did a thing".into()],
                    ..Entry::default()
                }],
            }],
            ..ResumeDoc::default()
        };
        let draft = drafts::save_manual(&db, "resume", None, doc).await.unwrap();

        let (release, gate) = std::sync::mpsc::channel::<()>();
        let (held, holding) = tokio::sync::oneshot::channel::<()>();
        let hold = tokio::task::spawn_blocking(move || {
            held.send(()).ok();
            gate.recv().ok();
        });
        holding.await.unwrap();

        let exports = db.paths().exports();
        let (started, render) = tokio::sync::oneshot::channel();
        let export = drafts::export_signalling(
            &db,
            &draft.id,
            Template::Classic,
            ExportFormat::Pdf,
            &exports,
            "cli",
            Some(started),
        );
        // Dropped the moment the render is under way, queued behind the held thread.
        tokio::select! {
            _ = export => panic!("the export cannot finish while the thread is held"),
            sent = render => sent.unwrap(),
        }

        release.send(()).unwrap();
        hold.await.unwrap();
        wait_for("the file lands", async || {
            std::fs::read_dir(db.paths().exports()).is_ok_and(|mut d| {
                d.any(|f| f.is_ok_and(|f| f.path().extension().is_some_and(|e| e == "pdf")))
            })
        })
        .await;
        wait_for("its audit row", async || {
            count(
                &db,
                "SELECT count(*) FROM audit_log WHERE action = 'export'",
            )
            .await
                == 1
        })
        .await;
    });
}
