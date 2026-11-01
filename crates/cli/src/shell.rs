//! `jobpilot shell`: the restricted prompt behind the app's terminal panel. A line is split
//! with `shlex` and parsed by clap in this process. No system shell ever sees it, so
//! `$(id)`, backticks, pipes, and redirects are just unknown words. The one exception is
//! `claude`, which starts the owner's own interactive Claude Code session.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use clap::Parser;
use jobpilot_core::{AppPaths, Db, Error, Result, settings};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;

use crate::{Cli, Command as Jobpilot};

pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const PROMPT: &str = "jobpilot> ";
pub const REFUSAL: &str = "Run this in an ordinary terminal, or use the app.";
pub const CLAUDE_WARNING: &str = "Starting your own claude session. Anthropic's consumer terms apply to what you type here; JobPilot does not read this session. Do not paste job postings into it.";

/// The setting that names the `claude` binary when discovery cannot find it.
pub const CLAUDE_PATH_SETTING: &str = "ai.claude_path";
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum Allowed {
    /// Arguments for the jobpilot parser, without the program name.
    Jobpilot(Vec<String>),
    /// Arguments handed to the located `claude`.
    Claude(Vec<String>),
    Refused,
}

/// One leading `jobpilot` is dropped. `claude` starts a session; a nested `shell` is refused.
pub fn allowed(argv: &[String]) -> Allowed {
    let argv = match argv.first().map(String::as_str) {
        Some("jobpilot") => &argv[1..],
        _ => argv,
    };
    match argv.first().map(String::as_str) {
        Some("claude") => Allowed::Claude(argv[1..].to_vec()),
        Some("shell") => Allowed::Refused,
        _ => Allowed::Jobpilot(argv.to_vec()),
    }
}

enum Line {
    Eof,
    Text(String),
    TooLong,
}

/// Reads one line, holding at most `MAX_LINE_BYTES`. The rest of an oversize line is read
/// and dropped, so a pasted wall of text can neither grow memory nor reach the parser.
async fn read_line(input: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Line> {
    let mut line = Vec::new();
    let mut seen = false;
    let mut too_long = false;
    loop {
        let chunk = input.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(match (seen, too_long) {
                (false, _) => Line::Eof,
                (true, true) => Line::TooLong,
                (true, false) => Line::Text(decode(&line)),
            });
        }
        seen = true;
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let used = newline.map_or(chunk.len(), |at| at + 1);
        if !too_long {
            let body = &chunk[..newline.unwrap_or(chunk.len())];
            if line.len() + body.len() > MAX_LINE_BYTES {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(body);
            }
        }
        input.consume(used);
        if newline.is_some() {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Text(decode(&line))
            });
        }
    }
}

fn decode(line: &[u8]) -> String {
    String::from_utf8_lossy(line)
        .trim_end_matches('\r')
        .to_string()
}

pub async fn run(
    mut input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Result<()> {
    // Ctrl-C belongs to what runs in the foreground, not to this prompt: a stray one
    // must not close the panel. Commands that listen for it (a model pull) still get it.
    let swallow = tokio::spawn(async {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
        }
    });
    let result = repl(&mut input, &mut output).await;
    swallow.abort();
    result
}

