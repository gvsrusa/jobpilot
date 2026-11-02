//! The workflow runner, driven by scripted workflows: no model, no network, no keychain.

use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use jobpilot_core::agents::{self, analytics, discovery, draft, matching, memory, tracking};
use jobpilot_core::ai::{BoxFuture, CancellationToken, DataCategory, ProviderId, TaskType};
use jobpilot_core::runner::{
    self, Agent, Ctx, Limits, Permission, RunInput, RunState, StepDef, StepOutput, Workflow,
    spec_of,
};
use jobpilot_core::{AppPaths, Db, Error, Result, audit};
use sqlx::error::{DatabaseError, ErrorKind};
use tempfile::TempDir;

const ID: &str = "0123456789abcdef0123456789abcdef";

type Body = dyn for<'a> Fn(&'a Ctx<'a>, usize) -> BoxFuture<'a, Result<StepOutput>> + Send + Sync;

/// A workflow whose steps do what the test says.
struct Script {
    agent: Agent,
    steps: &'static [StepDef],
    body: Box<Body>,
    abandoned: Arc<AtomicBool>,
}

fn script<F>(agent: Agent, steps: &'static [StepDef], body: F) -> Script
where
    F: for<'a> Fn(&'a Ctx<'a>, usize) -> BoxFuture<'a, Result<StepOutput>> + Send + Sync + 'static,
{
    Script {
        agent,
        steps,
        body: Box::new(body),
        abandoned: Arc::default(),
    }
}

impl Workflow for Script {
    fn agent(&self) -> Agent {
        self.agent
    }

    fn input(&self) -> RunInput {
        RunInput::Analytics
    }

    fn steps(&self) -> &'static [StepDef] {
        self.steps
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        (self.body)(ctx, index)
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
}

const READ: &[Permission] = &[Permission::ReadApplications];
static ONE: &[StepDef] = &[StepDef {
    name: "only",
    needs: READ,
    attempts: 1,
}];
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
static TWICE: &[StepDef] = &[StepDef {
    name: "only",
    needs: READ,
    attempts: 2,
}];

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

async fn log_states(db: &Db, run_id: &str) -> Vec<RunState> {
    let detail = runner::get(db, run_id).await.unwrap();
    detail.log.iter().map(|row| row.state).collect()
}

/// What SQLite's busy error looks like to a caller.
#[derive(Debug)]
struct Busy(&'static str);

impl fmt::Display for Busy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("database is locked")
    }
}

impl StdError for Busy {}

impl DatabaseError for Busy {
    fn message(&self) -> &str {
        "database is locked"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        Some(self.0.into())
    }

    fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

fn busy(code: &'static str) -> Error {
    Error::Db(sqlx::Error::Database(Box::new(Busy(code))))
}

/// A row as a process that died would leave it.
async fn seed_row(db: &Db, id: &str, state: &str, heartbeat_age: &str) {
    sqlx::query(
        "INSERT INTO agent_runs (id, agent, state, actor, input, step, steps, heartbeat_at, \
         created_at) VALUES (?, 'tracking', ?, 'gui', '{\"kind\":\"analytics\"}', 0, 1, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .bind(id)
    .bind(state)
    .bind(heartbeat_age)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn set_halted(db: &Db, run_id: &str) {
    sqlx::query("UPDATE agent_runs SET state = 'halted', finished_at = NULL WHERE id = ?")
        .bind(run_id)
        .execute(&db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_run_logs_each_state_in_order() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, TWO, |_, _| {
        Box::pin(async {
            Ok(StepOutput {
                provider: Some(ProviderId::NoAi),
                categories: &[DataCategory::JobText],
            })
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Completed);
    use RunState::{Checking, Checkpointed, Completed, Queued, Running};
    assert_eq!(
        log_states(&db, &done.run_id).await,
        [
            Queued,
            Checking,
            Running,
            Checkpointed,
            Checking,
            Running,
            Checkpointed,
            Completed
        ]
    );
    let detail = runner::get(&db, &done.run_id).await.unwrap();
    assert_eq!(detail.run.step, 2);
    assert_eq!(detail.run.steps, 2);
    assert_eq!(detail.run.agent, Agent::Tracking);
    assert_eq!(detail.run.actor, "gui");
    assert!(detail.run.finished_at.is_some());
    let done_row = &detail.log[3];
    assert_eq!(done_row.step, "first");
    assert_eq!(done_row.provider.as_deref(), Some("no_ai"));
    assert_eq!(done_row.data_categories, [DataCategory::JobText]);
    assert_eq!(done_row.outcome, "ok");
    assert_eq!(detail.log[0].provider, None);
    assert!(detail.log[0].data_categories.is_empty());
}

#[tokio::test]
async fn a_step_outside_the_agents_set_never_runs() {
    static NOT_TRACKING: &[StepDef] = &[StepDef {
        name: "only",
        needs: &[Permission::ReadApplications, Permission::WriteReports],
        attempts: 1,
    }];
    let (_tmp, db) = setup().await;
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();
    let workflow = script(Agent::Tracking, NOT_TRACKING, move |_, _| {
        seen.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Refused);
    assert_eq!(done.reason.as_deref(), Some("permission_denied"));
    assert!(!ran.load(Ordering::SeqCst));
    let error = done.into_result().unwrap_err();
    assert_eq!(error.code(), "refused");
}

#[tokio::test]
async fn a_check_inside_a_step_refuses_too() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async {
            ctx.check(Permission::ReadApplications)?;
            ctx.check(Permission::WriteDrafts)?;
            Ok(StepOutput::default())
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Refused);
    assert_eq!(done.reason.as_deref(), Some("permission_denied"));
}

#[test]
fn every_agent_step_stays_in_its_set() {
    let table: &[(Agent, &[StepDef])] = &[
        (Agent::Tracking, tracking::STEPS),
        (Agent::Analytics, analytics::STEPS),
        (Agent::Resume, draft::RESUME_STEPS),
        (Agent::CoverLetter, draft::LETTER_STEPS),
        (Agent::CareerMemory, memory::STEPS),
        (Agent::Discovery, discovery::STEPS),
        (Agent::Matching, matching::STEPS),
    ];
    for (agent, steps) in table {
        let spec = spec_of(*agent);
        for step in *steps {
            for need in step.needs {
                assert!(
                    spec.permissions.contains(need),
                    "{agent:?} {}: {need:?}",
                    step.name
                );
            }
            assert!(!step.needs.is_empty(), "{agent:?} {}", step.name);
        }
    }
}

/// Each step's needs, exactly: a step that asks for less than it does, or more than it needs,
/// is a change someone should see.
#[test]
fn each_step_names_exactly_what_it_needs() {
    use Permission::*;
    let resume = [
        ReadResumeText,
        ReadProfileBasic,
        ReadJobText,
        Model(TaskType::DraftResume),
        WriteDrafts,
    ];
    let letter = [
        ReadResumeText,
        ReadProfileBasic,
        ReadJobText,
        ReadCompanyText,
        Model(TaskType::DraftCoverLetter),
        WriteDrafts,
    ];
    let render = [ReadResumeText, WriteDrafts];
    type Wanted<'a> = &'a [(&'a str, &'a [Permission])];
    let table: &[(&[StepDef], Wanted)] = &[
        (
            draft::RESUME_STEPS,
            &[("draft", &resume), ("render", &render)],
        ),
        (
            draft::LETTER_STEPS,
            &[("draft", &letter), ("render", &render)],
        ),
        (
            memory::STEPS,
            &[
                (
                    "classify",
                    &[
                        ReadCareerMemory,
                        ReadJobText,
                        Model(TaskType::ClassifyAnswer),
                        WriteCareerMemory,
                    ],
                ),
                (
                    "seal",
                    &[ReadCareerMemory, ReadSensitive, WriteCareerMemory],
                ),
                ("embed", &[ReadCareerMemory, EmbedLocal, WriteCareerMemory]),
            ],
        ),
        (
            discovery::STEPS,
            &[
                ("fetch", &[NetConnector, ReadJobText, WriteJobs]),
                ("dedupe", &[ReadJobText, WriteJobs]),
            ],
        ),
        (
            matching::STEPS,
            &[
                (
                    "fields",
                    &[ReadJobText, Model(TaskType::ParseJob), WriteJobFields],
                ),
                (
                    "score",
                    &[ReadJobText, ReadProfileBasic, ReadResumeText, WriteMatches],
                ),
                (
                    "semantic",
                    &[
                        ReadJobText,
                        ReadResumeText,
                        Model(TaskType::SemanticMatch),
                        WriteMatches,
                    ],
                ),
            ],
        ),
    ];
    for (steps, wanted) in table {
        assert_eq!(steps.len(), wanted.len());
        for (step, (name, needs)) in steps.iter().zip(*wanted) {
            assert_eq!(step.name, *name);
            assert_eq!(step.needs, *needs, "{name}");
            assert_eq!(step.attempts, 1, "{name}");
        }
    }
}

#[test]
fn permission_sets_match_the_spec_table() {
    use Permission::*;
    let table: &[(Agent, &[Permission], Duration)] = &[
        (
            Agent::Resume,
            &[
                ReadResumeText,
                ReadProfileBasic,
                ReadJobText,
                Model(TaskType::DraftResume),
                WriteDrafts,
            ],
            Duration::from_secs(30 * 60),
        ),
        (
            Agent::CoverLetter,
            &[
                ReadResumeText,
                ReadProfileBasic,
                ReadJobText,
                ReadCompanyText,
                Model(TaskType::DraftCoverLetter),
                WriteDrafts,
            ],
            Duration::from_secs(30 * 60),
        ),
        (
            Agent::CareerMemory,
            &[
                ReadCareerMemory,
                ReadJobText,
                ReadSensitive,
                WriteCareerMemory,
                EmbedLocal,
                Model(TaskType::ClassifyAnswer),
            ],
            Duration::from_secs(10 * 60),
        ),
        (
            Agent::Tracking,
            &[ReadApplications, WriteApplications],
            Duration::from_secs(30),
        ),
        (
            Agent::Analytics,
            &[ReadApplications, WriteReports],
            Duration::from_secs(60),
        ),
        (
            Agent::Discovery,
            &[NetConnector, ReadJobText, WriteJobs],
            Duration::from_secs(10 * 60),
        ),
        (
            Agent::Matching,
            &[
                ReadJobText,
                ReadProfileBasic,
                ReadResumeText,
                Model(TaskType::ParseJob),
                Model(TaskType::SemanticMatch),
                WriteJobFields,
                WriteMatches,
            ],
            Duration::from_secs(30 * 60),
        ),
    ];
    for (agent, permissions, timeout) in table {
        let spec = spec_of(*agent);
        assert_eq!(spec.agent, *agent);
        assert_eq!(spec.permissions, *permissions, "{agent:?}");
        assert_eq!(spec.run_timeout, *timeout, "{agent:?}");
    }
}

#[test]
fn no_permission_names_a_forbidden_power() {
    use Permission::*;
    let all = [
        ReadJobText,
        ReadCompanyText,
        ReadProfileBasic,
        ReadResumeText,
        ReadCareerMemory,
        ReadSensitive,
        ReadApplications,
        WriteDrafts,
        WriteCareerMemory,
        WriteApplications,
        WriteReports,
        Model(TaskType::ParseJob),
        Model(TaskType::DraftResume),
        Model(TaskType::DraftCoverLetter),
        Model(TaskType::ClassifyAnswer),
        Model(TaskType::Embed),
        Model(TaskType::SemanticMatch),
        Model(TaskType::SummarizeCompany),
        EmbedLocal,
        ReadMatches,
        WriteJobs,
        WriteMatches,
        WriteCompanyNotes,
        WriteJobFields,
        NetConnector,
    ];
    for permission in all {
        let name = format!("{permission:?}").to_lowercase();
        for word in ["shell", "secret", "approve", "path", "browser", "pause"] {
            assert!(!name.contains(word), "{name}");
        }
    }
}

async fn retried(code: &'static str) {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, TWICE, move |_, _| {
        let call = count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if call == 0 {
                Err(busy(code))
            } else {
                Ok(StepOutput::default())
            }
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(done.state, RunState::Completed);
    let states = log_states(&db, &done.run_id).await;
    assert!(states.contains(&RunState::Retrying), "{states:?}");
    assert_eq!(states.last(), Some(&RunState::Completed));
}

#[tokio::test]
async fn a_busy_database_is_retried_once() {
    retried("5").await;
}

/// `SQLITE_BUSY_SNAPSHOT`: what a deferred transaction meets when it starts to write.
#[tokio::test]
async fn an_extended_busy_code_counts_as_busy() {
    retried("517").await;
    retried("SQLITE_BUSY").await;
}

#[tokio::test]
async fn busy_past_the_attempts_fails() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, TWICE, move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(busy("5")) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.reason.as_deref(), Some("db"));
}

#[tokio::test]
async fn a_validation_error_is_not_retried() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, TWICE, move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(Error::Validation("a plain reason".into())) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.reason.as_deref(), Some("validation"));
    match done.into_result() {
        Err(Error::Validation(message)) => assert_eq!(message, "a plain reason"),
        other => panic!("{other:?}"),
    }
}

/// Waits for the token, the way a router task does.
async fn until_stopped(ctx: &Ctx<'_>) -> Result<StepOutput> {
    loop {
        ctx.stop_point()?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_step_past_the_run_timeout_fails_and_its_token_fires() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| Box::pin(until_stopped(ctx)));
    let cancel = token();
    let limits = Limits::for_test(Duration::from_millis(100));
    let done = runner::start_with(&db, "gui", &workflow, cancel.clone(), limits)
        .await
        .unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.reason.as_deref(), Some("timeout"));
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
    assert!(matches!(done.into_result(), Err(Error::Timeout)));
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_step_that_ignores_its_token_is_dropped_after_the_grace() {
    let (_tmp, db) = setup().await;
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = dropped.clone();
    let workflow = script(Agent::Tracking, ONE, move |_, _| {
        let guard = DropFlag(flag.clone());
        Box::pin(async move {
            let _guard = guard;
            std::future::pending::<Result<StepOutput>>().await
        })
    });
    let limits = Limits::for_test(Duration::from_millis(50));
    let done = runner::start_with(&db, "gui", &workflow, token(), limits)
        .await
        .unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(done.reason.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn cancel_fires_the_token_and_ends_cancelled() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, TWO, |ctx, index| {
        Box::pin(async move {
            if index == 0 {
                return Ok(StepOutput::default());
            }
            until_stopped(ctx).await
        })
    });
    let cancel = token();
    let fired = cancel.clone();
    let canceller = async {
        // The second step is the one running: the first has finished and been stored.
        wait_for(
            &db,
            "SELECT count(*) FROM agent_runs WHERE state = 'running' AND step = 1",
        )
        .await;
        fired.cancel();
    };
    let (done, ()) = tokio::join!(
        runner::start(&db, "gui", &workflow, cancel.clone()),
        canceller
    );
    let done = done.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(done.reason, None);
    assert!(workflow.abandoned.load(Ordering::SeqCst));
    assert_eq!(
        log_states(&db, &done.run_id).await.last(),
        Some(&RunState::Cancelled)
    );
    let detail = runner::get(&db, &done.run_id).await.unwrap();
    assert_eq!(detail.run.step, 1, "a finished step stays finished");
    assert!(detail.run.finished_at.is_some());
    assert!(matches!(done.into_result(), Err(Error::Cancelled)));
}

#[tokio::test]
async fn a_cancel_requested_elsewhere_stops_the_run() {
    let (tmp, db) = setup().await;
    let other = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let workflow = script(Agent::Tracking, ONE, |ctx, _| Box::pin(until_stopped(ctx)));
    let cancel = token();
    let requester = async {
        let clock = Instant::now();
        let id = loop {
            let runs = runner::list(&other, 10).await.unwrap();
            if let Some(run) = runs.first() {
                break run.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        agents::cancel_run(&other, "cli", &id).await.unwrap();
        clock.elapsed()
    };
    let clock = Instant::now();
    let (done, _) = tokio::join!(
        runner::start(&db, "gui", &workflow, cancel.clone()),
        requester
    );
    let done = done.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert!(cancel.is_cancelled(), "the token fires for the step");
    assert!(
        clock.elapsed() < Duration::from_secs(1),
        "{:?}",
        clock.elapsed()
    );
    other.close().await;

    let rows = audit::recent(&db, 10).await.unwrap();
    let row = rows.iter().find(|r| r.action == "run_cancelled").unwrap();
    assert_eq!(row.actor, "cli");
    assert_eq!(row.target.as_deref(), Some(done.run_id.as_str()));
    assert_eq!(row.detail["agent"], "tracking");
}

#[tokio::test]
async fn a_cancel_requested_before_a_step_runs_stops_there() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, TWO, move |ctx, index| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if index == 0 {
                let (id,): (String,) = sqlx::query_as("SELECT id FROM agent_runs")
                    .fetch_one(&ctx.db.pool)
                    .await?;
                agents::cancel_run(ctx.db, "gui", &id).await?;
            }
            Ok(StepOutput::default())
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_cancelled_run_cannot_resume() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, ONE, move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(Error::Cancelled) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    let error = runner::resume(&db, "gui", &done.run_id, &workflow, token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "validation");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        runner::get(&db, &done.run_id).await.unwrap().run.state,
        RunState::Cancelled
    );
}

#[tokio::test]
async fn a_checkpoint_refuses_anything_but_an_id() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async {
            for text in [
                "Dear hiring manager",
                "",
                "abc",
                &ID.to_uppercase(),
                &format!("{ID}0"),
            ] {
                assert_eq!(ctx.save("draft", text).unwrap_err().code(), "validation");
            }
            assert_eq!(ctx.saved("draft"), None);
            ctx.save("draft", ID)?;
            assert_eq!(ctx.saved("draft").as_deref(), Some(ID));
            Ok(StepOutput::default())
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(done.saved.get("draft"), Some(ID));
    let (stored,): (String,) = sqlx::query_as("SELECT checkpoint FROM agent_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, format!("{{\"draft\":\"{ID}\"}}"));
}

#[tokio::test]
async fn resume_starts_after_the_last_checkpoint() {
    let (_tmp, db) = setup().await;
    let firsts = Arc::new(AtomicU32::new(0));
    let seconds = Arc::new(AtomicU32::new(0));
    let (first, second) = (firsts.clone(), seconds.clone());
    let workflow = script(Agent::Tracking, TWO, move |ctx, index| {
        let call = if index == 0 {
            first.fetch_add(1, Ordering::SeqCst)
        } else {
            second.fetch_add(1, Ordering::SeqCst)
        };
        Box::pin(async move {
            match (index, call) {
                (0, _) => {
                    ctx.save("first", ID)?;
                    Ok(StepOutput::default())
                }
                (_, 0) => Err(Error::Paused),
                _ => {
                    assert_eq!(ctx.saved("first").as_deref(), Some(ID));
                    Ok(StepOutput::default())
                }
            }
        })
    });
    let halted = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(halted.state, RunState::Halted);
    assert_eq!(halted.reason.as_deref(), Some("paused"));
    let run_id = halted.run_id.clone();
    assert!(matches!(halted.into_result(), Err(Error::Paused)));

    let done = runner::resume(&db, "cli", &run_id, &workflow, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(done.reason, None);
    assert_eq!(firsts.load(Ordering::SeqCst), 1);
    assert_eq!(seconds.load(Ordering::SeqCst), 2);
    assert_eq!(done.saved.get("first"), Some(ID));

    let states = log_states(&db, &run_id).await;
    let halted_at = states.iter().position(|s| *s == RunState::Halted).unwrap();
    assert_eq!(states[halted_at + 1], RunState::Checking, "{states:?}");
    assert_eq!(states[halted_at + 2], RunState::Running, "{states:?}");
    let rows = audit::recent(&db, 10).await.unwrap();
    let row = rows.iter().find(|r| r.action == "run_resumed").unwrap();
    assert_eq!(row.actor, "cli");
    assert_eq!(row.target.as_deref(), Some(run_id.as_str()));
    assert_eq!(row.detail["agent"], "tracking");

    // Finished work is not offered again.
    let error = runner::resume(&db, "cli", &run_id, &workflow, token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "validation");
}

#[tokio::test]
async fn a_run_halted_after_its_last_step_repeats_nothing() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, ONE, move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    set_halted(&db, &done.run_id).await;
    let again = runner::resume(&db, "gui", &done.run_id, &workflow, token())
        .await
        .unwrap();
    assert_eq!(again.state, RunState::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_run_resumes_only_under_its_own_workflow() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Err(Error::Paused) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    let other = script(Agent::Analytics, ONE, |_, _| {
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let error = runner::resume(&db, "gui", &done.run_id, &other, token())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "validation");
    assert_eq!(
        runner::resume(&db, "gui", "missing", &workflow, token())
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test]
async fn only_a_person_starts_resumes_or_cancels_a_run() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Err(Error::Paused) })
    });
    let refused = runner::start(&db, "system", &workflow, token()).await;
    assert_eq!(refused.unwrap_err().code(), "refused");
    assert!(runner::list(&db, 10).await.unwrap().is_empty());
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    let refused = runner::resume(&db, "system", &done.run_id, &workflow, token()).await;
    assert_eq!(refused.unwrap_err().code(), "refused");
    let refused = agents::cancel_run(&db, "system", &done.run_id).await;
    assert_eq!(refused.unwrap_err().code(), "refused");
    assert_eq!(
        runner::get(&db, &done.run_id).await.unwrap().run.state,
        RunState::Halted
    );
}

#[tokio::test]
async fn a_dead_runs_row_is_reaped_to_halted() {
    let (_tmp, db) = setup().await;
    let dead = "aa".repeat(16);
    let live = "bb".repeat(16);
    seed_row(&db, &dead, "running", "-60 seconds").await;
    seed_row(&db, &live, "running", "-1 seconds").await;

    let runs = runner::list(&db, 10).await.unwrap();
    let of = |id: &str| runs.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(of(&dead).state, RunState::Halted);
    assert_eq!(of(&dead).reason.as_deref(), Some("interrupted"));
    assert_eq!(of(&dead).finished_at, None, "a person still acts on it");
    assert_eq!(of(&live).state, RunState::Running);
    let log = runner::get(&db, &dead).await.unwrap().log;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].state, RunState::Halted);
    assert_eq!(log[0].outcome, "interrupted");
    assert!(runner::get(&db, &live).await.unwrap().log.is_empty());
    assert_eq!(runner::reap_stale(&db).await.unwrap(), 0);
}

