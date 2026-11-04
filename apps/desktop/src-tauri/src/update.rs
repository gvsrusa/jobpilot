//! Updates. A person presses "Check for updates", or turns on the check at launch: nothing
//! here reaches the network otherwise. The updater plugin verifies every download against the
//! public key pinned in `tauri.conf.json`, and installs nothing it cannot verify. JS has no
//! updater permission: these commands are the only path.
//!
//! An install is refused while an apply run has not ended or the emergency pause is on, so no
//! restart lands mid-application. Core answers both, here and again after the download.

use std::sync::Mutex;

use jobpilot_core::{Db, Error, apply, audit, pause, settings};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::RwLockReadGuard;

use crate::state::AppState;

const ACTOR: &str = "gui";

/// What the launch check emits when it finds a newer version.
pub const UPDATE_AVAILABLE: &str = "update-available";

/// `{code, message}`, as core's errors reach the webview, with this module's own codes.
#[derive(Debug, Serialize)]
pub struct CmdError {
    code: &'static str,
    message: String,
}

impl CmdError {
    fn new(code: &'static str, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    fn check_failed(error: impl ToString) -> Self {
        Self::new("update_check_failed", error)
    }

    fn install_failed(error: impl ToString) -> Self {
        Self::new("update_install_failed", error)
    }
}

impl From<Error> for CmdError {
    fn from(error: Error) -> Self {
        Self::new(error.code(), &error)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

impl From<&Update> for UpdateInfo {
    fn from(update: &Update) -> Self {
        Self {
            version: update.version.clone(),
            notes: update.body.clone(),
            date: update.date.map(|date| date.to_string()),
        }
    }
}

/// The update the last check found, at launch or by a person. Install takes exactly this one,
/// so what a person saw is what installs, and the install makes no second check.
#[derive(Default)]
pub struct UpdateState(Mutex<Option<Update>>);

impl UpdateState {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Update>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn remember(&self, found: Option<Update>) {
        *self.slot() = found;
    }

    fn found(&self) -> Option<Update> {
        self.slot().clone()
    }
}

/// An install waits while the emergency pause is on or any apply run has not ended.
pub fn install_blocked(paused: bool, active_runs: usize) -> bool {
    paused || active_runs > 0
}

const BLOCKED_BY_RUN: &str = "update_blocked_by_run";

/// For a command that starts a run: refused while an install holds `installing`. Hold the guard
/// until the run's row exists, so the install's own check sees it.
pub fn starting_run(state: &AppState) -> jobpilot_core::Result<RwLockReadGuard<'_, ()>> {
    state
        .installing
        .try_read()
        .map_err(|_| Error::Refused(BLOCKED_BY_RUN.into()))
}

fn blocked_by_run() -> CmdError {
    CmdError::new(
        BLOCKED_BY_RUN,
        "an apply run is active or the emergency pause is on",
    )
}

async fn blocked(db: &Db) -> Result<bool, CmdError> {
    // A run whose host is gone by evidence is ended first, or it would refuse every update.
    let _ = apply::session::sweep_orphans_quickly(db).await;
    Ok(install_blocked(
        pause::is_set(db).await?,
        apply::active_runs(db).await?,
    ))
}

async fn refuse_if_blocked(state: &AppState) -> Result<(), CmdError> {
    if blocked(&*state.work().await?).await? {
        return Err(blocked_by_run());
    }
    Ok(())
}

async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Option<Update>, CmdError> {
    app.updater()
        .map_err(CmdError::check_failed)?
        .check()
        .await
        .map_err(CmdError::check_failed)
}

#[tauri::command]
pub async fn update_check<R: Runtime>(
    app: AppHandle<R>,
    updates: State<'_, UpdateState>,
) -> Result<Option<UpdateInfo>, CmdError> {
    let found = check(&app).await?;
    let info = found.as_ref().map(UpdateInfo::from);
    updates.remember(found);
    Ok(info)
}

/// What a check, at launch or by a person, found last. No network.
#[tauri::command]
pub fn update_pending(updates: State<'_, UpdateState>) -> Option<UpdateInfo> {
    updates.found().as_ref().map(UpdateInfo::from)
}

async fn record(state: &AppState, action: &str, detail: serde_json::Value) {
    let written = match state.work().await {
        Ok(db) => audit::record(&db, ACTOR, action, None, detail).await,
        Err(error) => Err(error),
    };
    if let Err(error) = written {
        eprintln!("update: the {action} audit row was not written: {error}");
    }
}

/// Downloads, verifies, installs, and restarts the update the last check found. From the
/// first check to the restart it holds `installing`, so no run can start (`starting_run`),
/// and it is refused with `update_blocked_by_run` while a run is starting, has not ended, or
/// the pause is on: before the download and again before the install.
#[tauri::command]
pub async fn update_install<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    updates: State<'_, UpdateState>,
) -> Result<(), CmdError> {
    let _installing = state.installing.try_write().map_err(|_| blocked_by_run())?;
    refuse_if_blocked(&state).await?;
    let update = updates
        .found()
        .ok_or_else(|| CmdError::install_failed("no update was found: check for updates first"))?;
    // `download` verifies the signature against the pinned key; `install` verifies again.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(CmdError::install_failed)?;
    refuse_if_blocked(&state).await?;
    let detail = json!({ "from": update.current_version, "to": update.version });
    // On Windows `install` starts the installer and exits this process with no `RunEvent::Exit`,
    // so the row and the app's exit teardown come first.
    #[cfg(windows)]
    {
        record(&state, "update_installed", detail.clone()).await;
        crate::teardown(&app).await;
    }
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(CmdError::install_failed)
        .and_then(|done| done.map_err(CmdError::install_failed));
    #[cfg(windows)]
    if let Err(error) = &installed {
        record(
            &state,
            "update_install_failed",
            json!({ "from": detail["from"], "to": detail["to"], "error": error.message }),
        )
        .await;
    }
    installed?;
    // The new version is on disk: restart whatever the audit write does.
    #[cfg(not(windows))]
    record(&state, "update_installed", detail).await;
    // From a command thread this asks the event loop to exit, so `RunEvent::Exit` closes the
    // database and the terminal before the new version starts.
    app.restart()
}

