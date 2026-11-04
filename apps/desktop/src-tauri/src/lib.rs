mod commands;
mod menu;
mod native;
mod pty;
mod state;
mod update;

use jobpilot_core::AppPaths;
use tauri::{AppHandle, Manager, RunEvent, Runtime};

use pty::PtyState;
use state::AppState;
use update::UpdateState;

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Each is best effort: the launch that asked for this has already exited.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// A `jobpilot pause set` from the CLI ends a download, an index, or an apply session started
/// here, within a second. Runs are ended by their own poll in core.
fn watch_pause(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Some(state) = app.try_state::<AppState>() {
                commands::stop_if_paused(&state, &app.state::<PtyState>()).await;
            }
        }
    });
}

/// Once: what an apply host that died left behind. Off the setup thread, as a purge can wait.
fn sweep_orphans(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Some(state) = app.try_state::<AppState>() {
            commands::sweep_at_start(&state).await;
        }
    });
}

/// The app's way out: the terminal closed, a copied answer cleared, running tasks ended. Run at
/// `RunEvent::Exit`, and by a Windows update install, whose installer exits the process with no
/// such event.
pub(crate) async fn teardown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(pty) = app.try_state::<PtyState>() {
        pty.close();
    }
    commands::clear_copied(app).await;
    if let Some(state) = app.try_state::<AppState>() {
        state.shutdown().await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // First, so a second launch exits before it opens the database.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // Rust opens a stored https link. No capability gives the webview this plugin.
        .plugin(tauri_plugin_opener::init())
        // Rust only: no capability gives the webview the updater or process plugin. The
        // commands in `update` are the one path, and they verify against the pinned key.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(PtyState::default())
        .manage(UpdateState::default())
        .menu(menu::build)
        .on_menu_event(menu::on_event)
        .on_window_event(pty::on_window_event)
        .invoke_handler(commands::handler())
        .setup(|app| {
            let state = match AppPaths::from_env() {
                Ok(paths) => tauri::async_runtime::block_on(AppState::open(paths)),
                Err(error) => AppState::no_root(&error),
            };
            app.manage(state);
            sweep_orphans(app.handle().clone());
            watch_pause(app.handle().clone());
            update::check_at_launch(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if matches!(event, RunEvent::Exit) {
                tauri::async_runtime::block_on(teardown(app));
            }
        });
}