#[tokio::test]
async fn a_finished_row_is_never_reaped() {
    let (_tmp, db) = setup().await;
    for (id, state) in [("aa", "completed"), ("bb", "failed"), ("cc", "halted")] {
        seed_row(&db, &id.repeat(16), state, "-60 seconds").await;
    }
    assert_eq!(runner::reap_stale(&db).await.unwrap(), 0);
}

/// A laptop that slept: another handle reaped the run, and then the step came back.
#[tokio::test]
async fn a_run_reaped_under_a_live_process_stops_and_stays_halted() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, TWO, |ctx, index| {
        Box::pin(async move {
            if index == 0 {
                sqlx::query(
                    "UPDATE agent_runs SET heartbeat_at = \
                     strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-60 seconds')",
                )
                .execute(&ctx.db.pool)
                .await?;
                assert_eq!(runner::reap_stale(ctx.db).await?, 1);
                return Ok(StepOutput::default());
            }
            panic!("a reaped run took another step");
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Halted);
    assert_eq!(done.reason.as_deref(), Some("interrupted"));
    let detail = runner::get(&db, &done.run_id).await.unwrap();
    assert_eq!(detail.run.state, RunState::Halted);
    assert_eq!(detail.run.step, 0);
}

#[tokio::test]
async fn cancelling_a_halted_run_ends_it_here() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Err(Error::Paused) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    agents::cancel_run(&db, "gui", &done.run_id).await.unwrap();
    let detail = runner::get(&db, &done.run_id).await.unwrap();
    assert_eq!(detail.run.state, RunState::Cancelled);
    assert!(detail.run.finished_at.is_some());
    assert_eq!(detail.log.last().unwrap().state, RunState::Cancelled);
    let error = agents::cancel_run(&db, "gui", &done.run_id)
        .await
        .unwrap_err();
    match error {
        Error::Validation(message) => assert_eq!(message, "this run has ended"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        agents::cancel_run(&db, "gui", "missing")
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test]
async fn manual_required_ends_as_manual_required_with_its_reason() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Err(Error::ManualRequired("no_ai".into())) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::ManualRequired);
    assert_eq!(done.reason.as_deref(), Some("no_ai"));
    let summary = runner::list(&db, 1).await.unwrap().remove(0);
    assert_eq!(summary.reason.as_deref(), Some("no_ai"));
    assert!(matches!(
        done.into_result(),
        Err(Error::ManualRequired(reason)) if reason == "no_ai"
    ));
}

