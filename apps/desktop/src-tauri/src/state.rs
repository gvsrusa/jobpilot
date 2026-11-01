//! What the app holds between commands. The `db` lock guards the handle and the
//! failure together: a slot is `None` exactly when `startup_error` says why.

use std::io;
use std::sync::{Mutex, PoisonError};

use jobpilot_core::{AppPaths, Db, Error, Result, VERSION};
use serde::Serialize;
use tokio::sync::RwLock;

pub struct AppState {
    pub paths: AppPaths,
    pub db: RwLock<Option<Db>>,
    pub startup_error: Mutex<Option<StartupError>>,
}

/// Why no handle is open. Not only from startup: a failed reopen lands here too.
#[derive(Debug, Clone, Serialize)]
pub struct StartupError {
    pub code: String,
    pub message: String,
}

impl From<&Error> for StartupError {
    fn from(error: &Error) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// `"ready"` or `"recovery"`.
    pub state: String,
    pub schema_version: Option<i64>,
    pub app_version: String,
    pub data_root: String,
    pub error: Option<StartupError>,
}

impl AppState {
    pub async fn open(paths: AppPaths) -> Self {
        match Db::open(&paths).await {
            Ok(db) => Self {
                paths,
                db: RwLock::new(Some(db)),
                startup_error: Mutex::new(None),
            },
            Err(error) => Self::failed(paths, (&error).into()),
        }
    }

    pub fn failed(paths: AppPaths, error: StartupError) -> Self {
        Self {
            paths,
            db: RwLock::new(None),
            startup_error: Mutex::new(Some(error)),
        }
    }

    /// `AppPaths::from_env` found no data folder. The empty root is never used: every
    /// path below it would be relative to the working folder, so `known_root` stops
    /// each caller first.
    pub fn no_root(error: &Error) -> Self {
        Self::failed(AppPaths::new(""), error.into())
    }

    pub fn known_root(&self) -> Result<&AppPaths> {
        if self.paths.root().as_os_str().is_empty() {
            return Err(Error::Unavailable);
        }
        Ok(&self.paths)
    }

    /// Call with the `db` write lock held, so nothing sees the slot and this disagree.
    pub fn set_failure(&self, error: Option<StartupError>) {
        *self.failure() = error;
    }

    /// `status` never fails, so a poisoned lock still answers: the value is one
    /// assignment, and it cannot be left half written.
    fn failure(&self) -> std::sync::MutexGuard<'_, Option<StartupError>> {
        self.startup_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub async fn status(&self, db: Option<&Db>) -> Status {
        let (state, schema_version) = match db {
            Some(db) => ("ready", db.schema_version().await.ok()),
            None => ("recovery", None),
        };
        Status {
            state: state.to_owned(),
            schema_version,
            app_version: VERSION.to_owned(),
            data_root: self.paths.root().to_string_lossy().into_owned(),
            error: self.failure().clone(),
        }
    }

    /// Restore with no handle replaces the database unread, so it runs only when that
    /// database is what failed, matching the CLI. Otherwise the stored failure is the
    /// answer and `retry_open` the way out. `restored_not_opened` counts: the recovery
    /// screen offers the `pre-restore` snapshot from there.
    pub fn check_offline_restore(&self) -> Result<()> {
        let Some(failure) = self.failure().clone() else {
            return Err(Error::Unavailable);
        };
        match failure.code.as_str() {
            "db" | "migration_failed" | "schema_too_new" | "restored_not_opened" => Ok(()),
            "in_use" => Err(Error::InUse),
            "root_not_owned" => Err(Error::RootNotOwned(self.paths.root().to_path_buf())),
            "io" => {
                // `Error::Io` adds this prefix back when it is displayed.
                let cause = failure.message.strip_prefix("file error: ");
                let cause = cause.unwrap_or(&failure.message);
                Err(Error::Io(io::Error::other(cause)))
            }
            _ => Err(Error::Unavailable),
        }
    }
}
