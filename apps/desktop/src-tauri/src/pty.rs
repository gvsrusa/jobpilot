//! The terminal panel's PTY: one session of the bundled `jobpilot shell`, never a system
//! shell. `pty_open` takes a size and nothing else, so the webview cannot choose what runs.
//! Keystrokes are bytes on a queue, because a write can block for as long as the child
//! ignores its input and commands run on the main thread. Output and the exit code go out
//! as events to the window that opened the session.

use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use jobpilot_core::ai::claude_code::ClaudeCode;
use jobpilot_core::{Error, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tauri::{Emitter, EventTarget, Manager, Runtime, State, WebviewWindow, Window, WindowEvent};

use crate::state::AppState;

pub const MAX_WRITE_BYTES: usize = 64 * 1024;
/// Writes waiting on a child that is not reading. More than this is refused.
const INPUT_QUEUE: usize = 64;
const READ_CHUNK: usize = 8 * 1024;
/// How long an ended session's last output gets. A daemon the child left behind can hold
/// the terminal open for ever, and the exit must still be reported.
const DRAIN: Duration = Duration::from_millis(500);

/// What the panel's child inherits beyond the headless adapter's list: an interactive
/// session and the login-shell lookup for `claude` want them. Still no API keys or tokens.
const PANEL_ENV: &[&str] = &["SHELL", "LOGNAME", "USERNAME", "PATHEXT"];
/// A Finder launch has only the system folders on `PATH`. Besides `claude`, which the shell
/// also finds through `ai.claude_path`, a session needs what `claude` itself runs.
#[cfg(unix)]
const TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The slot holds at most one session: that is the cap.
#[derive(Default)]
pub struct PtyState {
    slot: Mutex<Option<Session>>,
    next_id: AtomicU64,
}

pub struct Session {
    id: u64,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    input: SyncSender<Vec<u8>>,
    /// Set by `close`: from then on this session emits nothing, or its late output and
    /// exit would land in the session that follows it.
    silent: Arc<AtomicBool>,
    #[cfg(test)]
    pid: Option<u32>,
}

impl PtyState {
    /// Nothing here blocks while holding the lock, and a poisoned value is still one
    /// assignment: `close` must work on the way out whatever happened.
    fn slot(&self) -> MutexGuard<'_, Option<Session>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the session only if it is still `id`: one closed earlier is not this one.
    fn take(&self, id: u64) -> Option<Session> {
        let mut slot = self.slot();
        if slot.as_ref()?.id == id {
            slot.take()
        } else {
            None
        }
    }

    /// Hangs up on the child; the waiter thread reaps it. A failed kill means it is gone.
    /// Dropped elsewhere: closing a pseudoconsole on Windows waits for its output to drain.
    pub fn close(&self) {
        let session = self.slot().take();
        if let Some(mut session) = session {
            session.silent.store(true, Ordering::SeqCst);
            let _ = session.killer.kill();
            thread::spawn(move || drop(session));
        }
    }

    #[cfg(test)]
    pub fn pid(&self) -> Option<u32> {
        self.slot().as_ref()?.pid
    }
}

/// The CLI beside the app: tauri copies `externalBin` there and the installers keep it
/// there. Debug builds also look one folder up, for a test binary in `target/debug/deps`.
pub fn sidecar_path() -> Result<PathBuf> {
    find_sidecar(&std::env::current_exe()?, cfg!(debug_assertions))
}

fn find_sidecar(exe: &Path, debug: bool) -> Result<PathBuf> {
    let name = format!("jobpilot{}", std::env::consts::EXE_SUFFIX);
    let dir = exe.parent().ok_or(Error::Unavailable)?;
    let up = dir.parent().filter(|_| debug);
    std::iter::once(dir)
        .chain(up)
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
        .ok_or(Error::Unavailable)
}

/// The child's whole environment. `base` is the allow-list, so the app's own variables
/// stay behind; `service` is the debug-only test keychain.
fn panel_env(
    base: Vec<(String, String)>,
    root: &Path,
    service: Option<String>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<_, _> = base.into_iter().collect();
    #[cfg(unix)]
    if let Some(path) = with_tool_dirs(env.get("PATH"), env.get("HOME")) {
        env.insert("PATH".to_owned(), path);
    }
    env.insert(
        "JOBPILOT_HOME".to_owned(),
        root.to_string_lossy().into_owned(),
    );
    env.insert("JOBPILOT_PANEL".to_owned(), "1".to_owned());
    env.insert("TERM".to_owned(), "xterm-256color".to_owned());
    if let Some(service) = service {
        env.insert("JOBPILOT_SECRET_SERVICE".to_owned(), service);
    }
    env
}

/// Appended, so the owner's own `PATH` still wins.
#[cfg(unix)]
fn with_tool_dirs(path: Option<&String>, home: Option<&String>) -> Option<String> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path?).collect();
    let local = home.map(|home| Path::new(home).join(".local").join("bin"));
    for dir in TOOL_DIRS.iter().map(PathBuf::from).chain(local) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let joined = std::env::join_paths(dirs).ok()?;
    joined.into_string().ok()
}

