//! The official `claude` CLI on the owner's own login (D1). The prompt goes in on stdin and
//! a JSON result comes back on stdout. The CLI finds its own login: nothing here reads
//! `~/.claude`, the keychain, or any token (M-32).
//!
//! Flags verified against `claude --help`, 2.1.316. `--bare` is never used: it ignores OAuth.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use super::validate::MAX_OUTPUT_BYTES;
use super::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Prompt, Provider,
    ProviderId, RUN_REASONS,
};
use crate::error::{Error, Result};

pub const PROVIDER: &str = "claude_code/cli_login";
pub const TERMS_DATE: &str = "2026-11-01";

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// The CLI's own text: `Not logged in · Please run /login`.
const NOT_LOGGED_IN: &str = "Not logged in";

/// Copied from the parent; everything else is dropped, API keys and tokens above all.
/// `USER` is the keychain account and the two config-dir names decide the keychain service,
/// so the login breaks without them (read from the CLI's 2.1.316 source).
const ALLOWED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "SystemRoot",
    "ComSpec",
    "LANG",
    "LC_ALL",
    "TERM",
    "XDG_CONFIG_HOME",
    "USER",
    "TMPDIR",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

pub struct ClaudeCode {
    bin: PathBuf,
    model: Option<String>,
}

impl ClaudeCode {
    /// `JOBPILOT_CLAUDE_BIN` when set, else the first absolute `PATH` entry holding `claude`.
    /// A relative entry is skipped: it would run whatever is in the working directory.
    pub fn locate() -> Result<ClaudeCode> {
        if let Some(bin) = std::env::var_os("JOBPILOT_CLAUDE_BIN").filter(|bin| !bin.is_empty()) {
            return Ok(ClaudeCode::with_binary(std::path::absolute(bin)?, None));
        }
        let path = std::env::var_os("PATH").ok_or(Error::Unavailable)?;
        std::env::split_paths(&path)
            .filter(|entry| entry.is_absolute())
            .flat_map(|entry| BIN_NAMES.iter().map(move |name| entry.join(name)))
            .find(|candidate| is_program(candidate))
            .map(|bin| ClaudeCode::with_binary(bin, None))
            .ok_or(Error::Unavailable)
    }

    pub fn with_binary(bin: PathBuf, model: Option<String>) -> ClaudeCode {
        ClaudeCode { bin, model }
    }

    pub fn path(&self) -> &Path {
        &self.bin
    }

    /// The exact argument list. The job text is never in it: that goes to stdin (M-15).
    /// `--tools ""` removes the built-in tools, `--strict-mcp-config` keeps out MCP servers,
    /// `--disable-slash-commands` skills and commands, and an empty `--setting-sources` the
    /// user, project, and local settings. None of them unloads plugins: the real `init` still
    /// lists them. The threat model records that residual.
    /// `--json-schema` works through a `StructuredOutput` tool the CLI adds, which only hands
    /// the reply back. It alone is allowed; denying it (`--disallowedTools "*"`) turns the
    /// reply into free text with a `permission_denials` entry (real 2.1.316 run).
    pub fn argv(&self, schema: &str, system: &str) -> Vec<OsString> {
        // Line breaks are the only change: Windows refuses them in a batch-file argument.
        let schema = serde_json::from_str::<Value>(schema)
            .map_or_else(|_| schema.to_string(), |value| value.to_string());
        let system = if self.is_batch() {
            system.replace("\r\n", " ").replace('\n', " ")
        } else {
            system.to_string()
        };
        let mut args: Vec<OsString> = [
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            &schema,
            "--tools",
            "",
            "--allowedTools",
            "StructuredOutput",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
            "--system-prompt",
            &system,
        ]
        .map(OsString::from)
        .into();
        if let Some(model) = &self.model {
            args.extend(["--model", model].map(OsString::from));
        }
        args
    }

    pub fn child_env() -> Vec<(String, String)> {
        let copied = ALLOWED_ENV.iter();
        copied
            .filter_map(|name| Some(((*name).to_string(), std::env::var(name).ok()?)))
            .collect()
    }

    fn is_batch(&self) -> bool {
        let extension = self.bin.extension().unwrap_or_default();
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }

    /// The child leads a process group of its own (macOS), so `KillTree` reaches what it
    /// started. Stderr is never read, so there is no pipe to drain and nothing to leak.
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.bin);
        command
            .env_clear()
            .envs(ClaudeCode::child_env())
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }

    async fn run(&self, prompt: &Prompt) -> Result<String> {
        // Dropped last, once `tree` and `child` are gone: Windows keeps a live cwd in use.
        let dir = tempfile::tempdir()?;
        let mut command = self.command(dir.path());
        command
            .args(self.argv(prompt.schema, &prompt.system))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = command.spawn()?;
        let tree = KillTree::of(&child);
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Error::Failed("no_pipes".into()));
        };

        // Together: a child that writes before it has read everything must not stall us.
        let write = async move {
            // A child that stops reading says why in its exit status and stdout.
            let _ = stdin.write_all(prompt.user.as_bytes()).await;
        };
        let limit = MAX_OUTPUT_BYTES as u64;
        let mut output = Vec::new();
        let mut capped = stdout.take(limit + 1);
        let read = capped.read_to_end(&mut output);
        let ((), read) = tokio::join!(write, read);
        read?;
        if output.len() as u64 > limit {
            return Err(Error::TooLarge { limit });
        }
        let status = child.wait().await?;
        tree.disarm();
        read_reply(&output, status)
    }
}

