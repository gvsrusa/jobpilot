//! Claude Code apply runs. `start` and `session` host a tool-enabled session in this terminal,
//! so the panel refuses them, as it does `apply-mcp`. The rest read, stop, or finish.

use std::io::Write;
use std::path::Path;

use clap::{Subcommand, ValueEnum};
use jobpilot_core::ai::CancellationToken;
use jobpilot_core::apply::{self, Preflight, Resolution, RunView};
use jobpilot_core::{Db, Result, agents};
use serde_json::json;

use crate::{ACTOR, Output, out, text};

#[derive(Subcommand)]
pub enum ApplyCommand {
    /// Check applications before a run: files, answers, the browser runtime, and Claude Code.
    Preflight {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Start a run over these applications and host its Claude Code session in this terminal.
    Start {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Release the sealed answers and work authorization the preflight counted, for
        /// this run only.
        #[arg(long)]
        release_sealed: bool,
    },
    /// Host the session of a run that is ready. The app's terminal starts this.
    Session {
        id: String,
    },
    /// The newest run.
    Status,
    Show {
        id: String,
    },
    /// End a pause after you handled it in the browser.
    Resolve {
        pause: String,
        resolution: ResolutionArg,
    },
    /// Write the statuses a run left unwritten, as after an emergency pause.
    Settle {
        id: String,
    },
    /// Stop a run.
    Cancel {
        id: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ResolutionArg {
    Continue,
    TakeOver,
}

pub async fn apply(db: &Db, command: ApplyCommand) -> Result<Output> {
    match command {
        ApplyCommand::Preflight { ids } => {
            let checked = apply::preflight(db, &ids).await?;
            let text = preflight_text(&checked);
            out(checked, text)
        }
        ApplyCommand::Start {
            ids,
            release_sealed,
        } => {
            let checked = apply::preflight(db, &ids).await?;
            println!("{}", text::sanitize(&preflight_text(&checked)));
            let id = apply::start(db, ACTOR, &ids, release_sealed).await?;
            println!("run {id}: starting Claude Code");
            host(db, &id).await
        }
        ApplyCommand::Session { id } => host(db, &id).await,
        ApplyCommand::Status => match apply::latest(db).await? {
            Some(run) => {
                let text = summary(&run);
                out(run, text)
            }
            None => out(json!(null), "no apply run yet".to_owned()),
        },
        ApplyCommand::Show { id } => {
            let run = apply::view(db, &id).await?;
            let text = summary(&run);
            out(run, text)
        }
        ApplyCommand::Resolve { pause, resolution } => {
            let resolution = match resolution {
                ResolutionArg::Continue => Resolution::Continue,
                ResolutionArg::TakeOver => Resolution::TakeOver,
            };
            apply::resolve_pause(db, ACTOR, &pause, resolution).await?;
            out(
                json!({"pause": pause, "resolution": resolution}),
                format!("pause {pause}: {}", resolution.as_str()),
            )
        }
        ApplyCommand::Settle { id } => {
            let settled = apply::settle(db, ACTOR, &id).await?;
            out(
                json!({"id": id, "settled": settled}),
                format!("run {id}: {settled} written"),
            )
        }
        ApplyCommand::Cancel { id } => {
            apply::cancel(db, ACTOR, &id).await?;
            out(json!({"id": id}), format!("cancel asked for run {id}"))
        }
    }
}

/// Runs the session on this terminal, then prints one line per item, however it ended. Ctrl-C,
/// a closed terminal or window (SIGHUP, a close event), and a quitting app or system (SIGTERM,
/// a shutdown event) all cancel the run and wait while it winds up: the items settle and the
/// session's processes and folder go. Only a second Ctrl-C gives up waiting. The emergency
/// pause reaches the run through the runner's own watch.
async fn host(db: &Db, id: &str) -> Result<Output> {
    // Before the session starts, so a signal in its first moments is not the default kill.
    let mut stops = Stops::new()?;
    let token = CancellationToken::new();
    let task = agents::claude_apply(db, ACTOR, id, token.clone());
    tokio::pin!(task);
    let hosted = tokio::select! {
        done = &mut task => done,
        () = stops.next() => {
            token.cancel();
            tokio::select! {
                done = &mut task => done,
                () = second_ctrl_c() => std::process::exit(130),
            }
        }
    };
    match hosted {
        Ok(run) => {
            let text = summary(&run);
            out(run, text)
        }
        Err(error) => {
            if let Ok(run) = apply::view(db, id).await {
                // The terminal may be gone: a failed write is not an error here.
                let _ = writeln!(
                    std::io::stdout().lock(),
                    "{}",
                    text::sanitize(&summary(&run))
                );
            }
            Err(error)
        }
    }
}

async fn second_ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The signals that end a hosted session.
struct Stops {
    #[cfg(unix)]
    hangup: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    shutdown: tokio::signal::windows::CtrlShutdown,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
}

impl Stops {
    #[cfg(unix)]
    fn new() -> Result<Stops> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Stops {
            hangup: signal(SignalKind::hangup())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    #[cfg(windows)]
    fn new() -> Result<Stops> {
        use tokio::signal::windows;
        Ok(Stops {
            close: windows::ctrl_close()?,
            shutdown: windows::ctrl_shutdown()?,
            ctrl_break: windows::ctrl_break()?,
        })
    }

    #[cfg(unix)]
    async fn next(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.hangup.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }

    #[cfg(windows)]
    async fn next(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.close.recv() => {}
            _ = self.shutdown.recv() => {}
            _ = self.ctrl_break.recv() => {}
        }
    }
}

/// Hidden: the MCP server a session talks to. Stdout is the protocol, so nothing else is
/// written there.
pub async fn mcp(db: &Db, dir: &Path) -> Result<()> {
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    apply::proxy::serve(db, dir, input, tokio::io::stdout()).await
}

fn summary(run: &RunView) -> String {
    let mut lines = vec![format!("run {}: {}", run.id, run.state)];
    for item in &run.items {
        let mut line = format!("{}. {}: {}", item.position + 1, item.host, item.state);
        if let Some(reason) = &item.reason {
            line.push_str(&format!(" ({reason})"));
        }
        lines.push(line);
    }
    if let Some(pause) = &run.active_pause {
        lines.push(format!(
            "waiting for you: pause {} ({}). Handle it in the browser, then `jobpilot apply \
             resolve {} continue`.",
            pause.id, pause.kind, pause.id
        ));
    }
    lines.join("\n")
}

fn preflight_text(checked: &Preflight) -> String {
    let mut lines = Vec::new();
    for item in &checked.items {
        let mut line = format!(
            "{} at {} ({}): {} answers, {} sealed",
            item.role, item.company, item.host, item.answers, item.sealed
        );
        if !item.blocks.is_empty() {
            line.push_str(&format!(", blocked: {}", item.blocks.join(", ")));
        }
        lines.push(line);
    }
    lines.push(format!("browser runtime: {}", checked.runtime));
    lines.push(format!(
        "claude: {}",
        checked.claude.as_deref().unwrap_or("not found")
    ));
    if !checked.consent_missing.is_empty() {
        lines.push(format!(
            "consent missing: {}",
            serde_json::to_string(&checked.consent_missing).unwrap_or_default()
        ));
    }
    lines.join("\n")
}