#[tokio::test]
async fn a_reason_outside_the_allow_list_is_stored_as_its_code() {
    let (_tmp, db) = setup().await;
    let cases: [(Error, RunState, &str); 3] = [
        (
            Error::ManualRequired("Bearer sk-abc".into()),
            RunState::ManualRequired,
            "manual_required",
        ),
        (
            Error::Refused("Bearer sk-abc".into()),
            RunState::Refused,
            "refused",
        ),
        (
            Error::Failed("Bearer sk-abc".into()),
            RunState::Failed,
            "failed",
        ),
    ];
    for (error, state, reason) in cases {
        let held = std::sync::Mutex::new(Some(error));
        let workflow = script(Agent::Tracking, ONE, move |_, _| {
            let error = held.lock().unwrap().take().unwrap();
            Box::pin(async move { Err(error) })
        });
        let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
        assert_eq!(done.state, state);
        assert_eq!(done.reason.as_deref(), Some(reason));
        let detail = runner::get(&db, &done.run_id).await.unwrap();
        assert_eq!(detail.run.reason.as_deref(), Some(reason));
        for row in &detail.log {
            assert!(!row.outcome.contains("sk-abc"));
        }
    }
}

#[tokio::test]
async fn into_result_falls_back_on_the_stored_reason() {
    let finished = |state, reason: &str| runner::Finished {
        run_id: ID.into(),
        state,
        reason: Some(reason.into()),
        saved: runner::Checkpoint::default(),
        error: None,
    };
    assert!(matches!(
        finished(RunState::Failed, "bad_reply").into_result(),
        Err(Error::Failed(reason)) if reason == "bad_reply"
    ));
    assert!(matches!(
        finished(RunState::Refused, "permission_denied").into_result(),
        Err(Error::Refused(reason)) if reason == "permission_denied"
    ));
    assert!(matches!(
        finished(RunState::ManualRequired, "no_ai").into_result(),
        Err(Error::ManualRequired(reason)) if reason == "no_ai"
    ));
    assert!(finished(RunState::Completed, "").into_result().is_ok());
}