fn checked_size(cols: u16, rows: u16) -> Result<PtySize> {
    if cols == 0 || rows == 0 {
        return Err(Error::Validation("terminal size must not be zero".into()));
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn pty_error(error: impl std::fmt::Display) -> Error {
    Error::Io(io::Error::other(error.to_string()))
}

/// Closes the session with the window. `RunEvent::Exit` covers a quit that destroys no window.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if matches!(event, WindowEvent::Destroyed) {
        window.state::<PtyState>().close();
    }
}

#[tauri::command]
pub async fn pty_open<R: Runtime>(
    window: WebviewWindow<R>,
    app: State<'_, AppState>,
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
) -> Result<()> {
    let size = checked_size(cols, rows)?;
    let root = app.known_root()?.root().to_path_buf();
    let sidecar = sidecar_path()?;
    let mut slot = state.slot();
    if slot.is_some() {
        return Err(Error::Refused("a terminal session is already open".into()));
    }

    let pair = native_pty_system().openpty(size).map_err(pty_error)?;
    let mut command = CommandBuilder::new(sidecar);
    command.arg("shell");
    command.env_clear();
    let mut base = ClaudeCode::child_env();
    base.extend(
        PANEL_ENV
            .iter()
            .filter_map(|name| Some(((*name).to_owned(), std::env::var(name).ok()?))),
    );
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .ok()
        .filter(|_| cfg!(debug_assertions));
    for (name, value) in panel_env(base, &root, service) {
        command.env(name, value);
    }
    let mut child = pair.slave.spawn_command(command).map_err(pty_error)?;
    // The parent's copy would keep the terminal open after the child has gone.
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    let writer = pair.master.take_writer().map_err(pty_error)?;
    let (input, queue) = mpsc::sync_channel(INPUT_QUEUE);
    let silent = Arc::new(AtomicBool::new(false));
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let killer = child.clone_killer();
    #[cfg(test)]
    let pid = child.process_id();

    thread::spawn(move || write_input(queue, writer));
    let drained = read_output(reader, window.clone(), silent.clone());
    let quiet = silent.clone();
    thread::spawn(move || {
        let code = child.wait().ok().map(|status| status.exit_code());
        // `None`: `close` took it, so nobody is waiting for an exit.
        let Some(session) = window.state::<PtyState>().take(id) else {
            return;
        };
        // Windows only ends the reader when the pseudoconsole closes.
        drop(session);
        let _ = drained.recv_timeout(DRAIN);
        quiet.store(true, Ordering::SeqCst);
        let _ = window.emit_to(target(&window), "pty:exit", code);
    });

    *slot = Some(Session {
        id,
        master: pair.master,
        killer,
        input,
        silent,
        #[cfg(test)]
        pid,
    });
    Ok(())
}

/// The webview that opened the session, and no other.
fn target<R: Runtime>(window: &WebviewWindow<R>) -> EventTarget {
    EventTarget::webview_window(window.label())
}

fn write_input(queue: mpsc::Receiver<Vec<u8>>, mut writer: Box<dyn Write + Send>) {
    for bytes in queue {
        if writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .is_err()
        {
            break;
        }
    }
}

/// Reads to the end even when silent: on Windows the pseudoconsole cannot close until its
/// output pipe is drained. The receiver hears when the output has ended.
fn read_output<R: Runtime>(
    mut reader: Box<dyn Read + Send>,
    window: WebviewWindow<R>,
    silent: Arc<AtomicBool>,
) -> mpsc::Receiver<()> {
    let (done, drained) = mpsc::channel();
    thread::spawn(move || {
        let mut chunk = vec![0; READ_CHUNK];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) if !silent.load(Ordering::SeqCst) => {
                    // ponytail: no backpressure on a flood; the child's output is short.
                    let _ = window.emit_to(target(&window), "pty:data", chunk[..read].to_vec());
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = done.send(());
    });
    drained
}

