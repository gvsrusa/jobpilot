//! Agents as the adapters reach them.

use jobpilot_core::agents::{self, tracking};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{BoxFuture, CancellationToken};
use jobpilot_core::applications::{self, Application, ApplicationInput};
use jobpilot_core::runner::{
    self, Agent, Ctx, Permission, RunInput, RunState, StepDef, StepOutput, Workflow,
};
use jobpilot_core::{AppPaths, Db, Result, backup, transfer};
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
