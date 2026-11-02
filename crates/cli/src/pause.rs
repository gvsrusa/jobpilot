//! The emergency pause. Setting it is allowed in the terminal panel, because it only
//! stops things. Clearing it is not: the app confirms that in a native dialog.

use clap::Subcommand;
use jobpilot_core::pause::{self, PauseState};
use jobpilot_core::{Db, Result};

use crate::{ACTOR, Output, out};

#[derive(Subcommand)]
pub enum PauseCommand {
    /// Stop everything automated now, in every JobPilot process.
    Set,
    /// Whether the pause is on.
    Status,
    /// Turn the pause off. Halted runs stay halted until you resume each one.
    Clear,
}

/// The line `status` and `pause status` print.
pub fn describe(state: &PauseState) -> String {
    match (&state.set_at, &state.set_by) {
        (Some(at), Some(by)) if state.paused => format!("paused: yes, set {at} by {by}"),
        _ => "paused: no".to_owned(),
    }
}

pub async fn pause(db: &Db, command: PauseCommand) -> Result<Output> {
    let state = match command {
        PauseCommand::Set => pause::set(db, ACTOR).await?,
        PauseCommand::Status => pause::state(db).await?,
        PauseCommand::Clear => pause::clear(db, ACTOR).await?,
    };
    let text = describe(&state);
    out(&state, text)
}
