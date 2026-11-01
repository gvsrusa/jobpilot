//! `jobpilot shell`: the restricted prompt behind the app's terminal panel. A line is split
//! with `shlex` and parsed by clap in this process. No system shell ever sees it, so
//! `$(id)`, backticks, pipes, and redirects are just unknown words. The one exception is
//! `claude`, which starts the owner's own interactive Claude Code session.

use std::future::Future;
use std::io;
use std::process::Stdio;

use clap::Parser;
use jobpilot_core::ai::claude_code::find_binary;
use jobpilot_core::{AppPaths, Db, Error, Result};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;

use crate::{Cli, Command as Jobpilot};

pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const PROMPT: &str = "jobpilot> ";
pub const REFUSAL: &str = "Run this in an ordinary terminal, or use the app.";
pub const CLAUDE_WARNING: &str = "Starting your own claude session. Anthropic's consumer terms apply to what you type here; JobPilot does not read this session. Do not paste job postings into it.";

/// What `claude` may be given here. A pasted line must not choose the session's flags.
const CLAUDE_ARGS: &[&str] = &["--version", "--help", "--resume", "--continue"];

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
        Some("claude")
            if argv[1..]
                .iter()
                .all(|arg| CLAUDE_ARGS.contains(&arg.as_str()))
                && argv.len() <= 2 =>
        {
            Allowed::Claude(argv[1..].to_vec())
        }
        Some("claude") => Allowed::Refused,
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
            Allowed::Claude(args) => crate::text::sanitize(&claude(output, &args).await?),
            Allowed::Jobpilot(args) => guarded(jobpilot(args)).await,
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

/// A panic inside one command ends that command, not the session.
async fn guarded(command: impl Future<Output = String> + Send + 'static) -> String {
    match tokio::spawn(command).await {
        Ok(reply) => reply,
        Err(_) => "error[internal]: that command failed unexpectedly".to_string(),
    }
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

/// Starts the owner's own session with this process's environment and the terminal, then
/// waits. In the panel that environment is the PTY layer's allow-list, so API keys stay out.
/// The adapter's no-tools flags and scrubbed environment are for headless drafting only.
async fn claude(output: &mut (impl AsyncWrite + Unpin), args: &[String]) -> Result<String> {
    let bin = match locate().await {
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

/// The shared discovery in core, with the data folder's `ai.claude_path` setting.
async fn locate() -> std::result::Result<std::path::PathBuf, String> {
    let db = match AppPaths::from_env() {
        Ok(paths) => Db::open(&paths).await.ok(),
        Err(_) => None,
    };
    let found = find_binary(db.as_ref()).await;
    if let Some(db) = db {
        db.close().await;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobpilot_core::ai::claude_code::{PATH_SETTING, checked};
    #[cfg(unix)]
    use jobpilot_core::ai::claude_code::{fallback_dirs, login_shell_lookup, search};
    #[cfg(unix)]
    use std::path::Path;
    use std::path::PathBuf;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[tokio::test]
    async fn a_panicking_command_does_not_end_the_session() {
        let reply = guarded(async { panic!("boom") }).await;
        assert!(reply.starts_with("error[internal]"), "{reply}");
        assert_eq!(guarded(async { "fine".to_string() }).await, "fine");
    }

    #[test]
    fn claude_arguments_are_an_allow_list() {
        assert_eq!(
            allowed(&words("claude --version")),
            Allowed::Claude(words("--version"))
        );
        assert_eq!(allowed(&words("claude")), Allowed::Claude(vec![]));
        for line in [
            "claude -p x",
            "claude --version --help",
            "claude --model x",
            "claude hello",
        ] {
            assert_eq!(allowed(&words(line)), Allowed::Refused, "{line}");
        }
        assert_eq!(
            allowed(&words("CLAUDE")),
            Allowed::Jobpilot(words("CLAUDE"))
        );
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
        let message = checked(PathBuf::from("/no/such/claude"), PATH_SETTING).unwrap_err();
        assert!(message.contains("ai.claude_path"), "{message}");
    }
}
