//! Emergency pause. Every provider and workflow here is a mock: no model, no network, no
//! keychain.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use jobpilot_core::agents;
use jobpilot_core::ai::ollama::{DEFAULT_URL, Ollama};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Outcome, Prompt, Provider,
    ProviderId, TaskType, spec,
};
use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::memory;
use jobpilot_core::runner::{
    self, Agent, Ctx, Permission, RunInput, RunState, StepDef, StepOutput, Workflow,
};
use jobpilot_core::{AppPaths, Db, Error, Result, audit, pause, settings};
use serde_json::json;
use tempfile::TempDir;

const READ: &[Permission] = &[Permission::ReadApplications];
static ONE: &[StepDef] = &[StepDef {
    name: "only",
    needs: READ,
    attempts: 1,
}];

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

async fn second_handle(tmp: &TempDir) -> Db {
    Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap()
}

async fn count(db: &Db, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn actions(db: &Db, action: &str) -> Vec<String> {
    let rows = audit::recent(db, 200).await.unwrap();
    rows.into_iter()
        .filter(|row| row.action == action)
        .map(|row| row.actor)
        .collect()
}

/// A step that waits for its token, as a model call does.
struct Blocker {
    saw_cancel: Arc<AtomicBool>,
}

impl Blocker {
    fn new() -> Blocker {
        Blocker {
            saw_cancel: Arc::default(),
        }
    }
}

impl Workflow for Blocker {
    fn agent(&self) -> Agent {
        Agent::Tracking
    }

    fn input(&self) -> RunInput {
        RunInput::Analytics
    }

    fn steps(&self) -> &'static [StepDef] {
        ONE
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, _index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            while !ctx.cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            self.saw_cancel.store(true, Ordering::SeqCst);
            Err(Error::Cancelled)
        })
    }
}

/// A step that finishes at once.
struct Instant1;

impl Workflow for Instant1 {
    fn agent(&self) -> Agent {
        Agent::Tracking
    }

    fn input(&self) -> RunInput {
        RunInput::Analytics
    }

    fn steps(&self) -> &'static [StepDef] {
        ONE
    }

    fn step<'a>(&'a self, _ctx: &'a Ctx<'a>, _index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async { Ok(StepOutput::default()) })
    }
}

