//! The workflow runner (FR-11.10 to FR-11.16). `docs/agents.md` section 7 is the contract.
//!
//! A workflow's steps are fixed in code. The runner moves a run through its states, checks
//! each step against the agent's static set, bounds it, retries a busy database, stores a
//! checkpoint after each step, and logs every change. Rows hold ids and codes only (M-26).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep, sleep_until};

use crate::ai::router::run_reason;
use crate::ai::{BoxFuture, CancellationToken, DataCategory, ProviderId, RUN_REASONS};
use crate::db::Db;
use crate::drafts::ExportFormat;
use crate::error::{Error, Result};
use crate::pause;
use crate::render::Template;

mod perms;
pub mod store;

pub use perms::{AgentSpec, Permission, spec_of};

use store::Change;

/// How often a running step's process looks for a cancel from another handle.
const WATCH: Duration = Duration::from_millis(250);
const HEARTBEAT: Duration = Duration::from_secs(2);
/// How long a step has to end after its token fires. Then it is dropped, which kills any child.
const GRACE: Duration = Duration::from_secs(5);
/// A row quiet for this long lost its process. Far above `HEARTBEAT`, so a slow beat is safe.
pub(crate) const STALE: Duration = Duration::from_secs(30);
/// Reasons a model task can end on beside the router's own.
const MANUAL_REASONS: &[&str] = &["no_ai", "no_evidence"];
const RUN_STEP: &str = "run";
const PAUSED: &str = "paused";
/// Another process holds the row and has not ended it, so this one has nothing to report.
const TAKEN_OVER: &str = "taken_over";
const LIST_LIMIT: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    Resume,
    CoverLetter,
    CareerMemory,
    Tracking,
    Analytics,
    Discovery,
    Matching,
    Research,
    BrowserApplication,
    ClaudeApply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Checking,
    Running,
    Checkpointed,
    Retrying,
    WaitingForHuman,
    Halted,
    Completed,
    ManualRequired,
    Refused,
    Failed,
    Cancelled,
}

impl RunState {
    /// `halted` is not here: it waits for a person to resume or cancel it.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunState::Completed
                | RunState::ManualRequired
                | RunState::Refused
                | RunState::Failed
                | RunState::Cancelled
        )
    }
}

pub struct StepDef {
    pub name: &'static str,
    pub needs: &'static [Permission],
    pub attempts: u32,
}

