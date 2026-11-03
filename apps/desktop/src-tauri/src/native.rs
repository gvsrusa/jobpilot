//! Dialogs Rust opens itself (M-13). The webview can ask for an action and can never answer
//! for the user: what is confirmed or picked comes back here, not through IPC, so no webview
//! capability is involved. Each dialog waits on a blocking thread, and no caller may hold a
//! state lock while one is open. The clipboard is read here for the same reason: a key comes
//! to Rust from the system, and the webview has no clipboard permission at all.

use std::path::PathBuf;

use jobpilot_core::secrets::Zeroizing;
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
    match ask(app, title, message, action, "Cancel").await? {
        true => Ok(()),
        false => Err(Error::ConfirmationRequired),
    }
}

/// Hands a link to the system browser. The caller has already checked it is `https`.
#[cfg(not(test))]
pub async fn open_url<R: Runtime>(app: &AppHandle<R>, url: &str) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;

    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| Error::Failed("the link could not be opened".into()))
}

/// `Err(Cancelled)` when the user closes the picker.
pub async fn open_file<R: Runtime>(app: &AppHandle<R>, dialog: FileDialog<'_>) -> Result<PathBuf> {
    pick(app, dialog, false).await?.ok_or(Error::Cancelled)
}

/// `Err(Cancelled)` when the user closes the picker.
pub async fn save_file<R: Runtime>(app: &AppHandle<R>, dialog: FileDialog<'_>) -> Result<PathBuf> {
    pick(app, dialog, true).await?.ok_or(Error::Cancelled)
}

/// `None` when it holds no text, an image for one. `Failed` when it could not be read.
#[cfg(not(test))]
pub async fn clipboard_text<R: Runtime>(app: &AppHandle<R>) -> Result<Option<String>> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    // ponytail: the plugin flattens arboard's errors to strings, so its `ContentNotAvailable`
    // is told by its words. If they change, an empty clipboard reads as a failed one, which
    // refuses a paste and asks for a clear that was not needed. No key is lost either way.
    const NO_TEXT: &str = "not available";
    let app = app.clone();
    off_the_main_thread(move || match app.clipboard().read_text() {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.to_string().contains(NO_TEXT) => Ok(None),
        // The words are the plugin's, so they stay here.
        Err(_) => Err(Error::Failed("the clipboard could not be read".into())),
    })
    .await?
}

/// The dialog promised a clear, and the key may still be on the clipboard.
fn not_cleared() -> Error {
    Error::Failed(
        "the key is stored, but the clipboard could not be cleared. Clear it yourself".into(),
    )
}

/// Clears the clipboard only if it still holds `was`: what the user copied since is theirs.
// ponytail: the check and the clear are two calls, as the system has no compare-and-clear.
// A copy landing between them is lost.
pub async fn clear_clipboard<R: Runtime>(app: &AppHandle<R>, was: &str) -> Result<()> {
    let now = clipboard_text(app).await.map_err(|_| not_cleared())?;
    match now.map(Zeroizing::new) {
        Some(now) if now.as_str() == was => wipe_clipboard(app).await,
        _ => Ok(()),
    }
}

#[cfg(not(test))]
async fn wipe_clipboard<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let app = app.clone();
    off_the_main_thread(move || app.clipboard().clear().is_ok())
        .await?
        .then_some(())
        .ok_or_else(not_cleared)
}

#[cfg(not(test))]
async fn off_the_main_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| Error::Failed("the dialog stopped unexpectedly".into()))
}

/// True for `action`, false for `cancel` or a closed dialog. Both answers are the person's.
#[cfg(not(test))]
pub(crate) async fn ask<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: String,
    action: &str,
    cancel: &str,
) -> Result<bool> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    let builder = app
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            action.to_owned(),
            cancel.to_owned(),
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
    /// What the system clipboard holds, as the mock runtime has none.
    pub clipboard: std::sync::Mutex<Option<String>>,
    /// The next clipboard reads fail, as a busy clipboard does.
    pub clipboard_fails: std::sync::atomic::AtomicBool,
    /// Every link handed to the system browser.
    pub opened: std::sync::Mutex<Vec<String>>,
    /// Every dialog opened, as title and message.
    pub asked: std::sync::Mutex<Vec<String>>,
    /// For each confirmation, whether a `Work` was open while it waited.
    pub work_at_dialog: std::sync::Mutex<Vec<bool>>,
    /// Runs while a confirmation is open, as the world moving under the person's feet.
    #[allow(clippy::type_complexity)]
    pub on_ask: std::sync::Mutex<
        Option<Box<dyn Fn() -> jobpilot_core::ai::BoxFuture<'static, ()> + Send + Sync>>,
    >,
}

#[cfg(test)]
impl Default for NativeHook {
    fn default() -> Self {
        Self {
            confirm: true.into(),
            picked: Default::default(),
            clipboard: Default::default(),
            clipboard_fails: false.into(),
            opened: Default::default(),
            asked: Default::default(),
            work_at_dialog: Default::default(),
            on_ask: Default::default(),
        }
    }
}

#[cfg(test)]
pub async fn open_url<R: Runtime>(app: &AppHandle<R>, url: &str) -> Result<()> {
    use tauri::Manager;

    app.state::<NativeHook>()
        .opened
        .lock()
        .unwrap()
        .push(url.to_owned());
    Ok(())
}

#[cfg(test)]
pub async fn clipboard_text<R: Runtime>(app: &AppHandle<R>) -> Result<Option<String>> {
    use tauri::Manager;

    let hook = app.state::<NativeHook>();
    if hook
        .clipboard_fails
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err(Error::Failed("the clipboard could not be read".into()));
    }
    let text = hook.clipboard.lock().unwrap().clone();
    Ok(text)
}

#[cfg(test)]
async fn wipe_clipboard<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    use tauri::Manager;

    let hook = app.state::<NativeHook>();
    hook.clipboard.lock().unwrap().take();
    Ok(())
}

#[cfg(test)]
pub(crate) async fn ask<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: String,
    _action: &str,
    _cancel: &str,
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
    let moving = hook.on_ask.lock().unwrap().as_ref().map(|change| change());
    if let Some(moving) = moving {
        moving.await;
    }
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