async fn wait_for(db: &Db, sql: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while count(db, sql).await == 0 {
        assert!(Instant::now() < deadline, "never reached: {sql}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Starts a blocked run on `db`, pauses from `other`, and returns the halted run's id.
async fn halted_by_pause(db: &Arc<Db>, other: &Db) -> String {
    let held = db.clone();
    let task = tokio::spawn(async move {
        let workflow = Blocker::new();
        runner::start(&held, "cli", &workflow, CancellationToken::new()).await
    });
    wait_for(
        db,
        "SELECT count(*) FROM agent_runs WHERE state = 'running'",
    )
    .await;
    pause::set(other, "cli").await.unwrap();
    let done = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(done.state, RunState::Halted);
    done.run_id
}

#[tokio::test]
async fn set_and_clear_each_audit_once() {
    let (_tmp, db) = setup().await;
    assert!(!pause::state(&db).await.unwrap().paused);

    let on = pause::set(&db, "cli").await.unwrap();
    assert!(on.paused);
    assert_eq!(on.set_by.as_deref(), Some("cli"));
    assert!(on.set_at.is_some());
    pause::set(&db, "gui").await.unwrap();
    assert_eq!(actions(&db, "emergency_pause_set").await, ["cli"]);
    assert_eq!(
        pause::state(&db).await.unwrap().set_by.as_deref(),
        Some("cli"),
        "a second set keeps the first row"
    );

    let off = pause::clear(&db, "gui").await.unwrap();
    assert!(!off.paused && off.set_at.is_none() && off.set_by.is_none());
    pause::clear(&db, "gui").await.unwrap();
    assert_eq!(actions(&db, "emergency_pause_cleared").await, ["gui"]);
}

#[tokio::test]
async fn the_pause_survives_a_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    pause::set(&db, "cli").await.unwrap();
    db.close().await;

    let db = Db::open(&paths).await.unwrap();
    assert!(pause::is_set(&db).await.unwrap());
    assert_eq!(pause::ensure_off(&db).await.unwrap_err().code(), "paused");
    db.close().await;
}

#[tokio::test]
async fn system_cannot_set_or_clear() {
    let (_tmp, db) = setup().await;
    for actor in ["system", "", "root"] {
        let error = pause::set(&db, actor).await.unwrap_err();
        assert!(matches!(&error, Error::Refused(why) if why == "only_a_person"));
    }
    pause::set(&db, "cli").await.unwrap();
    let error = pause::clear(&db, "system").await.unwrap_err();
    assert!(matches!(&error, Error::Refused(why) if why == "only_a_person"));
    assert!(pause::is_set(&db).await.unwrap());
}

#[tokio::test]
async fn pause_halts_a_run_and_cancels_the_in_flight_task() {
    let (tmp, db) = setup().await;
    let db = Arc::new(db);
    let other = second_handle(&tmp).await;
    let workflow = Arc::new(Blocker::new());
    let (held, step) = (db.clone(), workflow.clone());
    let task =
        tokio::spawn(
            async move { runner::start(&held, "cli", &*step, CancellationToken::new()).await },
        );
    wait_for(
        &db,
        "SELECT count(*) FROM agent_runs WHERE state = 'running'",
    )
    .await;

    let began = Instant::now();
    pause::set(&other, "cli").await.unwrap();
    let done = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("the run ended within a second")
        .unwrap()
        .unwrap();
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(done.state, RunState::Halted);
    assert_eq!(done.reason.as_deref(), Some("paused"));
    assert!(
        workflow.saw_cancel.load(Ordering::SeqCst),
        "the token fired"
    );
    assert_eq!(done.into_result().unwrap_err().code(), "paused");
    let stored = runner::list(&db, 5).await.unwrap();
    assert_eq!(stored[0].state, RunState::Halted);
    assert_eq!(stored[0].reason.as_deref(), Some("paused"));
}

#[tokio::test]
async fn a_step_that_finished_before_the_pause_checkpoints_first() {
    let (_tmp, db) = setup().await;
    struct SetsThenFinishes<'a>(&'a Db);
    impl Workflow for SetsThenFinishes<'_> {
        fn agent(&self) -> Agent {
            Agent::Tracking
        }
        fn input(&self) -> RunInput {
            RunInput::Analytics
        }
        fn steps(&self) -> &'static [StepDef] {
            ONE
        }
        fn step<'a>(
            &'a self,
            _ctx: &'a Ctx<'a>,
            _index: usize,
        ) -> BoxFuture<'a, Result<StepOutput>> {
            Box::pin(async move {
                pause::set(self.0, "cli").await?;
                Ok(StepOutput::default())
            })
        }
    }
    let done = runner::start(&db, "cli", &SetsThenFinishes(&db), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        done.state,
        RunState::Completed,
        "the only step had finished"
    );
}

#[tokio::test]
async fn a_paused_flag_between_steps_halts_before_the_next() {
    static TWO: &[StepDef] = &[
        StepDef {
            name: "first",
            needs: READ,
            attempts: 1,
        },
        StepDef {
            name: "second",
            needs: READ,
            attempts: 1,
        },
    ];
    struct Two<'a>(&'a Db, Arc<AtomicU32>);
    impl Workflow for Two<'_> {
        fn agent(&self) -> Agent {
            Agent::Tracking
        }
        fn input(&self) -> RunInput {
            RunInput::Analytics
        }
        fn steps(&self) -> &'static [StepDef] {
            TWO
        }
        fn step<'a>(
            &'a self,
            _ctx: &'a Ctx<'a>,
            index: usize,
        ) -> BoxFuture<'a, Result<StepOutput>> {
            Box::pin(async move {
                self.1.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    pause::set(self.0, "cli").await?;
                }
                Ok(StepOutput::default())
            })
        }
    }
    let (_tmp, db) = setup().await;
    let ran = Arc::new(AtomicU32::new(0));
    let done = runner::start(&db, "cli", &Two(&db, ran.clone()), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Halted);
    assert_eq!(done.reason.as_deref(), Some("paused"));
    assert_eq!(ran.load(Ordering::SeqCst), 1, "the second step never began");
}

#[tokio::test]
async fn no_run_starts_while_paused() {
    let (_tmp, db) = setup().await;
    pause::set(&db, "cli").await.unwrap();
    let error = runner::start(&db, "cli", &Instant1, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "paused");
    assert_eq!(count(&db, "SELECT count(*) FROM agent_runs").await, 0);
}

struct Mock {
    calls: Arc<AtomicU32>,
    /// Sets the pause during the first call, then fails with an error that retries.
    pause_in_first_call: Option<Arc<Db>>,
}

