//! The official `claude` CLI on the owner's own login (D1). The prompt goes in on stdin and
//! a JSON result comes back on stdout. The CLI finds its own login: nothing here reads
//! `~/.claude`, the keychain, or any token (M-32).
//!
//! Flags verified against `claude --help`, 2.1.316. `--bare` is never used: it ignores OAuth.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use super::child::{self, ChildRun, KillTree, is_program};
use super::validate::MAX_OUTPUT_BYTES;
use super::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Prompt, Provider,
    ProviderId, RUN_REASONS,
};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::settings;

pub const PROVIDER: &str = "claude_code/cli_login";
pub const TERMS_DATE: &str = "2026-11-01";

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// The CLI's own text: `Not logged in · Please run /login`.
const NOT_LOGGED_IN: &str = "Not logged in";

/// On top of the shared list: `USER` is the keychain account and the two config-dir names
/// decide the keychain service, so the login breaks without them (read from the CLI's
/// 2.1.316 source).
const CLAUDE_ENV: &[&str] = &["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"];

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
        search(absolute_dirs(&path))
            .map(|bin| ClaudeCode::with_binary(bin, None))
            .ok_or(Error::Unavailable)
    }

    /// The full discovery (`find_binary`), so a GUI launch with a minimal `PATH` still
    /// finds `claude`. Not found leaves the adapter listed but unavailable.
    pub async fn locate_in(db: &Db, model: Option<String>) -> Result<ClaudeCode> {
        find_binary(Some(db))
            .await
            .map(|bin| ClaudeCode::with_binary(bin, model))
            .map_err(|_| Error::Unavailable)
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
        child::child_env(CLAUDE_ENV)
    }

    fn is_batch(&self) -> bool {
        let extension = self.bin.extension().unwrap_or_default();
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }

    fn command(&self, cwd: &Path) -> Command {
        child::command(&self.bin, &ClaudeCode::child_env(), cwd)
    }

    async fn run(&self, prompt: &Prompt) -> Result<String> {
        // Dropped last, once `tree` and `child` are gone: Windows keeps a live cwd in use.
        let dir = tempfile::tempdir()?;
        let run = ChildRun {
            bin: &self.bin,
            args: self.argv(prompt.schema, &prompt.system),
            env: ClaudeCode::child_env(),
            cwd: dir.path(),
            stdin: prompt.user.as_bytes(),
            limit: MAX_OUTPUT_BYTES as u64,
            stderr_limit: 0,
        };
        let output = child::run_piped(run).await?;
        read_reply(&output.stdout, output.status)
    }
}

impl Provider for ClaudeCode {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::ClaudeCode,
            capabilities: vec![Capability::StructuredOutput, Capability::NoTools],
            local: false,
            // The standard window. The router's estimate runs high, so no margin is taken here.
            context_limit: 200_000,
            timeout: Duration::from_secs(180),
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

/// The setting that names the `claude` binary when discovery cannot find it.
pub const PATH_SETTING: &str = "ai.claude_path";

/// A relative entry would run whatever sits in the working directory.
pub fn absolute_dirs(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    child::absolute_dirs(path)
}

pub fn not_found() -> String {
    format!(
        "claude was not found. Install Claude Code, or set its full path with \
         `jobpilot ai claude-path <path>` (setting {PATH_SETTING}) in an ordinary terminal."
    )
}

/// Only a program named like Claude Code's own is ever run from a configured path, so a
/// setting cannot turn into "run this other program".
pub fn checked(bin: PathBuf, source: &str) -> std::result::Result<PathBuf, String> {
    if bin.is_absolute() && has_claude_name(&bin) && is_program(&bin) {
        Ok(bin)
    } else {
        Err(format!(
            "{source} points to {}, which is not an executable named claude. {}",
            bin.display(),
            not_found()
        ))
    }
}

pub fn has_claude_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    #[cfg(unix)]
    let named = name == "claude";
    #[cfg(windows)]
    let named = ["claude", "claude.exe", "claude.cmd"]
        .iter()
        .any(|allowed| name.eq_ignore_ascii_case(allowed));
    named || is_release(path, name)
}

