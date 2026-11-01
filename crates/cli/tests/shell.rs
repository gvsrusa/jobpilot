//! Drives `jobpilot shell` over pipes, the way the app's terminal panel does (marker set).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const REFUSAL: &str = "Run this in an ordinary terminal, or use the app.";
const WARNING: &str = "Starting your own claude session. Anthropic's consumer terms apply to what you type here; JobPilot does not read this session. Do not paste job postings into it.";

fn session(home: &Path, input: &[u8], envs: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jobpilot"));
    command
        .arg("shell")
        .env("JOBPILOT_HOME", home)
        .env("JOBPILOT_PANEL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    // A line over the pipe buffer needs a writer thread, or both ends block.
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    output
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn fresh_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    (dir, home)
}

#[test]
fn panel_line_is_never_a_shell_command() {
    let (dir, home) = fresh_home();
    let canary = dir.path().join("canary");
    std::fs::write(&canary, "x").unwrap();
    let input = format!("rm -f {}\n$(id)\n`id`\nls | cat\n", canary.display());
    let out = text(&session(&home, input.as_bytes(), &[]));
    assert_eq!(out.matches("unrecognized subcommand").count(), 4, "{out}");
    assert!(canary.exists());
    assert!(!out.contains("uid="), "{out}");
}

#[test]
fn jobpilot_commands_run_in_process() {
    let (_dir, home) = fresh_home();
    let out = text(&session(
        &home,
        b"doc add \"a b\" --body c\ndoc list\n",
        &[],
    ));
    assert!(out.contains("a b"), "{out}");
}

#[test]
fn leading_jobpilot_token_is_accepted() {
    let (_dir, home) = fresh_home();
    let plain = text(&session(&home, b"status\n", &[]));
    let prefixed = text(&session(&home, b"jobpilot status\n", &[]));
    assert!(plain.contains("schema"), "{plain}");
    assert_eq!(plain, prefixed);
}

#[test]
fn refused_commands_print_the_refusal_line() {
    let (_dir, home) = fresh_home();
    session(&home, b"doc add keep --body data\n", &[]);
    let out = text(&session(&home, b"data delete --confirm DELETE\n", &[]));
    assert!(
        out.contains(&format!("error[panel_refused]: {REFUSAL}")),
        "{out}"
    );
    let listed = text(&session(&home, b"doc list\n", &[]));
    assert!(listed.contains("keep"), "{listed}");
}

#[test]
fn nested_shell_is_refused() {
    let (_dir, home) = fresh_home();
    let out = text(&session(&home, b"shell\nstatus\n", &[]));
    assert!(
        out.contains(&format!("error[panel_refused]: {REFUSAL}")),
        "{out}"
    );
    assert!(out.contains("schema"), "session continued: {out}");
}

#[test]
fn long_line_is_discarded() {
    let (_dir, home) = fresh_home();
    let mut input = vec![b'a'; 70 * 1024];
    input.extend_from_slice(b"\ndoc add after --body x\ndoc list\n");
    let out = text(&session(&home, &input, &[]));
    assert!(out.contains("line too long"), "{out}");
    assert!(out.contains("after"), "{out}");
}

#[cfg(unix)]
const MOCK: &str = "claude";
#[cfg(windows)]
const MOCK: &str = "claude.cmd";

/// The mock answers `--version` before it records arguments, so a wrapper records them.
fn wrapper(dir: &Path, mock: &Path, args: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("claude");
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec '{}' \"$@\"\n",
            args.display(),
            mock.display()
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(windows)]
    {
        let path = dir.join("claude.cmd");
        let body = format!(
            "@echo off\r\necho %*> \"{}\"\r\ncall \"{}\" %*\r\n",
            args.display(),
            mock.display()
        );
        std::fs::write(&path, body).unwrap();
        path
    }
}

#[test]
fn claude_launch_prints_warning_and_uses_located_binary() {
    let (dir, home) = fresh_home();
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-claude");
    let args = dir.path().join("args");
    let bin = wrapper(dir.path(), &mock.join(MOCK), &args);
    let out = text(&session(
        &home,
        b"claude --version\n",
        &[("JOBPILOT_CLAUDE_BIN", &bin)],
    ));
    let warning = out.find(WARNING).unwrap_or_else(|| panic!("{out}"));
    let reply = out
        .find("2.1.316 (mock)")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(warning < reply, "{out}");
    assert!(
        std::fs::read_to_string(&args)
            .unwrap()
            .contains("--version")
    );
}

#[test]
fn missing_claude_says_how_to_fix_it() {
    let (_dir, home) = fresh_home();
    let nothing = home.join("no-such-claude");
    let out = text(&session(
        &home,
        b"claude\nstatus\n",
        &[("JOBPILOT_CLAUDE_BIN", &nothing)],
    ));
    assert!(out.contains("ai.claude_path"), "{out}");
    assert!(out.contains("schema"), "session continued: {out}");
}

#[test]
fn exit_ends_the_session_with_status_zero() {
    let (_dir, home) = fresh_home();
    let output = session(&home, b"exit\nstatus\n", &[]);
    assert!(output.status.success());
    let out = text(&output);
    assert_eq!(out.matches("jobpilot> ").count(), 1, "{out}");
    assert!(!out.contains("schema"), "{out}");
}

#[test]
fn claude_path_setting_is_used_when_nothing_else_finds_claude() {
    let (dir, home) = fresh_home();
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-claude");
    let args = dir.path().join("args");
    let bin = wrapper(dir.path(), &mock.join(MOCK), &args);
    let set = format!("ai claude-path \"{}\"\nclaude --version\n", bin.display());
    let out = text(&session(&home, set.as_bytes(), &[]));
    assert!(out.contains("2.1.316 (mock)"), "{out}");
}