impl Provider for ClaudeCode {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::ClaudeCode,
            capabilities: vec![Capability::StructuredOutput, Capability::NoTools],
            local: false,
            context_limit: 200_000,
            timeout: Duration::from_secs(60),
            sign_in: format!("claude cli on its own login ({})", self.bin.display()),
        }
    }

    /// `claude --version`. Sign-in is not probed: the first real call reports it.
    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async move {
            let unavailable = |why: &str| Availability::Unavailable(why.into());
            let Ok(dir) = tempfile::tempdir() else {
                return unavailable("no_temp_dir");
            };
            let Ok(mut child) = self.command(dir.path()).arg("--version").spawn() else {
                return unavailable("not_installed");
            };
            let tree = KillTree::of(&child);
            let status = tokio::time::timeout(VERSION_TIMEOUT, child.wait()).await;
            let Ok(Ok(status)) = status else {
                return unavailable("no_answer");
            };
            tree.disarm();
            if status.success() {
                Availability::Available
            } else {
                unavailable("version_failed")
            }
        })
    }

    /// Cancel and timeout are the router's: it drops this future, and `KillTree` does the rest.
    fn complete<'a>(
        &'a self,
        prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.run(prompt))
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        _inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
        Box::pin(async { Err(Error::Refused("no_embeddings".into())) })
    }
}

/// `--output-format json` gives one result event, or with verbose on an array of events
/// ending in it. Which one arrives can depend on the user's own CLI config. Anything that
/// is not marked `type: result` is a shape this code has never seen, so it does not count.
fn result_event(stdout: &[u8]) -> Option<Value> {
    let is_result = |event: &Value| event["type"] == "result";
    match serde_json::from_slice::<Value>(stdout).ok()? {
        Value::Array(events) => events.into_iter().rfind(is_result),
        reply if is_result(&reply) => Some(reply),
        _ => None,
    }
}

/// Fails closed on drift: a CLI update that drops or retypes a field here ends the attempt,
/// since each one is what proves the reply clean. Every code is on `RUN_REASONS`. A login
/// failure can be `subtype: success`, so it is checked first, and only in a failed reply:
/// a good one may quote the words from a posting.
fn read_reply(stdout: &[u8], status: ExitStatus) -> Result<String> {
    let bad_reply = || Error::Failed("bad_reply".into());
    let cli_error = || Error::Failed("cli_error".into());
    let refused_login = || Error::Refused("not_logged_in".into());

    let Some(reply) = result_event(stdout) else {
        return Err(if String::from_utf8_lossy(stdout).contains(NOT_LOGGED_IN) {
            refused_login()
        } else if status.success() {
            bad_reply()
        } else {
            cli_error()
        });
    };

    let clean = reply["is_error"] == false && reply["subtype"] == "success";
    let text = reply["result"].as_str().unwrap_or_default();
    if !clean && text.contains(NOT_LOGGED_IN) {
        return Err(refused_login());
    }
    let (Some(is_error), Some(subtype), Some(denials)) = (
        reply["is_error"].as_bool(),
        reply["subtype"].as_str(),
        reply["permission_denials"].as_array(),
    ) else {
        return Err(bad_reply());
    };
    if is_error || subtype != "success" {
        let known = RUN_REASONS.contains(&subtype);
        return Err(Error::Refused(
            if known { subtype } else { "cli_error" }.into(),
        ));
    }
    // A tool was reached for and denied, so this is not the reply the task asked for.
    if !denials.is_empty() {
        return Err(Error::Refused("permission_denied".into()));
    }
    if !status.success() {
        return Err(cli_error());
    }
    match (&reply["structured_output"], reply["result"].as_str()) {
        (Value::Null, Some(text)) => Ok(text.to_string()),
        (Value::Null, None) => Err(bad_reply()),
        (structured, _) => Ok(structured.to_string()),
    }
}

/// Ends the child and everything it started when dropped armed: `kill_on_drop` reaches the
/// direct child only, and a `.cmd` shim or a helper would live on. It must drop before the
/// `Child`, which `run` gets by declaring it later: on Windows the tree walk starts from a
/// live parent, and on macOS the pid must not have been reaped and reused.
// ponytail: nothing kills the child if the app dies without dropping this. The 60 s limit
// lives in the router, so it dies too, and `process_group(0)` also hides the child from a
// terminal Ctrl-C. The ceiling is the CLI's own run, which this app does not bound; broken
// pipes are all it notices. A Windows job object and a macOS watchdog would close that.
struct KillTree(Option<u32>);

impl KillTree {
    fn of(child: &Child) -> KillTree {
        KillTree(child.id())
    }

    /// The child was waited for, so its pid is no longer ours to signal.
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for KillTree {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_tree(pid);
        }
    }
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    if let Ok(group) = i32::try_from(pid) {
        // SAFETY: no pointers. `group` is the group `process_group(0)` made for this child,
        // and the child is unreaped, so the id cannot have been reused.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
}

// ponytail: blocks the runtime thread for the taskkill run, on cancel and timeout only. It
// has to finish first, because `kill_on_drop` then ends the parent the walk starts from.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let _ = std::process::Command::new(PathBuf::from(root).join(r"System32\taskkill.exe"))
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(unix)]
const BIN_NAMES: &[&str] = &["claude"];
// The native binary first: cmd.exe caps a command line at 8191 characters, the .exe does not.
#[cfg(windows)]
const BIN_NAMES: &[&str] = &["claude.exe", "claude.cmd"];

#[cfg(unix)]
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_program(path: &Path) -> bool {
    path.is_file()
}
