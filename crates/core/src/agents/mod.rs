//! The agents, as the GUI and the CLI reach them. A workflow is deterministic Rust: a model
//! gets no tools, and nothing in its output starts anything (`docs/agents.md`).

use crate::ai::CancellationToken;
use crate::ai::router::Router;
use crate::applications::{self, Application};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::runner::{self, Finished, RunInput, RunState, store};

pub mod tracking;

use tracking::Tracking;

/// Bad input gets its plain error and no run, which also keeps a run's stored input to a
/// checked id, status, and date.
pub async fn set_status(
    db: &Db,
    actor: &str,
    application_id: &str,
    status: &str,
    on: Option<&str>,
    note: &str,
    cancel: CancellationToken,
) -> Result<Application> {
    applications::get(db, application_id).await?;
    applications::check_change(db, status, on, note).await?;
    let workflow = Tracking {
        application_id: application_id.to_owned(),
        status: status.to_owned(),
        on: on.map(str::to_owned),
        note: note.to_owned(),
    };
    runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    applications::get(db, application_id).await
}

/// Rebuilds the workflow from the stored input. Tracking's note is not stored, so a resumed
/// update carries an empty one.
pub async fn resume_run(
    db: &Db,
    _router: &Router,
    actor: &str,
    run_id: &str,
    cancel: CancellationToken,
) -> Result<Finished> {
    match store::load(db, run_id).await?.input {
        RunInput::Tracking {
            application_id,
            status,
            on,
        } => {
            let workflow = Tracking {
                application_id,
                status,
                on,
                note: String::new(),
            };
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        _ => Err(Error::Validation("this run cannot resume".into())),
    }
}

/// An active run gets a flag its process polls. A `halted` run has no process, so it moves
/// to `cancelled` here, with no router: only Analytics overrides `abandon`, so its case
/// belongs before the move, rebuilt from the stored input, when that workflow lands.
pub async fn cancel_run(db: &Db, actor: &str, run_id: &str) -> Result<()> {
    runner::person(actor)?;
    runner::reap_stale(db).await?;
    let stored = store::load(db, run_id).await?;
    let moved = if stored.state == RunState::Halted {
        store::cancel_halted(db, actor, run_id, &stored).await?
    } else if stored.state.is_terminal() {
        false
    } else {
        store::request_cancel(db, actor, run_id, stored.agent).await?
    };
    if moved {
        Ok(())
    } else {
        Err(Error::Validation("this run has ended".into()))
    }
}
