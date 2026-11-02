//! Analytics works from the application store alone: no model, no network, no keychain.

use jobpilot_core::agents::{self, analytics as agent};
use jobpilot_core::ai::{BoxFuture, CancellationToken};
use jobpilot_core::analytics;
use jobpilot_core::applications::{self, Application, ApplicationInput};
use jobpilot_core::runner::{
    self, Agent, Checkpoint, Ctx, RunInput, RunState, StepDef, StepOutput, Workflow,
};
use jobpilot_core::{AppPaths, Db, Result};
use tempfile::TempDir;

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

async fn add(db: &Db, company: &str, role: &str, notes: &str) -> Application {
    let input = ApplicationInput {
        company: company.into(),
        role: role.into(),
        notes: notes.into(),
        ..ApplicationInput::default()
    };
    applications::add(db, "cli", input).await.unwrap()
}

async fn go(db: &Db, id: &str, status: &str) {
    applications::set_status(db, "cli", id, status, None, "")
        .await
        .unwrap();
}

/// Puts the applied date and each status event on a chosen day.
async fn date(db: &Db, id: &str, applied_on: &str, responded_on: Option<&str>) {
    sqlx::query("UPDATE applications SET applied_on = ? WHERE id = ?")
        .bind(applied_on)
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    if let Some(day) = responded_on {
        sqlx::query(
            "UPDATE application_events SET at = ? || 'T09:00:00.000Z' \
             WHERE application_id = ? AND to_status != 'applied'",
        )
        .bind(day)
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    }
}

async fn report(db: &Db) -> analytics::Report {
    agents::analytics_report(db, "cli", token()).await.unwrap()
}

#[tokio::test]
async fn an_empty_history_reports_zeros_and_no_rates() {
    let (_tmp, db) = setup().await;
    let report = report(&db).await;
    assert_eq!(report.volume.total, 0);
    assert!(report.volume.by_status.is_empty());
    assert_eq!(report.volume.applied_last_30_days, 0);
    assert_eq!(report.funnel.applied, 0);
    assert_eq!(report.funnel.response_rate, None);
    assert_eq!(report.funnel.offer_rate, None);
    assert_eq!(report.timing.median_days_to_response, None);
    assert_eq!(report.timing.responses_counted, 0);
}

#[tokio::test]
async fn applied_means_ever_applied() {
    let (_tmp, db) = setup().await;
    let app = add(&db, "Acme", "Engineer", "").await;
    go(&db, &app.id, "applied").await;
    go(&db, &app.id, "rejected").await;
    let report = report(&db).await;
    assert_eq!(report.funnel.applied, 1);
    assert_eq!(report.funnel.responded, 1);
    assert_eq!(report.volume.by_status.get("rejected"), Some(&1));
    assert_eq!(report.volume.by_status.get("applied"), None);
}

#[tokio::test]
async fn response_rate_is_responded_over_applied() {
    let (_tmp, db) = setup().await;
    for index in 0..4 {
        let app = add(&db, &format!("Co{index}"), "Engineer", "").await;
        go(&db, &app.id, "applied").await;
        if index == 0 {
            go(&db, &app.id, "screening").await;
        }
    }
    let report = report(&db).await;
    assert_eq!(report.funnel.applied, 4);
    assert_eq!(report.funnel.responded, 1);
    assert_eq!(report.funnel.response_rate, Some(0.25));
    assert_eq!(report.funnel.offer_rate, Some(0.0));
}

#[tokio::test]
async fn interviews_and_offers_are_counted_from_events() {
    let (_tmp, db) = setup().await;
    let a = add(&db, "A", "R", "").await;
    let b = add(&db, "B", "R", "").await;
    for app in [&a, &b] {
        go(&db, &app.id, "applied").await;
        go(&db, &app.id, "interviewing").await;
    }
    go(&db, &b.id, "offer").await;
    go(&db, &b.id, "accepted").await;
    let report = report(&db).await;
    assert_eq!(report.funnel.interviews, 2);
    assert_eq!(report.funnel.offers, 1);
    assert_eq!(report.funnel.offer_rate, Some(0.5));
}

