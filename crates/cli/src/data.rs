//! Status, documents, backups, and the data commands, including recovery keys.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::{
    AppPaths, Db, Error, Result, app_schema_version, backup, documents, memory, profile, recovery,
    transfer,
};
use serde_json::json;

use crate::{ACTOR, Output, out};

#[derive(Subcommand)]
pub enum DocCommand {
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
pub enum BackupCommand {
    Create,
    List,
    Restore { path: PathBuf },
}

#[derive(Subcommand)]
pub enum DataCommand {
    Export,
    Delete {
        /// Must be exactly DELETE.
        #[arg(long)]
        confirm: Option<String>,
        #[arg(long)]
        include_backups: bool,
    },
    /// Export or import the data key's recovery file.
    #[command(subcommand)]
    RecoveryKey(RecoveryKeyCommand),
    /// Clear the fields no available key can open. Needs the key id as confirmation.
    DiscardLocked {
        #[arg(long)]
        confirm: String,
    },
}

#[derive(Subcommand)]
pub enum RecoveryKeyCommand {
    /// Write the key to a new file outside the data folder.
    Export {
        #[arg(long)]
        out: PathBuf,
    },
    Import {
        path: PathBuf,
    },
}

pub fn describe_key(state: &KeyState) -> String {
    match state {
        KeyState::NotLoaded => "key not loaded".into(),
        KeyState::Unlocked { key_id } => format!("key unlocked ({key_id})"),
        KeyState::KeyUnavailable { reason, .. } if reason == crypto::NO_KEY_YET => {
            "no key yet; one is made when you first save something private".into()
        }
        KeyState::KeyUnavailable { reason, .. } => format!("key store unavailable: {reason}"),
        KeyState::KeyMissing { key_id } => format!(
            "key missing: {key_id}. Import its recovery file with `jobpilot data recovery-key import <path>`."
        ),
        KeyState::KeyMismatch { key_id } => format!("key does not match {key_id}"),
    }
}

pub async fn status(db: &Db) -> Result<Output> {
    let schema = db.schema_version().await?;
    // No keychain call: status must never make a key or prompt.
    let key_state = crypto::known_state(db).await?;
    let ai = jobpilot_core::ai::status(db).await?;
    let ollama =
        serde_json::to_value(&ai.ollama).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    let swap = db.swap_interrupted();
    let mut text = format!(
        "jobpilot {}\nschema {schema}\ndata {}\n{}\nollama: {}\nclaude code: {}",
        jobpilot_core::VERSION,
        db.paths().root().display(),
        describe_key(&key_state),
        ollama["state"].as_str().unwrap_or("unknown"),
        if ai.claude_code.available {
            "available"
        } else {
            "not found"
        },
    );
    if swap {
        text.push_str("\nwarning: a restore was interrupted; check your documents");
    }
    out(
        json!({
            "version": jobpilot_core::VERSION,
            "schema_version": schema,
            "app_schema_version": app_schema_version(),
            "root": db.paths().root(),
            "key_state": key_state,
            "swap_interrupted": swap,
            "ai": {
                "ollama": ollama,
                "claude_code": { "available": ai.claude_code.available },
            },
        }),
        text,
    )
}

pub async fn doc(db: &Db, command: DocCommand) -> Result<Output> {
    match command {
        DocCommand::List => {
            let docs = documents::list(db).await?;
            let text = docs
                .iter()
                .map(|d| format!("{}  {}", d.id, d.title))
                .collect::<Vec<_>>()
                .join("\n");
            out(docs, text)
        }
        DocCommand::Add { title, body } => {
            let doc = documents::create(db, &title, &body).await?;
            let text = format!("added {}", doc.id);
            out(doc, text)
        }
        DocCommand::Search { query } => {
            let hits = documents::search(db, &query).await?;
            let text = hits
                .iter()
                .map(|h| format!("{}  {}  {}", h.id, h.title, h.snippet))
                .collect::<Vec<_>>()
                .join("\n");
            out(hits, text)
        }
    }
}

pub async fn backup(db: &Db, command: BackupCommand) -> Result<Output> {
    match command {
        BackupCommand::Create => {
            let info = backup::create(db, ACTOR).await?;
            let text = format!("backup created at {}", info.path.display());
            out(info, text)
        }
        BackupCommand::List => {
            let list = backup::list(db.paths()).await?;
            let text = list
                .iter()
                .map(|b| format!("{}  {}", b.created_at, b.path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            out(list, text)
        }
        // Handled in `run`, which owns the handle.
        BackupCommand::Restore { .. } => unreachable!("handled before a borrowed handle exists"),
    }
}

pub async fn delete(
    paths: &AppPaths,
    confirm: Option<&str>,
    include_backups: bool,
) -> Result<Output> {
    let db = Db::open(paths).await?;
    transfer::delete_all(db, confirm.unwrap_or(""), include_backups).await?;
    out(
        json!({ "deleted": true }),
        "all JobPilot data deleted".into(),
    )
}

pub async fn restore(paths: &AppPaths, source: &Path) -> Result<Output> {
    // When the data cannot open, restore is the recovery route, so go offline.
    let report = match Db::open(paths).await {
        Ok(db) => backup::restore(db, source, ACTOR).await?,
        // Only failures of the database itself; `InUse` and `RootNotOwned` are real answers.
        Err(Error::Db(_) | Error::MigrationFailed(_) | Error::SchemaTooNew { .. }) => {
            backup::restore_offline(paths, source, ACTOR).await?
        }
        Err(error) => return Err(error),
    };
    // The state on this machine now is the truth; the report's is what the backup's key was.
    let key_state = crypto::peek(&report.db).await;
    report.db.close().await;
    let key_state = key_state?;
    let text = format!(
        "restored from {}\n{}",
        source.display(),
        describe_key(&key_state)
    );
    out(
        json!({
            "restored": true,
            "from": source,
            "key_state": key_state,
            "backup_key_state": report.backup_key_state,
        }),
        text,
    )
}

pub async fn data(db: &Db, command: DataCommand) -> Result<Output> {
    match command {
        DataCommand::Export => {
            // Before writing: the export holds these in plain text when unlocked.
            let (_, authorization) = profile::export_authorization(db).await?;
            let (_, answers, _) = memory::export_sealed(db).await?;
            let sensitive_plain = authorization || answers;
            if sensitive_plain {
                eprintln!(
                    "warning: this export holds your work authorization answers and sealed \
                     career answers as plain text. Keep the folder private."
                );
            }
            let report = transfer::export_report(db, ACTOR).await?;
            let (path, withheld) = (report.path, report.withheld);
            if withheld > 0 {
                let plural = if withheld == 1 { "" } else { "s" };
                eprintln!(
                    "warning: {withheld} sealed career answer{plural} left out of this export, \
                     as no key could open them. They are still in JobPilot."
                );
            }
            out(
                json!({
                    "path": path,
                    "sensitive_plain": sensitive_plain,
                    "withheld": withheld,
                }),
                format!("exported to {}", path.display()),
            )
        }
        DataCommand::RecoveryKey(RecoveryKeyCommand::Export { out: file }) => {
            // A fresh process is NotLoaded; ask the store first, without making a key.
            let state = crypto::peek(db).await?;
            if matches!(&state, KeyState::KeyUnavailable { reason, .. } if reason == crypto::NO_KEY_YET)
            {
                return Err(Error::Refused(
                    "there is no key yet, so there is nothing to export".into(),
                ));
            }
            let receipt = recovery::export(db, &file, ACTOR).await?;
            let text = format!(
                "recovery key {} written to {}\nKeep it somewhere private and off this computer.",
                receipt.key_id,
                receipt.path.display()
            );
            out(receipt, text)
        }
        DataCommand::RecoveryKey(RecoveryKeyCommand::Import { path }) => {
            crypto::peek(db).await?;
            let state = recovery::import(db, &path, ACTOR).await?;
            let text = describe_key(&state);
            out(json!({ "key_state": state }), text)
        }
        DataCommand::DiscardLocked { confirm } => {
            crypto::peek(db).await?;
            let state = recovery::discard_locked(db, &confirm, ACTOR).await?;
            let text = format!("locked fields discarded\n{}", describe_key(&state));
            out(json!({ "key_state": state }), text)
        }
        // Handled in `run`, which owns the handle.
        DataCommand::Delete { .. } => unreachable!("handled before a borrowed handle exists"),
    }
}
