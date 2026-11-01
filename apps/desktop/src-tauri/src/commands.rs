//! The IPC surface: a thin adapter over `jobpilot-core`. Each command takes the `db`
//! lock, calls core, and returns. Only `backup_restore`, `data_delete`, and `retry_open`
//! change the slot, under the write lock, so no command sees a half-swapped handle.
//!
//! There is deliberately no `secret_get`: secret values arrive here and never leave.

use std::path::PathBuf;

use jobpilot_core::audit::{self, AuditEntry};
use jobpilot_core::backup::{self, BackupInfo};
use jobpilot_core::documents::{self, Document, SearchHit};
use jobpilot_core::{Db, Error, Result, secrets, settings, transfer};
use serde_json::Value;
use tauri::ipc::Invoke;
use tauri::{Runtime, State};

use crate::state::{AppState, StartupError, Status};

const ACTOR: &str = "gui";

pub fn handler<R: Runtime>() -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        status,
        retry_open,
        documents_list,
        documents_create,
        documents_update,
        documents_delete,
        documents_search,
        settings_get,
        settings_set,
        backup_create,
        backup_list,
        backup_restore,
        data_export,
        data_delete,
        secret_set,
        secret_delete,
        secret_names,
        audit_recent,
        crate::pty::pty_open,
        crate::pty::pty_write,
        crate::pty::pty_resize,
        crate::pty::pty_close,
    ]
}

fn ready(slot: &Option<Db>) -> Result<&Db> {
    slot.as_ref().ok_or(Error::Unavailable)
}

/// For an operation that consumed a live handle. Core closed it on every path, so a
/// new one goes in the slot whatever happened; `failure` is what the operation returned.
/// If the app cannot reopen it is in recovery, and that is the answer the UI must act
/// on, so it comes back as a `Status` carrying `failure` in its message.
async fn reopen(state: &AppState, slot: &mut Option<Db>, failure: Option<Error>) -> Result<Status> {
    match Db::open(&state.paths).await {
        Ok(db) => {
            *slot = Some(db);
            state.set_failure(None);
            match failure {
                Some(error) => Err(error),
                None => Ok(state.status(slot.as_ref()).await),
            }
        }
        Err(error) => {
            let mut stored = StartupError::from(&error);
            if let Some(failure) = failure {
                stored.message = format!("{} (after: {failure})", stored.message);
            }
            state.set_failure(Some(stored));
            Ok(state.status(None).await)
        }
    }
}

#[tauri::command]
pub async fn status(state: State<'_, AppState>) -> Result<Status> {
    let slot = state.db.read().await;
    Ok(state.status(slot.as_ref()).await)
}

/// User-initiated only: a failed migration adds a snapshot per open, so nothing else
/// may call `Db::open` while the app is in recovery.
#[tauri::command]
pub async fn retry_open(state: State<'_, AppState>) -> Result<Status> {
    let mut slot = state.db.write().await;
    if slot.is_none()
        && let Ok(paths) = state.known_root()
    {
        match Db::open(paths).await {
            Ok(db) => {
                *slot = Some(db);
                state.set_failure(None);
            }
            Err(error) => state.set_failure(Some((&error).into())),
        }
    }
    Ok(state.status(slot.as_ref()).await)
}

#[tauri::command]
pub async fn documents_list(state: State<'_, AppState>) -> Result<Vec<Document>> {
    let slot = state.db.read().await;
    documents::list(ready(&slot)?).await
}

#[tauri::command]
pub async fn documents_create(
    state: State<'_, AppState>,
    title: String,
    body: String,
) -> Result<Document> {
    let slot = state.db.read().await;
    documents::create(ready(&slot)?, &title, &body).await
}

#[tauri::command]
pub async fn documents_update(
    state: State<'_, AppState>,
    id: String,
    title: String,
    body: String,
) -> Result<Document> {
    let slot = state.db.read().await;
    documents::update(ready(&slot)?, &id, &title, &body).await
}

#[tauri::command]
pub async fn documents_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    let slot = state.db.read().await;
    documents::delete(ready(&slot)?, &id).await
}

#[tauri::command]
pub async fn documents_search(state: State<'_, AppState>, query: String) -> Result<Vec<SearchHit>> {
    let slot = state.db.read().await;
    documents::search(ready(&slot)?, &query).await
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>, key: String) -> Result<Option<Value>> {
    let slot = state.db.read().await;
    settings::get(ready(&slot)?, &key).await
}

#[tauri::command]
pub async fn settings_set(state: State<'_, AppState>, key: String, value: Value) -> Result<()> {
    let slot = state.db.read().await;
    settings::set(ready(&slot)?, &key, &value).await
}

#[tauri::command]
pub async fn backup_create(state: State<'_, AppState>) -> Result<BackupInfo> {
    let slot = state.db.read().await;
    backup::create(ready(&slot)?, ACTOR).await
}