#[tokio::test]
async fn median_days_uses_the_first_response() {
    let (_tmp, db) = setup().await;
    // 2, 4 and 10 days: the median is 4. The first response counts, not the later ones.
    for (index, gap) in [("01", 2), ("02", 4), ("03", 10)] {
        let app = add(&db, &format!("Co{index}"), "R", "").await;
        go(&db, &app.id, "applied").await;
        go(&db, &app.id, "screening").await;
        go(&db, &app.id, "rejected").await;
        date(&db, &app.id, "2026-03-01", None).await;
        let first = format!("2026-03-{:02}", 1 + gap);
        // The rejection comes later than the first response.
        sqlx::query(
            "UPDATE application_events SET at = ? || 'T09:00:00.000Z' \
             WHERE application_id = ? AND to_status = 'screening'",
        )
        .bind(&first)
        .bind(&app.id)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE application_events SET at = '2026-04-20T09:00:00.000Z' \
             WHERE application_id = ? AND to_status = 'rejected'",
        )
        .bind(&app.id)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let report = report(&db).await;
    assert_eq!(report.timing.median_days_to_response, Some(4.0));
    assert_eq!(report.timing.responses_counted, 3);
}

#[tokio::test]
async fn an_even_count_takes_the_mean_of_the_middle_two() {
    let (_tmp, db) = setup().await;
    for gap in [2, 5] {
        let app = add(&db, &format!("Co{gap}"), "R", "").await;
        go(&db, &app.id, "applied").await;
        go(&db, &app.id, "screening").await;
        date(
            &db,
            &app.id,
            "2026-03-01",
            Some(&format!("2026-03-{:02}", 1 + gap)),
        )
        .await;
    }
    assert_eq!(report(&db).await.timing.median_days_to_response, Some(3.5));
}

#[tokio::test]
async fn a_negative_gap_is_not_counted() {
    let (_tmp, db) = setup().await;
    let app = add(&db, "Acme", "R", "").await;
    go(&db, &app.id, "applied").await;
    go(&db, &app.id, "screening").await;
    date(&db, &app.id, "2026-03-10", Some("2026-03-05")).await;
    let report = report(&db).await;
    assert_eq!(report.funnel.responded, 1, "it still responded");
    assert_eq!(report.timing.responses_counted, 0);
    assert_eq!(report.timing.median_days_to_response, None);
}

#[tokio::test]
async fn saved_only_applications_count_in_volume_alone() {
    let (_tmp, db) = setup().await;
    add(&db, "Acme", "R", "").await;
    add(&db, "Beta", "R", "").await;
    let report = report(&db).await;
    assert_eq!(report.volume.total, 2);
    assert_eq!(report.volume.by_status.get("saved"), Some(&2));
    assert_eq!(report.volume.applied_last_30_days, 0);
    assert_eq!(report.funnel.applied, 0);
    assert_eq!(report.funnel.response_rate, None);
}

#[tokio::test]
async fn applied_in_the_last_30_days_follows_the_applied_date() {
    let (_tmp, db) = setup().await;
    let recent = add(&db, "A", "R", "").await;
    let old = add(&db, "B", "R", "").await;
    go(&db, &recent.id, "applied").await;
    go(&db, &old.id, "applied").await;
    date(&db, &old.id, "2020-01-01", None).await;
    let report = report(&db).await;
    assert_eq!(report.funnel.applied, 2);
    assert_eq!(report.volume.applied_last_30_days, 1);
}

#[tokio::test]
async fn a_report_holds_no_text() {
    let (_tmp, db) = setup().await;
    let app = add(&db, "CanaryCorp", "CanaryRole", "canary-note").await;
    go(&db, &app.id, "applied").await;
    applications::add_note(&db, "cli", &app.id, "canary-event-note")
        .await
        .unwrap();
    report(&db).await;
    let (figures,): (String,) = sqlx::query_as("SELECT figures FROM reports")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!figures.to_lowercase().contains("canary"), "{figures}");
    let (input, checkpoint): (String, String) =
        sqlx::query_as("SELECT input, checkpoint FROM agent_runs")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!(input + &checkpoint).contains("anary"));
}

#[tokio::test]
async fn latest_returns_the_newest_complete_report() {
    let (_tmp, db) = setup().await;
    assert!(analytics::latest(&db).await.unwrap().is_none());
    let first = report(&db).await;
    add(&db, "Acme", "R", "").await;
    let second = report(&db).await;
    assert_ne!(first.id, second.id);
    let latest = analytics::latest(&db).await.unwrap().unwrap();
    assert_eq!(latest.id, second.id);
    assert_eq!(latest.volume.total, 1);
}