async fn repl(
    input: &mut (impl AsyncBufRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    loop {
        say(output, PROMPT).await?;
        let line = match read_line(input).await? {
            Line::Eof => return Ok(()),
            Line::TooLong => {
                say(output, "line too long\n").await?;
                continue;
            }
            Line::Text(line) => line,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "exit" | "quit") {
            return Ok(());
        }
        let Some(argv) = shlex::split(line) else {
            say(output, "error[parse]: unbalanced quote\n").await?;
            continue;
        };
        let reply = match allowed(&argv) {
            Allowed::Refused => refusal(),
            Allowed::Claude(args) => claude(output, &args).await?,
            Allowed::Jobpilot(args) => jobpilot(args).await,
        };
        if !reply.is_empty() {
            say(output, &format!("{}\n", reply.trim_end())).await?;
        }
    }
}

async fn say(output: &mut (impl AsyncWrite + Unpin), text: &str) -> io::Result<()> {
    output.write_all(text.as_bytes()).await?;
    output.flush().await
}

fn refusal() -> String {
    crate::error_line(&Error::PanelRefused)
}

/// Parses in this process and runs the command; the reply is the text to print.
async fn jobpilot(args: Vec<String>) -> String {
    let cli = match Cli::try_parse_from(std::iter::once("jobpilot".to_string()).chain(args)) {
        Ok(cli) => cli,
        // Clap's message echoes the typed word: strip escapes from it too.
        Err(error) => return crate::text::sanitize(&error.to_string()),
    };
    // Checked here, not left to the environment marker: this prompt is the panel.
    if matches!(cli.command, Jobpilot::Shell) || cli.command.refused_in_panel() {
        return refusal();
    }
    match crate::run(cli.command).await {
        Ok(output) => crate::render(output, cli.json),
        Err(error) => crate::error_line(&error),
    }
}

/// Starts the owner's own session with the full environment and the terminal, then waits.
/// The adapter's no-tools flags and scrubbed environment are for headless drafting only.
async fn claude(output: &mut (impl AsyncWrite + Unpin), args: &[String]) -> Result<String> {
    let bin = match locate_claude().await {
        Ok(bin) => bin,
        Err(message) => return Ok(format!("error[unavailable]: {message}")),
    };
    say(output, &format!("{CLAUDE_WARNING}\n")).await?;
    let spawned = Command::new(&bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return Ok(format!(
                "error[io]: could not start {}: {error}",
                bin.display()
            ));
        }
    };
    let status = child.wait().await?;
    Ok(match status.code() {
        Some(code) => format!("claude exited with code {code}"),
        None => "claude was stopped by a signal".to_string(),
    })
}

fn not_found() -> String {
    format!(
        "claude was not found. Install Claude Code, or set its full path with \
         `jobpilot ai claude-path <path>` (setting {CLAUDE_PATH_SETTING})."
    )
}

