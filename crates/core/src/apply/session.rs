//! The host of a Claude Code apply session: the exact `claude` command line, its run folder,
//! the kill paths, and what every ending cleans up. `jobpilot apply session <id>` runs this,
//! in the app's terminal or an ordinary one, so the emergency pause reaches it either way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::time::Instant;

use crate::ai::CancellationToken;
use crate::ai::child;
use crate::ai::claude_code::{self, ClaudeCode};
use crate::browser::runtime::{self, BrowserSpec};
use crate::browser::sweep::kill_marked;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::{audit, runner};

pub const SESSION_PROMPT: &str = "Call apply_next and begin.";
/// Names a terminal session needs beside `ClaudeCode::child_env`'s. Task 8's panel reads it.
pub const TERMINAL_ENV: &[&str] = &["SHELL", "LOGNAME", "USERNAME", "PATHEXT"];
/// A `ready` run nobody opened a session for, older than this, is ended by the sweep.
pub const READY_STALE: Duration = Duration::from_secs(10 * 60);
/// Fixed: no fact and no page text. Each application's facts come from `apply_next`.
pub const SYSTEM_PROMPT: &str = include_str!("../../assets/apply_system_prompt.txt");

/// In the run folder, written by `prepare`, gone with the folder.
const MCP_CONFIG: &str = "mcp.json";
pub const SYSTEM_PROMPT_FILE: &str = "system-prompt.txt";
/// The session folder `claude` runs in. Fixed, so `claude purge` can name it.
const SESSION: &str = "session";
/// How often the host looks for the cancel token.
const TICK: Duration = Duration::from_millis(100);
/// How often it reads `apply_runs.ended_at`.
const ENDED_POLL: Duration = Duration::from_secs(1);
/// What `claude` gets for its last message once the run has ended.
const LAST_WORD: Duration = Duration::from_secs(10);
/// A run folder that will not go, on Windows a file still held, is tried again for this long.
const REMOVE_RETRY: Duration = Duration::from_secs(2);
const PURGE_TIMEOUT: Duration = Duration::from_secs(30);
/// The run reason, and the items' reason, when `claude` exits with a failure.
pub const CLAUDE_EXITED: &str = "claude_exited";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// The run ended (`apply_next` found nothing left) and the session was then ended.
    Finished,
    /// `claude` exited on its own first.
    ClaudeExited,
}

/// Test settings. A release build ignores them all: the sidecar is this program, the runtime
/// reads no debug variable, and the run has the agent's own deadline.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct HostKnobs {
    /// The `jobpilot` the session starts as `apply-mcp`.
    pub sidecar: Option<PathBuf>,
    /// Read as `JOBPILOT_PLAYWRIGHT_CLI`, so a test needs no process-wide variable.
    pub playwright_cli: Option<PathBuf>,
    /// A shorter deadline than the agent's eight hours.
    pub run_timeout: Option<Duration>,
}

impl HostKnobs {
    pub(crate) const NONE: HostKnobs = HostKnobs {
        sidecar: None,
        playwright_cli: None,
        run_timeout: None,
    };

    pub(crate) fn sidecar(&self) -> Result<PathBuf> {
        match self.sidecar.clone().filter(|_| cfg!(debug_assertions)) {
            Some(sidecar) => Ok(sidecar),
            None => Ok(std::env::current_exe()?),
        }
    }

    pub(crate) fn run_timeout(&self) -> Option<Duration> {
        self.run_timeout.filter(|_| cfg!(debug_assertions))
    }

    pub(crate) fn playwright_cli(&self) -> Option<OsString> {
        self.playwright_cli
            .clone()
            .filter(|_| cfg!(debug_assertions))
            .map(OsString::from)
    }
}

/// One server, ours. Playwright is the proxy's child and never appears here.
pub fn mcp_config(sidecar: &Path, root: &Path, dir: &Path, service: Option<&str>) -> Value {
    let mut env = json!({ "JOBPILOT_HOME": root });
    if cfg!(debug_assertions)
        && let Some(service) = service
    {
        env["JOBPILOT_SECRET_SERVICE"] = json!(service);
    }
    json!({
        "mcpServers": {
            "jobpilot": {
                "command": sidecar,
                "args": ["apply-mcp", "--dir", dir],
                "env": env,
            }
        }
    })
}

