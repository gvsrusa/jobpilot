//! What the app holds between commands. The `db` lock guards the handle and the
//! failure together: a slot is `None` exactly when `startup_error` says why.
//!
//! The `db` lock is for short work. A task that can wait on a model or a keychain prompt
//! holds a [`Work`] instead, so `status` answers throughout and a queued restore does not
//! stall every reader behind it. Restore and delete take the gate first: every `Work` is
//! gone once they have it, so theirs is the only reference to the handle.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jobpilot_core::ai::CancellationToken;
use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::{AppPaths, Db, Error, Result, VERSION, recovery};
use serde::Serialize;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// How long app exit waits for a cancelled task. The router drops a cancelled attempt within
/// 100 ms, which kills the child, so only a task that ignores the token reaches this.
const EXIT_WAIT: Duration = Duration::from_secs(5);

pub struct AppState {
    pub paths: AppPaths,
    pub db: RwLock<Option<Arc<Db>>>,
    pub startup_error: Mutex<Option<StartupError>>,
    /// Always taken before `db`. Readers are the open [`Work`]s.
    gate: RwLock<()>,
    /// The router task: drafting and indexing.
    pub ai: InFlight,
    /// A model download, which can outlast many tasks and is cancelled on its own.
    pub pull: InFlight,
    restored_key_state: Mutex<Option<KeyState>>,
    /// The answer `apply_copy` last put on the clipboard, until its clear: the exit clears it
    /// when the timer has not.
    pub copied: Mutex<Option<jobpilot_core::secrets::Zeroizing<String>>>,
}

/// A handle that stays open without the `db` lock, for as long as this is held. `db` is
/// declared first so it drops first: whoever wins the gate next must find the only reference.
pub struct Work<'a> {
    db: Arc<Db>,
    _gate: RwLockReadGuard<'a, ()>,
}

impl std::ops::Deref for Work<'_> {
    type Target = Db;

    fn deref(&self) -> &Db {
        &self.db
    }
}

/// The token of the one running task. One slot means one task: a second token here would
/// leave the first run beyond the reach of Cancel, consent revoke, and app exit.
#[derive(Default)]
pub struct InFlight(
    Mutex<Option<CancellationToken>>,
    Mutex<Option<&'static str>>,
);

/// Frees the slot when the task ends, however it ends.
pub struct Running<'a> {
    slot: &'a InFlight,
    pub token: CancellationToken,
}

impl InFlight {
    fn token(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Before [`AppState::work`], so a cancel that comes while the task waits for its handle
    /// is not lost.
    pub fn begin(&self) -> Result<Running<'_>> {
        self.begin_as(None)
    }

    /// `begin`, naming what holds the slot, so a task refused meanwhile says why: a Copilot
    /// run holds it until the person ends its review stop.
    pub fn begin_as(&self, holder: Option<&'static str>) -> Result<Running<'_>> {
        let mut slot = self.token();
        if slot.is_some() {
            let reason = match *self.holder() {
                Some(holder) => format!("another_task_is_running: {holder}"),
                None => "another_task_is_running".into(),
            };
            return Err(Error::Refused(reason));
        }
        let token = CancellationToken::new();
        *slot = Some(token.clone());
        *self.holder() = holder;
        Ok(Running { slot: self, token })
    }

    fn holder(&self) -> MutexGuard<'_, Option<&'static str>> {
        self.1.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn cancel(&self) {
        if let Some(token) = self.token().as_ref() {
            token.cancel();
        }
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        *self.slot.holder() = None;
        *self.slot.token() = None;
    }
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
    /// `None` with no handle. Never asks the keychain, so it says `key_not_loaded` until
    /// something else has: that means neither locked nor unlocked.
    pub key_state: Option<KeyState>,
    pub swap_interrupted: bool,
    /// False only while the active key has no recovery file, so also true with no key yet.
    pub recovery_key_exported: bool,
    /// What the last restore found about its backup's key. `None` for any other data.
    pub restored_key_state: Option<KeyState>,
}

impl AppState {
    fn with_slot(paths: AppPaths, db: Option<Db>, error: Option<StartupError>) -> Self {
        Self {
            paths,
            db: RwLock::new(db.map(Arc::new)),
            startup_error: Mutex::new(error),
            gate: RwLock::new(()),
            ai: InFlight::default(),
            pull: InFlight::default(),
            restored_key_state: Mutex::new(None),
            copied: Mutex::new(None),
        }
    }

    pub async fn open(paths: AppPaths) -> Self {
        match Db::open(&paths).await {
            Ok(db) => Self::with_slot(paths, Some(db), None),
            Err(error) => Self::failed(paths, (&error).into()),
        }
    }

    pub fn failed(paths: AppPaths, error: StartupError) -> Self {
        Self::with_slot(paths, None, Some(error))
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

    pub fn set_restored_key_state(&self, state: Option<KeyState>) {
        *self
            .restored_key_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = state;
    }

    /// The handle for one command, with no state lock held while it runs. Waits while a
    /// restore or delete has the gate.
    pub async fn work(&self) -> Result<Work<'_>> {
        let gate = self.gate.read().await;
        let db = self.db.read().await.clone().ok_or(Error::Unavailable)?;
        Ok(Work { db, _gate: gate })
    }

    #[cfg(test)]
    pub fn work_is_open(&self) -> bool {
        self.gate.try_write().is_err()
    }

    /// For what replaces the handle: ends running tasks, as they would write to the data
    /// being replaced and a download can last hours, then waits for them to let go.
    pub async fn exclusive(
        &self,
    ) -> (
        RwLockWriteGuard<'_, ()>,
        RwLockWriteGuard<'_, Option<Arc<Db>>>,
    ) {
        self.ai.cancel();
        self.pull.cancel();
        let gate = self.gate.write().await;
        (gate, self.db.write().await)
    }

    /// On the way out. Tasks live on the async runtime, which dies with the process, and
    /// a dropped task is what kills its `claude` child, so this waits for that.
    // ponytail: past `EXIT_WAIT` the app leaves anyway. `kb_search` has no token, so a hung
    // Ollama can hold the gate this long, and it starts no paid child.
    pub async fn shutdown(&self) {
        self.ai.cancel();
        self.pull.cancel();
        let _ = tokio::time::timeout(EXIT_WAIT, self.gate.write()).await;
    }

    pub async fn status(&self, db: Option<&Db>) -> Status {
        let (state, schema_version) = match db {
            Some(db) => ("ready", db.schema_version().await.ok()),
            None => ("recovery", None),
        };
        let key_state = match db {
            Some(db) => last_known_key_state(db).await,
            None => None,
        };
        let no_recovery_file = match db {
            Some(db) => recovery::no_recovery_key_exported(db)
                .await
                .unwrap_or(false),
            None => false,
        };
        let restored_key_state = self
            .restored_key_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Status {
            state: state.to_owned(),
            schema_version,
            app_version: VERSION.to_owned(),
            data_root: self.paths.root().to_string_lossy().into_owned(),
            error: self.failure().clone(),
            key_state,
            swap_interrupted: db.is_some_and(Db::swap_interrupted),
            recovery_key_exported: !no_recovery_file,
            restored_key_state,
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

/// What this handle last learned, with no keychain call, so `status` cannot prompt.
async fn last_known_key_state(db: &Db) -> Option<KeyState> {
    match db.key_state() {
        KeyState::NotLoaded => crypto::known_state(db).await.ok(),
        known => Some(known),
    }
}
