use std::path::PathBuf;
use std::time::Duration;

use serde::ser::{Serialize, SerializeStruct, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("database schema {db} is newer than this app supports ({app})")]
    SchemaTooNew { db: i64, app: i64 },
    #[error("migration failed: {0}")]
    MigrationFailed(String),
    #[error("invalid backup: {0}")]
    InvalidBackup(String),
    /// The restore happened: live data is now the backup. Only reopening failed.
    #[error("restore finished, but the restored data could not be opened: {0}")]
    RestoredButNotOpened(String),
    #[error("data folder is in use by another JobPilot process")]
    InUse,
    /// The folder holds files and no `.jobpilot-root`, so nothing proves it is ours.
    #[error("{} is not a JobPilot data folder", .0.display())]
    RootNotOwned(PathBuf),
    #[error("not available on this system")]
    Unavailable,
    #[error("secret store error: {0}")]
    Secret(String),
    #[error("not found")]
    NotFound,
    #[error("confirmation required")]
    ConfirmationRequired,
    /// The key store could not answer. Temporary: nothing about the key is known.
    #[error("the key store could not answer: {0}")]
    KeyUnavailable(String),
    #[error("the key store has no data key {key_id}")]
    KeyMissing { key_id: String },
    #[error("the data key does not match {key_id}")]
    KeyMismatch { key_id: String },
    /// A sensitive field was asked for while the key is not unlocked.
    #[error("sensitive fields are locked")]
    Locked,
    #[error("refused in the terminal panel")]
    PanelRefused,
    #[error("invalid input: {0}")]
    Validation(String),
    #[error("this needs manual work: {0}")]
    ManualRequired(String),
    /// A policy, consent, or capability refusal.
    #[error("refused: {0}")]
    Refused(String),
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("too large: the limit is {limit} bytes")]
    TooLarge { limit: u64 },
    /// The router tried every provider and ended `Outcome::Failed`.
    #[error("failed: {0}")]
    Failed(String),
    #[error("emergency pause is on")]
    Paused,
    /// A provider answered 429 or 503. `retry_after` is its header, when it gave whole seconds.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable identifier for adapters; the UI and CLI branch on this, not on the message.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Db(_) => "db",
            Error::SchemaTooNew { .. } => "schema_too_new",
            Error::MigrationFailed(_) => "migration_failed",
            Error::InvalidBackup(_) => "invalid_backup",
            Error::RestoredButNotOpened(_) => "restored_not_opened",
            Error::InUse => "in_use",
            Error::RootNotOwned(_) => "root_not_owned",
            Error::Unavailable => "unavailable",
            Error::Secret(_) => "secret",
            Error::NotFound => "not_found",
            Error::ConfirmationRequired => "confirmation_required",
            Error::KeyUnavailable(_) => "key_unavailable",
            Error::KeyMissing { .. } => "key_missing",
            Error::KeyMismatch { .. } => "key_mismatch",
            Error::Locked => "locked",
            Error::PanelRefused => "panel_refused",
            Error::Validation(_) => "validation",
            Error::ManualRequired(_) => "manual_required",
            Error::Refused(_) => "refused",
            Error::Timeout => "timeout",
            Error::Cancelled => "cancelled",
            Error::TooLarge { .. } => "too_large",
            Error::Failed(_) => "failed",
            Error::Paused => "paused",
            Error::RateLimited { .. } => "rate_limited",
        }
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Error", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_its_code() {
        let cases = [
            (Error::Io(std::io::Error::other("x")), "io"),
            (Error::Db(sqlx::Error::PoolClosed), "db"),
            (Error::SchemaTooNew { db: 2, app: 1 }, "schema_too_new"),
            (Error::MigrationFailed("x".into()), "migration_failed"),
            (Error::InvalidBackup("x".into()), "invalid_backup"),
            (
                Error::RestoredButNotOpened("x".into()),
                "restored_not_opened",
            ),
            (Error::InUse, "in_use"),
            (Error::RootNotOwned("x".into()), "root_not_owned"),
            (Error::Unavailable, "unavailable"),
            (Error::Secret("x".into()), "secret"),
            (Error::NotFound, "not_found"),
            (Error::ConfirmationRequired, "confirmation_required"),
            (Error::KeyUnavailable("x".into()), "key_unavailable"),
            (Error::KeyMissing { key_id: "x".into() }, "key_missing"),
            (Error::KeyMismatch { key_id: "x".into() }, "key_mismatch"),
            (Error::Locked, "locked"),
            (Error::PanelRefused, "panel_refused"),
            (Error::Validation("x".into()), "validation"),
            (Error::ManualRequired("x".into()), "manual_required"),
            (Error::Refused("x".into()), "refused"),
            (Error::Timeout, "timeout"),
            (Error::Cancelled, "cancelled"),
            (Error::TooLarge { limit: 1 }, "too_large"),
            (Error::Failed("x".into()), "failed"),
            (Error::Paused, "paused"),
            (Error::RateLimited { retry_after: None }, "rate_limited"),
        ];
        for (error, code) in cases {
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn serializes_as_code_and_message() {
        let value = serde_json::to_value(Error::SchemaTooNew { db: 9, app: 1 }).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "code": "schema_too_new",
                "message": "database schema 9 is newer than this app supports (1)",
            })
        );
    }
}
