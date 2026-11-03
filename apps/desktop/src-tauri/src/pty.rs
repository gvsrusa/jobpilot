//! The terminal panel's PTY: one session of the bundled `jobpilot shell`, or of `jobpilot apply
//! session <id>` for a `ready` apply run, never a system shell. `pty_open` takes a size and
//! nothing else, and `pty_open_apply` a size and a run id, so the webview cannot choose what
//! runs.
//! Keystrokes are bytes on a queue, because a write can block for as long as the child
//! ignores its input and commands run on the main thread. Output and the exit code go out
//! as events to the window that opened the session, and output is paced by acks from the
//! panel: Claude Code repaints continuously, so a child that outruns xterm.js must block.

use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use jobpilot_core::ai::claude_code::ClaudeCode;
use jobpilot_core::browser::sweep::kill_marked;
use jobpilot_core::{AppPaths, Error, Result, apply};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tauri::{
    Emitter, EventTarget, Listener, Manager, Runtime, State, WebviewWindow, Window, WindowEvent,
};

use crate::state::AppState;

pub const MAX_WRITE_BYTES: usize = 64 * 1024;
/// Writes waiting on a child that is not reading. More than this is refused.
const INPUT_QUEUE: usize = 64;
const READ_CHUNK: usize = 8 * 1024;
/// Output bytes sent and not yet acked. Past this the reader stops, the terminal driver's
/// buffer fills, and the child blocks in its own write.
const OUTPUT_WINDOW: usize = 128 * 1024;
/// A sane ceiling on the size the webview may ask for.
const MAX_DIMENSION: u16 = 1000;
/// How long a hung-up child gets before its process groups are killed.
#[cfg(unix)]
const HANGUP_GRACE: Duration = Duration::from_millis(250);
/// How long an ended session's last output gets. A daemon the child left behind can hold
/// the terminal open for ever, and the exit must still be reported.
const DRAIN: Duration = Duration::from_millis(500);

/// What the panel's child inherits beyond the headless adapter's list: an interactive
/// session and the login-shell lookup for `claude` want them. Still no API keys or tokens.
/// One list, the apply host's own.
const PANEL_ENV: &[&str] = jobpilot_core::apply::session::TERMINAL_ENV;
/// A Finder launch has only the system folders on `PATH`. Besides `claude`, which the shell
/// also finds through `ai.claude_path`, a session needs what `claude` itself runs.
#[cfg(unix)]
const TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The slot holds at most one session: that is the cap.
#[derive(Default)]
pub struct PtyState {
    slot: Mutex<Option<Session>>,
    next_id: AtomicU64,
    /// Closes running in the background, which `close` waits for on the way out.
    ending: Mutex<Vec<JoinHandle<()>>>,
}

/// What the session runs. The webview names at most a run id; Rust builds the command.
pub enum Launch {
    Shell,
    /// `jobpilot apply session <id>`, for a run checked `ready` first.
    Apply(String),
}

pub struct Session {
    id: u64,
    /// The apply run this session hosts, if any.
    apply: Option<String>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    input: SyncSender<Vec<u8>>,
    output: Arc<Output>,
    #[cfg(any(unix, test))]
    pid: Option<u32>,
}

impl Session {
    /// Ends the child and whatever it runs. Blocks for up to the grace period, and on Windows
    /// until the pseudoconsole has closed: that close is what ends the attached processes,
    /// and the reader keeps draining meanwhile. A failed kill means the child has gone.
    fn end(mut self) {
        self.output.silence();
        #[cfg(unix)]
        let groups = [
            self.master.process_group_leader(),
            self.pid.and_then(|pid| libc::pid_t::try_from(pid).ok()),
        ];
        let _ = self.killer.kill();
        #[cfg(unix)]
        kill_groups_after_grace(groups);
    }
}

