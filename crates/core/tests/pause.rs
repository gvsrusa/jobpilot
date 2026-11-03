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
    abandoned: Arc<AtomicBool>,
}

impl Blocker {
    fn new() -> Blocker {
        Blocker {
            saw_cancel: Arc::default(),
            abandoned: Arc::default(),
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

    fn abandon<'a>(
        &'a self,
        _db: &'a Db,
        _saved: &'a runner::Checkpoint,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async {
            self.abandoned.store(true, Ordering::SeqCst);
            Ok(())
        })
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

/// Every `.rs` file under a source tree, whole: a `#[cfg(test)]` module is scanned too, so
/// nothing hides below one.
fn source_files(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push((path.clone(), fs::read_to_string(&path).unwrap()));
        }
    }
}

/// Lower case, with every run of whitespace one space, so case and line breaks cannot hide a
/// statement.
fn flat(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Each `update applications` statement, up to where it is bound or run, with the file it
/// is in and the name of the function that holds it.
fn applications_updates(files: &[(PathBuf, String)]) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for (path, text) in files {
        let text = flat(text);
        let mut from = 0;
        while let Some(at) = text[from..].find("update applications") {
            let start = from + at;
            let rest = &text[start..];
            let end = [".bind(", ".execute(", ".fetch"]
                .iter()
                .filter_map(|stop| rest.find(stop))
                .min()
                .unwrap_or(rest.len());
            let owner = text[..start]
                .rfind(" fn ")
                .map(|i| text[i + 4..].split(['(', '<']).next().unwrap().to_owned())
                .unwrap_or_default();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            found.push((file, owner, rest[..end].to_owned()));
            from = start + 1;
        }
    }
    found
}

/// Every route to a status write. The only statement that sets the column is inside
/// `set_status_with`, which gates itself before it. A new one, however it is spelled, fails here
/// until it is looked at: `force_status`, `SET updated_at = 'x', status = ...`, `set status`.
#[test]
fn every_route_to_a_status_write_passes_the_gate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [
        manifest.join("src"),
        manifest.join("../cli/src"),
        manifest.join("../../apps/desktop/src-tauri/src"),
    ] {
        source_files(&dir, &mut files);
    }

    let writes: Vec<_> = applications_updates(&files)
        .into_iter()
        .filter(|(_, _, statement)| statement.contains("status"))
        .collect();
    assert_eq!(writes.len(), 1, "one statement sets status: {writes:?}");
    assert_eq!(
        (writes[0].0.as_str(), writes[0].1.as_str()),
        ("applications.rs", "set_status_with")
    );

    // That function refuses first. `set_status` is its wrapper with no submission row.
    let (_, text) = files
        .iter()
        .find(|(path, _)| path.ends_with("src/applications.rs"))
        .unwrap();
    let body = flat(text);
    let body = &body[body.find("pub(crate) async fn set_status_with").unwrap()..];
    let gate = body
        .find("pause::ensure_off(db)")
        .expect("set_status_with gates itself");
    assert!(gate < body.find("update applications").unwrap());

    // Nothing but the gated function and the agent that runs it names it.
    let reaching = |needle: &str| -> Vec<String> {
        let mut found: Vec<String> = files
            .iter()
            .filter(|(_, text)| flat(text).contains(needle))
            .map(|(path, _)| {
                // A `mod.rs` is named with its folder, so a move to another module shows.
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                match path.parent().and_then(|p| p.file_name()) {
                    Some(dir) if name == "mod.rs" => format!("{}/mod.rs", dir.to_string_lossy()),
                    _ => name,
                }
            })
            .collect();
        found.sort();
        found
    };
    // The gated store function. The tracking step, which also runs behind `runner::start`,
    // reaches it through `set_status_with`, which can add Manual mode's submission row.
    assert_eq!(
        reaching("applications::set_status("),
        ["crypto.rs"],
        "crypto.rs is a unit test"
    );
    assert_eq!(reaching("applications::set_status_with("), ["tracking.rs"]);
    assert_eq!(reaching("agents::set_status_with("), ["apply/mod.rs"]);
    // The agent, whose `start` is a gate: the CLI's `app status`, the desktop command, and
    // `apply::settle` (apply/mod.rs), which records an apply run's submissions.
    assert_eq!(
        reaching("agents::set_status("),
        ["apply/mod.rs", "apps.rs", "commands.rs"]
    );
}

/// The scan reads statements the way the guard needs, whatever the case or the spelling.
#[test]
fn the_scan_sees_a_status_write_however_it_is_spelled() {
    let sources = |text: &str| vec![(PathBuf::from("x/other.rs"), text.to_owned())];
    for sneaky in [
        "fn force_status() { sqlx::query(\"UPDATE applications SET status = 'x'\").bind(1); }",
        "fn a() { sqlx::query(\"update applications set updated_at='x', status=?\").bind(1); }",
        "fn b() { sqlx::query(\"UPDATE\n  applications\n  SET Status = 1\").execute(p); }",
        "fn c() { sqlx::query(concat!(\"UPDATE applications SET status = ?1, updated_at = \", now!())).bind(1); }",
    ] {
        let found = applications_updates(&sources(sneaky));
        assert_eq!(found.len(), 1, "{sneaky}");
        assert!(found[0].2.contains("status"), "{sneaky}: {found:?}");
    }
    let harmless =
        sources("fn d() { sqlx::query(\"UPDATE applications SET notes = ?\").bind(1); }");
    assert!(!applications_updates(&harmless)[0].2.contains("status"));
}

