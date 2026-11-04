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

/// The newest version a check found, so the Status screen shows one the launch check found
/// before the screen was listening.
#[derive(Default)]
pub struct UpdateState(Mutex<Option<UpdateInfo>>);

impl UpdateState {
    fn remember(&self, found: Option<UpdateInfo>) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = found;
    }

    fn pending(&self) -> Option<UpdateInfo> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// An install waits while the emergency pause is on or any apply run has not ended.
pub fn install_blocked(paused: bool, active_runs: usize) -> bool {
    paused || active_runs > 0
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
        return Err(CmdError::new(
            "update_blocked_by_run",
            "an apply run is active or the emergency pause is on",
        ));
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
    let found = check(&app).await?.as_ref().map(UpdateInfo::from);
    updates.remember(found.clone());
    Ok(found)
}

/// What a check, at launch or by a person, found last. No network.
#[tauri::command]
pub fn update_pending(updates: State<'_, UpdateState>) -> Option<UpdateInfo> {
    updates.pending()
}

/// Downloads, verifies, installs, and restarts. Refused with `update_blocked_by_run` before
/// the download and again before the install, as a run could start while it downloads.
#[tauri::command]
pub async fn update_install<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<(), CmdError> {
    refuse_if_blocked(&state).await?;
    let update = check(&app)
        .await?
        .ok_or_else(|| CmdError::install_failed("no update is available"))?;
    // `download` verifies the signature against the pinned key; `install` verifies again.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(CmdError::install_failed)?;
    refuse_if_blocked(&state).await?;
    let detail = json!({ "from": update.current_version, "to": update.version });
    // On Windows `install` starts the installer and exits this process, so the row goes first.
    #[cfg(windows)]
    audit::record(
        &*state.work().await?,
        ACTOR,
        "update_installed",
        None,
        detail,
    )
    .await?;
    update.install(bytes).map_err(CmdError::install_failed)?;
    #[cfg(not(windows))]
    audit::record(
        &*state.work().await?,
        ACTOR,
        "update_installed",
        None,
        detail,
    )
    .await?;
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

/// Once, at startup, when a person turned it on: a check that only tells. It never installs.
pub fn check_at_launch<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let on = match state.work().await {
            Ok(db) => check_on_launch(&db).await.unwrap_or(false),
            Err(_) => false,
        };
        if !on {
            return;
        }
        match check(&app).await {
            Ok(Some(update)) => {
                let found = UpdateInfo::from(&update);
                app.state::<UpdateState>().remember(Some(found.clone()));
                let _ = app.emit(UPDATE_AVAILABLE, found);
            }
            Ok(None) => {}
            Err(error) => eprintln!("update check at launch: {}", error.message),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::install_blocked;

    #[test]
    fn install_refused_while_paused_or_running() {
        assert!(install_blocked(true, 0));
        assert!(install_blocked(false, 1));
        assert!(!install_blocked(false, 0));
    }
}
