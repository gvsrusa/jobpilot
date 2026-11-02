//! Dialogs Rust opens itself (M-13). The webview can ask for an action and can never answer
//! for the user: what is confirmed or picked comes back here, not through IPC, so no webview
//! capability is involved. Each dialog waits on a blocking thread, and no caller may hold a
//! state lock while one is open.

use std::path::PathBuf;

use jobpilot_core::{Error, Result};
use tauri::{AppHandle, Runtime};

/// What a file dialog offers, and the name a save dialog starts with. No `extensions` means
/// no filter: a Unix program has no extension to match.
pub struct FileDialog<'a> {
    pub title: &'a str,
    pub filter: &'a str,
    pub extensions: &'a [&'a str],
    pub file_name: Option<String>,
}

/// `Err(ConfirmationRequired)` unless the user pressed the action button.
pub async fn confirmed<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: String,
    action: &str,
) -> Result<()> {
    match ask(app, title, message, action).await? {
        true => Ok(()),
        false => Err(Error::ConfirmationRequired),
    }
}

/// `Err(Cancelled)` when the user closes the picker.
pub async fn open_file<R: Runtime>(app: &AppHandle<R>, dialog: FileDialog<'_>) -> Result<PathBuf> {
    pick(app, dialog, false).await?.ok_or(Error::Cancelled)
}

/// `Err(Cancelled)` when the user closes the picker.
pub async fn save_file<R: Runtime>(app: &AppHandle<R>, dialog: FileDialog<'_>) -> Result<PathBuf> {
    pick(app, dialog, true).await?.ok_or(Error::Cancelled)
}

#[cfg(not(test))]
async fn off_the_main_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| Error::Failed("the dialog stopped unexpectedly".into()))
}

#[cfg(not(test))]
async fn ask<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: String,
    action: &str,
) -> Result<bool> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    let builder = app
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            action.to_owned(),
            "Cancel".to_owned(),
        ));
    off_the_main_thread(move || builder.blocking_show()).await
}

#[cfg(not(test))]
async fn pick<R: Runtime>(
    app: &AppHandle<R>,
    dialog: FileDialog<'_>,
    save: bool,
) -> Result<Option<PathBuf>> {
    use tauri_plugin_dialog::DialogExt;

    let mut builder = app.dialog().file().set_title(dialog.title);
    if !dialog.extensions.is_empty() {
        builder = builder.add_filter(dialog.filter, dialog.extensions);
    }
    if let Some(name) = dialog.file_name {
        builder = builder.set_file_name(name);
    }
    let picked = off_the_main_thread(move || match save {
        true => builder.blocking_save_file(),
        false => builder.blocking_pick_file(),
    })
    .await?;
    picked
        .map(|path| {
            path.into_path()
                .map_err(|_| Error::Validation("this is not a file on this computer".into()))
        })
        .transpose()
}

/// `NATIVE_CONFIRM` and the picked path, per harness so parallel tests cannot race. It
/// replaces only the dialog itself: the mock runtime has no window to show one in.
#[cfg(test)]
pub struct NativeHook {
    pub confirm: std::sync::atomic::AtomicBool,
    pub picked: std::sync::Mutex<Option<PathBuf>>,
    /// Every dialog opened, as title and message.
    pub asked: std::sync::Mutex<Vec<String>>,
    /// For each confirmation, whether a `Work` was open while it waited.
    pub work_at_dialog: std::sync::Mutex<Vec<bool>>,
}

#[cfg(test)]
impl Default for NativeHook {
    fn default() -> Self {
        Self {
            confirm: true.into(),
            picked: Default::default(),
            asked: Default::default(),
            work_at_dialog: Default::default(),
        }
    }
}

#[cfg(test)]
async fn ask<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: String,
    _action: &str,
) -> Result<bool> {
    use std::sync::atomic::Ordering;
    use tauri::Manager;

    let hook = app.state::<NativeHook>();
    hook.asked
        .lock()
        .unwrap()
        .push(format!("{title}: {message}"));
    let work = app.state::<crate::state::AppState>().work_is_open();
    hook.work_at_dialog.lock().unwrap().push(work);
    Ok(hook.confirm.load(Ordering::SeqCst))
}

#[cfg(test)]
async fn pick<R: Runtime>(
    app: &AppHandle<R>,
    dialog: FileDialog<'_>,
    _save: bool,
) -> Result<Option<PathBuf>> {
    use tauri::Manager;

    let hook = app.state::<NativeHook>();
    let FileDialog {
        title,
        filter,
        extensions,
        file_name,
    } = dialog;
    let asked = format!("{title}: {filter} {extensions:?} {file_name:?}");
    hook.asked.lock().unwrap().push(asked);
    Ok(hook.picked.lock().unwrap().take())
}