#[tauri::command]
pub async fn update_auto_get(state: State<'_, AppState>) -> Result<bool, CmdError> {
    Ok(check_on_launch(&*state.work().await?).await?)
}

#[tauri::command]
pub async fn update_auto_set(state: State<'_, AppState>, on: bool) -> Result<(), CmdError> {
    let db = state.work().await?;
    settings::set(&db, settings::UPDATES_CHECK_ON_LAUNCH, &json!(on)).await?;
    Ok(())
}

async fn check_on_launch(db: &Db) -> jobpilot_core::Result<bool> {
    Ok(settings::get(db, settings::UPDATES_CHECK_ON_LAUNCH)
        .await?
        .and_then(|value| value.as_bool())
        .unwrap_or(false))
}

/// Whether the launch check may reach the network: only a stored `true`. Anything else,
/// an error included, is no.
pub async fn launch_check_wanted(db: &Db) -> bool {
    check_on_launch(db).await.unwrap_or(false)
}

/// Once, at startup, when a person turned it on: a check that only tells. It never installs.
pub fn check_at_launch<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let wanted = match state.work().await {
            Ok(db) => launch_check_wanted(&db).await,
            Err(_) => false,
        };
        if !wanted {
            return;
        }
        match check(&app).await {
            Ok(Some(update)) => {
                let found = UpdateInfo::from(&update);
                app.state::<UpdateState>().remember(Some(update));
                let _ = app.emit(UPDATE_AVAILABLE, found);
            }
            Ok(None) => {}
            Err(error) => eprintln!("update check at launch: {}", error.message),
        }
    });
}

#[cfg(test)]
mod tests {
    use jobpilot_core::{AppPaths, Db, settings};
    use serde_json::json;

    use super::{install_blocked, launch_check_wanted};

    #[test]
    fn the_launch_check_runs_only_when_turned_on() {
        tauri::async_runtime::block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let db = Db::open(&AppPaths::new(tmp.path().join("data")))
                .await
                .unwrap();
            assert!(!launch_check_wanted(&db).await);
            settings::set(&db, settings::UPDATES_CHECK_ON_LAUNCH, &json!(true))
                .await
                .unwrap();
            assert!(launch_check_wanted(&db).await);
            settings::set(&db, settings::UPDATES_CHECK_ON_LAUNCH, &json!("yes"))
                .await
                .unwrap();
            assert!(!launch_check_wanted(&db).await);
            settings::set(&db, settings::UPDATES_CHECK_ON_LAUNCH, &json!(false))
                .await
                .unwrap();
            assert!(!launch_check_wanted(&db).await);
            db.close().await;
        });
    }

    #[test]
    fn install_refused_while_paused_or_running() {
        assert!(install_blocked(true, 0));
        assert!(install_blocked(false, 1));
        assert!(!install_blocked(false, 0));
    }
}