impl Mock {
    fn new() -> (Mock, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let mock = Mock {
            calls: calls.clone(),
            pause_in_first_call: None,
        };
        (mock, calls)
    }
}

impl Provider for Mock {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::Ollama,
            capabilities: vec![
                Capability::StructuredOutput,
                Capability::NoTools,
                Capability::Embeddings,
                Capability::LocalExecution,
            ],
            local: true,
            context_limit: 8192,
            timeout: Duration::from_secs(5),
            sign_in: "none".into(),
        }
    }

    fn available(&self) -> Pin<Box<dyn Future<Output = Availability> + Send + '_>> {
        Box::pin(async { Availability::Available })
    }

    fn complete<'a>(
        &'a self,
        _prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if let (1, Some(db)) = (call, &self.pause_in_first_call) {
                pause::set(db, "cli").await?;
            }
            Err(Error::Io(std::io::Error::other("broken pipe")))
        })
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(inputs.iter().map(|_| vec![1.0, 0.0]).collect()) })
    }
}

fn prompt() -> Prompt {
    Prompt {
        system: "rules".into(),
        user: "text".into(),
        schema: spec(TaskType::ParseJob).schema,
    }
}

#[tokio::test]
async fn the_router_refuses_while_paused() {
    let (_tmp, db) = setup().await;
    let (mock, calls) = Mock::new();
    let router = Router::with_providers(vec![Box::new(mock)]);
    pause::set(&db, "cli").await.unwrap();
    let error = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "paused");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM task_runs").await, 0);
}

#[tokio::test]
async fn pause_stops_between_router_attempts() {
    let (_tmp, db) = setup().await;
    let db = Arc::new(db);
    let (mut mock, calls) = Mock::new();
    mock.pause_in_first_call = Some(db.clone());
    let router = Router::with_providers(vec![Box::new(mock)]);
    // ParseJob allows two attempts, and an io error would retry.
    let outcome = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no second attempt");
}

#[tokio::test]
async fn embed_while_paused_is_manual_so_text_search_still_works() {
    let (_tmp, db) = setup().await;
    settings::set(&db, "ai.embedding_model", &json!("test-embed"))
        .await
        .unwrap();
    let question = memory::add_question(&db, "Why do you want this job?", None)
        .await
        .unwrap();
    let answer = memory::add_answer(&db, &question.id, "Because.", None)
        .await
        .unwrap();
    memory::approve(&db, "cli", &answer.id, true).await.unwrap();
    let (mock, calls) = Mock::new();
    let router = Router::with_providers(vec![Box::new(mock)]);
    pause::set(&db, "cli").await.unwrap();

    let error = router
        .embed(&db, &["why".to_string()], CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::ManualRequired(why) if why == "paused"));
    let hits = memory::search(&db, &router, "job", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].via, "text");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the provider was never asked"
    );
}

#[tokio::test]
async fn a_pull_is_refused_while_paused() {
    let (_tmp, db) = setup().await;
    pause::set(&db, "cli").await.unwrap();
    let ollama = Ollama::new(DEFAULT_URL, None, None).unwrap();
    let error = ollama
        .pull(&db, "cli", "llama3", |_, _| {}, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "paused");
    assert!(
        actions(&db, "model_download_started").await.is_empty(),
        "nothing was audited as started"
    );
}