/// Order: `JOBPILOT_CLAUDE_BIN`, the `ai.claude_path` setting, `PATH`, the usual install
/// folders, then one lookup through the owner's login shell. GUI launches on macOS carry a
/// minimal `PATH`, which is why the last three exist.
async fn locate_claude() -> std::result::Result<PathBuf, String> {
    if let Some(bin) = std::env::var_os("JOBPILOT_CLAUDE_BIN").filter(|bin| !bin.is_empty()) {
        return checked(PathBuf::from(bin), "JOBPILOT_CLAUDE_BIN");
    }
    if let Some(bin) = claude_path_setting().await {
        return checked(PathBuf::from(bin), CLAUDE_PATH_SETTING);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let on_path = std::env::split_paths(&path).filter(|dir| dir.is_absolute());
    if let Some(found) = search(on_path.chain(fallback_dirs(home_dir()))) {
        return Ok(found);
    }
    #[cfg(unix)]
    if let Some(found) = login_shell_lookup(&std::env::var_os("SHELL").unwrap_or_default()).await {
        return Ok(found);
    }
    Err(not_found())
}

fn checked(bin: PathBuf, source: &str) -> std::result::Result<PathBuf, String> {
    if bin.is_absolute() && is_program(&bin) {
        Ok(bin)
    } else {
        Err(format!(
            "{source} points to {}, which is not an executable file. {}",
            bin.display(),
            not_found()
        ))
    }
}

/// A failure to read the setting just means discovery continues without it.
async fn claude_path_setting() -> Option<String> {
    let paths = AppPaths::from_env().ok()?;
    let db = Db::open(&paths).await.ok()?;
    let value = settings::get(&db, CLAUDE_PATH_SETTING).await;
    db.close().await;
    value
        .ok()??
        .as_str()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
const BIN_NAMES: &[&str] = &["claude"];
// The native binary first, as in the adapter: cmd.exe caps a command line at 8191 characters.
#[cfg(windows)]
const BIN_NAMES: &[&str] = &["claude.exe", "claude.cmd"];

/// Where Claude Code's installers and the common package managers put it.
fn fallback_dirs(home: Option<PathBuf>) -> Vec<PathBuf> {
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

fn search(dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.into_iter()
        .flat_map(|dir| BIN_NAMES.iter().map(move |name| dir.join(name)))
        .find(|candidate| is_program(candidate))
}

/// Asks the owner's login shell where `claude` is: a fixed command, never user input. The
/// shell's startup files may print noise, so the last absolute line is the answer.
#[cfg(unix)]
async fn login_shell_lookup(shell: &std::ffi::OsStr) -> Option<PathBuf> {
    let shell = Path::new(shell);
    if !shell.is_absolute() {
        return None;
    }
    let lookup = Command::new(shell)
        .args(["-l", "-c", "command -v claude"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(LOGIN_SHELL_TIMEOUT, lookup)
        .await
        .ok()?
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| PathBuf::from(line.trim()))
        .rfind(|path| path.is_absolute() && is_program(path))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn allowed_sorts_lines() {
        assert_eq!(
            allowed(&words("jobpilot doc list")),
            Allowed::Jobpilot(words("doc list"))
        );
        assert_eq!(
            allowed(&words("doc list")),
            Allowed::Jobpilot(words("doc list"))
        );
        assert_eq!(
            allowed(&words("claude --resume")),
            Allowed::Claude(words("--resume"))
        );
        assert_eq!(allowed(&words("shell")), Allowed::Refused);
        assert_eq!(allowed(&words("jobpilot shell")), Allowed::Refused);
        // Only a leading token is special.
        assert_eq!(
            allowed(&words("doc claude")),
            Allowed::Jobpilot(words("doc claude"))
        );
    }

    #[tokio::test]
    async fn oversize_line_is_dropped_whole() {
        let mut data = vec![b'x'; MAX_LINE_BYTES + 1];
        data.extend_from_slice(b"\nok\n");
        let mut input = tokio::io::BufReader::with_capacity(16, &data[..]);
        assert!(matches!(
            read_line(&mut input).await.unwrap(),
            Line::TooLong
        ));
        assert!(matches!(read_line(&mut input).await.unwrap(), Line::Text(t) if t == "ok"));
        assert!(matches!(read_line(&mut input).await.unwrap(), Line::Eof));
        let exact = vec![b'y'; MAX_LINE_BYTES];
        let mut input = &exact[..];
        assert!(matches!(
            read_line(&mut input).await.unwrap(),
            Line::Text(_)
        ));
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn fallbacks_cover_gui_launches_and_search_finds_a_binary() {
        let home = tempfile::tempdir().unwrap();
        let dirs = fallback_dirs(Some(home.path().to_path_buf()));
        assert!(dirs.contains(&home.path().join(".local/bin")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        let placed = script(&home.path().join(".local/bin"), "claude", "#!/bin/sh\n");
        assert_eq!(search(dirs), Some(placed));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn login_shell_lookup_takes_the_last_real_path() {
        let dir = tempfile::tempdir().unwrap();
        let claude = script(dir.path(), "claude", "#!/bin/sh\n");
        let shell = script(
            dir.path(),
            "fake-shell",
            &format!(
                "#!/bin/sh\necho welcome back\necho /no/such/claude\necho {}\n",
                claude.display()
            ),
        );
        assert_eq!(login_shell_lookup(shell.as_os_str()).await, Some(claude));
        assert_eq!(login_shell_lookup("relative-shell".as_ref()).await, None);
    }

    #[test]
    fn checked_names_its_source() {
        let message = checked(PathBuf::from("/no/such/claude"), CLAUDE_PATH_SETTING).unwrap_err();
        assert!(message.contains("ai.claude_path"), "{message}");
    }
}