/// Needs no handle: the recovery screen's Restore lists snapshots from here. The read
/// lock keeps it clear of a restore or delete changing `backups/`.
#[tauri::command]
pub async fn backup_list(state: State<'_, AppState>) -> Result<Vec<BackupInfo>> {
    let _slot = state.db.read().await;
    backup::list(state.known_root()?).await
}

#[tauri::command]
pub async fn backup_restore(state: State<'_, AppState>, source: String) -> Result<Status> {
    let mut slot = state.db.write().await;
    let source = PathBuf::from(source);
    let restored = match slot.take() {
        Some(db) => match backup::restore(db, &source, ACTOR).await {
            Err(error) if !matches!(error, Error::RestoredButNotOpened(_)) => {
                return reopen(&state, &mut slot, Some(error)).await;
            }
            restored => restored,
        },
        None => {
            state.check_offline_restore()?;
            match backup::restore_offline(&state.paths, &source, ACTOR).await {
                // Live data is unchanged, so the stored failure still stands. No
                // reopen here: `retry_open` is the user's call.
                Err(error) if !matches!(error, Error::RestoredButNotOpened(_)) => {
                    return Err(error);
                }
                restored => restored,
            }
        }
    };
    match restored {
        Ok(report) => {
            *slot = Some(report.db);
            state.set_failure(None);
        }
        // Live data is now the backup. This must not read as "nothing changed".
        Err(error) => state.set_failure(Some((&error).into())),
    }
    Ok(state.status(slot.as_ref()).await)
}

