//! Claude Code apply runs. `start` and `session` host a tool-enabled session in this terminal,
//! so the panel refuses them, as it does `apply-mcp`, `copilot`, which starts a browser, and
//! `autonomous grant`, which widens what may run. The rest read, stop, take away, or finish.

use std::io::Write;
use std::path::Path;

use clap::{Subcommand, ValueEnum};
use jobpilot_core::ai::CancellationToken;
use jobpilot_core::ai::router::Router;
use jobpilot_core::apply::{self, Material, Preflight, Resolution, RunView};
use jobpilot_core::{Db, Result, agents, permissions};
use serde_json::json;

use crate::ai::with_interrupts;
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
    /// Manual mode: the files, approved answers, and mode notes for one application. No
    /// answer text is shown; take one with `memory use`.
    Material {
        application: String,
    },
    /// Copilot: fill one application's form in a visible browser and stop for you to review
    /// and submit it. It never clicks submit. End its stops with `apply resolve`.
    Copilot {
        application: String,
    },
    /// Autonomous mode: the connectors you authorized. A grant alone makes no site eligible:
    /// a site also needs an allowed register row and a verified submit, and none has one.
    #[command(subcommand)]
    Autonomous(AutonomousCommand),
    /// Manual mode: record what happened after you applied by hand.
    Record {
        application: String,
        /// saved, applied, screening, interviewing, offer, accepted, rejected, or withdrawn.
        #[arg(long)]
        outcome: String,
        /// The day you applied, YYYY-MM-DD.
        #[arg(long)]
        on: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(Subcommand)]
pub enum AutonomousCommand {
    /// The connectors you authorized.
    List,
    /// Authorize Autonomous submission on a connector: greenhouse, lever, ashby, workday,
    /// indeed, or linkedin.
    Grant { connector: String },
    /// Take an authorization away.
    Revoke { connector: String },
    /// Fill and submit one application with no review by you. Refused, with nothing
    /// started, unless its site's register row allows it, its submit is verified, and you
    /// authorized its connector: today no site qualifies. End its stops with `apply resolve`.
    Run { application: String },
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
        ApplyCommand::Status => match apply::latest_swept(db).await? {
            Some(run) => {
                let text = summary(&run);
                out(run, text)
            }
            None => out(json!(null), "no apply run yet".to_owned()),
        },
        ApplyCommand::Show { id } => {
            let run = apply::view_swept(db, &id).await?;
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
        ApplyCommand::Copilot { application } => {
            let router = Router::from_settings(db).await?;
            eprintln!(
                "Copilot is filling the form in a browser window. It stops for you there: \
                 `jobpilot apply status` shows the stop, and `jobpilot apply resolve <pause> \
                 continue` (after you submitted) or `take-over` ends it."
            );
            let run = with_interrupts(db, |cancel| {
                agents::copilot(db, &router, ACTOR, &application, cancel)
            })
            .await?;
            let text = summary(&run);
            out(run, text)
        }
        ApplyCommand::Autonomous(command) => autonomous(db, command).await,
        ApplyCommand::Material { application } => {
            let material = apply::material(db, &application).await?;
            let text = material_text(&material);
            // The panel may run this, so the stated work-authorization values stay out of the
            // JSON as they do of the text: only whether the country has a stated row.
            let mut shown = serde_json::to_value(&material).map_err(std::io::Error::other)?;
            shown["authorization"] = json!({
                "country": material.authorization.country,
                "stated": material.authorization.stated,
            });
            out(shown, text)
        }
        ApplyCommand::Record {
            application,
            outcome,
            on,
            note,
        } => {
            let saved = with_interrupts(db, |cancel| {
                apply::record_outcome(
                    db,
                    ACTOR,
                    &application,
                    &outcome,
                    on.as_deref(),
                    &note,
                    cancel,
                )
            })
            .await?;
            out(&saved, format!("{} is {}", saved.id, saved.status))
        }
    }
}

async fn autonomous(db: &Db, command: AutonomousCommand) -> Result<Output> {
    let (connector, text) = match command {
        AutonomousCommand::List => {
            let granted = permissions::grants(db).await?;
            let text = match granted.is_empty() {
                true => "no connector authorized for Autonomous".to_owned(),
                false => format!("authorized for Autonomous: {}", granted.join(", ")),
            };
            return out(json!({"grants": granted}), text);
        }
        AutonomousCommand::Run { application } => {
            agents::autonomous_check(db, &application).await?;
            eprintln!(
                "Autonomous is filling the form in a browser window and will submit it once. It \
                 stops for you there: `jobpilot apply status` shows the stop."
            );
            let run = with_interrupts(db, |cancel| {
                agents::autonomous(db, ACTOR, &application, cancel)
            })
            .await?;
            let text = summary(&run);
            return out(run, text);
        }
        AutonomousCommand::Grant { connector } => {
            permissions::grant(db, ACTOR, &connector).await?;
            let text = format!(
                "{connector}: authorized for Autonomous. A site is eligible only when its \
                 register row allows it and its submit is verified as well."
            );
            (connector, text)
        }
        AutonomousCommand::Revoke { connector } => {
            permissions::revoke(db, ACTOR, &connector).await?;
            let text = format!("{connector}: not authorized for Autonomous");
            (connector, text)
        }
    };
    let granted = permissions::grants(db).await?;
    out(json!({"connector": connector, "grants": granted}), text)
}

/// Names and states only: a sealed answer's text and the stated authorization values stay
/// out, as the panel may show this.
fn material_text(material: &Material) -> String {
    let mut lines = Vec::new();
    for mode in &material.modes {
        let state = match mode.available {
            true => "available",
            false => "not available",
        };
        lines.push(format!("{}: {state}. {}", mode.mode, mode.note));
    }
    let file = |name: &str, path: &Option<std::path::PathBuf>| match path {
        Some(path) => format!("{name}: {}", path.display()),
        None => format!("{name}: nothing prepared"),
    };
    lines.push(file("resume", &material.resume));
    lines.push(file("cover letter", &material.letter));
    if material.answers.is_empty() {
        lines.push("No approved answers for this form. You can fill it by hand.".into());
    }
    for row in &material.answers {
        let mut state = match row.needs_confirmation {
            true => format!("{}: confirm before use", row.class.to_uppercase()),
            false => format!("{}, approved", row.class),
        };
        if row.stale {
            state.push_str(", not reviewed for over 180 days");
        }
        lines.push(format!("{}  {}  {state}", row.answer_id, row.question));
    }
    let authorization = &material.authorization;
    lines.push(match (authorization.country.as_str(), authorization.stated) {
        ("", _) => "work authorization: not stated by you (no country)".into(),
        (country, true) => format!(
            "work authorization ({country}): stated on your profile; `memory authorization {country}` shows it"
        ),
        (country, false) => format!("work authorization ({country}): not stated by you"),
    });
    lines.join("\n")
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