/// A hangup is only a request: `claude` may ignore it. The foreground group is the running
/// program, and the shell's own group holds whatever it started. Read before the hangup,
/// which takes the terminal away.
#[cfg(unix)]
fn kill_groups_after_grace(groups: [Option<libc::pid_t>; 2]) {
    // `kill` reads 0 and 1 as "my group" and "everything": never pass those on.
    let groups = || groups.iter().flatten().copied().filter(|group| *group > 1);
    // SAFETY: `kill` takes plain integers and touches no memory.
    let exists = |group: libc::pid_t| unsafe { libc::kill(-group, 0) } == 0;
    let until = Instant::now() + HANGUP_GRACE;
    while groups().any(exists) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    for group in groups() {
        // SAFETY: as above. A group that has gone answers ESRCH.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
}

/// The credit between the reader and the panel.
#[derive(Default)]
struct Output {
    credit: Mutex<Credit>,
    changed: Condvar,
}

#[derive(Default)]
struct Credit {
    unacked: usize,
    /// From here on this session emits nothing, or its late output and exit would land in
    /// the session that follows it.
    silent: bool,
}

impl Output {
    fn credit(&self) -> MutexGuard<'_, Credit> {
        self.credit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Blocks while the window is full. `false` means drop the chunk: nobody is listening.
    fn admit(&self, bytes: usize) -> bool {
        let mut credit = self.credit();
        while !credit.silent && credit.unacked >= OUTPUT_WINDOW {
            credit = self
                .changed
                .wait(credit)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if !credit.silent {
            credit.unacked += bytes;
        }
        !credit.silent
    }

    fn ack(&self, bytes: usize) {
        let mut credit = self.credit();
        credit.unacked = credit.unacked.saturating_sub(bytes);
        self.changed.notify_all();
    }

    fn silence(&self) {
        self.credit().silent = true;
        self.changed.notify_all();
    }
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

    /// Ends the session and returns once it has ended, closes already under way included:
    /// the process may be about to exit, and a thread would die with it. The waiter thread
    /// reaps the child.
    pub fn close(&self) {
        let session = self.slot().take();
        if let Some(session) = session {
            session.end();
        }
        let ending = std::mem::take(&mut *self.ending());
        for close in ending {
            let _ = close.join();
        }
    }

    /// For the main thread: stops the output now and leaves the grace period to a thread.
    fn close_soon(&self) {
        let session = self.slot().take();
        if let Some(session) = session {
            session.output.silence();
            self.ending().push(thread::spawn(move || session.end()));
        }
    }

    fn ending(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.ending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// For the emergency pause: ends an apply session at once, and then everything its run
    /// folder names, the browser included, which runs in a process group of its own. A plain
    /// shell session is left alone: what the owner runs there is the owner's (M-14).
    pub fn close_apply(&self, paths: &AppPaths) {
        let mut slot = self.slot();
        let Some(apply_id) = slot.as_ref().and_then(|session| session.apply.clone()) else {
            return;
        };
        let session = slot.take();
        drop(slot);
        if let Some(session) = session {
            session.output.silence();
            self.ending().push(thread::spawn(move || session.end()));
        }
        kill_marked(&paths.apply().join(apply_id));
    }

    #[cfg(test)]
    pub fn pid(&self) -> Option<u32> {
        self.slot().as_ref()?.pid
    }

    /// Stands an open shell session in for an apply session, as a real one needs a browser.
    #[cfg(all(test, unix))]
    pub fn mark_apply(&self, apply_id: &str) {
        if let Some(session) = self.slot().as_mut() {
            session.apply = Some(apply_id.to_owned());
        }
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
/// stay behind; `service` is the debug-only test keychain. `marker` sets `JOBPILOT_PANEL`,
/// which an apply host must not have: the CLI refuses `apply session` under it.
fn panel_env(
    base: Vec<(String, String)>,
    root: &Path,
    service: Option<String>,
    marker: bool,
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
    if marker {
        env.insert("JOBPILOT_PANEL".to_owned(), "1".to_owned());
    }
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
    let allowed = 1..=MAX_DIMENSION;
    if !allowed.contains(&cols) || !allowed.contains(&rows) {
        return Err(Error::Validation(format!(
            "terminal size must be 1 to {MAX_DIMENSION} in each direction"
        )));
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
    open_session(window, &app, &state, size, Launch::Shell)
}

/// Hosts a `ready` apply run's session. The id is checked against the store here, and the
/// command line is Rust's: the webview names the run and the size, and nothing else.
#[tauri::command]
pub async fn pty_open_apply<R: Runtime>(
    window: WebviewWindow<R>,
    app: State<'_, AppState>,
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
    apply_id: String,
) -> Result<()> {
    let size = checked_size(cols, rows)?;
    let run = apply::view(&*app.work().await?, &apply_id).await?;
    if run.state != "ready" {
        return Err(Error::Refused("apply_run_not_ready".into()));
    }
    open_session(window, &app, &state, size, Launch::Apply(run.id))
}

fn open_session<R: Runtime>(
    window: WebviewWindow<R>,
    app: &AppState,
    state: &PtyState,
    size: PtySize,
    launch: Launch,
) -> Result<()> {
    let root = app.known_root()?.root().to_path_buf();
    let sidecar = sidecar_path()?;
    let mut slot = state.slot();
    if slot.is_some() {
        return Err(Error::Refused("a terminal session is already open".into()));
    }

    let pair = native_pty_system().openpty(size).map_err(pty_error)?;
    let mut command = CommandBuilder::new(sidecar);
    let apply_id = match launch {
        Launch::Shell => {
            command.arg("shell");
            None
        }
        Launch::Apply(id) => {
            command.args(["apply", "session", id.as_str()]);
            Some(id)
        }
    };
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
    for (name, value) in panel_env(base, &root, service, apply_id.is_none()) {
        command.env(name, value);
    }
    let mut child = pair.slave.spawn_command(command).map_err(pty_error)?;
    // The parent's copy would keep the terminal open after the child has gone.
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    let writer = pair.master.take_writer().map_err(pty_error)?;
    let (input, queue) = mpsc::sync_channel(INPUT_QUEUE);
    let output = Arc::new(Output::default());
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let killer = child.clone_killer();
    #[cfg(any(unix, test))]
    let pid = child.process_id();

    thread::spawn(move || write_input(queue, writer));
    let drained = read_output(reader, window.clone(), output.clone());
    let credit = output.clone();
    // Scoped to this window; a late ack from an earlier session frees a little credit, no more.
    let acks = window.listen("pty:ack", move |event| {
        credit.ack(serde_json::from_str(event.payload()).unwrap_or(0));
    });
    let ended = output.clone();
    thread::spawn(move || {
        let code = child.wait().ok().map(|status| status.exit_code());
        // `None`: `close` took it, so nobody is waiting for an exit.
        if let Some(session) = window.state::<PtyState>().take(id) {
            // Windows only ends the reader when the pseudoconsole closes.
            drop(session);
            let _ = drained.recv_timeout(DRAIN);
            ended.silence();
            let _ = window.emit_to(target(&window), "pty:exit", code);
        }
        window.unlisten(acks);
    });

    *slot = Some(Session {
        id,
        apply: apply_id,
        master: pair.master,
        killer,
        input,
        output,
        #[cfg(any(unix, test))]
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
    output: Arc<Output>,
) -> mpsc::Receiver<()> {
    let (done, drained) = mpsc::channel();
    thread::spawn(move || {
        let mut chunk = vec![0; READ_CHUNK];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    if output.admit(read) {
                        let _ = window.emit_to(target(&window), "pty:data", chunk[..read].to_vec());
                    }
                }
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
    state.close_soon();
    Ok(())
}

/// A browser as Playwright starts one, for tests: the run folder on its command line, and
/// deaf to SIGTERM.
#[cfg(all(test, unix))]
pub mod test_support {
    use std::path::Path;
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant};

    pub fn marked_browser(dir: &Path) -> Child {
        let flag = format!("--user-data-dir={}", dir.join("profile").display());
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "trap '' TERM; while :; do sleep 1; done",
                "browser",
                &flag,
            ])
            .spawn()
            .unwrap();
        // Until the shell has read its trap and the system shows its arguments.
        thread::sleep(Duration::from_millis(200));
        child
    }

    pub fn gone_within_two_seconds(child: &mut Child) {
        let until = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "process {} survived", child.id());
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use jobpilot_core::AppPaths;
    use tauri::test::MockRuntime;
    use tauri::{App, WebviewWindowBuilder};

    #[cfg(unix)]
    use super::test_support::{gone_within_two_seconds, marked_browser};
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let owned = pairs.iter();
        owned.map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn panel_env_carries_the_marker_root_and_terminal() {
        let found = panel_env(env(&[("TERM", "dumb")]), Path::new("/data"), None, true);
        assert_eq!(found["JOBPILOT_PANEL"], "1");
        assert_eq!(found["JOBPILOT_HOME"], "/data");
        assert_eq!(found["TERM"], "xterm-256color");
        assert!(!found.contains_key("JOBPILOT_SECRET_SERVICE"));
    }

    #[test]
    fn panel_env_passes_only_what_it_was_given() {
        let found = panel_env(
            env(&[("USER", "me")]),
            Path::new("/data"),
            Some("t".into()),
            true,
        );
        assert_eq!(found["USER"], "me");
        assert_eq!(found["JOBPILOT_SECRET_SERVICE"], "t");
        assert!(!found.contains_key("ANTHROPIC_API_KEY"));
    }

    /// A Finder launch hands the app `/usr/bin:/bin:/usr/sbin:/sbin`.
    #[cfg(unix)]
    #[test]
    fn panel_path_reaches_the_usual_tools_on_a_finder_launch() {
        let base = env(&[("PATH", "/usr/bin:/bin"), ("HOME", "/Users/me")]);
        let path = panel_env(base, Path::new("/data"), None, true)["PATH"].clone();
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
        let path = panel_env(base, Path::new("/data"), None, true)["PATH"].clone();
        assert_eq!(path, "/opt/homebrew/bin:/usr/local/bin");
    }

    fn mock_window() -> (App<MockRuntime>, WebviewWindow<MockRuntime>) {
        let app = tauri::test::mock_app();
        let window = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        (app, window)
    }

    fn output_events(window: &WebviewWindow<MockRuntime>) -> Arc<AtomicU64> {
        let seen = Arc::new(AtomicU64::new(0));
        let count = seen.clone();
        window.listen("pty:data", move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        seen
    }

    /// Late output of a closed session must not land in the session after it, and the
    /// reader still runs to the end: a pseudoconsole cannot close until it is drained.
    #[test]
    fn a_silent_session_drains_and_emits_nothing() {
        let (_app, window) = mock_window();
        let seen = output_events(&window);
        let read = |silent: bool| {
            let bytes = Box::new(io::Cursor::new(b"late".to_vec()));
            let output = Arc::new(Output::default());
            if silent {
                output.silence();
            }
            let drained = read_output(bytes, window.clone(), output);
            drained.recv_timeout(Duration::from_secs(10)).unwrap();
        };
        read(true);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        read(false);
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    /// A child that never stops printing, counting what the reader has taken from it.
    struct Flood(Arc<AtomicU64>);

    impl Read for Flood {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.0.fetch_add(buffer.len() as u64, Ordering::SeqCst);
            buffer.fill(b'x');
            Ok(buffer.len())
        }
    }

    fn settled(count: &AtomicU64) -> u64 {
        // Bounded, so a reader that never stops fails the test instead of hanging it.
        let mut last = u64::MAX;
        for _ in 0..50 {
            let now = count.load(Ordering::SeqCst);
            if now == last {
                break;
            }
            last = now;
            thread::sleep(Duration::from_millis(100));
        }
        last
    }

    #[test]
    fn the_reader_stops_without_acks_and_resumes_with_them() {
        let (_app, window) = mock_window();
        let taken = Arc::new(AtomicU64::new(0));
        let output = Arc::new(Output::default());
        let flood = Box::new(Flood(taken.clone()));
        let _drained = read_output(flood, window.clone(), output.clone());

        let stalled = settled(&taken);
        // One chunk past the window: the one in hand when it filled, plus the one it blocks on.
        let ceiling = (OUTPUT_WINDOW + 2 * READ_CHUNK) as u64;
        assert!(stalled <= ceiling, "read {stalled} bytes with no ack");

        output.ack(OUTPUT_WINDOW);
        let resumed = settled(&taken);
        assert!(resumed > stalled, "an ack must let it read on");
        assert!(resumed <= stalled + ceiling);

        // Closing frees a stalled reader, so nothing waits on a panel that has gone.
        output.silence();
        wait_until(|| taken.load(Ordering::SeqCst) > resumed + ceiling);
    }

    #[test]
    fn an_ack_cannot_buy_more_than_the_window() {
        let output = Output::default();
        output.ack(usize::MAX);
        assert_eq!(output.credit().unacked, 0);
        assert!(output.admit(OUTPUT_WINDOW));
        assert_eq!(output.credit().unacked, OUTPUT_WINDOW);
    }

    #[test]
    fn size_is_one_to_a_thousand_each_way() {
        assert!(checked_size(1, 1).is_ok());
        assert!(checked_size(1000, 1000).is_ok());
        for (cols, rows) in [
            (0, 24),
            (80, 0),
            (1001, 24),
            (80, 1001),
            (u16::MAX, u16::MAX),
        ] {
            let refused = checked_size(cols, rows).unwrap_err();
            assert_eq!(refused.code(), "validation", "{cols}x{rows}");
        }
    }

    fn wait_until(done: impl Fn() -> bool) {
        let until = std::time::Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(std::time::Instant::now() < until, "timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The real CLI under a real terminal, with a stand-in `claude` that ignores hangups.
    #[cfg(unix)]
    struct Stubborn {
        _tmp: tempfile::TempDir,
        app: App<MockRuntime>,
        pid: i32,
    }

    #[cfg(unix)]
    impl Stubborn {
        fn start() -> Self {
            use std::os::unix::fs::PermissionsExt;

            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("fake");
            std::fs::create_dir(&bin).unwrap();
            let claude = bin.join("claude");
            let pid_file = tmp.path().join("claude.pid");
            let body = format!(
                "#!/bin/sh\ntrap '' HUP\necho $$ > {}\necho claude-ready\nwhile :; do sleep 1; done\n",
                pid_file.display()
            );
            std::fs::write(&claude, body).unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

            let paths = AppPaths::new(tmp.path().join("data"));
            let state = tauri::async_runtime::block_on(async {
                let state = AppState::open(paths).await;
                let path = serde_json::json!(claude);
                let slot = state.db.read().await;
                let db = slot.as_ref().unwrap();
                jobpilot_core::settings::set(db, "ai.claude_path", &path)
                    .await
                    .unwrap();
                drop(slot);
                state
            });
            let app = tauri::test::mock_builder()
                .manage(state)
                .manage(PtyState::default())
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            let window = WebviewWindowBuilder::new(&app, "main", Default::default())
                .build()
                .unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = seen.clone();
            window.listen("pty:data", move |event| {
                let bytes: Vec<u8> = serde_json::from_str(event.payload()).unwrap();
                sink.lock().unwrap().extend(bytes);
            });
            let wait_for = |needle: &str| {
                wait_until(|| String::from_utf8_lossy(&seen.lock().unwrap()).contains(needle));
            };
            tauri::async_runtime::block_on(pty_open(
                window.clone(),
                app.state(),
                app.state(),
                200,
                40,
            ))
            .expect("build the CLI first: cargo build -p jobpilot-cli");
            wait_for("jobpilot> ");
            pty_write(app.state(), b"claude\r".to_vec()).unwrap();
            wait_for("claude-ready");
            let pid = std::fs::read_to_string(pid_file).unwrap();
            let pid = pid.trim().parse().unwrap();
            Stubborn {
                _tmp: tmp,
                app,
                pid,
            }
        }

        /// Soon after the kill, not eventually: this runs after the close has returned.
        fn assert_gone(&self) {
            // SAFETY: `kill` takes plain integers; signal 0 only asks if the process exists.
            let alive = || unsafe { libc::kill(self.pid, 0) } == 0;
            let until = Instant::now() + Duration::from_secs(2);
            while alive() {
                assert!(Instant::now() < until, "process {} survived", self.pid);
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn close_kills_a_claude_that_ignores_the_hangup() {
        let session = Stubborn::start();
        session.app.state::<PtyState>().close();
        session.assert_gone();
    }

    /// The panel's `emit` arrives as an app-wide event, which the session must hear.
    #[cfg(unix)]
    #[test]
    fn an_ack_event_frees_the_sessions_credit() {
        let session = Stubborn::start();
        let state = session.app.state::<PtyState>();
        let unacked = || state.slot().as_ref().unwrap().output.credit().unacked;
        assert!(unacked() > 0);
        session.app.emit("pty:ack", OUTPUT_WINDOW).unwrap();
        wait_until(|| unacked() == 0);
    }

    /// `RunEvent::Exit` calls `close`: the process ends right after, so it must also wait
    /// for a close that `pty_close` left running in the background.
    #[cfg(unix)]
    #[test]
    fn close_waits_for_a_pty_close_still_under_way() {
        let session = Stubborn::start();
        let started = std::time::Instant::now();
        pty_close(session.app.state()).unwrap();
        session.app.state::<PtyState>().close();
        assert!(started.elapsed() >= HANGUP_GRACE, "close did not wait");
        session.assert_gone();
    }

    #[test]
    fn panel_env_leaves_the_marker_out_for_an_apply_host() {
        let found = panel_env(env(&[("USER", "me")]), Path::new("/data"), None, false);
        assert!(!found.contains_key("JOBPILOT_PANEL"));
        assert_eq!(found["JOBPILOT_HOME"], "/data");
        assert_eq!(PANEL_ENV, jobpilot_core::apply::session::TERMINAL_ENV);
    }

    #[cfg(unix)]
    #[test]
    fn close_apply_kills_a_fake_claude_and_a_marked_browser() {
        let session = Stubborn::start();
        let state = session.app.state::<PtyState>();
        let paths = AppPaths::new(session._tmp.path().join("data"));
        let id = "0123abcd";
        state.mark_apply(id);
        let mut browser = marked_browser(&paths.apply().join(id));

        state.close_apply(&paths);

        assert!(state.pid().is_none());
        session.assert_gone();
        gone_within_two_seconds(&mut browser);
    }

    #[cfg(unix)]
    #[test]
    fn close_apply_leaves_a_plain_shell_session_alone() {
        let session = Stubborn::start();
        let state = session.app.state::<PtyState>();
        let paths = AppPaths::new(session._tmp.path().join("data"));
        let pid = state.pid();

        state.close_apply(&paths);

        thread::sleep(HANGUP_GRACE * 2);
        assert_eq!(state.pid(), pid);
        // SAFETY: `kill` takes plain integers; signal 0 only asks if the process exists.
        assert_eq!(
            unsafe { libc::kill(session.pid, 0) },
            0,
            "claude must still run"
        );
        state.close();
        session.assert_gone();
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