#[tokio::test]
async fn run_rows_hold_no_text_a_step_was_given() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Err(Error::Validation("Dear hiring manager".into())) })
    });
    runner::start(&db, "gui", &workflow, token()).await.unwrap();
    let dump: (String,) = sqlx::query_as(
        "SELECT (SELECT group_concat(id || agent || state || input || checkpoint || \
         coalesce(reason, '')) FROM agent_runs) || \
         (SELECT group_concat(step || state || coalesce(provider, '') || data_categories || \
         outcome) FROM agent_run_log)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!dump.0.contains("Dear hiring manager"), "{}", dump.0);
}

#[tokio::test]
async fn list_is_newest_first_and_limited() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let first = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    let second = runner::start(&db, "cli", &workflow, token()).await.unwrap();
    let runs = runner::list(&db, 10).await.unwrap();
    assert_eq!(runs[0].id, second.run_id);
    assert_eq!(runs[1].id, first.run_id);
    assert_eq!(runner::list(&db, 1).await.unwrap().len(), 1);
    assert_eq!(runner::get(&db, "x").await.unwrap_err().code(), "not_found");
}

fn hurried() -> Limits {
    Limits::for_test(Duration::from_secs(5))
}

/// Retries, because a live step's heartbeat can land between the two statements.
async fn age_and_reap(db: &Db) {
    for _ in 0..100 {
        sqlx::query(
            "UPDATE agent_runs SET heartbeat_at = \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-60 seconds')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        if runner::reap_stale(db).await.unwrap() == 1 {
            return;
        }
    }
    panic!("the run was never reaped");
}