/// The exact argument list, verified against `claude --help` 2.1.316 and 2.1.317. `--tools
/// ToolSearch` keeps one built-in, which loads tool schemas and nothing else: an interactive
/// Claude Code 2.1.317 defers MCP tools behind it, so `--tools ""` leaves the proxy's tools
/// unreachable. `--strict-mcp-config` leaves only the proxy's tools, `--allowedTools` allows
/// them without a prompt, and `dontAsk` denies anything else. The system prompt is a file in
/// the run folder (`--system-prompt-file`): a `claude.cmd` passes its arguments through
/// cmd.exe, which turns every line break into a space. The prompt goes last, after a one-value
/// flag, because the list flags take every word after them.
pub fn argv(mcp_config: &Path, system_prompt: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--tools",
        "ToolSearch",
        "--strict-mcp-config",
        "--mcp-config",
    ]
    .map(OsString::from)
    .into();
    args.push(mcp_config.as_os_str().to_owned());
    args.extend(
        [
            "--allowedTools",
            "mcp__jobpilot__*",
            "--disable-slash-commands",
            "--setting-sources",
            "",
            "--settings",
            r#"{"disableAllHooks":true}"#,
            "--permission-mode",
            "dontAsk",
            "--no-chrome",
            "--system-prompt-file",
        ]
        .map(OsString::from),
    );
    args.push(system_prompt.as_os_str().to_owned());
    args.extend(["--name", "jobpilot-apply", SESSION_PROMPT].map(OsString::from));
    args
}

/// `ClaudeCode::child_env`, the terminal's own names, and the two switches that keep the
/// owner's `CLAUDE.md` files and memory out of the session. No key, no `JOBPILOT_PANEL`.
pub fn child_env() -> Vec<(String, String)> {
    let mut env = ClaudeCode::child_env();
    for name in TERMINAL_ENV {
        if env.iter().any(|(known, _)| known == name) {
            continue;
        }
        if let Ok(value) = std::env::var(name) {
            env.push(((*name).to_owned(), value));
        }
    }
    for name in [
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
    ] {
        env.push((name.to_owned(), "1".to_owned()));
    }
    env
}

/// `<root>/apply/session`.
pub fn session_cwd(paths: &AppPaths) -> PathBuf {
    paths.apply().join(SESSION)
}

/// Run ids are hex, and `session` is not a run: anything else never names a folder to delete.
fn checked_id(apply_id: &str) -> Result<&str> {
    let ok = !apply_id.is_empty()
        && apply_id != SESSION
        && apply_id.bytes().all(|b| b.is_ascii_alphanumeric());
    if ok {
        Ok(apply_id)
    } else {
        Err(Error::Validation("not an apply run id".into()))
    }
}

fn write_mcp_config(paths: &AppPaths, dir: &Path, sidecar: &Path) -> Result<PathBuf> {
    let service = match cfg!(debug_assertions) {
        true => std::env::var("JOBPILOT_SECRET_SERVICE")
            .ok()
            .filter(|service| !service.is_empty()),
        false => None,
    };
    std::fs::create_dir_all(dir)?;
    let path = dir.join(MCP_CONFIG);
    let config = mcp_config(sidecar, paths.root(), dir, service.as_deref());
    std::fs::write(&path, config.to_string())?;
    Ok(path)
}

/// The run folder and the session folder, as the `prepare` step leaves them.
pub(crate) fn prepare_folders(db: &Db, apply_id: &str, sidecar: &Path) -> Result<()> {
    let dir = super::run_dir(db, checked_id(apply_id)?);
    write_mcp_config(db.paths(), &dir, sidecar)?;
    std::fs::write(dir.join(SYSTEM_PROMPT_FILE), SYSTEM_PROMPT)?;
    let spec = BrowserSpec {
        profile: dir.join("profile"),
        output: dir.join("output"),
    };
    std::fs::write(
        dir.join("playwright.json"),
        runtime::playwright_config(&spec).to_string(),
    )?;
    for folder in [dir.join("files"), spec.output, session_cwd(db.paths())] {
        std::fs::create_dir_all(folder)?;
    }
    Ok(())
}

