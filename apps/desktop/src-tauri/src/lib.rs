mod commands;
mod state;

use jobpilot_core::AppPaths;
use tauri::{AppHandle, Manager};

use state::AppState;

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Each is best effort: the launch that asked for this has already exited.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
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
        .invoke_handler(commands::handler())
        .setup(|app| {
            let state = match AppPaths::from_env() {
                Ok(paths) => tauri::async_runtime::block_on(AppState::open(paths)),
                Err(error) => AppState::no_root(&error),
            };
            app.manage(state);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
