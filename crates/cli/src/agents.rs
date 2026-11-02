//! Agent runs and their logs.

use clap::Subcommand;
use jobpilot_core::ai::router::Router;
use jobpilot_core::runner::{self, LogRow, RunSummary};
use jobpilot_core::{Db, Result, agents};
use serde::Serialize;
use serde_json::json;

use crate::ai::with_interrupts;
use crate::{ACTOR, Output, out};

#[derive(Subcommand)]
pub enum AgentsCommand {
    /// Newest first.
    List {
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    Show {
        id: String,
    },
    /// Every state a run went through, oldest first.
    Log {
        id: String,
    },
    /// Stop a run. A finished step stays finished.
    Cancel {
        id: String,
    },
    /// Continue a halted run after its last checkpoint.
    Resume {
        id: String,
    },
}

/// The snake_case name of a run value.
fn word(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn line(run: &RunSummary) -> String {
    format!(
        "{}  {}  {}  {} of {}  {}  {}",
        run.id,
        word(&run.agent),
        word(&run.state),
        run.step,
        run.steps,
        run.created_at,
        run.reason.as_deref().unwrap_or("-"),
    )
}

fn log_line(row: &LogRow) -> String {
    let categories: Vec<String> = row.data_categories.iter().map(word).collect();
    format!(
        "{}  {}  {}  {} ms  {}  {}  {}",
        row.at,
        row.step,
        word(&row.state),
        row.duration_ms,
        row.provider.as_deref().unwrap_or("-"),
        if categories.is_empty() {
            "-".to_owned()
        } else {
            categories.join(",")
        },
        row.outcome,
    )
}

pub async fn agents(db: &Db, command: AgentsCommand) -> Result<Output> {
    match command {
        AgentsCommand::List { limit } => {
            let runs = runner::list(db, limit).await?;
            let text = runs.iter().map(line).collect::<Vec<_>>().join("\n");
            out(runs, text)
        }
        AgentsCommand::Show { id } => {
            let run = runner::get(db, &id).await?.run;
            out(&run, line(&run))
        }
        AgentsCommand::Log { id } => {
            let log = runner::get(db, &id).await?.log;
            let text = log.iter().map(log_line).collect::<Vec<_>>().join("\n");
            out(log, text)
        }
        AgentsCommand::Cancel { id } => {
            agents::cancel_run(db, ACTOR, &id).await?;
            out(json!({ "id": id }), format!("cancel asked for {id}"))
        }
        AgentsCommand::Resume { id } => {
            let router = Router::from_settings(db).await?;
            let finished = with_interrupts(db, |cancel| {
                agents::resume_run(db, &router, ACTOR, &id, cancel)
            })
            .await?;
            let state = word(&finished.state);
            finished.into_result()?;
            out(
                json!({ "id": id, "state": state }),
                format!("{id} is {state}"),
            )
        }
    }
}