#[tokio::test]
async fn latest_ignores_an_incomplete_report() {
    let (_tmp, db) = setup().await;
    let done = report(&db).await;
    sqlx::query(
        "INSERT INTO reports (id, created_at, complete, figures) VALUES \
         ('ffffffffffffffffffffffffffffffff', '2999-01-01T00:00:00.000Z', 0, \
          '{\"volume\":{\"total\":99}}')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let latest = analytics::latest(&db).await.unwrap().unwrap();
    assert_eq!(latest.id, done.id);
    sqlx::query("DELETE FROM reports")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(analytics::latest(&db).await.unwrap().is_none());
}

/// Runs Analytics for real and fires the token once the first section is written.
struct CancelAfterFirst {
    inner: agent::Analytics,
    cancel: CancellationToken,
}

impl Workflow for CancelAfterFirst {
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
            let out = self.inner.step(ctx, index).await?;
            if index == 0 {
                self.cancel.cancel();
            }
            Ok(out)
        })
    }

    fn abandon<'a>(&'a self, db: &'a Db, saved: &'a Checkpoint) -> BoxFuture<'a, Result<()>> {
        self.inner.abandon(db, saved)
    }
}

async fn reports_left(db: &Db) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM reports")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    n
}

#[tokio::test]
async fn a_cancelled_report_is_dropped() {
    let (_tmp, db) = setup().await;
    let cancel = token();
    let workflow = CancelAfterFirst {
        inner: agent::Analytics,
        cancel: cancel.clone(),
    };
    let done = runner::start(&db, "gui", &workflow, cancel).await.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(reports_left(&db).await, 0);
    assert!(analytics::latest(&db).await.unwrap().is_none());
}

#[tokio::test]
async fn a_cancelled_halted_report_is_dropped_by_cancel_run() {
    let (_tmp, db) = setup().await;
    let cancel = token();
    let workflow = CancelAfterFirst {
        inner: agent::Analytics,
        cancel: cancel.clone(),
    };
    // Stop after section one, then rewind the run to `halted` as an interrupted one is.
    let done = runner::start(&db, "gui", &workflow, cancel).await.unwrap();
    let id = done.run_id;
    let (report_id,): (String,) = sqlx::query_as("SELECT lower(hex(randomblob(16)))")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO reports (id, created_at, complete, figures) VALUES (?, \
         strftime('%Y-%m-%dT%H:%M:%fZ','now'), 0, '{}')",
    )
    .bind(&report_id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE agent_runs SET state = 'halted', finished_at = NULL, step = 1, \
         checkpoint = json_object('report', ?) WHERE id = ?",
    )
    .bind(&report_id)
    .bind(&id)
    .execute(&db.pool)
    .await
    .unwrap();

    agents::cancel_run(&db, "cli", &id).await.unwrap();
    assert_eq!(reports_left(&db).await, 0);
    let run = runner::get(&db, &id).await.unwrap().run;
    assert_eq!(run.state, RunState::Cancelled);
}

#[tokio::test]
async fn a_halted_report_resumes_into_the_same_row() {
    let (_tmp, db) = setup().await;
    add(&db, "Acme", "R", "").await;
    // A halted run with a saved row and one finished section resumes at the second step.
    let fresh = report(&db).await;
    sqlx::query(
        "UPDATE reports SET complete = 0, figures = json_remove(figures, '$.funnel', '$.timing')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let id = runner::list(&db, 1).await.unwrap().remove(0).id;
    sqlx::query(
        "UPDATE agent_runs SET state = 'halted', finished_at = NULL, step = 1, \
         checkpoint = json_object('report', ?) WHERE id = ?",
    )
    .bind(&fresh.id)
    .bind(&id)
    .execute(&db.pool)
    .await
    .unwrap();
    let router = jobpilot_core::ai::router::Router::from_settings(&db)
        .await
        .unwrap();
    let resumed = agents::resume_run(&db, &router, "cli", &id, token())
        .await
        .unwrap();
    assert_eq!(resumed.state, RunState::Completed);
    let latest = analytics::latest(&db).await.unwrap().unwrap();
    assert_eq!(latest.id, fresh.id);
    assert_eq!(latest.volume.total, 1);
    assert_eq!(latest.funnel.applied, 0);
}

#[tokio::test]
async fn analytics_runs_on_the_runner() {
    let (_tmp, db) = setup().await;
    report(&db).await;
    let runs = runner::list(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].agent, Agent::Analytics);
    assert_eq!(runs[0].state, RunState::Completed);
    let detail = runner::get(&db, &runs[0].id).await.unwrap();
    let checkpointed = detail
        .log
        .iter()
        .filter(|row| row.state == RunState::Checkpointed)
        .count();
    assert_eq!(checkpointed, 3);
    for row in &detail.log {
        assert_eq!(row.provider, None);
        assert!(row.data_categories.is_empty());
    }
}

