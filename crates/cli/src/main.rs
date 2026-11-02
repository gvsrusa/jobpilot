//! JobPilot CLI: a thin adapter over `jobpilot-core`. Parse, call core, print.

mod agents;
mod ai;
mod analytics;
mod apps;
mod data;
mod jobs;
mod memory;
mod pause;
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
    Job(jobs::JobCommand),
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
    /// Counts and rates over your applications.
    #[command(subcommand)]
    Analytics(analytics::AnalyticsCommand),
    /// AI providers, models, and consent.
    #[command(subcommand)]
    Ai(ai::AiCommand),
    /// Stop all automation.
    #[command(subcommand)]
    Pause(pause::PauseCommand),
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
        use jobs::JobCommand;
        use memory::MemoryCommand;
        use pause::PauseCommand;
        use profile::KbCommand;
        use resume::{ExportArgs, LetterCommand, ResumeCommand};
        matches!(
            self,
            Command::Backup(BackupCommand::Restore { .. })
                | Command::Ai(
                    AiCommand::ClaudePath { .. }
                        | AiCommand::CliPath { .. }
                        | AiCommand::Consent(ConsentCommand::Give { .. })
                        | AiCommand::Key(_)
                        | AiCommand::Model { .. }
                        | AiCommand::Endpoint { .. }
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
                | Command::Pause(PauseCommand::Clear)
                | Command::Kb(KbCommand::Reveal { .. })
                | Command::Memory(
                    MemoryCommand::Approve { .. }
                        | MemoryCommand::Reviewed { .. }
                        | MemoryCommand::SetClass { .. }
                        | MemoryCommand::Reveal { .. }
                        | MemoryCommand::Confirm { .. }
                        | MemoryCommand::Use { .. }
                        | MemoryCommand::Authorization { .. }
                        | MemoryCommand::Refresh { yes: true }
                )
                | Command::Job(
                    JobCommand::Paste { file: Some(_), .. }
                        | JobCommand::Import { file: Some(_), .. }
                )
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
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let args: Vec<String> = std::env::args_os()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let Some(line) = parse_error(&args, &error) else {
                error.exit()
            };
            eprintln!("{line}");
            return ExitCode::from(2);
        }
    };
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

/// clap repeats the word it could not use, and after `ai key` that word can be a key. So
/// those errors get this line, naming no argument. `None` leaves clap's own text, which
/// `--help` needs. Flags are passed over, as `--json` may stand between the two words.
pub fn parse_error(args: &[String], error: &clap::Error) -> Option<&'static str> {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let key_command = words.windows(2).any(|pair| pair == ["ai", "key"]);
    (key_command && error.use_stderr()).then_some(
        "error[usage]: use `jobpilot ai key set <provider>` or `jobpilot ai key remove \
         <provider>`. A key is never an argument: `set` asks for it.",
    )
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

/// Plain words for a job-source refusal. The reason words are the ones on `RUN_REASONS`.
fn source_reason(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "policy_unknown" => {
            "The policy register has no clear answer for this source, so nothing was requested."
        }
        "policy_blocked" => {
            "The policy register blocks reading this source, so nothing was requested."
        }
        "policy_user_assisted" => {
            "This source is user-assisted only, so nothing is read from it. Import the posting. \
             Use `jobpilot job import --url <link> --title <title>` with its text on standard \
             input."
        }
        "operation_not_declared" => {
            "This source does not declare that operation, so nothing was requested. Import the \
             posting. Use `jobpilot job import --url <link> --title <title>` with its text on \
             standard input."
        }
        "source_blocked" => {
            "The source refused or showed a challenge page. JobPilot stops there, tries nothing else, and leaves it alone for a day."
        }
        "source_not_found" => "The source says that board or feed does not exist. Check the name.",
        "bad_feed" => {
            "The source answered in a shape JobPilot does not read, so nothing was stored."
        }
        "needs_text" => "That posting has no text to read. Import it with its text instead.",
        "network" => "The source could not be reached. Check the connection and try again.",
        "no_sources" => {
            "This company has no stored text to read yet. Import a posting or paste a note first."
        }
        "company_gone" => "This company no longer exists, so nothing was saved.",
        _ => return None,
    })
}

