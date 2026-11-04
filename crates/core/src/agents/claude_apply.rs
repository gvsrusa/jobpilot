//! The Claude Code apply session as a workflow: `prepare` checks what the session needs and
//! lays out its folder, `session` runs `claude` until it ends. Whatever the ending, the host
//! then settles every item and cleans up, so a pause, a cancel, and an early exit end alike.

use std::sync::{Mutex, PoisonError};

use crate::ai::claude_code;
use crate::ai::{BoxFuture, CancellationToken, ProviderId};
use crate::apply::session::{self, HostKnobs};
use crate::apply::{self, RunView};
use crate::browser::runtime::{self, RuntimeState};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::runner::{self, Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

pub struct ClaudeApply {
    pub apply_id: String,
}

/// `ClaudeApply` as a host runs it: with test settings, and the agent run whose `prepare`
/// claimed the apply run, so the host knows the run is its own whatever the runner returns.
struct Hosted<'k> {
    apply_id: String,
    knobs: &'k HostKnobs,
    claimed: Mutex<Option<String>>,
}

static STEPS: &[StepDef] = &[
    StepDef {
        name: "prepare",
        needs: &[Permission::ReadApplications, Permission::BrowserDrive],
        attempts: 3,
    },
    // One attempt: a session cannot be run again.
    StepDef {
        name: "session",
        needs: &[
            Permission::ReadProfileBasic,
            Permission::ReadCareerMemory,
            Permission::ReadJobText,
            Permission::WriteApplications,
            Permission::BrowserDrive,
            Permission::BrowserFill,
            Permission::BrowserUpload,
            Permission::BrowserSubmit,
        ],
        attempts: 1,
    },
];

fn step<'a>(
    apply_id: &'a str,
    knobs: &'a HostKnobs,
    claimed: Option<&'a Mutex<Option<String>>>,
    ctx: &'a Ctx<'a>,
    index: usize,
) -> BoxFuture<'a, Result<StepOutput>> {
    Box::pin(async move {
        match index {
            0 => prepare(ctx, apply_id, knobs, claimed).await,
            _ => run_session(ctx, apply_id).await,
        }
    })
}

impl Workflow for ClaudeApply {
    fn agent(&self) -> Agent {
        Agent::ClaudeApply
    }

    fn input(&self) -> RunInput {
        RunInput::ClaudeApply {
            apply_id: self.apply_id.clone(),
        }
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        step(&self.apply_id, &HostKnobs::NONE, None, ctx, index)
    }
}

impl Workflow for Hosted<'_> {
    fn agent(&self) -> Agent {
        Agent::ClaudeApply
    }

    fn input(&self) -> RunInput {
        RunInput::ClaudeApply {
            apply_id: self.apply_id.clone(),
        }
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        step(&self.apply_id, self.knobs, Some(&self.claimed), ctx, index)
    }
}

/// The run is this host's from here, so a second host for it stops at this line. Then the
/// browser runtime and `claude` must be there, and the folders are laid out.
async fn prepare(
    ctx: &Ctx<'_>,
    apply_id: &str,
    knobs: &HostKnobs,
    claimed: Option<&Mutex<Option<String>>>,
) -> Result<StepOutput> {
    let db = ctx.db;
    let claimed_rows = sqlx::query(
        "UPDATE apply_runs SET agent_run_id = ? \
         WHERE id = ? AND state = 'ready' AND (agent_run_id IS NULL OR agent_run_id = ?)",
    )
    .bind(ctx.run_id())
    .bind(apply_id)
    .bind(ctx.run_id())
    .execute(&db.pool)
    .await?
    .rows_affected();
    if claimed_rows == 0 {
        return Err(Error::Refused("apply_run_not_ready".into()));
    }
    if let Some(claimed) = claimed {
        *claimed.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx.run_id().to_owned());
    }
    let cli = knobs.playwright_cli();
    let get = |name: &str| match (name, &cli) {
        ("JOBPILOT_PLAYWRIGHT_CLI", Some(cli)) => Some(cli.clone()),
        _ => std::env::var_os(name),
    };
    if !matches!(runtime::state_by(db, &get).await?, RuntimeState::Ready(_)) {
        return Err(Error::ManualRequired("no_browser_runtime".into()));
    }
    claude_code::find_binary(Some(db))
        .await
        .map_err(|_| Error::ManualRequired("claude_not_found".into()))?;
    session::prepare_folders(db, apply_id, &knobs.sidecar()?)?;
    // Who holds the claim, for whoever kills this process by pid (the app's panel).
    apply::record_host(
        &apply::run_dir(db, apply_id),
        std::process::id(),
        ctx.run_id(),
    )?;
    let running = sqlx::query(
        "UPDATE apply_runs SET state = 'running' \
         WHERE id = ? AND state = 'ready' AND agent_run_id = ?",
    )
    .bind(apply_id)
    .bind(ctx.run_id())
    .execute(&db.pool)
    .await?
    .rows_affected();
    if running == 0 {
        return Err(Error::Refused("apply_run_not_ready".into()));
    }
    Ok(StepOutput::default())
}