/// Runs `claude` on the inherited terminal, with the `mcp.json` `prepare` wrote in `dir`, until
/// it exits, the run ends, or `cancel` fires. No process group of its own: it needs the
/// terminal's foreground group. A cancel sweeps the run folder at once, so `claude`,
/// `apply-mcp`, the Playwright server, and the browser go together, and returns
/// `Err(Cancelled)` for the runner to map. A `claude` that exits with a failure before the run
/// ended is `Err(Failed("claude_exited"))`: signed out, or a flag it no longer takes.
pub async fn run_claude(
    bin: &Path,
    dir: &Path,
    db: &Db,
    apply_id: &str,
    cancel: CancellationToken,
) -> Result<SessionEnd> {
    let config = dir.join(MCP_CONFIG);
    let system = dir.join(SYSTEM_PROMPT_FILE);
    if !config.is_file() || !system.is_file() {
        return Err(Error::Validation(
            "the run folder has no mcp.json or system prompt".into(),
        ));
    }
    let cwd = session_cwd(db.paths());
    std::fs::create_dir_all(&cwd)?;
    let mut child = Command::new(bin)
        .args(argv(&config, &system))
        .env_clear()
        .envs(child_env())
        .current_dir(&cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::Failed("cli_error".into()))?;

    let mut polled = Instant::now();
    loop {
        if cancel.is_cancelled() {
            stop(&mut child, dir).await;
            return Err(Error::Cancelled);
        }
        if let Ok(waited) = tokio::time::timeout(TICK, child.wait()).await {
            let clean = waited.is_ok_and(|status| status.success());
            return match (run_ended(db, apply_id).await, clean) {
                (true, _) => Ok(SessionEnd::Finished),
                (false, true) => Ok(SessionEnd::ClaudeExited),
                (false, false) => Err(Error::Failed(CLAUDE_EXITED.into())),
            };
        }
        if polled.elapsed() < ENDED_POLL {
            continue;
        }
        polled = Instant::now();
        if run_ended(db, apply_id).await {
            let begun = Instant::now();
            while begun.elapsed() < LAST_WORD && !cancel.is_cancelled() {
                if tokio::time::timeout(TICK, child.wait()).await.is_ok() {
                    break;
                }
            }
            stop(&mut child, dir).await;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            return Ok(SessionEnd::Finished);
        }
    }
}

/// `ended_at` is stamped. A failed read says no: a busy database must not end a session. A
/// run that is gone has ended.
async fn run_ended(db: &Db, apply_id: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT ended_at IS NOT NULL FROM apply_runs WHERE id = ?")
        .bind(apply_id)
        .fetch_optional(&db.pool)
        .await
        .map(|ended| ended.unwrap_or(true))
        .unwrap_or(false)
}

async fn stop(child: &mut Child, dir: &Path) {
    sweep_folder(dir).await;
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// `kill_marked` reads every process on the machine: off the async runtime, so the runner's
/// watch and heartbeat keep going meanwhile.
async fn sweep_folder(dir: &Path) {
    let dir = dir.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || kill_marked(&dir)).await;
}

/// What every ending gets, whoever ends it: the items settled, their statuses written when the
/// pause allows, and the run's processes and folder gone. A status left unwritten waits for
/// `apply settle`.
pub(crate) async fn wind_up(
    db: &Db,
    apply_id: &str,
    bin: Option<&Path>,
    because: Option<&str>,
) -> Result<()> {
    let finished = super::finish_items_because(db, apply_id, because).await;
    let actor: Option<String> = sqlx::query_scalar("SELECT actor FROM apply_runs WHERE id = ?")
        .bind(apply_id)
        .fetch_optional(&db.pool)
        .await?;
    let settled = match actor {
        Some(actor) => match super::settle(db, &actor, apply_id).await {
            Err(error @ Error::Db(_)) => Err(error),
            Ok(_) | Err(_) => Ok(()),
        },
        None => Ok(()),
    };
    let cleaned = cleanup(db, apply_id, bin).await;
    finished?;
    settled?;
    cleaned
}

