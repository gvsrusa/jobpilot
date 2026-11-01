//! JobPilot CLI: a thin adapter over `jobpilot-core`. Parse, call core, print.

mod ai;
mod data;
mod profile;
mod resume;
mod shell;
mod text;

use std::io::Write;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jobpilot_core::{AppPaths, Db, Error, Result, recovery};
use serde_json::Value;

pub const ACTOR: &str = "cli";

#[derive(Parser)]
#[command(name = "jobpilot", version, about = "JobPilot AI command line")]
pub struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show versions, the data folder, the key state, and AI availability.
    Status,
    /// Manage documents.
    #[command(subcommand)]
    Doc(data::DocCommand),
    /// Manage backups.
    #[command(subcommand)]
    Backup(data::BackupCommand),
    /// Export, delete, or recover data.
    #[command(subcommand)]
    Data(data::DataCommand),
    /// Name, places, and work authorization.
    #[command(subcommand)]
    Profile(profile::ProfileCommand),
    /// Work modes and employment types.
    #[command(subcommand)]
    Prefs(profile::PrefsCommand),
    /// Career knowledge items.
    #[command(subcommand)]
    Kb(profile::KbCommand),
    /// Pasted job postings.
    #[command(subcommand)]
    Job(profile::JobCommand),
    /// Resume drafts.
    #[command(subcommand)]
    Resume(resume::ResumeCommand),
    /// Cover letter drafts.
    #[command(subcommand)]
    Letter(resume::LetterCommand),
    /// AI providers, models, and consent.
    #[command(subcommand)]
    Ai(ai::AiCommand),
    /// Restricted prompt used by the app's terminal panel.
    Shell,
}

impl Command {
    /// The terminal panel's input has no shell to clear its marker with, so what could
    /// leak a key or destroy data is refused there.
    pub fn refused_in_panel(&self) -> bool {
        use data::{BackupCommand, DataCommand};
        matches!(
            self,
            Command::Backup(BackupCommand::Restore { .. })
                | Command::Ai(ai::AiCommand::ClaudePath { .. })
                | Command::Data(
                    DataCommand::Delete { .. }
                        | DataCommand::RecoveryKey(_)
                        | DataCommand::DiscardLocked { .. }
                )
        )
    }
}

/// What a command prints: the JSON form and the text form.
pub struct Output {
    json: Value,
    text: String,
}

pub fn out(value: impl serde::Serialize, text: String) -> Result<Output> {
    let json = serde_json::to_value(value).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    Ok(Output { json, text })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Shell) {
        // Not through `run`: the panel itself starts this, marker set.
        let input = tokio::io::BufReader::new(tokio::io::stdin());
        return match shell::run(input, tokio::io::stdout()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!(
                    "error[{}]: {}",
                    error.code(),
                    text::sanitize(&message(&error))
                );
                ExitCode::from(1)
            }
        };
    }
    match run(cli.command).await {
        Ok(output) => {
            let line = render(output, cli.json);
            let written = if line.is_empty() {
                Ok(())
            } else {
                writeln!(std::io::stdout().lock(), "{line}")
            };
            match written {
                Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                    eprintln!("error[io]: {e}");
                    ExitCode::from(1)
                }
                _ => ExitCode::SUCCESS,
            }
        }
        Err(error) => {
            eprintln!("{}", error_line(&error));
            ExitCode::from(1)
        }
    }
}

/// Stored and parsed text is untrusted: no escape reaches the terminal.
pub fn render(output: Output, json: bool) -> String {
    if json {
        text::sanitize_value(output.json).to_string()
    } else {
        text::sanitize(&output.text)
    }
}

pub fn error_line(error: &Error) -> String {
    format!(
        "error[{}]: {}",
        error.code(),
        text::sanitize(&message(error))
    )
}

fn message(error: &Error) -> String {
    match error {
        Error::PanelRefused => shell::REFUSAL.into(),
        Error::ManualRequired(reason) if reason == "not_logged_in" => {
            "Claude Code is not signed in. Run `claude` once to log in, then try again.".into()
        }
        other => other.to_string(),
    }
}

pub async fn run(command: Command) -> Result<Output> {
    if command.refused_in_panel() && recovery::panel_marker_set() {
        return Err(Error::PanelRefused);
    }
    let paths = AppPaths::from_env()?;
    match command {
        // These take the handle by value; each closes it on every path.
        Command::Backup(data::BackupCommand::Restore { path }) => {
            data::restore(&paths, &path).await
        }
        Command::Data(data::DataCommand::Delete {
            confirm,
            include_backups,
        }) => data::delete(&paths, confirm.as_deref(), include_backups).await,
        command => {
            let db = Db::open(&paths).await?;
            let result = with_db(&db, command).await;
            db.close().await;
            result
        }
    }
}

async fn with_db(db: &Db, command: Command) -> Result<Output> {
    match command {
        Command::Status => data::status(db).await,
        Command::Doc(command) => data::doc(db, command).await,
        Command::Backup(command) => data::backup(db, command).await,
        Command::Data(command) => data::data(db, command).await,
        Command::Profile(command) => profile::profile(db, command).await,
        Command::Prefs(command) => profile::prefs(db, command).await,
        Command::Kb(command) => profile::kb(db, command).await,
        Command::Job(command) => profile::job(db, command).await,
        Command::Resume(command) => resume::resume(db, command).await,
        Command::Letter(command) => resume::letter(db, command).await,
        Command::Ai(command) => ai::ai(db, command).await,
        // `main` and the shell handle this before any command runs.
        Command::Shell => Err(Error::PanelRefused),
    }
}