async fn run_session(ctx: &Ctx<'_>, apply_id: &str) -> Result<StepOutput> {
    let db = ctx.db;
    let bin = claude_code::find_binary(Some(db))
        .await
        .map_err(|_| Error::ManualRequired("claude_not_found".into()))?;
    let dir = apply::run_dir(db, apply_id);
    session::run_claude(&bin, &dir, db, apply_id, ctx.cancel.clone()).await?;
    Ok(StepOutput {
        provider: Some(ProviderId::ClaudeCode),
        categories: &apply::CATEGORIES,
        dropped: 0,
    })
}

/// A person's act: hosts the session of a `ready` run in this process, on this terminal. A
/// run that has started or ended is refused, so a halted run never resumes. Returns the run as
/// it ended, or what stopped it, once every item is settled and the run's processes, folder,
/// and transcripts are gone.
pub async fn claude_apply(
    db: &Db,
    actor: &str,
    apply_id: &str,
    cancel: CancellationToken,
) -> Result<RunView> {
    host(db, actor, apply_id, cancel, &HostKnobs::NONE).await
}

#[doc(hidden)]
pub async fn claude_apply_with(
    db: &Db,
    actor: &str,
    apply_id: &str,
    cancel: CancellationToken,
    knobs: &HostKnobs,
) -> Result<RunView> {
    host(db, actor, apply_id, cancel, knobs).await
}

async fn host(
    db: &Db,
    actor: &str,
    apply_id: &str,
    cancel: CancellationToken,
    knobs: &HostKnobs,
) -> Result<RunView> {
    runner::person(actor)?;
    crate::pause::ensure_off(db).await?;
    session::sweep_orphans(db).await?;
    let state: String = sqlx::query_scalar("SELECT state FROM apply_runs WHERE id = ?")
        .bind(apply_id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or(Error::NotFound)?;
    if state != "ready" {
        return Err(Error::Validation(
            "this run has already started or ended; start a new run".into(),
        ));
    }
    let workflow = Hosted {
        apply_id: apply_id.to_owned(),
        knobs,
        claimed: Mutex::new(None),
    };
    let started = match knobs.run_timeout() {
        Some(timeout) => {
            let limits = runner::Limits::for_test(timeout);
            runner::start_with(db, actor, &workflow, cancel, limits).await
        }
        None => runner::start(db, actor, &workflow, cancel).await,
    };

    // Only the host that holds the run ends it: a second host refused in `prepare` must not.
    // The claim is read from `prepare` itself, so a runner error does not hide it.
    let holder: Option<String> =
        sqlx::query_scalar("SELECT agent_run_id FROM apply_runs WHERE id = ?")
            .bind(apply_id)
            .fetch_optional(&db.pool)
            .await?
            .flatten();
    let claimed = workflow
        .claimed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let ours = match &holder {
        None => true,
        Some(holder) => claimed.as_ref() == Some(holder),
    };
    if ours {
        // A `claude` that failed leaves its reason on every item it did not finish.
        let because = match &started {
            Ok(finished) if finished.reason.as_deref() == Some(session::CLAUDE_EXITED) => {
                Some(session::CLAUDE_EXITED)
            }
            _ => None,
        };
        let bin = claude_code::find_binary(Some(db)).await.ok();
        session::wind_up(db, apply_id, bin.as_deref(), because).await?;
    }
    started?.into_result()?;
    apply::view(db, apply_id).await
}