#[tokio::test]
async fn pause_reaps_dead_runs() {
    let (_tmp, db) = setup().await;
    sqlx::query(
        "INSERT INTO agent_runs (id, agent, state, actor, input, step, steps, heartbeat_at, \
         created_at) VALUES (?, 'tracking', 'running', 'gui', '{\"kind\":\"analytics\"}', 0, 1, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-60 seconds'), \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .bind("aa".repeat(16))
    .execute(&db.pool)
    .await
    .unwrap();
    pause::set(&db, "cli").await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM agent_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(state, "halted");
}

#[tokio::test]
async fn halted_runs_do_not_resume_on_clear() {
    let (tmp, db) = setup().await;
    let db = Arc::new(db);
    let other = second_handle(&tmp).await;
    let id = halted_by_pause(&db, &other).await;
    pause::clear(&db, "cli").await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let run = runner::get(&db, &id).await.unwrap().run;
    assert_eq!(run.state, RunState::Halted);
    assert_eq!(run.reason.as_deref(), Some("paused"));
}

#[tokio::test]
async fn resume_is_refused_until_cleared_then_passes_checking() {
    let (tmp, db) = setup().await;
    let db = Arc::new(db);
    let other = second_handle(&tmp).await;
    let id = halted_by_pause(&db, &other).await;
    let checking = |detail: &runner::RunDetail| {
        detail
            .log
            .iter()
            .filter(|row| row.state == RunState::Checking)
            .count()
    };
    let before = checking(&runner::get(&db, &id).await.unwrap());

    let error = runner::resume(&db, "cli", &id, &Instant1, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "paused");
    assert_eq!(
        runner::get(&db, &id).await.unwrap().run.state,
        RunState::Halted
    );

    pause::clear(&db, "cli").await.unwrap();
    let done = runner::resume(&db, "cli", &id, &Instant1, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(checking(&runner::get(&db, &id).await.unwrap()), before + 1);
}

#[tokio::test]
async fn cancel_works_while_paused() {
    let (tmp, db) = setup().await;
    let db = Arc::new(db);
    let other = second_handle(&tmp).await;
    let id = halted_by_pause(&db, &other).await;
    assert!(pause::is_set(&db).await.unwrap());
    agents::cancel_run(&db, "gui", &id).await.unwrap();
    assert_eq!(
        runner::get(&db, &id).await.unwrap().run.state,
        RunState::Cancelled
    );
}

async fn an_application(db: &Db) -> String {
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        ..ApplicationInput::default()
    };
    applications::add(db, "cli", input).await.unwrap().id
}

#[tokio::test]
async fn status_edits_say_paused_and_field_edits_still_work() {
    let (_tmp, db) = setup().await;
    let id = an_application(&db).await;
    pause::set(&db, "cli").await.unwrap();

    let error = agents::set_status(
        &db,
        "cli",
        &id,
        "applied",
        None,
        "",
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "paused");
    // The same write reached directly, as a caller that skipped the agent would.
    let direct = applications::set_status(&db, "cli", &id, "applied", None, "").await;
    assert_eq!(direct.unwrap_err().code(), "paused");
    assert_eq!(applications::get(&db, &id).await.unwrap().status, "saved");

    let edited = ApplicationInput {
        company: "Acme Ltd".into(),
        role: "Engineer".into(),
        ..ApplicationInput::default()
    };
    applications::update(&db, "cli", &id, edited).await.unwrap();
    applications::add_note(&db, "cli", &id, "called them")
        .await
        .unwrap();
    assert_eq!(
        applications::get(&db, &id).await.unwrap().company,
        "Acme Ltd"
    );

    pause::clear(&db, "cli").await.unwrap();
    agents::set_status(
        &db,
        "cli",
        &id,
        "applied",
        None,
        "",
        CancellationToken::new(),
    )
    .await
    .unwrap();
}

/// The files of a source tree, with anything after `#[cfg(test)]` cut off: a unit test
/// calling a function is not a route to it.
fn code_files(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            code_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            let live = text.split("#[cfg(test)]").next().unwrap().to_owned();
            found.push((path, live));
        }
    }
}

/// Every route to a status write, by file. A new one fails here until it is listed, and
/// listing it means saying why the pause covers it.
#[test]
fn every_route_to_a_status_write_passes_the_gate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [
        manifest.join("src"),
        manifest.join("../cli/src"),
        manifest.join("../../apps/desktop/src-tauri/src"),
    ] {
        code_files(&dir, &mut files);
    }
    let reached = |needle: &str| -> Vec<String> {
        let mut found: Vec<String> = files
            .iter()
            .filter(|(_, text)| text.contains(needle))
            .map(|(path, _)| {
                let name = path.components().rev().take(2).collect::<Vec<_>>();
                format!(
                    "{}/{}",
                    name[1].as_os_str().to_string_lossy(),
                    name[0].as_os_str().to_string_lossy()
                )
            })
            .collect();
        found.sort();
        found
    };

    // The store function: the agent's tracking step, which runs behind `runner::start`.
    // It also refuses on its own, so a new caller cannot skip the pause.
    assert_eq!(reached("applications::set_status("), ["agents/tracking.rs"]);
    // The agent: the CLI's `apps status` and the desktop's `app_set_status`. It starts a
    // run, and `runner::start` is the gate.
    assert_eq!(
        reached("agents::set_status("),
        ["src/apps.rs", "src/commands.rs"]
    );
    // Nothing else writes the column.
    assert_eq!(reached("SET status"), ["src/applications.rs"]);
}