#[tauri::command]
pub async fn data_export(state: State<'_, AppState>) -> Result<String> {
    let slot = state.db.read().await;
    let path = transfer::export_all(ready(&slot)?, ACTOR).await?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn data_delete(
    state: State<'_, AppState>,
    confirm: String,
    include_backups: bool,
) -> Result<Status> {
    let mut slot = state.db.write().await;
    let db = slot.take().ok_or(Error::Unavailable)?;
    let failure = transfer::delete_all(db, &confirm, include_backups)
        .await
        .err();
    reopen(&state, &mut slot, failure).await
}

#[tauri::command]
pub async fn secret_set(state: State<'_, AppState>, name: String, value: String) -> Result<()> {
    let slot = state.db.read().await;
    secrets::set(ready(&slot)?, &name, &value, ACTOR).await
}

#[tauri::command]
pub async fn secret_delete(state: State<'_, AppState>, name: String) -> Result<()> {
    let slot = state.db.read().await;
    secrets::delete(ready(&slot)?, &name, ACTOR).await
}

#[tauri::command]
pub async fn secret_names(state: State<'_, AppState>) -> Result<Vec<String>> {
    let slot = state.db.read().await;
    secrets::names(ready(&slot)?).await
}

#[tauri::command]
pub async fn audit_recent(state: State<'_, AppState>, limit: i64) -> Result<Vec<AuditEntry>> {
    let slot = state.db.read().await;
    audit::recent(ready(&slot)?, limit).await
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use jobpilot_core::{AppPaths, Db, Error, backup};
    use serde_json::{Value, json};
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;
    use tauri::{App, Listener, Manager, WebviewWindow, WebviewWindowBuilder, WindowEvent};
    use tempfile::TempDir;

    use super::handler;
    use crate::pty::{self, MAX_WRITE_BYTES, PtyState};
    use crate::state::{AppState, StartupError};

    const OPENS: &str = "schema_too_new";

    struct Harness {
        _tmp: TempDir,
        paths: AppPaths,
        app: App<MockRuntime>,
        window: WebviewWindow<MockRuntime>,
    }

    fn temp_root() -> (TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        (tmp, paths)
    }

    fn block_on<F: Future>(work: F) -> F::Output {
        tauri::async_runtime::block_on(work)
    }

    fn harness(tmp: TempDir, paths: AppPaths, state: AppState) -> Harness {
        let app = mock_builder()
            .manage(state)
            .manage(PtyState::default())
            .on_window_event(pty::on_window_event)
            .invoke_handler(handler())
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        Harness {
            _tmp: tmp,
            paths,
            app,
            window,
        }
    }

    /// The app as it starts with healthy data.
    fn ready() -> Harness {
        let (tmp, paths) = temp_root();
        let state = block_on(AppState::open(paths.clone()));
        harness(tmp, paths, state)
    }

    /// Healthy data on disk and no handle: a startup failure of `code`. The data is
    /// healthy so a test can tell an attempt to open from no attempt.
    fn recovery(code: &str) -> Harness {
        let (tmp, paths) = temp_root();
        block_on(async { Db::open(&paths).await.unwrap().close().await });
        recovery_at(tmp, paths, code)
    }

    fn recovery_at(tmp: TempDir, paths: AppPaths, code: &str) -> Harness {
        let state = AppState::failed(
            paths.clone(),
            StartupError {
                code: code.into(),
                message: format!("file error: stored {code}"),
            },
        );
        harness(tmp, paths, state)
    }

    /// A real backup of a database holding one document titled `title`, kept outside
    /// the data root so a test with a recovery state has it too.
    fn backup_holding(tmp: &TempDir, title: &str) -> PathBuf {
        let paths = AppPaths::new(tmp.path().join(format!("source-{title}")));
        block_on(async {
            let db = Db::open(&paths).await.unwrap();
            jobpilot_core::documents::create(&db, title, "body")
                .await
                .unwrap();
            let info = backup::create(&db, "test").await.unwrap();
            db.close().await;
            info.path
        })
    }

    impl Harness {
        fn call(&self, cmd: &str, args: Value) -> Result<Value, Value> {
            invoke(&self.window, cmd, args)
        }

        fn ok(&self, cmd: &str, args: Value) -> Value {
            self.call(cmd, args).unwrap()
        }

        fn code(&self, cmd: &str, args: Value) -> String {
            let error = self.call(cmd, args).unwrap_err();
            error["code"].as_str().unwrap().to_owned()
        }

        fn status(&self) -> Value {
            self.ok("status", json!({}))
        }

        fn has_handle(&self) -> bool {
            block_on(self.app.state::<AppState>().db.read()).is_some()
        }

        fn titles(&self) -> Vec<String> {
            let docs = self.ok("documents_list", json!({}));
            let docs = docs.as_array().unwrap();
            docs.iter()
                .map(|doc| doc["title"].as_str().unwrap().to_owned())
                .collect()
        }

        fn assert_ready(&self) {
            let status = self.status();
            assert_eq!(status["state"], "ready", "{status}");
            assert_eq!(status["error"], Value::Null);
            assert!(self.has_handle());
        }

        fn assert_recovery(&self, code: &str) {
            let status = self.status();
            assert_eq!(status["state"], "recovery", "{status}");
            assert_eq!(status["error"]["code"], code, "{status}");
            assert!(!self.has_handle());
        }

        fn snapshots(&self) -> usize {
            match fs::read_dir(self.paths.backups()) {
                Ok(entries) => entries.count(),
                Err(_) => 0,
            }
        }
    }

    fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
        let request = InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            // The app's own origin: Tauri rejects other origins before any command.
            url: if cfg!(windows) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        get_ipc_response(window, request).map(|body| body.deserialize().unwrap())
    }

    fn take_exclusive(paths: &AppPaths) -> File {
        let lock = File::options().write(true).open(paths.lock()).unwrap();
        lock.try_lock().unwrap();
        lock
    }

    fn restore(source: &Path) -> Value {
        json!({ "source": source })
    }

    fn delete(confirm: &str, include_backups: bool) -> Value {
        json!({ "confirm": confirm, "includeBackups": include_backups })
    }

    #[test]
    fn status_is_recovery_when_db_failed_to_open() {
        let h = recovery(OPENS);

        let status = h.status();

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], OPENS);
        assert_eq!(
            status["error"]["message"],
            "file error: stored schema_too_new"
        );
        assert_eq!(status["schema_version"], Value::Null);
        assert_eq!(status["app_version"], jobpilot_core::VERSION);
        assert_eq!(status["data_root"], h.paths.root().to_str().unwrap());
    }

    #[test]
    fn status_is_ready_with_a_handle() {
        let h = ready();

        let status = h.status();

        assert_eq!(status["state"], "ready");
        assert_eq!(
            status["schema_version"],
            jobpilot_core::app_schema_version()
        );
        assert_eq!(status["error"], Value::Null);
    }

    #[test]
    fn startup_failure_is_stored_with_its_code() {
        let (tmp, paths) = temp_root();
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(paths.root().join("stranger.txt"), "not ours").unwrap();

        let state = block_on(AppState::open(paths.clone()));
        let h = harness(tmp, paths, state);

        h.assert_recovery("root_not_owned");
    }

    #[test]
    fn data_commands_return_unavailable_in_recovery() {
        let h = recovery(OPENS);
        let commands = [
            ("documents_list", json!({})),
            ("documents_create", json!({ "title": "t", "body": "b" })),
            (
                "documents_update",
                json!({ "id": "i", "title": "t", "body": "b" }),
            ),
            ("documents_delete", json!({ "id": "i" })),
            ("documents_search", json!({ "query": "q" })),
            ("settings_get", json!({ "key": "k" })),
            ("settings_set", json!({ "key": "k", "value": 1 })),
            ("backup_create", json!({})),
            ("data_export", json!({})),
            ("data_delete", delete("DELETE", true)),
            ("secret_set", json!({ "name": "n", "value": "v" })),
            ("secret_delete", json!({ "name": "n" })),
            ("secret_names", json!({})),
            ("audit_recent", json!({ "limit": 1 })),
        ];

        for (command, args) in commands {
            assert_eq!(h.code(command, args), "unavailable", "{command}");
        }
        h.assert_recovery(OPENS);
    }

    #[test]
    fn no_secret_read_command_is_registered() {
        let h = ready();

        let error = h.call("secret_get", json!({ "name": "n" })).unwrap_err();

        assert!(error.as_str().unwrap().contains("not found"), "{error}");
    }

    #[test]
    fn a_rejected_secret_never_echoes_its_value() {
        let h = ready();

        let error = h
            .call("secret_set", json!({ "name": "", "value": "hunter2" }))
            .unwrap_err();

        assert_eq!(error["code"], "secret");
        assert!(!error.to_string().contains("hunter2"), "{error}");
        assert_eq!(h.ok("secret_names", json!({})), json!([]));
    }

    #[test]
    fn data_commands_reach_core_as_gui() {
        let h = ready();

        let doc = h.ok(
            "documents_create",
            json!({ "title": "Resume", "body": "rust" }),
        );
        let id = doc["id"].as_str().unwrap();
        h.ok(
            "documents_update",
            json!({ "id": id, "title": "CV", "body": "rust" }),
        );
        assert_eq!(h.titles(), ["CV"]);
        let hits = h.ok("documents_search", json!({ "query": "rust" }));
        assert_eq!(hits[0]["id"], id);
        h.ok("documents_delete", json!({ "id": id }));
        assert!(h.titles().is_empty());
        assert_eq!(h.code("documents_delete", json!({ "id": id })), "not_found");

        assert_eq!(h.ok("settings_get", json!({ "key": "k" })), Value::Null);
        h.ok("settings_set", json!({ "key": "k", "value": { "a": 1 } }));
        assert_eq!(
            h.ok("settings_get", json!({ "key": "k" })),
            json!({ "a": 1 })
        );

        let info = h.ok("backup_create", json!({}));
        assert_eq!(h.ok("backup_list", json!({}))[0]["path"], info["path"]);
        let export = h.ok("data_export", json!({}));
        assert!(
            Path::new(export.as_str().unwrap())
                .join("data.json")
                .is_file()
        );
        assert_eq!(h.code("secret_delete", json!({ "name": "n" })), "not_found");

        let audit = h.ok("audit_recent", json!({ "limit": 2 }));
        assert_eq!(audit[0]["action"], "export");
        assert_eq!(audit[0]["actor"], "gui");
        assert_eq!(audit[1]["action"], "backup");
        assert_eq!(audit[1]["actor"], "gui");
    }

    #[test]
    fn restore_swaps_the_live_handle() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "kept", "body": "" }));
        let info = h.ok("backup_create", json!({}));
        h.ok("documents_create", json!({ "title": "later", "body": "" }));
        let source = Path::new(info["path"].as_str().unwrap());

        let status = h.ok("backup_restore", restore(source));

        assert_eq!(status["state"], "ready");
        h.assert_ready();
        assert_eq!(h.titles(), ["kept"]);
        let audit = h.ok("audit_recent", json!({ "limit": 1 }));
        assert_eq!(audit[0]["action"], "restore");
        assert_eq!(audit[0]["actor"], "gui");
    }

    #[test]
    fn failed_restore_reopens_and_stays_ready() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let missing = h.paths.root().join("no-such-backup");

        assert_eq!(
            h.code("backup_restore", restore(&missing)),
            "invalid_backup"
        );

        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
    }

    #[test]
    fn restore_refused_while_another_handle_is_open_stays_ready() {
        let h = ready();
        let info = h.ok("backup_create", json!({}));
        let source = Path::new(info["path"].as_str().unwrap());
        let other = block_on(Db::open(&h.paths)).unwrap();

        assert_eq!(h.code("backup_restore", restore(source)), "in_use");

        h.assert_ready();
        block_on(other.close());
    }

    #[test]
    fn restored_but_not_reopened_is_a_recovery_status_not_an_error() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let source = backup_holding(&h._tmp, "restored");
        // The online path does not look at the marker, so only the reopen fails.
        fs::remove_file(h.paths.marker()).unwrap();

        let status = h.ok("backup_restore", restore(&source));

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], "restored_not_opened");
        let message = status["error"]["message"].as_str().unwrap();
        assert!(message.contains("restore finished"), "{message}");
        h.assert_recovery("restored_not_opened");

        fs::write(h.paths.marker(), "io.github.gvsrusa.jobpilot").unwrap();
        assert_eq!(h.ok("retry_open", json!({}))["state"], "ready");
        assert_eq!(h.titles(), ["restored"]);
    }

    #[test]
    fn failed_restore_with_a_failed_reopen_is_a_recovery_status() {
        let h = ready();
        fs::remove_file(h.paths.marker()).unwrap();
        let missing = h.paths.root().join("no-such-backup");

        let status = h.ok("backup_restore", restore(&missing));

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], "root_not_owned");
        let message = status["error"]["message"].as_str().unwrap();
        assert!(message.contains("invalid backup"), "{message}");
        h.assert_recovery("root_not_owned");
    }

    #[test]
    fn restore_works_from_recovery_state() {
        for code in [
            "schema_too_new",
            "migration_failed",
            "db",
            "restored_not_opened",
        ] {
            let h = recovery(code);
            let source = backup_holding(&h._tmp, "restored");

            let status = h.ok("backup_restore", restore(&source));

            assert_eq!(status["state"], "ready", "{code}");
            h.assert_ready();
            assert_eq!(h.titles(), ["restored"]);
        }
    }

    #[test]
    fn offline_restore_is_refused_unless_the_database_cannot_open() {
        for (code, expected) in [
            ("in_use", "in_use"),
            ("root_not_owned", "root_not_owned"),
            ("io", "io"),
            ("unavailable", "unavailable"),
        ] {
            let h = recovery(code);
            let source = backup_holding(&h._tmp, "restored");

            assert_eq!(h.code("backup_restore", restore(&source)), expected);

            h.assert_recovery(code);
            assert_eq!(h.snapshots(), 0, "nothing was written for {code}");
        }
    }

    #[test]
    fn a_refused_io_restore_keeps_a_single_message_prefix() {
        let h = recovery("io");
        let source = backup_holding(&h._tmp, "restored");

        let error = h.call("backup_restore", restore(&source)).unwrap_err();

        assert_eq!(error["message"], "file error: stored io");
    }

    #[test]
    fn failed_offline_restore_stays_in_recovery_without_opening() {
        // The data would open, so a handle here could only come from a retry nobody
        // asked for. Each one adds a snapshot when the failure is a migration.
        let h = recovery("migration_failed");
        let missing = h.paths.root().join("no-such-backup");

        assert_eq!(
            h.code("backup_restore", restore(&missing)),
            "invalid_backup"
        );

        h.assert_recovery("migration_failed");
    }

    #[test]
    fn offline_restore_blocked_by_a_lock_keeps_the_startup_failure() {
        let h = recovery(OPENS);
        let source = backup_holding(&h._tmp, "restored");
        let held = take_exclusive(&h.paths);

        assert_eq!(h.code("backup_restore", restore(&source)), "in_use");

        h.assert_recovery(OPENS);
        drop(held);
    }

    #[test]
    fn no_command_sees_a_half_swapped_handle() {
        let h = ready();
        let info = h.ok("backup_create", json!({}));
        let source = PathBuf::from(info["path"].as_str().unwrap());

        let reader = {
            let window = h.window.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    invoke(&window, "documents_list", json!({})).unwrap();
                    let status = invoke(&window, "status", json!({})).unwrap();
                    assert_eq!(status["state"], "ready");
                }
            })
        };
        for _ in 0..5 {
            h.ok("backup_restore", restore(&source));
        }

        reader.join().unwrap();
    }

    #[test]
    fn data_delete_with_no_handle_is_unavailable_and_changes_nothing() {
        let h = recovery(OPENS);

        assert_eq!(h.code("data_delete", delete("DELETE", true)), "unavailable");

        h.assert_recovery(OPENS);
        assert!(h.paths.db().is_file());
    }

    #[test]
    fn wrong_confirmation_leaves_the_app_ready() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));

        let code = h.code("data_delete", delete("delete", true));

        assert_eq!(code, "confirmation_required");
        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
    }

    #[test]
    fn data_delete_refused_while_another_handle_is_open_stays_ready() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let other = block_on(Db::open(&h.paths)).unwrap();

        assert_eq!(h.code("data_delete", delete("DELETE", true)), "in_use");

        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
        block_on(other.close());
    }

    #[test]
    fn data_delete_reopens_on_empty_data() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "gone", "body": "" }));
        h.ok("backup_create", json!({}));

        let status = h.ok("data_delete", delete("DELETE", false));

        assert_eq!(status["state"], "ready");
        h.assert_ready();
        assert!(h.titles().is_empty());
        assert_eq!(h.ok("backup_list", json!({})).as_array().unwrap().len(), 1);
    }

    #[test]
    fn data_delete_with_a_failed_reopen_is_a_recovery_status() {
        let h = ready();
        fs::remove_file(h.paths.marker()).unwrap();

        let status = h.ok("data_delete", delete("delete", false));

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], "root_not_owned");
        h.assert_recovery("root_not_owned");
    }

    #[test]
    fn retry_open_recovers_when_the_cause_has_gone() {
        let h = recovery("in_use");

        let status = h.ok("retry_open", json!({}));

        assert_eq!(status["state"], "ready");
        h.assert_ready();
    }

    #[test]
    fn failed_retry_replaces_the_stored_failure() {
        let h = recovery(OPENS);
        let held = take_exclusive(&h.paths);

        let status = h.ok("retry_open", json!({}));

        assert_eq!(status["error"]["code"], "in_use");
        h.assert_recovery("in_use");
        drop(held);
    }

    #[test]
    fn retry_open_with_a_handle_does_not_open_again() {
        let h = ready();
        // An open would fail on this, so a stored failure would prove it ran.
        fs::remove_file(h.paths.marker()).unwrap();

        assert_eq!(h.ok("retry_open", json!({}))["state"], "ready");

        h.assert_ready();
    }

    #[test]
    fn backups_are_listed_in_recovery_for_the_picker() {
        let (tmp, paths) = temp_root();
        let made = block_on(async {
            let db = Db::open(&paths).await.unwrap();
            let info = backup::create(&db, "test").await.unwrap();
            db.close().await;
            info
        });
        let h = recovery_at(tmp, paths, "migration_failed");

        let list = h.ok("backup_list", json!({}));

        assert_eq!(list[0]["path"], made.path.to_str().unwrap());
    }

    #[test]
    fn with_no_data_root_only_status_answers() {
        let (tmp, paths) = temp_root();
        let state = AppState::no_root(&Error::Unavailable);
        let h = harness(tmp, paths, state);
        let source = backup_holding(&h._tmp, "restored");

        h.assert_recovery("unavailable");
        assert_eq!(h.status()["data_root"], "");
        assert_eq!(
            h.ok("retry_open", json!({}))["error"]["code"],
            "unavailable"
        );
        assert_eq!(h.code("backup_list", json!({})), "unavailable");
        assert_eq!(h.code("backup_restore", restore(&source)), "unavailable");
        assert_eq!(h.code("data_delete", delete("DELETE", true)), "unavailable");
    }

    // The PTY tests drive the real debug CLI: `cargo build -p jobpilot-cli` first (a
    // workspace test run builds it). Wide, so ConPTY does not wrap a long temp path.
    const SIZE: (u16, u16) = (400, 40);
    const PTY_WAIT: Duration = Duration::from_secs(20);

    /// What the webview would see of one terminal session.
    struct Panel<'a> {
        harness: &'a Harness,
        data: Arc<Mutex<Vec<u8>>>,
        exits: Arc<Mutex<Vec<Value>>>,
        answered: Cell<usize>,
    }

    impl<'a> Panel<'a> {
        fn listening(harness: &'a Harness) -> Self {
            let data = Arc::new(Mutex::new(Vec::new()));
            let exits = Arc::new(Mutex::new(Vec::new()));
            let sink = data.clone();
            harness.window.listen("pty:data", move |event| {
                let bytes: Vec<u8> = serde_json::from_str(event.payload()).unwrap();
                sink.lock().unwrap().extend(bytes);
            });
            let sink = exits.clone();
            harness.window.listen("pty:exit", move |event| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::from_str(event.payload()).unwrap());
            });
            Panel {
                harness,
                data,
                exits,
                answered: Cell::new(0),
            }
        }

        fn open(harness: &'a Harness) -> Self {
            let panel = Panel::listening(harness);
            let (cols, rows) = SIZE;
            invoke(
                &harness.window,
                "pty_open",
                json!({ "cols": cols, "rows": rows }),
            )
            .expect("build the CLI first: cargo build -p jobpilot-cli");
            panel.wait_for("jobpilot> ");
            panel
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.data.lock().unwrap()).into_owned()
        }

        fn mark(&self) -> usize {
            self.data.lock().unwrap().len()
        }

        fn send(&self, bytes: &[u8]) -> Result<Value, Value> {
            invoke(&self.harness.window, "pty_write", json!({ "data": bytes }))
        }

        /// Enter, as xterm.js sends it.
        fn line(&self, line: &str) {
            self.send(format!("{line}\r").as_bytes()).unwrap();
        }

        fn wait_for(&self, needle: &str) -> String {
            self.wait_after(0, needle)
        }

        fn wait_after(&self, mark: usize, needle: &str) -> String {
            let until = Instant::now() + PTY_WAIT;
            loop {
                let seen = self.data.lock().unwrap()[mark..].to_vec();
                let text = String::from_utf8_lossy(&seen).into_owned();
                if text.contains(needle) {
                    return text;
                }
                // ConPTY asks where the cursor is before it paints; xterm.js answers this.
                let asked = text.matches("\x1b[6n").count();
                if mark == 0 && asked > self.answered.get() {
                    self.answered.set(asked);
                    self.send(b"\x1b[1;1R").unwrap();
                }
                assert!(Instant::now() < until, "no {needle:?} in {text:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn close(&self) {
            invoke(&self.harness.window, "pty_close", json!({})).unwrap();
        }
    }

    impl Drop for Panel<'_> {
        fn drop(&mut self) {
            self.harness.app.state::<PtyState>().close();
        }
    }

    #[cfg(unix)]
    fn running(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(windows)]
    fn running(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        let listed = std::process::Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&listed.stdout).contains(&pid.to_string())
    }

    fn wait_gone(pid: u32) {
        let until = Instant::now() + PTY_WAIT;
        while running(pid) {
            assert!(Instant::now() < until, "process {pid} still runs");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A stand-in for `claude` at a path the app was told in `ai.claude_path`.
    #[cfg(unix)]
    fn fake_claude(harness: &Harness, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = harness._tmp.path().join("fake");
        fs::create_dir(&dir).unwrap();
        let path = dir.join("claude");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let set = json!({ "key": "ai.claude_path", "value": path });
        invoke(&harness.window, "settings_set", set).unwrap();
        path
    }

    #[test]
    fn pty_write_without_session_is_unavailable() {
        let h = ready();
        let written = invoke(&h.window, "pty_write", json!({ "data": [97] }));
        assert_eq!(code_of(written), "unavailable");
        let resized = invoke(&h.window, "pty_resize", json!({ "cols": 80, "rows": 24 }));
        assert_eq!(code_of(resized), "unavailable");
        // Closing nothing is fine: the panel does it before every open.
        invoke(&h.window, "pty_close", json!({})).unwrap();
    }

    #[test]
    fn pty_write_over_the_limit_is_refused_before_anything_else() {
        let h = ready();
        let data = vec![b'x'; MAX_WRITE_BYTES + 1];
        let refused = invoke(&h.window, "pty_write", json!({ "data": data }));
        assert_eq!(code_of(refused), "too_large");
        let panel = Panel::open(&h);
        let refused = panel.send(&vec![b'x'; MAX_WRITE_BYTES + 1]);
        assert_eq!(code_of(refused), "too_large");
        panel.send(&vec![b'x'; MAX_WRITE_BYTES]).unwrap();
    }

    #[test]
    fn pty_open_spawns_shell_and_echoes_prompt() {
        let h = ready();
        let panel = Panel::open(&h);
        assert!(panel.text().contains("jobpilot> "));
        assert!(h.app.state::<PtyState>().pid().is_some());
    }

    #[test]
    fn pty_open_refuses_a_zero_size() {
        let h = ready();
        let refused = invoke(&h.window, "pty_open", json!({ "cols": 0, "rows": 24 }));
        assert_eq!(code_of(refused), "validation");
        assert!(h.app.state::<PtyState>().pid().is_none());
    }

    /// The webview names a size and nothing else: extra arguments do not exist for it.
    #[test]
    fn webview_cannot_choose_what_the_pty_runs() {
        let h = ready();
        let marker = h._tmp.path().join("ran");
        let (cols, rows) = SIZE;
        let script = format!("touch {}", marker.display());
        let panel = Panel::listening(&h);
        let args = json!({
            "cols": cols,
            "rows": rows,
            "command": "/bin/sh",
            "program": "/bin/sh",
            "args": ["-c", script],
            "argv": ["/bin/sh", "-c", script],
            "shell": "/bin/sh",
            "cwd": "/",
            "env": { "JOBPILOT_PANEL": "" },
        });
        invoke(&h.window, "pty_open", args).unwrap();
        panel.wait_for("jobpilot> ");
        panel.line("not-a-command");
        panel.wait_for("not-a-command");
        assert!(!marker.exists());
    }

    #[test]
    fn a_line_never_reaches_a_system_shell() {
        let h = ready();
        let panel = Panel::open(&h);
        let marker = h._tmp.path().join("ran");
        let mark = panel.mark();
        panel.line(&format!("sh -c \"echo hi > {}\"", marker.display()));
        panel.wait_after(mark, "error");
        assert!(!marker.exists());
    }

    #[test]
    fn sessions_are_capped_at_one() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid();
        let again = invoke(&h.window, "pty_open", json!({ "cols": 80, "rows": 24 }));
        assert_eq!(code_of(again), "refused");
        assert_eq!(h.app.state::<PtyState>().pid(), pid);
        panel.close();
    }

    #[test]
    fn pty_close_kills_child() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        assert!(running(pid));
        panel.close();
        assert_eq!(h.app.state::<PtyState>().pid(), None);
        wait_gone(pid);
        assert_eq!(code_of(panel.send(b"x")), "unavailable");
    }

    #[test]
    fn window_destroy_kills_the_session() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        // The mock runtime dispatches no window events, so this calls the handler `run` installs.
        let window = h.window.as_ref().window();
        pty::on_window_event(&window, &WindowEvent::Focused(false));
        assert_eq!(h.app.state::<PtyState>().pid(), Some(pid));
        pty::on_window_event(&window, &WindowEvent::Destroyed);
        wait_gone(pid);
        assert_eq!(h.app.state::<PtyState>().pid(), None);
        drop(panel);
    }

    /// Output belongs to the window that opened the session.
    #[test]
    fn output_goes_only_to_the_window_that_opened_it() {
        let h = ready();
        let other = WebviewWindowBuilder::new(&h.app, "other", Default::default())
            .build()
            .unwrap();
        let leaked = Arc::new(Mutex::new(0));
        for event in ["pty:data", "pty:exit"] {
            let sink = leaked.clone();
            other.listen(event, move |_| *sink.lock().unwrap() += 1);
        }
        let panel = Panel::open(&h);
        panel.line("exit");
        wait_until(|| !panel.exits.lock().unwrap().is_empty());
        assert_eq!(*leaked.lock().unwrap(), 0);
    }

    #[test]
    fn exit_is_reported_and_the_next_open_works() {
        let h = ready();
        let panel = Panel::open(&h);
        panel.line("exit");
        wait_until(|| !panel.exits.lock().unwrap().is_empty());
        assert_eq!(*panel.exits.lock().unwrap(), [json!(0)]);
        assert_eq!(h.app.state::<PtyState>().pid(), None);
        assert_eq!(code_of(panel.send(b"x")), "unavailable");
        let next = Panel::open(&h);
        next.close();
    }

    /// A closed session says nothing, or its exit would end the session after it.
    #[test]
    fn a_closed_session_stays_silent() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.close();
        wait_gone(pid);
        let mark = panel.mark();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(panel.mark(), mark);
        assert!(panel.exits.lock().unwrap().is_empty());
    }

    #[test]
    fn pty_child_env_has_panel_marker_and_home() {
        let h = ready();
        let panel = Panel::open(&h);
        let root = serde_json::to_string(h.paths.root()).unwrap();
        panel.line("jobpilot status --json");
        panel.wait_for(&root);

        let mark = panel.mark();
        panel.line("data delete --confirm DELETE");
        panel.wait_after(mark, "panel_refused");
        assert!(h.paths.root().exists());
    }

    /// With the terminal's foreground job gone, the prompt must still answer.
    #[test]
    fn ctrl_c_at_the_prompt_leaves_the_shell_running() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.send(b"\x03").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(running(pid));
        let mark = panel.mark();
        panel.line("jobpilot status --json");
        panel.wait_after(mark, "schema");
        assert_eq!(h.app.state::<PtyState>().pid(), Some(pid));
    }

    /// Ctrl-C is a byte until the terminal driver makes it a signal, so this needs the PTY.
    #[cfg(unix)]
    #[test]
    fn ctrl_c_reaches_the_foreground_child_and_the_shell_survives() {
        let h = ready();
        fake_claude(
            &h,
            "trap 'echo got-interrupt; exit 7' INT\necho claude-ready\nwhile :; do sleep 0.05; done",
        );
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.line("claude");
        panel.wait_for("claude-ready");
        panel.send(b"\x03").unwrap();
        panel.wait_for("got-interrupt");
        panel.wait_for("exited with code 7");
        let mark = panel.mark();
        panel.line("jobpilot status --json");
        panel.wait_after(mark, "schema");
        assert_eq!(h.app.state::<PtyState>().pid(), Some(pid));
    }

    /// The owner's session: found from the setting, with the terminal and its size.
    #[cfg(unix)]
    #[test]
    fn claude_starts_inside_the_panel_on_its_own_terminal() {
        let h = ready();
        fake_claude(
            &h,
            "test -t 0 && test -t 1 && echo on-a-tty\nstty size\necho \"term=$TERM\"\necho \"leak=[$CARGO_PKG_NAME]\"",
        );
        let panel = Panel::open(&h);
        panel.line("claude");
        panel.wait_for("on-a-tty");
        let (cols, rows) = SIZE;
        panel.wait_for(&format!("{rows} {cols}"));
        panel.wait_for("term=xterm-256color");
        // Cargo set this for the test process, so it stands for all the app's own variables.
        panel.wait_for("leak=[]");
    }

    #[cfg(unix)]
    #[test]
    fn pty_resize_reaches_the_terminal() {
        let h = ready();
        fake_claude(&h, "stty size\nread line\nstty size\n");
        let panel = Panel::open(&h);
        panel.line("claude");
        let (cols, rows) = SIZE;
        panel.wait_for(&format!("{rows} {cols}"));
        invoke(&h.window, "pty_resize", json!({ "cols": 91, "rows": 33 })).unwrap();
        panel.line("");
        panel.wait_for("33 91");
    }

    #[cfg(unix)]
    #[test]
    fn pty_close_takes_a_running_claude_with_it() {
        let h = ready();
        let pid_file = h._tmp.path().join("claude.pid");
        fake_claude(
            &h,
            &format!(
                "echo $$ > {}\necho claude-ready\nexec sleep 300",
                pid_file.display()
            ),
        );
        let panel = Panel::open(&h);
        panel.line("claude");
        panel.wait_for("claude-ready");
        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(running(pid));
        panel.close();
        wait_gone(pid);
    }

    /// `pty_write` runs on the main thread, so a child that stops reading must not stall
    /// the window; and Close must still work with input stuck behind it.
    #[cfg(unix)]
    #[test]
    fn a_child_that_stops_reading_never_blocks_the_caller() {
        let h = ready();
        // Raw, so the driver holds the input: a line-mode terminal drops what overflows.
        fake_claude(&h, "stty raw -echo\necho claude-ready\nexec sleep 300");
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.line("claude");
        panel.wait_for("claude-ready");
        let started = Instant::now();
        let refused = (0..1000)
            .map(|_| panel.send(&vec![b'x'; MAX_WRITE_BYTES]))
            .find(Result::is_err);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(code_of(refused.expect("the queue must fill")), "refused");
        panel.close();
        wait_gone(pid);
    }

    fn wait_until(done: impl Fn() -> bool) {
        let until = Instant::now() + PTY_WAIT;
        while !done() {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn code_of(result: Result<Value, Value>) -> String {
        result.unwrap_err()["code"].as_str().unwrap().to_owned()
    }
}
