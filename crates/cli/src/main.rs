//! JobPilot CLI: a thin adapter over `jobpilot-core`. Parse, call core, print.

mod agents;
mod ai;
mod apps;
mod data;
mod memory;
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
    /// Applications you are tracking.
    #[command(subcommand)]
    App(apps::AppCommand),
    /// Questions and answers you reuse.
    #[command(subcommand)]
    Memory(memory::MemoryCommand),
    /// Agent runs and their logs.
    #[command(subcommand)]
    Agents(agents::AgentsCommand),
    /// AI providers, models, and consent.
    #[command(subcommand)]
    Ai(ai::AiCommand),
    /// Restricted prompt used by the app's terminal panel.
    Shell,
}

impl Command {
    /// The terminal panel's input has no shell to clear its marker with, and script in the
    /// webview can write to it. So it refuses what could leak a key or destroy data, what
    /// the app confirms in a native dialog, what changes what runs or starts a download, and
    /// what reads or writes a path the caller names (M-14). Revoking consent stays: it only
    /// ever takes something away.
    pub fn refused_in_panel(&self) -> bool {
        use ai::{AiCommand, ConsentCommand};
        use data::{BackupCommand, DataCommand};
        use memory::MemoryCommand;
        use profile::{JobCommand, KbCommand};
        use resume::{ExportArgs, LetterCommand, ResumeCommand};
        matches!(
            self,
            Command::Backup(BackupCommand::Restore { .. })
                | Command::Ai(
                    AiCommand::ClaudePath { .. }
                        | AiCommand::Consent(ConsentCommand::Give { .. })
                        | AiCommand::Enable { .. }
                        | AiCommand::Disable { .. }
                        | AiCommand::Order { .. }
                        | AiCommand::SetModel { .. }
                        | AiCommand::Pull { .. }
                )
                | Command::Data(
                    DataCommand::Delete { .. }
                        | DataCommand::RecoveryKey(_)
                        | DataCommand::DiscardLocked { .. }
                )
                | Command::Kb(KbCommand::Reveal { .. })
                | Command::Memory(
                    MemoryCommand::Approve { .. }
                        | MemoryCommand::Reviewed { .. }
                        | MemoryCommand::SetClass { .. }
                        | MemoryCommand::Reveal { .. }
                        | MemoryCommand::Confirm { .. }
                        | MemoryCommand::Use { .. }
                        | MemoryCommand::Authorization { .. }
                )
                | Command::Job(JobCommand::Paste { file: Some(_), .. })
                | Command::Resume(
                    ResumeCommand::Import { .. }
                        | ResumeCommand::Export(ExportArgs { out: Some(_), .. })
                )
                | Command::Letter(LetterCommand::Export(ExportArgs { out: Some(_), .. }))
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
        Error::ManualRequired(reason) if reason == "consent_needed" => {
            "Claude Code is on, but your consent does not cover this task. Give consent with \
             `jobpilot ai consent give claude_code --categories \
             job_text,profile_basic,resume_text` in an ordinary terminal, or on the AI screen."
                .into()
        }
        Error::ManualRequired(reason) if reason == "provider_unavailable" => {
            "An AI provider is on but cannot be reached. Check `jobpilot ai status`, or write \
             this one yourself."
                .into()
        }
        Error::ManualRequired(reason) if reason == "no_model_set" => {
            "No local model is chosen. Set one with `jobpilot ai set-model --drafting <name>` \
             in an ordinary terminal, or write this one yourself."
                .into()
        }
        Error::ManualRequired(reason) if reason == "model_not_installed" => {
            "The chosen model is not installed in Ollama. Check `jobpilot ai models`.".into()
        }
        Error::Refused(reason) if reason == "use_profile_by_country" => {
            "Work authorization is answered from your profile, for one country at a time. If \
             this question asks something else, remove it and add it in other words."
                .into()
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
        Command::App(command) => apps::app(db, command).await,
        Command::Memory(command) => memory::memory(db, command).await,
        Command::Agents(command) => agents::agents(db, command).await,
        Command::Ai(command) => ai::ai(db, command).await,
        // `main` and the shell handle this before any command runs.
        Command::Shell => Err(Error::PanelRefused),
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, message};
    use jobpilot_core::Error;

    /// Every leaf command, with whether the panel refuses it. Script in the webview can type
    /// into the panel, so a command whose GUI twin needs a native dialog, or that changes
    /// what runs, is refused there. A new command fails `every_command_is_classified` until
    /// it is given a row here, which is the moment to ask which kind it is.
    const PANEL: &[(&str, bool)] = &[
        ("status", false),
        ("doc list", false),
        ("doc add t", false),
        ("doc search q", false),
        ("backup create", false),
        ("backup list", false),
        ("backup restore /b", true),
        ("data export", false),
        ("data delete --confirm DELETE", true),
        ("data recovery-key export --out /k", true),
        ("data recovery-key import /k", true),
        ("data discard-locked --confirm id", true),
        ("profile show", false),
        ("profile set --name n", false),
        ("profile add countries PT", false),
        ("profile remove countries PT", false),
        ("profile auth set PT", false),
        ("profile auth remove PT", false),
        ("prefs work-mode remote", false),
        ("prefs employment add PT full-time", false),
        ("prefs employment remove PT full-time", false),
        ("kb list", false),
        ("kb add note b", false),
        ("kb remove i", false),
        ("kb approve i", false),
        ("kb unapprove i", false),
        ("kb mark-sensitive i", false),
        ("kb unmark i", false),
        ("kb reveal i", true),
        ("kb search q", false),
        ("job paste t", false),
        ("job paste t --file /f", true),
        ("job list", false),
        ("job show i", false),
        ("resume import /r", true),
        ("resume list", false),
        ("resume versions l", false),
        ("resume generate j", false),
        ("resume compare d", false),
        ("resume export d --format pdf", false),
        ("resume export d --format pdf --out /o", true),
        ("resume edit-claim c --text t", false),
        ("resume remove-claim c", false),
        ("letter generate j", false),
        ("letter export d --format pdf", false),
        ("letter export d --format pdf --out /o", true),
        ("app add --company c --role r", false),
        ("app list", false),
        ("app show i", false),
        ("app history i", false),
        ("app edit i --role r", false),
        ("app status i applied", false),
        ("app note i n", false),
        ("app remove i", false),
        ("memory add-question q", false),
        ("memory add-answer q a", false),
        ("memory list", false),
        ("memory show q", false),
        ("memory edit-answer a t", false),
        ("memory unapprove a", false),
        ("memory remove-question q", false),
        ("memory remove-answer a", false),
        ("memory approve a", true),
        ("memory reviewed a", true),
        ("memory set-class q normal", true),
        ("memory reveal a", true),
        ("memory confirm a --application x", true),
        ("memory use a --application x", true),
        ("memory authorization PT", true),
        ("memory search q", false),
        ("memory refresh", false),
        ("app answers i", false),
        ("agents list", false),
        ("agents list --limit 5", false),
        ("agents show r", false),
        ("agents log r", false),
        ("agents cancel r", false),
        ("agents resume r", false),
        ("ai status", false),
        ("ai models", false),
        ("ai pull m", true),
        ("ai set-model --drafting m", true),
        ("ai order ollama", true),
        ("ai enable claude_code", true),
        ("ai disable claude_code", true),
        ("ai claude-path /c", true),
        ("ai consent give claude_code --categories job_text", true),
        ("ai consent revoke claude_code", false),
        ("ai index", false),
    ];

    fn parsed(line: &str) -> Command {
        let argv = std::iter::once("jobpilot").chain(line.split(' '));
        match Cli::try_parse_from(argv) {
            Ok(cli) => cli.command,
            Err(error) => panic!("{line}: {error}"),
        }
    }

    /// Each leaf's path, and whether it takes an argument that is a file path.
    fn leaves(command: &clap::Command, path: &str, found: &mut Vec<(String, bool)>) {
        if command.get_subcommands().next().is_none() {
            let paths = clap::builder::ValueParser::path_buf().type_id();
            let takes_path = command
                .get_arguments()
                .any(|arg| arg.get_value_parser().type_id() == paths);
            found.push((path.trim_start().to_string(), takes_path));
        }
        for sub in command.get_subcommands() {
            leaves(sub, &format!("{path} {}", sub.get_name()), found);
        }
    }

    fn rows_of<'a>(path: &'a str) -> impl Iterator<Item = &'static (&'static str, bool)> + 'a {
        PANEL.iter().filter(move |(line, _)| {
            line.strip_prefix(path)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
    }

    #[test]
    fn consent_hint_names_the_whole_command() {
        let text = message(&Error::ManualRequired("consent_needed".into()));
        assert!(
            text.contains(
                "jobpilot ai consent give claude_code --categories job_text,profile_basic,resume_text"
            ),
            "{text}"
        );
    }

    #[test]
    fn model_reasons_say_what_to_do() {
        let none = message(&Error::ManualRequired("no_model_set".into()));
        assert!(
            none.contains("jobpilot ai set-model --drafting <name>"),
            "{none}"
        );
        let gone = message(&Error::ManualRequired("model_not_installed".into()));
        assert!(gone.contains("jobpilot ai models"), "{gone}");
    }

    #[test]
    fn every_command_is_classified() {
        let mut found = Vec::new();
        leaves(&Cli::command(), "", &mut found);
        // The shell refuses a nested shell itself, by name.
        found.retain(|(path, _)| path != "shell");
        for (path, takes_path) in found {
            assert!(rows_of(&path).next().is_some(), "not classified: {path}");
            // Script in the webview must never name a file, for reading or for writing.
            let refused = rows_of(&path).any(|(_, refused)| *refused);
            assert!(
                !takes_path || refused,
                "takes a path, has no refused row: {path}"
            );
        }
    }

    #[test]
    fn the_panel_refuses_what_needs_a_user_gesture() {
        for (line, refused) in PANEL {
            assert_eq!(parsed(line).refused_in_panel(), *refused, "{line}");
        }
    }
}