/// Waits until the count a query returns is above zero, so a test acts on what the run has
/// reached and not on a guess about how fast this machine is.
async fn wait_for(db: &Db, sql: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if found > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "never reached: {sql}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A step is running now.
async fn wait_for_running(db: &Db) {
    wait_for(
        db,
        "SELECT count(*) FROM agent_runs WHERE state = 'running'",
    )
    .await;
}

async fn run_id(db: &Db) -> String {
    let (id,): (String,) = sqlx::query_as("SELECT id FROM agent_runs ORDER BY seq DESC")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    id
}

async fn cancel_audits(db: &Db) -> Vec<String> {
    let rows = audit::recent(db, 100).await.unwrap();
    rows.into_iter()
        .filter(|row| row.action == "run_cancelled")
        .map(|row| row.actor)
        .collect()
}

#[tokio::test]
async fn a_cancel_in_the_last_step_leaves_the_run_completed() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async move {
            agents::cancel_run(ctx.db, "gui", &run_id(ctx.db).await).await?;
            Ok(StepOutput::default())
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
    assert!(cancel_audits(&db).await.is_empty(), "nothing was cancelled");
    assert!(done.into_result().is_ok());
}

#[tokio::test]
async fn a_token_fired_in_the_last_step_leaves_the_run_completed() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        ctx.cancel.cancel();
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_deadline_passed_by_the_last_step_leaves_the_run_completed() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(StepOutput::default())
        })
    });
    let limits = Limits::for_test(Duration::from_millis(10));
    let done = runner::start_with(&db, "gui", &workflow, token(), limits)
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(done.reason, None);
}

