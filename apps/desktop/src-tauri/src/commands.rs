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
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};

    use jobpilot_core::{AppPaths, Db, Error, backup};
    use serde_json::{Value, json};
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;
    use tauri::{App, Manager, WebviewWindow, WebviewWindowBuilder};
    use tempfile::TempDir;

    use super::handler;
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
}
