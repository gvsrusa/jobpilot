//! The Claude Code apply session as a workflow: `prepare` checks what the session needs and
//! lays out its folder, `session` runs `claude` until it ends. Whatever the ending, the host
//! then settles every item and cleans up, so a pause, a cancel, and an early exit end alike.

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

/// `ClaudeApply` with test settings.
struct Hosted<'k> {
    apply_id: String,
    knobs: &'k HostKnobs,
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
    ctx: &'a Ctx<'a>,
    index: usize,
) -> BoxFuture<'a, Result<StepOutput>> {
    Box::pin(async move {
        match index {
            0 => prepare(ctx, apply_id, knobs).await,
            _ => run_session(ctx, apply_id, knobs).await,
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
        step(&self.apply_id, &HostKnobs::NONE, ctx, index)
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
        step(&self.apply_id, self.knobs, ctx, index)
    }
}

/// The run is this host's from here, so a second host for it stops at this line. Then the
/// browser runtime and `claude` must be there, and the folders are laid out.
async fn prepare(ctx: &Ctx<'_>, apply_id: &str, knobs: &HostKnobs) -> Result<StepOutput> {
    let db = ctx.db;
    let claimed = sqlx::query(
        "UPDATE apply_runs SET agent_run_id = ? \
         WHERE id = ? AND state = 'ready' AND (agent_run_id IS NULL OR agent_run_id = ?)",
    )
    .bind(ctx.run_id())
    .bind(apply_id)
    .bind(ctx.run_id())
    .execute(&db.pool)
    .await?
    .rows_affected();
    if claimed == 0 {
        return Err(Error::Refused("apply_run_not_ready".into()));
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

async fn run_session(ctx: &Ctx<'_>, apply_id: &str, knobs: &HostKnobs) -> Result<StepOutput> {
    let db = ctx.db;
    let bin = claude_code::find_binary(Some(db))
        .await
        .map_err(|_| Error::ManualRequired("claude_not_found".into()))?;
    let dir = apply::run_dir(db, apply_id);
    let sidecar = knobs.sidecar()?;
    session::run_claude(&bin, &dir, &sidecar, db, apply_id, ctx.cancel.clone()).await?;
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
    };
    let started = runner::start(db, actor, &workflow, cancel).await;

    // Only the host that holds the run ends it: a second host refused in `prepare` must not.
    let holder: Option<String> =
        sqlx::query_scalar("SELECT agent_run_id FROM apply_runs WHERE id = ?")
            .bind(apply_id)
            .fetch_optional(&db.pool)
            .await?
            .flatten();
    let ours = match (&started, &holder) {
        (_, None) => true,
        (Ok(finished), Some(holder)) => *holder == finished.run_id,
        (Err(_), Some(_)) => false,
    };
    if ours {
        let bin = claude_code::find_binary(Some(db)).await.ok();
        session::wind_up(db, apply_id, bin.as_deref()).await?;
    }
    started?.into_result()?;
    apply::view(db, apply_id).await
}