#[tokio::test]
async fn a_person_runs_it_and_the_system_is_refused() {
    let (_tmp, db) = setup().await;
    let error = agents::analytics_report(&db, "system", token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert_eq!(reports_left(&db).await, 0);
}

#[tokio::test]
async fn a_future_applied_date_is_not_in_the_last_30_days() {
    let (_tmp, db) = setup().await;
    let soon = add(&db, "A", "R", "").await;
    go(&db, &soon.id, "applied").await;
    date(&db, &soon.id, "2999-01-01", None).await;
    let report = report(&db).await;
    assert_eq!(report.funnel.applied, 1);
    assert_eq!(report.volume.applied_last_30_days, 0);
}

#[tokio::test]
async fn an_applied_event_counts_without_an_applied_date() {
    let (_tmp, db) = setup().await;
    let app = add(&db, "A", "R", "").await;
    go(&db, &app.id, "applied").await;
    go(&db, &app.id, "screening").await;
    sqlx::query("UPDATE applications SET applied_on = NULL")
        .execute(&db.pool)
        .await
        .unwrap();
    let report = report(&db).await;
    assert_eq!(report.funnel.applied, 1);
    assert_eq!(report.funnel.responded, 1);
    assert_eq!(report.timing.responses_counted, 0, "no date, no gap");
}

/// Runs step 0 twice, as a retry after a failure past the insert would.
struct StepZeroTwice;

impl Workflow for StepZeroTwice {
    fn agent(&self) -> Agent {
        agent::Analytics.agent()
    }

    fn input(&self) -> RunInput {
        agent::Analytics.input()
    }

    fn steps(&self) -> &'static [StepDef] {
        agent::STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            if index == 0 {
                agent::Analytics.step(ctx, 0).await?;
            }
            agent::Analytics.step(ctx, index).await
        })
    }

    fn abandon<'a>(&'a self, db: &'a Db, saved: &'a Checkpoint) -> BoxFuture<'a, Result<()>> {
        agent::Analytics.abandon(db, saved)
    }
}

#[tokio::test]
async fn a_retried_first_step_leaves_one_row() {
    let (_tmp, db) = setup().await;
    let done = runner::start(&db, "cli", &StepZeroTwice, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(reports_left(&db).await, 1);
}

async fn plant(db: &Db, id: &str, age: &str) {
    sqlx::query(
        "INSERT INTO reports (id, created_at, complete, figures) VALUES \
         (?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?), 0, '{}')",
    )
    .bind(id)
    .bind(age)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_new_run_drops_what_a_failed_run_left() {
    let (_tmp, db) = setup().await;
    let orphan = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    plant(&db, orphan, "-1 hour").await;
    // A run that failed, with the row in its checkpoint, owns nothing any more.
    sqlx::query(
        "INSERT INTO agent_runs (id, agent, state, actor, input, step, steps, checkpoint, \
         heartbeat_at, created_at, finished_at) VALUES ('11111111111111111111111111111111', \
         'analytics', 'failed', 'cli', '{\"kind\":\"analytics\"}', 1, 3, json_object('report', ?), \
         strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'), \
         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(orphan)
    .execute(&db.pool)
    .await
    .unwrap();
    let kept = report(&db).await;
    assert_eq!(reports_left(&db).await, 1);
    assert_eq!(analytics::latest(&db).await.unwrap().unwrap().id, kept.id);
}

#[tokio::test]
async fn a_new_run_keeps_the_row_of_a_halted_run_and_a_fresh_row() {
    let (_tmp, db) = setup().await;
    let halted = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let fresh = "cccccccccccccccccccccccccccccccc";
    plant(&db, halted, "-1 hour").await;
    plant(&db, fresh, "-1 second").await;
    sqlx::query(
        "INSERT INTO agent_runs (id, agent, state, actor, input, step, steps, checkpoint, \
         heartbeat_at, created_at) VALUES ('22222222222222222222222222222222', \
         'analytics', 'halted', 'cli', '{\"kind\":\"analytics\"}', 1, 3, json_object('report', ?), \
         strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(halted)
    .execute(&db.pool)
    .await
    .unwrap();
    report(&db).await;
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM reports WHERE id IN (?, ?)")
        .bind(halted)
        .bind(fresh)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
}
