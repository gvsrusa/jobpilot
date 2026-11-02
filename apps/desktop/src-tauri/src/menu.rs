//! The native menu, whose one addition is the emergency pause. A menu item and its
//! accelerator belong to the operating system, so the pause works when the webview hangs.
//! The webview also listens for the key (`App.tsx`), for a WebView2 that keeps it. Both only
//! ever set the pause: only a dialog clears it.

use tauri::menu::{Menu, MenuEvent, MenuItem, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Manager, Runtime};

use crate::commands;
use crate::state::AppState;

pub const PAUSE_ID: &str = "emergency_pause";
/// Chromium keeps Ctrl+Shift+P, and Windows keeps Ctrl+Shift+Esc.
pub const PAUSE_ACCELERATOR: &str = "CmdOrCtrl+Shift+H";

pub const SAFETY: &str = "Safety";

/// The item, with the accelerator the operating system watches for.
pub fn pause_item<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<MenuItem<R>> {
    MenuItemBuilder::with_id(PAUSE_ID, "Emergency pause")
        .accelerator(PAUSE_ACCELERATOR)
        .build(app)
}

/// The default menu, so macOS keeps Edit with copy and paste, and then a `Safety` submenu.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::default(app)?;
    let safety = SubmenuBuilder::new(app, SAFETY)
        .item(&pause_item(app)?)
        .build()?;
    menu.append(&safety)?;
    Ok(menu)
}

pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    if event.id().0 == PAUSE_ID {
        pause(app);
    }
}

/// The same function `pause_set` runs, with actor `gui`. Off the menu thread, so a busy
/// database cannot freeze the menu.
fn pause<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        if let Err(error) = commands::pause_now(&state).await {
            // No data is open then, so nothing runs that the pause could stop.
            eprintln!("emergency pause: {error}");
        }
    });
}