/// The native installer's `.../claude/versions/<version>`, where its `claude` link points.
/// The macOS open panel resolves links and rfd has no switch for that, so a picked link
/// comes back as this file.
// ponytail: a path stored this way ends with that release: an update removes the file, and
// `find_binary` goes back to discovery. The link cannot be stored from the picker.
fn is_release(path: &Path, name: &str) -> bool {
    let folder_is = |level: usize, wanted: &str| {
        let folder = path.ancestors().nth(level).and_then(Path::file_name);
        folder.is_some_and(|folder| folder == wanted)
    };
    name.starts_with(|first: char| first.is_ascii_digit())
        && folder_is(1, "versions")
        && folder_is(2, "claude")
}

/// Order: `JOBPILOT_CLAUDE_BIN`, the `ai.claude_path` setting, `PATH`, the usual install
/// folders, then one lookup through the owner's login shell. GUI launches on macOS carry a
/// minimal `PATH`, which is why the last two exist. A setting that names something else is an
/// error, never a silent fall through. One that is well formed and whose file is gone is
/// skipped: an update removes the release it named, and discovery is what an unset path gets.
pub async fn find_binary(db: Option<&Db>) -> std::result::Result<PathBuf, String> {
    if let Some(found) = find_binary_quietly(db).await? {
        return Ok(found);
    }
    #[cfg(unix)]
    if let Some(found) = login_shell_lookup(&std::env::var_os("SHELL").unwrap_or_default()).await {
        return Ok(found);
    }
    Err(not_found())
}

/// [`find_binary`] with no login shell: it starts no process, so a screen may ask it on
/// every open. `Ok(None)` where `find_binary` would go on to the shell.
pub async fn find_binary_quietly(db: Option<&Db>) -> std::result::Result<Option<PathBuf>, String> {
    if let Some(bin) = std::env::var_os("JOBPILOT_CLAUDE_BIN").filter(|bin| !bin.is_empty()) {
        return checked(PathBuf::from(bin), "JOBPILOT_CLAUDE_BIN").map(Some);
    }
    if let Some(bin) = path_setting(db).await.map(PathBuf::from)
        && !is_gone(&bin)
    {
        return checked(bin, PATH_SETTING).map(Some);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    Ok(search(
        absolute_dirs(&path)
            .into_iter()
            .chain(fallback_dirs(home_dir())),
    ))
}

/// A path that could be Claude Code's, and that the file system says is not there. A path it
/// cannot answer for stays an error, as does any other name.
fn is_gone(bin: &Path) -> bool {
    bin.is_absolute() && has_claude_name(bin) && matches!(bin.try_exists(), Ok(false))
}

/// A failure to read the setting just means discovery continues without it.
async fn path_setting(db: Option<&Db>) -> Option<String> {
    let value = settings::get(db?, PATH_SETTING).await.ok()??;
    let path = value.as_str()?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Where Claude Code's installers and the common package managers put it.
pub fn fallback_dirs(home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".claude").join("local"));
        dirs.push(home.join(".npm-global").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
    }
    #[cfg(unix)]
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    #[cfg(windows)]
    for name in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(name).map(PathBuf::from) {
            dirs.push(base.join("npm"));
            dirs.push(base.join("Programs").join("claude"));
        }
    }
    dirs
}

pub fn search(dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    child::search(dirs, &child::bin_names("claude", cfg!(windows)))
}

/// Asks the owner's login shell where `claude` is: a fixed command, never user input. The
/// shell's startup files may print noise, so the last absolute line is the answer.
#[cfg(unix)]
pub async fn login_shell_lookup(shell: &std::ffi::OsStr) -> Option<PathBuf> {
    child::login_shell_lookup(shell, "claude", &has_claude_name).await
}

#[cfg(test)]
mod locate_tests {
    use super::*;

    #[test]
    fn relative_path_entries_are_dropped() {
        let absolute = if cfg!(windows) {
            r"C:\abs\bin"
        } else {
            "/abs/bin"
        };
        let joined = std::env::join_paths(["rel/bin", absolute, "."]).unwrap();
        assert_eq!(absolute_dirs(&joined), vec![PathBuf::from(absolute)]);
    }

    #[test]
    fn only_claude_names_pass() {
        assert!(has_claude_name(Path::new("/x/claude")));
        assert!(!has_claude_name(Path::new("/bin/sh")));
        assert!(!has_claude_name(Path::new("/x/claude-evil")));
        assert!(!has_claude_name(Path::new("/x/Claude.sh")));
        let message = checked(PathBuf::from("/bin/sh"), PATH_SETTING).unwrap_err();
        assert!(message.contains("ai.claude_path"), "{message}");
    }
}