/// Queues the bytes and returns. Oversize is refused here, before the session is looked at.
#[tauri::command]
pub fn pty_write(state: State<'_, PtyState>, data: Vec<u8>) -> Result<()> {
    if data.len() > MAX_WRITE_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_WRITE_BYTES as u64,
        });
    }
    let slot = state.slot();
    let session = slot.as_ref().ok_or(Error::Unavailable)?;
    session.input.try_send(data).map_err(|error| match error {
        TrySendError::Full(_) => Error::Refused("the terminal is not reading its input".into()),
        TrySendError::Disconnected(_) => Error::Unavailable,
    })
}

#[tauri::command]
pub fn pty_resize(state: State<'_, PtyState>, cols: u16, rows: u16) -> Result<()> {
    let size = checked_size(cols, rows)?;
    let slot = state.slot();
    let session = slot.as_ref().ok_or(Error::Unavailable)?;
    session.master.resize(size).map_err(pty_error)
}

/// Fine with no session: the panel calls this before every open, to clear one a page
/// reload left behind with nobody listening.
#[tauri::command]
pub fn pty_close(state: State<'_, PtyState>) -> Result<()> {
    state.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use tauri::Listener;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let owned = pairs.iter();
        owned.map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn panel_env_carries_the_marker_root_and_terminal() {
        let found = panel_env(env(&[("TERM", "dumb")]), Path::new("/data"), None);
        assert_eq!(found["JOBPILOT_PANEL"], "1");
        assert_eq!(found["JOBPILOT_HOME"], "/data");
        assert_eq!(found["TERM"], "xterm-256color");
        assert!(!found.contains_key("JOBPILOT_SECRET_SERVICE"));
    }

    #[test]
    fn panel_env_passes_only_what_it_was_given() {
        let found = panel_env(env(&[("USER", "me")]), Path::new("/data"), Some("t".into()));
        assert_eq!(found["USER"], "me");
        assert_eq!(found["JOBPILOT_SECRET_SERVICE"], "t");
        assert!(!found.contains_key("ANTHROPIC_API_KEY"));
    }

    /// A Finder launch hands the app `/usr/bin:/bin:/usr/sbin:/sbin`.
    #[cfg(unix)]
    #[test]
    fn panel_path_reaches_the_usual_tools_on_a_finder_launch() {
        let base = env(&[("PATH", "/usr/bin:/bin"), ("HOME", "/Users/me")]);
        let path = panel_env(base, Path::new("/data"), None)["PATH"].clone();
        assert!(path.starts_with("/usr/bin:/bin:"), "{path}");
        for dir in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/Users/me/.local/bin",
        ] {
            assert!(path.split(':').any(|entry| entry == dir), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn panel_path_adds_nothing_twice() {
        let base = env(&[("PATH", "/opt/homebrew/bin:/usr/local/bin")]);
        let path = panel_env(base, Path::new("/data"), None)["PATH"].clone();
        assert_eq!(path, "/opt/homebrew/bin:/usr/local/bin");
    }

    /// Late output of a closed session must not land in the session after it, and the
    /// reader still runs to the end: a pseudoconsole cannot close until it is drained.
    #[test]
    fn a_silent_session_drains_and_emits_nothing() {
        let app = tauri::test::mock_app();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let seen = Arc::new(AtomicU64::new(0));
        let count = seen.clone();
        window.listen("pty:data", move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let read = |silent: bool| {
            let bytes = Box::new(io::Cursor::new(b"late".to_vec()));
            let silent = Arc::new(AtomicBool::new(silent));
            let drained = read_output(bytes, window.clone(), silent);
            drained.recv_timeout(Duration::from_secs(10)).unwrap();
        };
        read(true);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        read(false);
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sidecar_sits_beside_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("jobpilot{}", std::env::consts::EXE_SUFFIX);
        let app = dir.path().join("app");
        assert_eq!(find_sidecar(&app, false).unwrap_err().code(), "unavailable");
        std::fs::write(dir.path().join(&name), "").unwrap();
        assert_eq!(find_sidecar(&app, false).unwrap(), dir.path().join(&name));
    }

    /// A test binary runs from `target/debug/deps`, with the CLI one level up.
    #[test]
    fn debug_builds_look_one_folder_up() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("jobpilot{}", std::env::consts::EXE_SUFFIX);
        let deps = dir.path().join("deps");
        std::fs::create_dir(&deps).unwrap();
        std::fs::write(dir.path().join(&name), "").unwrap();
        let test_exe = deps.join("test");
        assert_eq!(
            find_sidecar(&test_exe, true).unwrap(),
            dir.path().join(&name)
        );
        let refused = find_sidecar(&test_exe, false).unwrap_err();
        assert_eq!(refused.code(), "unavailable");
    }
}
