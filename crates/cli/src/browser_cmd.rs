//! The pinned browser runtime. `setup` starts a download, so the panel refuses it.

use clap::Subcommand;
use jobpilot_core::browser::runtime::{self, PLAYWRIGHT, PLAYWRIGHT_MCP, RuntimeState};
use jobpilot_core::{Db, Result};
use serde_json::json;

use crate::ai::with_interrupts;
use crate::{ACTOR, Output, out};

#[derive(Subcommand)]
pub enum BrowserCommand {
    /// Whether the browser runtime is installed.
    Status,
    /// Install the pinned Playwright MCP server and download Chromium. Needs Node 20 or later.
    Setup,
}

pub async fn browser(db: &Db, command: BrowserCommand) -> Result<Output> {
    match command {
        BrowserCommand::Status => {
            let (state, text) = match runtime::state(db).await? {
                RuntimeState::NoNode => (
                    "no_node",
                    "Node is not installed. Install Node 20 or later.".to_owned(),
                ),
                RuntimeState::NodeTooOld(found) => (
                    "node_too_old",
                    format!(
                        "Node {found} is too old. Install Node {} or later.",
                        runtime::NODE_MIN
                    ),
                ),
                RuntimeState::NotInstalled => (
                    "not_installed",
                    "browser runtime: not installed. Run `jobpilot browser setup`.".to_owned(),
                ),
                RuntimeState::WrongVersion(found) => (
                    "wrong_version",
                    format!(
                        "browser runtime: version {found}, expected {PLAYWRIGHT_MCP}. Run `jobpilot browser setup`."
                    ),
                ),
                RuntimeState::Ready(_) => (
                    "ready",
                    format!("browser runtime: ready (Playwright MCP {PLAYWRIGHT_MCP})"),
                ),
            };
            out(
                json!({"state": state, "playwright_mcp": PLAYWRIGHT_MCP, "playwright": PLAYWRIGHT}),
                text,
            )
        }
        BrowserCommand::Setup => {
            eprintln!(
                "installing Playwright MCP {PLAYWRIGHT_MCP} and Chromium; this downloads files"
            );
            with_interrupts(db, |cancel| runtime::setup(db, ACTOR, cancel)).await?;
            out(
                json!({"state": "ready"}),
                "browser runtime: ready".to_owned(),
            )
        }
    }
}
