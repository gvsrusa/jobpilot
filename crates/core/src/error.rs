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
    #[error("not available on this system")]
    Unavailable,
    #[error("secret store error: {0}")]
    Secret(String),
    #[error("not found")]
    NotFound,
    #[error("confirmation required")]
    ConfirmationRequired,
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
            Error::Unavailable => "unavailable",
            Error::Secret(_) => "secret",
            Error::NotFound => "not_found",
            Error::ConfirmationRequired => "confirmation_required",
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
            (Error::Unavailable, "unavailable"),
            (Error::Secret("x".into()), "secret"),
            (Error::NotFound, "not_found"),
            (Error::ConfirmationRequired, "confirmation_required"),
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