#[derive(Default)]
pub struct StepOutput {
    pub provider: Option<ProviderId>,
    pub categories: &'static [DataCategory],
    /// Model output the checks cut in this step, logged as a count.
    pub dropped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunInput {
    Resume {
        job_text_id: String,
        export: Option<ExportChoice>,
    },
    CoverLetter {
        job_text_id: String,
        export: Option<ExportChoice>,
    },
    MemoryRefresh,
    Tracking {
        application_id: String,
        status: String,
        on: Option<String>,
        /// A Manual-mode record, whose `applied` also writes the submission row. A flag, so
        /// the row holds no text; older rows read false.
        #[serde(default)]
        manual: bool,
    },
    Analytics,
    Discovery {
        source_ids: Vec<String>,
    },
    /// Hex job ids and a flag: no text.
    Matching {
        job_ids: Vec<String>,
        ai: bool,
    },
    /// A hex company id: no text.
    Research {
        company_id: String,
    },
    /// A hex apply-run id: the items and their files live in `apply_runs`.
    BrowserApply {
        apply_id: String,
    },
    ClaudeApply {
        apply_id: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExportChoice {
    pub format: ExportFormat,
    pub template: Template,
}

/// References to stored results. Keys are `&'static` in code and values are hex ids, so no
/// text can land here (FR-11.15).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Checkpoint(BTreeMap<String, String>);

impl Checkpoint {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

/// What every stored id looks like: `lower(hex(randomblob(16)))`.
fn is_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub trait Workflow: Send + Sync {
    fn agent(&self) -> Agent;
    fn input(&self) -> RunInput;
    fn steps(&self) -> &'static [StepDef];
    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>>;
    /// After a cancel, for a partial result the table in `docs/agents.md` says to drop.
    fn abandon<'a>(&'a self, _db: &'a Db, _saved: &'a Checkpoint) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// What a step gets. `cancel` fires on a cancel, a timeout, or a run lost to another handle.
pub struct Ctx<'a> {
    pub db: &'a Db,
    pub actor: &'a str,
    pub cancel: CancellationToken,
    run_id: String,
    agent: Agent,
    /// Bumped by each resume, so a run that was resumed has a generation above 0.
    generation: i64,
    checkpoint: Mutex<Checkpoint>,
}

impl Ctx<'_> {
    /// The row this step belongs to, so a result can be tied to its run.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether this process took the run over from an earlier one. A checkpoint cannot say:
    /// a crash stores none, and a pause may come before the first thing worth saving.
    pub fn resumed(&self) -> bool {
        self.generation > 0
    }

    pub fn check(&self, permission: Permission) -> Result<()> {
        if spec_of(self.agent).permissions.contains(&permission) {
            Ok(())
        } else {
            Err(Error::Refused("permission_denied".into()))
        }
    }

    pub fn save(&self, key: &'static str, id: &str) -> Result<()> {
        if !is_hex_id(id) {
            return Err(Error::Validation("a checkpoint holds ids only".into()));
        }
        self.held().0.insert(key.to_owned(), id.to_owned());
        Ok(())
    }

    pub fn saved(&self, key: &str) -> Option<String> {
        self.held().get(key).map(str::to_owned)
    }

    pub fn stop_point(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Checkpoint> {
        self.checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug)]
pub struct Finished {
    pub run_id: String,
    pub state: RunState,
    pub reason: Option<String>,
    pub saved: Checkpoint,
    /// What ended the run, kept for the caller. Never stored.
    pub error: Option<Error>,
}

impl Finished {
    pub fn into_result(self) -> Result<Checkpoint> {
        let reason = self.reason.unwrap_or_default();
        match (self.state, self.error) {
            (RunState::Completed, _) => Ok(self.saved),
            (RunState::Halted, _) if reason == PAUSED => Err(Error::Paused),
            (RunState::Cancelled, _) => Err(Error::Cancelled),
            (_, Some(error)) => Err(error),
            (RunState::ManualRequired, None) => Err(Error::ManualRequired(reason)),
            (RunState::Refused, None) => Err(Error::Refused(reason)),
            (_, None) => Err(Error::Failed(reason)),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub id: String,
    pub agent: Agent,
    pub state: RunState,
    pub step: i64,
    pub steps: i64,
    pub reason: Option<String>,
    pub actor: String,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDetail {
    pub run: RunSummary,
    pub log: Vec<LogRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogRow {
    pub at: String,
    pub step: String,
    pub state: RunState,
    pub duration_ms: i64,
    pub provider: Option<String>,
    pub data_categories: Vec<DataCategory>,
    pub outcome: String,
    pub dropped: usize,
}

/// A run's bounds. Public only so tests can shorten them through [`start_with`].
#[doc(hidden)]
pub struct Limits {
    run_timeout: Duration,
    grace: Duration,
    watch: Duration,
    beat: Duration,
    stale: Duration,
}

impl Limits {
    fn of(agent: Agent) -> Limits {
        Limits {
            run_timeout: spec_of(agent).run_timeout,
            grace: GRACE,
            watch: WATCH,
            beat: HEARTBEAT,
            stale: STALE,
        }
    }

    pub fn for_test(run_timeout: Duration) -> Limits {
        Limits {
            run_timeout,
            grace: Duration::from_millis(50),
            watch: Duration::from_millis(10),
            beat: Duration::from_millis(20),
            stale: STALE,
        }
    }

    pub fn with_stale(self, stale: Duration) -> Limits {
        Limits { stale, ..self }
    }
}

/// A person starts, resumes, and cancels runs, whatever the adapter does.
pub(crate) fn person(actor: &str) -> Result<()> {
    match actor {
        "gui" | "cli" => Ok(()),
        _ => Err(Error::Refused("only_a_person".into())),
    }
}

pub async fn start(
    db: &Db,
    actor: &str,
    workflow: &dyn Workflow,
    cancel: CancellationToken,
) -> Result<Finished> {
    let limits = Limits::of(workflow.agent());
    start_with(db, actor, workflow, cancel, limits).await
}

/// `start` with chosen bounds. `start` is the only production caller.
#[doc(hidden)]
pub async fn start_with(
    db: &Db,
    actor: &str,
    workflow: &dyn Workflow,
    cancel: CancellationToken,
    limits: Limits,
) -> Result<Finished> {
    person(actor)?;
    pause::ensure_off(db).await?;
    store::reap(db, limits.stale).await?;
    let agent = workflow.agent();
    let steps = i64::try_from(workflow.steps().len()).unwrap_or(i64::MAX);
    let id = store::insert(db, agent, actor, &workflow.input(), steps).await?;
    let run = Run {
        db,
        id,
        agent,
        state: RunState::Queued,
        generation: 0,
        step: 0,
        epoch: pause::epoch(db.paths()),
        clock: Instant::now(),
    };
    execute(run, Checkpoint::default(), actor, workflow, cancel, limits).await
}

/// From the step after the last checkpoint, with a fresh budget. Only a `halted` run resumes.
pub async fn resume(
    db: &Db,
    actor: &str,
    run_id: &str,
    workflow: &dyn Workflow,
    cancel: CancellationToken,
) -> Result<Finished> {
    person(actor)?;
    pause::ensure_off(db).await?;
    reap_stale(db).await?;
    let stored = store::load(db, run_id).await?;
    let steps = i64::try_from(workflow.steps().len()).unwrap_or(i64::MAX);
    if stored.agent != workflow.agent() || stored.steps != steps {
        return Err(Error::Validation("this run has other steps".into()));
    }
    if !store::claim(db, actor, run_id, &stored).await? {
        return Err(Error::Validation("only a halted run can resume".into()));
    }
    let run = Run {
        db,
        id: run_id.to_owned(),
        agent: stored.agent,
        state: RunState::Checking,
        generation: stored.generation + 1,
        step: stored.step,
        epoch: pause::epoch(db.paths()),
        clock: Instant::now(),
    };
    let limits = Limits::of(stored.agent);
    execute(run, stored.checkpoint, actor, workflow, cancel, limits).await
}

/// Active runs whose process stopped answering move to `halted`, reason `interrupted`.
/// They never resume on their own. A process that was only asleep cannot undo this: its
/// next write or heartbeat finds another state, or after a resume another generation, so it
/// stops without touching the row.
pub async fn reap_stale(db: &Db) -> Result<usize> {
    store::reap(db, STALE).await
}

/// Newest first. `limit` is clamped to 1..=1000: SQLite reads a negative `LIMIT` as no limit.
pub async fn list(db: &Db, limit: i64) -> Result<Vec<RunSummary>> {
    reap_stale(db).await?;
    store::list(db, limit.clamp(1, LIST_LIMIT)).await
}

pub async fn get(db: &Db, run_id: &str) -> Result<RunDetail> {
    reap_stale(db).await?;
    Ok(RunDetail {
        run: store::summary(db, run_id).await?,
        log: store::log(db, run_id).await?,
    })
}

/// Why a run stopped being this process's to write.
enum Stop {
    /// Another handle moved the row, or a resume took it. What it says now stands.
    Lost,
    Failed(Error),
}

impl From<Error> for Stop {
    fn from(error: Error) -> Stop {
        Stop::Failed(error)
    }
}

/// One run as its process holds it.
struct Run<'a> {
    db: &'a Db,
    id: String,
    agent: Agent,
    state: RunState,
    /// Bumped by each resume. A row under a later one belongs to another process.
    generation: i64,
    step: i64,
    /// [`pause::epoch`] when this process took the run: a later one means a pause began.
    epoch: u64,
    /// Since the last state change: a log row's duration is the time spent reaching it.
    clock: Instant,
}

impl Run<'_> {
    async fn change(
        &mut self,
        to: RunState,
        step_name: &str,
        ctx: &Ctx<'_>,
        reason: Option<&str>,
        output: Option<&StepOutput>,
    ) -> std::result::Result<(), Stop> {
        let checkpoint = ctx.held().clone();
        let outcome = match (reason, to) {
            (Some(reason), _) => reason,
            (None, RunState::Cancelled) => "cancelled",
            (None, _) => "ok",
        };
        let moved = store::transition(
            self.db,
            &Change {
                id: &self.id,
                agent: self.agent,
                from: self.state,
                to,
                generation: self.generation,
                bump: false,
                step: self.step,
                step_name,
                checkpoint: &checkpoint,
                reason,
                outcome,
                provider: output.and_then(|output| output.provider.as_ref()),
                categories: output.map_or(&[], |output| output.categories),
                duration_ms: i64::try_from(self.clock.elapsed().as_millis()).unwrap_or(i64::MAX),
                dropped: output.map_or(0, |output| i64::try_from(output.dropped).unwrap_or(0)),
            },
        )
        .await?;
        if !moved {
            return Err(Stop::Lost);
        }
        self.state = to;
        self.clock = Instant::now();
        Ok(())
    }

    /// Ends the run with what `error` maps to. A cancel drops its partial result first, so a
    /// failed drop leaves the run to be reaped and cancelled again. Only while this process
    /// holds the row: the partial result of a lost run is its new owner's to finish.
    async fn end(
        &mut self,
        step_name: &str,
        ctx: &Ctx<'_>,
        workflow: &dyn Workflow,
        error: Error,
    ) -> std::result::Result<Finished, Stop> {
        let (state, reason) = ending(&error);
        if state == RunState::Cancelled {
            if !store::owns(self.db, &self.id, self.state, self.generation).await? {
                return Err(Stop::Lost);
            }
            let saved = ctx.held().clone();
            workflow.abandon(self.db, &saved).await?;
        }
        self.change(state, step_name, ctx, reason.as_deref(), None)
            .await?;
        Ok(self.finished(ctx, reason, Some(error)))
    }

    fn finished(&self, ctx: &Ctx<'_>, reason: Option<String>, error: Option<Error>) -> Finished {
        Finished {
            run_id: self.id.clone(),
            state: self.state,
            reason,
            saved: ctx.held().clone(),
            error,
        }
    }
}

/// The state and stored reason for an error. Only a listed word leaves it: an error's text
/// can quote a provider, a prompt, or a secret.
fn ending(error: &Error) -> (RunState, Option<String>) {
    match error {
        Error::Cancelled => (RunState::Cancelled, None),
        Error::Paused => (RunState::Halted, Some(PAUSED.into())),
        Error::ManualRequired(why) => {
            let known = RUN_REASONS.iter().chain(MANUAL_REASONS).any(|r| r == why);
            let reason = if known { why } else { error.code() };
            (RunState::ManualRequired, Some(reason.to_owned()))
        }
        Error::Refused(_) => (RunState::Refused, Some(run_reason(error).to_owned())),
        _ => (RunState::Failed, Some(run_reason(error).to_owned())),
    }
}

/// `5` is `SQLITE_BUSY`. The low byte is the primary code, so this takes the extended ones
/// too, like the `BUSY_SNAPSHOT` a deferred transaction meets when it starts to write.
fn is_busy(error: &Error) -> bool {
    let Error::Db(sqlx::Error::Database(database)) = error else {
        return false;
    };
    database.code().is_some_and(|code| {
        code == "SQLITE_BUSY" || code.parse::<i64>().is_ok_and(|code| code & 0xff == 5)
    })
}

/// Returns when the step should stop: the token fired, a cancel came through the row, or
/// another handle took the run, which alone returns true. A failed read costs one poll: a
/// busy database must not end a step that is still working.
async fn watch(run: &Run<'_>, cancel: &CancellationToken, limits: &Limits) -> bool {
    let mut beat = Instant::now();
    loop {
        sleep(limits.watch).await;
        if cancel.is_cancelled() {
            return false;
        }
        if store::cancel_requested(run.db, &run.id)
            .await
            .unwrap_or(false)
        {
            return false;
        }
        if pause::is_set(run.db).await.unwrap_or(false) {
            return false;
        }
        if beat.elapsed() >= limits.beat {
            beat = Instant::now();
            let held = store::heartbeat(run.db, &run.id, run.generation).await;
            if matches!(held, Ok(false)) {
                return true;
            }
        }
    }
}

/// What the overseer found while a step ran.
#[derive(Default)]
struct Overseen {
    timed_out: bool,
    lost: bool,
}

/// Polls the step beside its overseer, never inside one of the overseer's awaits, so a step
/// holding a write transaction is not stalled by a heartbeat waiting on that same lock.
async fn drive(
    run: &Run<'_>,
    step: BoxFuture<'_, Result<StepOutput>>,
    cancel: &CancellationToken,
    deadline: Instant,
    limits: &Limits,
    seen: &mut Overseen,
) -> Result<StepOutput> {
    let overseer = async {
        tokio::select! {
            () = sleep_until(deadline) => seen.timed_out = true,
            lost = watch(run, cancel, limits) => seen.lost = lost,
        }
        cancel.cancel();
        sleep(limits.grace).await;
    };
    tokio::select! {
        biased;
        result = step => result,
        () = overseer => Err(Error::Cancelled),
    }
}

/// Between attempts and steps, where no watch is running: the row can hold a cancel, and the
/// pause can be on. What stopped the run is the error to end it with. A person's cancel
/// outranks the pause, and a token fired with the pause on is the pause.
async fn stop_asked(run: &Run<'_>, ctx: &Ctx<'_>) -> Result<Option<Error>> {
    if store::cancel_requested(run.db, &run.id).await? {
        ctx.cancel.cancel();
        return Ok(Some(Error::Cancelled));
    }
    if pause::epoch(run.db.paths()) != run.epoch || pause::is_set(run.db).await? {
        ctx.cancel.cancel();
        return Ok(Some(Error::Paused));
    }
    Ok(ctx.cancel.is_cancelled().then_some(Error::Cancelled))
}

/// Whether the pause, not a person's cancel, is why a step ended. The row may not be written
/// yet, or may be cleared already, so a pause begun in this process since the run took
/// hold counts too. A failed read says no.
async fn paused(run: &Run<'_>) -> bool {
    let cancelled = store::cancel_requested(run.db, &run.id)
        .await
        .unwrap_or(false);
    let begun = pause::epoch(run.db.paths()) != run.epoch;
    !cancelled && (begun || pause::is_set(run.db).await.unwrap_or(false))
}

async fn execute(
    run: Run<'_>,
    saved: Checkpoint,
    actor: &str,
    workflow: &dyn Workflow,
    cancel: CancellationToken,
    limits: Limits,
) -> Result<Finished> {
    let db = run.db;
    let id = run.id.clone();
    let ctx = Ctx {
        db,
        actor,
        cancel,
        run_id: id.clone(),
        agent: run.agent,
        generation: run.generation,
        checkpoint: Mutex::new(saved),
    };
    match walk(run, &ctx, workflow, &limits).await {
        Ok(finished) => Ok(finished),
        Err(Stop::Failed(error)) => Err(error),
        Err(Stop::Lost) => {
            ctx.cancel.cancel();
            let stored = store::load(db, &id).await?;
            let held = !stored.state.is_terminal() && stored.state != RunState::Halted;
            Ok(Finished {
                run_id: id,
                state: stored.state,
                reason: stored.reason.or(held.then(|| TAKEN_OVER.to_owned())),
                saved: stored.checkpoint,
                error: None,
            })
        }
    }
}

async fn walk(
    mut run: Run<'_>,
    ctx: &Ctx<'_>,
    workflow: &dyn Workflow,
    limits: &Limits,
) -> std::result::Result<Finished, Stop> {
    let steps = workflow.steps();
    let spec = spec_of(run.agent);
    let deadline = Instant::now() + limits.run_timeout;
    let mut seen = Overseen::default();
    loop {
        // First: with every step done the work is done, and a cancel or a deadline that came
        // while the last one finished has nothing left to stop.
        let index = usize::try_from(run.step).unwrap_or(usize::MAX);
        let Some(def) = steps.get(index) else {
            run.change(RunState::Completed, RUN_STEP, ctx, None, None)
                .await?;
            return Ok(run.finished(ctx, None, None));
        };
        if seen.timed_out || Instant::now() >= deadline {
            ctx.cancel.cancel();
            return run.end(RUN_STEP, ctx, workflow, Error::Timeout).await;
        }
        if let Some(stop) = stop_asked(&run, ctx).await? {
            return run.end(RUN_STEP, ctx, workflow, stop).await;
        }

        // A resume has already entered Checking.
        if run.state != RunState::Checking {
            run.change(RunState::Checking, def.name, ctx, None, None)
                .await?;
        }
        if !def.needs.iter().all(|n| spec.permissions.contains(n)) {
            let denied = Error::Refused("permission_denied".into());
            return run.end(def.name, ctx, workflow, denied).await;
        }

        let mut attempt = 1;
        let output = loop {
            run.change(RunState::Running, def.name, ctx, None, None)
                .await?;
            let step = workflow.step(ctx, index);
            let result = drive(&run, step, &ctx.cancel, deadline, limits, &mut seen).await;
            if seen.lost {
                return Err(Stop::Lost);
            }
            match result {
                Ok(output) => break output,
                Err(error)
                    if is_busy(&error) && !ctx.cancel.is_cancelled() && attempt < def.attempts =>
                {
                    attempt += 1;
                    run.change(RunState::Retrying, def.name, ctx, Some("busy"), None)
                        .await?;
                    if let Some(stop) = stop_asked(&run, ctx).await? {
                        return run.end(def.name, ctx, workflow, stop).await;
                    }
                }
                Err(error) => {
                    let error = if seen.timed_out {
                        Error::Timeout
                    } else if paused(&run).await {
                        // Whatever a step says on its way out, the pause stopped it.
                        Error::Paused
                    } else if ctx.cancel.is_cancelled() && !matches!(error, Error::Paused) {
                        // Whatever a step says on its way out, the person asked it to stop.
                        Error::Cancelled
                    } else {
                        error
                    };
                    return run.end(def.name, ctx, workflow, error).await;
                }
            }
        };
        run.step += 1;
        run.change(RunState::Checkpointed, def.name, ctx, None, Some(&output))
            .await?;
    }
}
