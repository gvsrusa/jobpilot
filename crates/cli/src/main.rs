//! JobPilot CLI: a thin adapter over `jobpilot-core`. Parse, call core, print.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jobpilot_core::{AppPaths, Db, Error, Result, app_schema_version, backup, documents, transfer};
use serde_json::{Value, json};

const ACTOR: &str = "cli";

#[derive(Parser)]
#[command(name = "jobpilot", version, about = "JobPilot AI command line")]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show versions and the data folder.
    Status,
    /// Manage documents.
    #[command(subcommand)]
    Doc(DocCommand),
    /// Manage backups.
    #[command(subcommand)]
    Backup(BackupCommand),
    /// Export or delete all data.
    #[command(subcommand)]
    Data(DataCommand),
}

#[derive(Subcommand)]
enum DocCommand {
    List,
    Add {
        title: String,
        #[arg(long, default_value = "")]
        body: String,
    },
    Search {
        query: String,
    },
}

#[derive(Subcommand)]
enum BackupCommand {
    Create,
    List,
    Restore { path: PathBuf },
}

#[derive(Subcommand)]
enum DataCommand {
    Export,
    Delete {
        /// Must be exactly DELETE.
        #[arg(long)]
        confirm: Option<String>,
        #[arg(long)]
        include_backups: bool,
    },
}

/// What a command prints: the JSON form and the text form.
struct Output {
    json: Value,
    text: String,
}

fn out(value: impl serde::Serialize, text: String) -> Result<Output> {
    let json = serde_json::to_value(value).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    Ok(Output { json, text })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command).await {
        Ok(output) => {
            let line = if cli.json {
                output.json.to_string()
            } else {
                output.text
            };
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
            eprintln!("error[{}]: {error}", error.code());
            ExitCode::from(1)
        }
    }
}

async fn run(command: Command) -> Result<Output> {
    let paths = AppPaths::from_env()?;
    match command {
        // These take the handle by value; each closes it on every path.
        Command::Backup(BackupCommand::Restore { path }) => restore(&paths, &path).await,
        Command::Data(DataCommand::Delete {
            confirm,
            include_backups,
        }) => {
            let db = Db::open(&paths).await?;
            transfer::delete_all(db, confirm.as_deref().unwrap_or(""), include_backups).await?;
            out(
                json!({ "deleted": true }),
                "all JobPilot data deleted".into(),
            )
        }
        command => {
            let db = Db::open(&paths).await?;
            let result = with_db(&db, command).await;
            db.close().await;
            result
        }
    }
}

async fn restore(paths: &AppPaths, source: &std::path::Path) -> Result<Output> {
    // When the data cannot open, restore is the recovery route, so go offline.
    let db = match Db::open(paths).await {
        Ok(db) => backup::restore(db, source, ACTOR).await?.db,
        // Only failures of the database itself; `InUse` and `RootNotOwned` are real answers.
        Err(Error::Db(_) | Error::MigrationFailed(_) | Error::SchemaTooNew { .. }) => {
            backup::restore_offline(paths, source, ACTOR).await?.db
        }
        Err(error) => return Err(error),
    };
    db.close().await;
    out(
        json!({ "restored": true, "from": source }),
        format!("restored from {}", source.display()),
    )
}

async fn with_db(db: &Db, command: Command) -> Result<Output> {
    match command {
        Command::Status => {
            let schema = db.schema_version().await?;
            out(
                json!({
                    "version": jobpilot_core::VERSION,
                    "schema_version": schema,
                    "app_schema_version": app_schema_version(),
                    "root": db.paths().root(),
                }),
                format!(
                    "jobpilot {}\nschema {schema}\ndata {}",
                    jobpilot_core::VERSION,
                    db.paths().root().display()
                ),
            )
        }
        Command::Doc(DocCommand::List) => {
            let docs = documents::list(db).await?;
            let text = docs
                .iter()
                .map(|d| format!("{}  {}", d.id, d.title))
                .collect::<Vec<_>>()
                .join("\n");
            out(docs, text)
        }
        Command::Doc(DocCommand::Add { title, body }) => {
            let doc = documents::create(db, &title, &body).await?;
            let text = format!("added {}", doc.id);
            out(doc, text)
        }
        Command::Doc(DocCommand::Search { query }) => {
            let hits = documents::search(db, &query).await?;
            let text = hits
                .iter()
                .map(|h| format!("{}  {}  {}", h.id, h.title, h.snippet))
                .collect::<Vec<_>>()
                .join("\n");
            out(hits, text)
        }
        Command::Backup(BackupCommand::Create) => {
            let info = backup::create(db, ACTOR).await?;
            let text = format!("backup created at {}", info.path.display());
            out(info, text)
        }
        Command::Backup(BackupCommand::List) => {
            let list = backup::list(db.paths()).await?;
            let text = list
                .iter()
                .map(|b| format!("{}  {}", b.created_at, b.path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            out(list, text)
        }
        Command::Data(DataCommand::Export) => {
            let path = transfer::export_all(db, ACTOR).await?;
            out(
                json!({ "path": path }),
                format!("exported to {}", path.display()),
            )
        }
        // Handled in `run`, which owns the handle.
        Command::Backup(BackupCommand::Restore { .. })
        | Command::Data(DataCommand::Delete { .. }) => {
            unreachable!("handled before a borrowed handle exists")
        }
    }
}