/// The desktop fires the tokens before it writes the row, so a step can end on its token
/// while the row is still being written behind a lock. It was a pause all the same.
#[tokio::test]
async fn a_token_fired_before_the_row_is_written_still_halts_the_run_as_paused() {
    let (tmp, db) = setup().await;
    let db = Arc::new(db);
    let third = second_handle(&tmp).await;
    let workflow = Arc::new(Blocker::new());
    let token = CancellationToken::new();
    let (held, step, cancel) = (db.clone(), workflow.clone(), token.clone());
    let task = tokio::spawn(async move { runner::start(&held, "gui", &*step, cancel).await });
    wait_for(
        &db,
        "SELECT count(*) FROM agent_runs WHERE state = 'running'",
    )
    .await;

    // Another handle holds the write lock for 300 ms, and the pause path runs behind it.
    let mut lock = third.pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .unwrap();
    pause::announce(db.paths());
    token.cancel();
    let writer = {
        let handle = second_handle(&tmp).await;
        tokio::spawn(async move { pause::set(&handle, "gui").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    sqlx::query("ROLLBACK").execute(&mut *lock).await.unwrap();
    drop(lock);

    let done = task.await.unwrap().unwrap();
    writer.await.unwrap().unwrap();
    assert_eq!(done.state, RunState::Halted);
    assert_eq!(done.reason.as_deref(), Some("paused"));
    assert!(
        !workflow.abandoned.load(Ordering::SeqCst),
        "nothing was dropped"
    );
    // Resumable once cleared.
    pause::clear(&db, "gui").await.unwrap();
    let again = runner::resume(
        &db,
        "gui",
        &done.run_id,
        &Instant1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(again.state, RunState::Completed);
}

/// A clear that lands while the stop is under way does not turn the stop into a cancel.
#[tokio::test]
async fn clearing_during_the_stop_still_ends_the_run_halted() {
    let (_tmp, db) = setup().await;
    let db = Arc::new(db);
    let workflow = Arc::new(Blocker::new());
    let token = CancellationToken::new();
    let (held, step, cancel) = (db.clone(), workflow.clone(), token.clone());
    let task = tokio::spawn(async move { runner::start(&held, "gui", &*step, cancel).await });
    wait_for(
        &db,
        "SELECT count(*) FROM agent_runs WHERE state = 'running'",
    )
    .await;

    pause::announce(db.paths());
    pause::set(&db, "gui").await.unwrap();
    pause::clear(&db, "gui").await.unwrap();
    token.cancel();

    let done = task.await.unwrap().unwrap();
    assert_eq!(done.state, RunState::Halted);
    assert_eq!(done.reason.as_deref(), Some("paused"));
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
}

/// A person's own cancel outranks a pause that was also under way.
#[tokio::test]
async fn a_persons_cancel_still_wins_over_the_pause() {
    let (_tmp, db) = setup().await;
    let db = Arc::new(db);
    let workflow = Arc::new(Blocker::new());
    let (held, step) = (db.clone(), workflow.clone());
    let task =
        tokio::spawn(
            async move { runner::start(&held, "gui", &*step, CancellationToken::new()).await },
        );
    wait_for(
        &db,
        "SELECT count(*) FROM agent_runs WHERE state = 'running'",
    )
    .await;
    let id: String = sqlx::query_scalar("SELECT id FROM agent_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();

    pause::announce(db.paths());
    agents::cancel_run(&db, "gui", &id).await.unwrap();

    let done = task.await.unwrap().unwrap();
    assert_eq!(done.state, RunState::Cancelled);
}

/// A pause announced for one data folder says nothing about another.
#[tokio::test]
async fn an_announced_pause_is_per_data_folder() {
    let (_a, one) = setup().await;
    let (_b, two) = setup().await;
    let before = pause::epoch(two.paths());
    pause::announce(one.paths());
    assert_eq!(pause::epoch(two.paths()), before);
    assert_ne!(pause::epoch(one.paths()), 0);
}

/// Indexing is automation: while paused it says so, instead of asking for manual work.
#[tokio::test]
async fn indexing_while_paused_says_paused() {
    let (_tmp, db) = setup().await;
    settings::set(&db, "ai.embedding_model", &json!("test-embed"))
        .await
        .unwrap();
    memory::add_question(&db, "Why do you want this job?", None)
        .await
        .unwrap();
    let (mock, calls) = Mock::new();
    let router = Router::with_providers(vec![Box::new(mock)]);
    pause::set(&db, "cli").await.unwrap();

    let items = jobpilot_core::embed::index_missing(&db, &router, CancellationToken::new()).await;
    assert_eq!(items.unwrap_err().code(), "paused");
    let questions = memory::index_missing(&db, &router, CancellationToken::new()).await;
    assert_eq!(questions.unwrap_err().code(), "paused");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Building a report is automation too: nothing is dropped or started while paused.
#[tokio::test]
async fn a_report_is_refused_while_paused_and_drops_nothing() {
    let (_tmp, db) = setup().await;
    sqlx::query(
        "INSERT INTO reports (id, created_at, complete, figures) VALUES ('half', '2020-01-01T00:00:00Z', 0, '{}')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    pause::set(&db, "cli").await.unwrap();

    let error = agents::analytics_report(&db, "cli", CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "paused");
    assert_eq!(
        count(&db, "SELECT count(*) FROM reports").await,
        1,
        "nothing was dropped"
    );
    assert_eq!(count(&db, "SELECT count(*) FROM agent_runs").await, 0);
}
