//! The agents, as the GUI and the CLI reach them. A workflow is deterministic Rust: a model
//! gets no tools, and nothing in its output starts anything (`docs/agents.md`).

use crate::ai::router::Router;
use crate::ai::{CancellationToken, TaskType};
use crate::applications::{self, Application};
use crate::connectors::fetch::Fetcher;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::runner::{self, Finished, RunInput, RunState, Workflow, store};

pub mod analytics;
pub mod browser;
pub mod claude_apply;
pub mod discovery;
pub mod draft;
pub mod matching;
pub mod memory;
pub mod research;
pub mod tracking;

use crate::analytics::Report;
use analytics::Analytics;
pub use browser::{BrowserWorkflow, autonomous, copilot};
#[doc(hidden)]
pub use browser::{autonomous_with, copilot_with};
#[doc(hidden)]
pub use claude_apply::claude_apply_with;
pub use claude_apply::{ClaudeApply, claude_apply};
use discovery::DiscoveryWorkflow;
pub use discovery::{DiscoveryReport, SourceResult, discover, discover_with};
use draft::DraftWorkflow;
pub use draft::{draft_cover_letter, draft_resume};
use matching::MatchingWorkflow;
pub use matching::{AiPlan, MATCH_BATCH, MatchReport, MatchRequest, match_jobs, match_plan};
use memory::MemoryWorkflow;
pub use memory::{MemoryRefreshed, memory_refresh};
use research::ResearchWorkflow;
pub use research::{CompanyResearch, research_company, research_plan};
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
    set_status_with(db, actor, application_id, status, on, note, false, cancel).await
}

/// [`set_status`], and for a `manual` record of `applied`, an `application_submitted` row in
/// the status write's own transaction, when no such row exists for the application.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn set_status_with(
    db: &Db,
    actor: &str,
    application_id: &str,
    status: &str,
    on: Option<&str>,
    note: &str,
    manual: bool,
    cancel: CancellationToken,
) -> Result<Application> {
    let app = applications::get(db, application_id).await?;
    applications::check_change(db, status, on, note).await?;
    let workflow = match manual {
        true => Tracking::manual(&app, status, on, note),
        false => Tracking::new(application_id, status, on, note),
    };
    runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    applications::get(db, application_id).await
}

/// Builds the figures from the application store and returns them. A cancel drops the
/// report; a finished one stays until the next.
pub async fn analytics_report(db: &Db, actor: &str, cancel: CancellationToken) -> Result<Report> {
    runner::person(actor)?;
    crate::pause::ensure_off(db).await?;
    crate::analytics::drop_unfinished(db).await?;
    let saved = runner::start(db, actor, &Analytics, cancel)
        .await?
        .into_result()?;
    let id = saved
        .get("report")
        .ok_or_else(|| Error::Failed("report_missing".into()))?;
    crate::analytics::finished(db, id).await
}

/// Rebuilds the workflow from the stored input. Tracking's note is not stored, so a resumed
/// update carries an empty one. A status moved on since the run began is refused, because
/// a resume must not rewind it: the run stays halted, for the person to cancel.
pub async fn resume_run(
    db: &Db,
    router: &Router,
    actor: &str,
    run_id: &str,
    cancel: CancellationToken,
) -> Result<Finished> {
    resume_from(db, router, None, actor, run_id, cancel).await
}

/// [`resume_run`] with the fetcher a discovery run reads through, so a test can point it at a
/// loopback server.
#[doc(hidden)]
pub async fn resume_run_with(
    db: &Db,
    router: &Router,
    fetcher: &Fetcher,
    actor: &str,
    run_id: &str,
    cancel: CancellationToken,
) -> Result<Finished> {
    resume_from(db, router, Some(fetcher), actor, run_id, cancel).await
}

/// Only a discovery run needs a fetcher, so only it builds the hosted one when none is given.
async fn resume_from(
    db: &Db,
    router: &Router,
    fetcher: Option<&Fetcher>,
    actor: &str,
    run_id: &str,
    cancel: CancellationToken,
) -> Result<Finished> {
    let stored = store::load(db, run_id).await?;
    match stored.input {
        RunInput::Tracking {
            application_id,
            status,
            on,
            manual,
        } => {
            let current = applications::get(db, &application_id).await?;
            if current.status != status
                && applications::status_changed_since(db, &application_id, &stored.created_at)
                    .await?
            {
                return Err(Error::Validation(
                    "this application's status changed after the run began; cancel the run".into(),
                ));
            }
            // A Manual record is checked again as `record_outcome` checks it, and its
            // submission row is rebuilt from the application as it is now.
            let workflow = match manual {
                true => {
                    crate::apply::refuse_in_a_run(db, &application_id).await?;
                    Tracking::manual(&current, &status, on.as_deref(), "")
                }
                false => Tracking::new(&application_id, &status, on.as_deref(), ""),
            };
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        RunInput::Analytics => runner::resume(db, actor, run_id, &Analytics, cancel).await,
        RunInput::Resume {
            job_text_id,
            export,
        } => {
            let workflow = DraftWorkflow::new(TaskType::DraftResume, &job_text_id, export, router);
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        RunInput::CoverLetter {
            job_text_id,
            export,
        } => {
            let task = TaskType::DraftCoverLetter;
            let workflow = DraftWorkflow::new(task, &job_text_id, export, router);
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        RunInput::Discovery { source_ids } => {
            let hosted;
            let fetcher = match fetcher {
                Some(fetcher) => fetcher,
                None => {
                    hosted = Fetcher::hosted()?;
                    &hosted
                }
            };
            let workflow = DiscoveryWorkflow::new(source_ids, fetcher);
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        RunInput::Matching { job_ids, ai } => {
            let workflow = MatchingWorkflow::new(router, job_ids, ai, true);
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        RunInput::Research { company_id } => {
            let workflow = ResearchWorkflow::new(router, company_id, true);
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
        RunInput::BrowserApply { .. } | RunInput::ClaudeApply { .. } => Err(Error::Validation(
            "an apply run does not resume; start a new run".into(),
        )),
        RunInput::MemoryRefresh => {
            let workflow = MemoryWorkflow::new(router, true);
            runner::resume(db, actor, run_id, &workflow, cancel).await
        }
    }
}

/// An active run gets a flag its process polls. A `halted` run has no process, so it moves
/// to `cancelled` here, with no router. Only Analytics overrides `abandon`, so its partial
/// report is dropped first, from the stored checkpoint: a failed drop leaves the run halted,
/// to be cancelled again.
pub async fn cancel_run(db: &Db, actor: &str, run_id: &str) -> Result<()> {
    runner::person(actor)?;
    runner::reap_stale(db).await?;
    let stored = store::load(db, run_id).await?;
    let moved = if stored.state == RunState::Halted {
        if matches!(stored.input, RunInput::Analytics) {
            Analytics.abandon(db, &stored.checkpoint).await?;
        }
        store::cancel_halted(db, actor, run_id, &stored).await?
    } else if stored.state.is_terminal() {
        false
    } else {
        store::request_cancel(db, actor, run_id).await?
    };
    if moved {
        Ok(())
    } else {
        Err(Error::Validation("this run has ended".into()))
    }
}