fn wait_words(wait: std::time::Duration) -> String {
    let secs = wait.as_secs();
    match secs {
        0..=89 => format!("{secs} seconds"),
        90..=5399 => format!("{} minutes", secs.div_ceil(60)),
        _ => format!("{} hours", secs.div_ceil(3600)),
    }
}

fn message(error: &Error) -> String {
    match error {
        Error::PanelRefused => shell::REFUSAL.into(),
        Error::Paused => "Emergency pause is on, so nothing automated runs. Clear it with \
                          `jobpilot pause clear` in an ordinary terminal, or from the title bar."
            .into(),
        Error::ManualRequired(reason) if reason == "no_evidence" => {
            "There is no approved knowledge to work from. Add an item with `jobpilot kb add \
             <kind> <text>`, approve it with `jobpilot kb approve <id>` (`jobpilot kb list` \
             shows the ids), then run this again."
                .into()
        }
        Error::ManualRequired(reason) if reason == "no_ai" => {
            "No AI provider is on, so this needs doing by hand. Turn one on with `jobpilot ai \
             enable <provider>`, then run this again."
                .into()
        }
        Error::ManualRequired(reason) if reason == "not_logged_in" => {
            "This CLI is not signed in. Run it once in your own terminal to sign in, then try \
             again."
                .into()
        }
        Error::ManualRequired(reason) if reason == "cli_account_refused" => {
            "Gemini CLI refused this account. Its plan may have moved to Antigravity CLI, which \
             JobPilot does not run. A paid Gemini API key in the CLI may work, or use another \
             provider."
                .into()
        }
        Error::ManualRequired(reason) if reason == "cli_too_old" => {
            "The installed CLI is older than the version JobPilot checked. Update it, then try \
             again."
                .into()
        }
        Error::ManualRequired(reason) if reason == "consent_needed" => {
            "An AI provider is on, but your consent does not cover this task. See `jobpilot ai \
             status`, then give consent in an ordinary terminal, for example `jobpilot ai \
             consent give claude_code --categories job_text,profile_basic,resume_text`, or on \
             the AI screen."
                .into()
        }
        Error::ManualRequired(reason) if reason == "consent_suspended" => {
            "Your consent for this provider was given before the terms on file now, so it is \
             paused and nothing is sent. Review it once on the AI screen. Or read it with \
             `jobpilot ai consent show`, then give it again with `jobpilot ai consent give` in \
             an ordinary terminal."
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
        Error::ManualRequired(reason) if reason == "unusable_reply" => {
            "The AI model answered, but not in a form JobPilot could use, so nothing was saved. \
             Try again, choose a larger model, or write this one yourself."
                .into()
        }
        Error::ManualRequired(reason) if reason == "output_truncated" => {
            "The AI model ran out of room before it finished, so nothing was saved. Remove some \
             knowledge items, shorten the posting, or write this one yourself."
                .into()
        }
        Error::ManualRequired(reason) if reason == "rate_limited" => {
            "The AI provider is busy and asked to wait. Try again shortly, or write this one \
             yourself."
                .into()
        }
        Error::ManualRequired(reason) if reason == "auth_failed" => {
            "The AI provider refused the key. Check it on the AI screen.".into()
        }
        Error::ManualRequired(reason) if reason == "quota_exhausted" => {
            "The AI provider says your usage or credit has run out. Add credit with the vendor, \
             then try again."
                .into()
        }
        Error::ManualRequired(reason) if reason == "no_key" => {
            "No key is stored for this provider. Add one with `jobpilot ai key set <provider>` \
             in an ordinary terminal, or on the AI screen."
                .into()
        }
        Error::ManualRequired(reason) if reason == "no_api_model" => {
            "No model is chosen for this provider. Set one with `jobpilot ai model <provider> \
             <id>`, or on the AI screen."
                .into()
        }
        Error::Refused(reason) if source_reason(reason).is_some() => {
            source_reason(reason).unwrap_or_default().into()
        }
        Error::RateLimited {
            retry_after: Some(wait),
        } => format!(
            "This source asked JobPilot to wait, or its limit is reached. Try again in about {}.",
            wait_words(*wait)
        ),
        Error::ManualRequired(reason) | Error::Refused(reason) if reason == "origin_refused" => {
            "That model is not eligible through OpenRouter. Use an openai/, anthropic/, or \
             google/ model, not a free one, with no suffix but :nitro or :floor."
                .into()
        }
        Error::ManualRequired(reason) | Error::Refused(reason) if reason == "model_has_tools" => {
            "That model searches by itself or drives a computer, so JobPilot cannot keep it from \
             tools while it reads a job posting. Choose one without search, research, or \
             computer-use in its name, and without :online."
                .into()
        }
        Error::ManualRequired(reason) if reason == "model_not_found" => {
            "The provider does not know that model id. Check it, then set it with `jobpilot ai \
             model <provider> <id>`, or `jobpilot ai set-model --drafting <name>` for Ollama, in \
             an ordinary terminal, or on the AI screen."
                .into()
        }
        Error::ManualRequired(reason) if reason == "model_refused" => {
            "The AI model declined this task, so nothing was generated. Try another model, or \
             write this one yourself."
                .into()
        }
        Error::ManualRequired(reason) if reason == "endpoint_changed" => {
            "The endpoint address changed since you gave consent, or the terms on file did, so \
             nothing is sent. Read it with `jobpilot ai consent show endpoint`, then give it \
             with `jobpilot ai consent give endpoint` in an ordinary terminal, or on the AI \
             screen."
                .into()
        }
        Error::Refused(reason) if reason == "attestation_needed" => {
            "This provider needs you to state the line above. If it is true, run this again \
             with --attest."
                .into()
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
        Command::Job(command) => jobs::job(db, command).await,
        Command::Resume(command) => resume::resume(db, command).await,
        Command::Letter(command) => resume::letter(db, command).await,
        Command::App(command) => apps::app(db, command).await,
        Command::Memory(command) => memory::memory(db, command).await,
        Command::Agents(command) => agents::agents(db, command).await,
        Command::Analytics(command) => analytics::analytics(db, command).await,
        Command::Ai(command) => ai::ai(db, command).await,
        Command::Pause(command) => pause::pause(db, command).await,
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
        ("job import --url u --title t", false),
        ("job import --url u --title t --file /f", true),
        ("job list", false),
        (
            "job list --country PT --mode remote --min-pay 90000:EUR:year --all",
            false,
        ),
        ("job search q", false),
        ("job show i", false),
        ("job remove i", false),
        ("job split i", false),
        ("job sources", false),
        ("job source list", false),
        ("job source add lever acme", false),
        ("job source remove i", false),
        ("job fetch", false),
        ("job fetch --source i", false),
        ("job analyze", false),
        ("job analyze i --ai --yes", false),
        ("job research c --yes", false),
        ("job company list", false),
        ("job company note c", false),
        ("job prefs show", false),
        ("job prefs set --seniority senior", false),
        ("resume import /r", true),
        ("resume list", false),
        ("resume versions l", false),
        ("resume generate j", false),
        ("resume generate j --export pdf", false),
        ("resume compare d", false),
        ("resume export d --format pdf", false),
        ("resume export d --format pdf --out /o", true),
        ("resume edit-claim c --text t", false),
        ("resume remove-claim c", false),
        ("letter generate j", false),
        ("letter generate j --export pdf", false),
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
        ("memory refresh --yes", true),
        ("app answers i", false),
        ("agents list", false),
        ("agents list --limit 5", false),
        ("agents show r", false),
        ("agents log r", false),
        ("agents cancel r", false),
        ("agents resume r", false),
        ("analytics report", false),
        ("analytics show", false),
        ("pause set", false),
        ("pause status", false),
        ("pause clear", true),
        ("ai status", false),
        ("ai models", false),
        ("ai pull m", true),
        ("ai set-model --drafting m", true),
        ("ai order ollama", true),
        ("ai enable claude_code", true),
        ("ai disable claude_code", true),
        ("ai claude-path /c", true),
        ("ai cli-path gemini_cli /c", true),
        ("ai enable openai", true),
        ("ai disable openai", true),
        ("ai order openai", true),
        ("ai key set openai", true),
        ("ai key remove openai", true),
        ("ai model openai m", true),
        ("ai endpoint http://127.0.0.1:1", true),
        ("ai consent give claude_code --categories job_text", true),
        ("ai consent give openai --categories job_text", true),
        ("ai consent revoke claude_code", false),
        ("ai consent revoke openai", false),
        ("ai consent show openai", false),
        ("ai consent show openai --job j", false),
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
    fn no_evidence_and_no_ai_say_what_to_do() {
        let evidence = message(&Error::ManualRequired("no_evidence".into()));
        assert!(evidence.contains("jobpilot kb add") && evidence.contains("jobpilot kb approve"));
        let none = message(&Error::ManualRequired("no_ai".into()));
        assert!(none.contains("jobpilot ai enable"), "{none}");
        assert!(!none.contains("no_ai") && !evidence.contains("no_evidence"));
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
    fn consent_reasons_name_no_one_provider() {
        let needed = message(&Error::ManualRequired("consent_needed".into()));
        assert!(needed.starts_with("An AI provider is on"), "{needed}");
        let suspended = message(&Error::ManualRequired("consent_suspended".into()));
        assert!(
            suspended.contains("jobpilot ai consent give"),
            "{suspended}"
        );
        assert!(
            suspended.contains("jobpilot ai consent show"),
            "{suspended}"
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
    fn hosted_failure_reasons_say_what_happened() {
        for (reason, words) in [
            ("rate_limited", "busy and asked to wait"),
            ("auth_failed", "refused the key"),
            ("quota_exhausted", "usage or credit has run out"),
        ] {
            let text = message(&Error::ManualRequired(reason.into()));
            assert!(text.contains(words), "{reason}: {text}");
        }
    }

    #[test]
    fn api_reasons_say_what_to_do() {
        let text = message(&Error::ManualRequired("no_key".into()));
        assert_eq!(
            text,
            "No key is stored for this provider. Add one with `jobpilot ai key set <provider>` \
             in an ordinary terminal, or on the AI screen."
        );
        let text = message(&Error::ManualRequired("no_api_model".into()));
        assert_eq!(
            text,
            "No model is chosen for this provider. Set one with `jobpilot ai model <provider> \
             <id>`, or on the AI screen."
        );
        let origin = "That model is not eligible through OpenRouter. Use an openai/, anthropic/, \
                      or google/ model, not a free one, with no suffix but :nitro or :floor.";
        assert_eq!(message(&Error::Refused("origin_refused".into())), origin);
        for error in [
            Error::Refused("model_has_tools".into()),
            Error::ManualRequired("model_has_tools".into()),
        ] {
            let text = message(&error);
            assert!(text.contains("searches by itself"), "{text}");
            assert!(text.contains(":online"), "{text}");
            assert!(text.contains("computer-use"), "{text}");
        }
        let text = message(&Error::ManualRequired("model_not_found".into()));
        assert!(text.contains("does not know that model id"), "{text}");
        assert!(text.contains("jobpilot ai model <provider> <id>"), "{text}");
        assert!(text.contains("jobpilot ai set-model"), "{text}");
        let text = message(&Error::ManualRequired("model_refused".into()));
        assert!(text.contains("declined this task"), "{text}");
        let text = message(&Error::ManualRequired("endpoint_changed".into()));
        assert!(text.contains("endpoint address changed"), "{text}");
        assert!(text.contains("jobpilot ai consent give endpoint"), "{text}");
        assert_eq!(
            message(&Error::ManualRequired("origin_refused".into())),
            origin
        );
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
    fn the_research_reasons_read_in_plain_words() {
        for word in ["no_sources", "company_gone"] {
            assert!(super::source_reason(word).is_some(), "{word}");
        }
    }

    #[test]
    fn the_panel_refuses_what_needs_a_user_gesture() {
        for (line, refused) in PANEL {
            assert_eq!(parsed(line).refused_in_panel(), *refused, "{line}");
        }
    }
}