/// A deadline with a step still to go stays a timeout.
#[tokio::test]
async fn a_deadline_passed_with_a_step_left_fails() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, TWO, |_, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(StepOutput::default())
        })
    });
    let limits = Limits::for_test(Duration::from_millis(10));
    let done = runner::start_with(&db, "gui", &workflow, token(), limits)
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.reason.as_deref(), Some("timeout"));
}

/// Reaped under a live step, then resumed: same state, so only the generation tells them
/// apart.
#[tokio::test]
async fn a_resume_stops_the_process_it_replaced() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let old = token();
    let watched = old.clone();
    let workflow = script(Agent::Tracking, ONE, move |ctx, _| {
        let call = count.fetch_add(1, Ordering::SeqCst);
        let watched = watched.clone();
        Box::pin(async move {
            if call == 0 {
                return until_stopped(ctx).await;
            }
            // Still active: the old process must stop on the generation, not on an ended row.
            let clock = Instant::now();
            while !watched.is_cancelled() && clock.elapsed() < Duration::from_secs(2) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(watched.is_cancelled(), "the old process kept running");
            Ok(StepOutput::default())
        })
    });
    let takeover = async {
        wait_for_running(&db).await;
        age_and_reap(&db).await;
        let id = run_id(&db).await;
        runner::resume(&db, "cli", &id, &workflow, token()).await
    };
    let clock = Instant::now();
    let first = async {
        let done = runner::start_with(&db, "gui", &workflow, old.clone(), hurried()).await;
        (done, clock.elapsed())
    };
    let ((first, stopped_after), second) = tokio::join!(first, takeover);
    assert!(old.is_cancelled());
    assert!(stopped_after < Duration::from_secs(2), "{stopped_after:?}");
    let first = first.unwrap();
    assert_ne!(first.state, RunState::Failed, "it did not end the run");
    let second = second.unwrap();
    assert_eq!(second.state, RunState::Completed);
    let states = log_states(&db, &second.run_id).await;
    let ends = states.iter().filter(|s| s.is_terminal()).count();
    assert_eq!(ends, 1, "{states:?}");
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_row_lost_under_a_step_fires_its_token() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| Box::pin(until_stopped(ctx)));
    let cancel = token();
    let meddle = async {
        wait_for_running(&db).await;
        age_and_reap(&db).await;
    };
    let clock = Instant::now();
    let (done, ()) = tokio::join!(
        runner::start_with(&db, "gui", &workflow, cancel.clone(), hurried()),
        meddle
    );
    let done = done.unwrap();
    assert!(cancel.is_cancelled());
    assert!(clock.elapsed() < Duration::from_secs(2));
    assert_eq!(done.state, RunState::Halted);
    assert_eq!(done.reason.as_deref(), Some("interrupted"));
    assert!(
        !workflow.abandoned.load(Ordering::SeqCst),
        "not this run's to drop"
    );
}

/// The step ends cancelled before any poll saw the loss.
#[tokio::test]
async fn a_cancel_on_a_lost_run_abandons_nothing() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async move {
            age_and_reap(ctx.db).await;
            Err(Error::Cancelled)
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Halted);
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_heartbeat_advances_while_a_step_runs() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async move {
            let read = || async {
                let (at,): (String,) = sqlx::query_as("SELECT heartbeat_at FROM agent_runs")
                    .fetch_one(&ctx.db.pool)
                    .await
                    .unwrap();
                at
            };
            let before = read().await;
            // A beat lands within a few polls; a slow runner gets seconds, not 150 ms.
            let clock = Instant::now();
            while read().await <= before {
                assert!(clock.elapsed() < Duration::from_secs(5), "no beat arrived");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(StepOutput::default())
        })
    });
    let done = runner::start_with(&db, "gui", &workflow, token(), hurried())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
}

/// With a fresh beat a row is not stale, however short the limit.
#[tokio::test]
async fn a_beat_keeps_a_long_step_from_being_reaped() {
    let (_tmp, db) = setup().await;
    // The step stays quiet until the bystander has run, so the bystander always meets a
    // step that has outlived its stale limit, whatever the speed of this machine.
    let finished = Arc::new(AtomicBool::new(false));
    let waiting = finished.clone();
    let workflow = script(Agent::Tracking, ONE, move |ctx, _| {
        let waiting = waiting.clone();
        Box::pin(async move {
            let clock = Instant::now();
            while !waiting.load(Ordering::SeqCst) {
                assert!(
                    clock.elapsed() < Duration::from_secs(10),
                    "the bystander never ran"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(ctx.saved("x"), None);
            Ok(StepOutput::default())
        })
    });
    let other = script(Agent::Analytics, ONE, |_, _| {
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let bystander = async {
        wait_for_running(&db).await;
        // Longer than the stale limit below: only the beats keep the row alive.
        tokio::time::sleep(Duration::from_millis(700)).await;
        let limits = hurried().with_stale(Duration::from_millis(500));
        runner::start_with(&db, "cli", &other, token(), limits)
            .await
            .unwrap();
        finished.store(true, Ordering::SeqCst);
    };
    let (done, ()) = tokio::join!(
        runner::start_with(&db, "gui", &workflow, token(), hurried()),
        bystander
    );
    assert_eq!(done.unwrap().state, RunState::Completed);
}

#[tokio::test]
async fn starting_a_run_reaps_a_dead_row() {
    let (_tmp, db) = setup().await;
    let dead = "aa".repeat(16);
    seed_row(&db, &dead, "running", "-60 seconds").await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Ok(StepOutput::default()) })
    });
    runner::start(&db, "gui", &workflow, token()).await.unwrap();
    let (state,): (String,) = sqlx::query_as("SELECT state FROM agent_runs WHERE id = ?")
        .bind(&dead)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(state, "halted");
}