/// Kills what the run folder names, deletes the folder, then purges the session's transcripts
/// with `claude purge`. Neither failure fails the run: each is logged by code with its folder,
/// and the next sweep tries the folder again. Nothing here prints a value.
pub async fn cleanup(db: &Db, apply_id: &str, bin: Option<&Path>) -> Result<()> {
    let dir = super::run_dir(db, checked_id(apply_id)?);
    sweep_folder(&dir).await;
    if !remove(&dir).await {
        let detail = json!({ "folder": dir.display().to_string() });
        audit::record(db, "system", "cleanup_failed", Some(apply_id), detail).await?;
    }
    let session = session_cwd(db.paths());
    // No session folder: no session ever ran here, so there is nothing to purge.
    if let Some(bin) = bin
        && session.is_dir()
        && !purge(bin, &session).await
    {
        let detail = json!({ "folder": session.display().to_string() });
        audit::record(db, "system", "purge_failed", Some(apply_id), detail).await?;
    }
    Ok(())
}

/// True when the folder is gone.
async fn remove(dir: &Path) -> bool {
    let begun = Instant::now();
    loop {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) if begun.elapsed() >= REMOVE_RETRY => return false,
            Err(_) => tokio::time::sleep(TICK).await,
        }
    }
}

/// `claude purge <session> --yes`. True when it ran and said so.
async fn purge(bin: &Path, session: &Path) -> bool {
    let mut command = child::command(bin, &child_env(), session);
    command.arg("purge").arg(session).arg("--yes");
    let Ok(mut purging) = command.spawn() else {
        return false;
    };
    matches!(
        tokio::time::timeout(PURGE_TIMEOUT, purging.wait()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Ends what a dead host left: first `runner::reap_stale`, so its agent run reads `halted`.
/// A run is active while its host lives (an agent run neither terminal nor halted), or while
/// it is `ready`, unclaimed, and younger than `READY_STALE`. A host that died in `prepare`
/// left its claim on a `ready` run, so the claim's agent run decides. Every other run not yet
/// ended gets what a host does at its end. Then a folder under `<root>/apply/` whose run is
/// gone or ended is cleaned. The session folder stays. Returns how many runs it ended.
pub async fn sweep_orphans(db: &Db) -> Result<usize> {
    runner::reap_stale(db).await?;
    let orphans: Vec<String> = sqlx::query_scalar(
        "SELECT r.id FROM apply_runs r LEFT JOIN agent_runs a ON a.id = r.agent_run_id \
         WHERE r.state <> 'ended' AND NOT ( \
           (a.state IS NOT NULL AND a.state NOT IN \
             ('completed', 'manual_required', 'refused', 'failed', 'cancelled', 'halted')) \
           OR (r.state = 'ready' AND r.agent_run_id IS NULL AND \
             (julianday('now') - julianday(r.created_at)) * 86400.0 < ?)) \
         ORDER BY r.seq",
    )
    .bind(READY_STALE.as_secs_f64())
    .fetch_all(&db.pool)
    .await?;

    let mut leftovers = Vec::new();
    if let Ok(entries) = std::fs::read_dir(db.paths().apply()) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if checked_id(&name).is_err() || !entry.path().is_dir() {
                continue;
            }
            let state: Option<String> =
                sqlx::query_scalar("SELECT state FROM apply_runs WHERE id = ?")
                    .bind(&name)
                    .fetch_optional(&db.pool)
                    .await?;
            if state.is_none_or(|state| state == "ended") {
                leftovers.push(name);
            }
        }
    }
    if orphans.is_empty() && leftovers.is_empty() {
        return Ok(0);
    }

    // Looked up only when something is left to purge for.
    let bin = match session_cwd(db.paths()).is_dir() {
        true => claude_code::find_binary(Some(db)).await.ok(),
        false => None,
    };
    for id in &orphans {
        wind_up(db, id, bin.as_deref(), None).await?;
    }
    for id in &leftovers {
        cleanup(db, id, bin.as_deref()).await?;
    }
    Ok(orphans.len())
}
