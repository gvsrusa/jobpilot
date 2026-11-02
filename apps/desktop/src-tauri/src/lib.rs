mod commands;
mod menu;
mod native;
mod pty;
mod state;

use jobpilot_core::AppPaths;
use tauri::{AppHandle, Manager, RunEvent};

use pty::PtyState;
use state::AppState;

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Each is best effort: the launch that asked for this has already exited.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// A `jobpilot pause set` from the CLI ends a download or an index started here, within a
/// second. Runs are ended by their own poll in core.
fn watch_pause(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Some(state) = app.try_state::<AppState>() {
                commands::stop_if_paused(&state).await;
            }
        }
    });
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
        .manage(PtyState::default())
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
            watch_pause(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if matches!(event, RunEvent::Exit) {
                app.state::<PtyState>().close();
                if let Some(state) = app.try_state::<AppState>() {
                    tauri::async_runtime::block_on(state.shutdown());
                }
            }
        });
}