#[tokio::test]
async fn a_stale_limit_decides_what_counts_as_dead() {
    let (_tmp, db) = setup().await;
    let dead = "aa".repeat(16);
    seed_row(&db, &dead, "running", "-5 seconds").await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let limits = hurried().with_stale(Duration::from_secs(1));
    runner::start_with(&db, "gui", &workflow, token(), limits)
        .await
        .unwrap();
    let (state,): (String,) = sqlx::query_as("SELECT state FROM agent_runs WHERE id = ?")
        .bind(&dead)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(state, "halted");
}

#[tokio::test]
async fn a_second_cancel_is_quiet_and_audits_once() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async move {
            let id = run_id(ctx.db).await;
            agents::cancel_run(ctx.db, "cli", &id).await?;
            agents::cancel_run(ctx.db, "gui", &id).await?;
            until_stopped(ctx).await
        })
    });
    let done = runner::start_with(&db, "gui", &workflow, token(), hurried())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(cancel_audits(&db).await, ["cli"]);
}

#[tokio::test]
async fn a_cancelled_token_alone_writes_no_cancel_audit() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |_, _| {
        Box::pin(async { Ok(StepOutput::default()) })
    });
    let cancel = token();
    cancel.cancel();
    let done = runner::start(&db, "gui", &workflow, cancel).await.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert!(cancel_audits(&db).await.is_empty());
}

#[tokio::test]
async fn an_interrupted_run_is_not_called_paused() {
    let finished = |reason: &str| runner::Finished {
        run_id: ID.into(),
        state: RunState::Halted,
        reason: Some(reason.into()),
        saved: runner::Checkpoint::default(),
        error: None,
    };
    assert!(matches!(
        finished("paused").into_result(),
        Err(Error::Paused)
    ));
    assert!(matches!(
        finished("interrupted").into_result(),
        Err(Error::Failed(reason)) if reason == "interrupted"
    ));
}

/// Another process holds the row, still active: this one has no state of its own to report.
#[tokio::test]
async fn a_run_taken_over_says_so() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| {
        Box::pin(async move {
            let id = run_id(ctx.db).await;
            age_and_reap(ctx.db).await;
            sqlx::query(
                "UPDATE agent_runs SET state = 'running', reason = NULL, \
                 generation = generation + 1 WHERE id = ?",
            )
            .bind(id)
            .execute(&ctx.db.pool)
            .await?;
            Ok(StepOutput::default())
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.reason.as_deref(), Some("taken_over"));
    assert!(matches!(
        done.into_result(),
        Err(Error::Failed(reason)) if reason == "taken_over"
    ));
}

#[tokio::test]
async fn a_cancel_is_checked_before_a_retry() {
    let (_tmp, db) = setup().await;
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let workflow = script(Agent::Tracking, TWICE, move |ctx, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            agents::cancel_run(ctx.db, "gui", &run_id(ctx.db).await).await?;
            Err(busy("5"))
        })
    });
    let done = runner::start(&db, "gui", &workflow, token()).await.unwrap();
    assert_eq!(done.state, RunState::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The row stays active with no gap between states, so only the generation can tell.
#[tokio::test]
async fn a_newer_generation_fires_the_token_of_the_old_process() {
    let (_tmp, db) = setup().await;
    let workflow = script(Agent::Tracking, ONE, |ctx, _| Box::pin(until_stopped(ctx)));
    let cancel = token();
    let meddle = async {
        wait_for_running(&db).await;
        sqlx::query("UPDATE agent_runs SET generation = generation + 1")
            .execute(&db.pool)
            .await
            .unwrap();
    };
    let clock = Instant::now();
    let (done, ()) = tokio::join!(
        runner::start_with(&db, "gui", &workflow, cancel.clone(), hurried()),
        meddle
    );
    assert!(
        clock.elapsed() < Duration::from_secs(2),
        "{:?}",
        clock.elapsed()
    );
    assert!(cancel.is_cancelled());
    assert_eq!(done.unwrap().reason.as_deref(), Some("taken_over"));
    assert!(!workflow.abandoned.load(Ordering::SeqCst));
}
