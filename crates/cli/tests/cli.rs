#[path = "../../core/tests/fake_http/mod.rs"]
mod fake_http;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fake_http::{Canned, Fake};
use jobpilot_core::{AppPaths, Db, app_schema_version, documents};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
        .env("JOBPILOT_SECRET_SERVICE", "jobpilot-test")
        .output()
        .unwrap()
}

fn stdout_json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fresh_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    (dir, home)
}

#[test]
fn status_json_reports_schema() {
    let (_dir, home) = fresh_home();
    let value = stdout_json(&run(&home, &["status", "--json"]));
    assert_eq!(value["schema_version"], app_schema_version());
}

#[tokio::test]
async fn doc_added_by_cli_is_found_by_library() {
    let (_dir, home) = fresh_home();
    let added = run(
        &home,
        &[
            "doc",
            "add",
            "Cover letter",
            "--body",
            "zebra stripes",
            "--json",
        ],
    );
    assert!(added.status.success());
    let found = stdout_json(&run(&home, &["doc", "search", "zebra", "--json"]));
    assert_eq!(found.as_array().unwrap().len(), 1);

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let hits = documents::search(&db, "zebra").await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Cover letter");
    db.close().await;
}

#[test]
fn backup_restore_round_trip() {
    let (_dir, home) = fresh_home();
    assert!(
        run(&home, &["doc", "add", "Keep", "--body", "alpha"])
            .status
            .success()
    );
    let backup = stdout_json(&run(&home, &["backup", "create", "--json"]));
    let path = backup["path"].as_str().unwrap().to_string();
    assert_eq!(
        stdout_json(&run(&home, &["backup", "list", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        1
    );

    assert!(
        run(&home, &["doc", "add", "Extra", "--body", "beta"])
            .status
            .success()
    );
    assert_eq!(
        stdout_json(&run(&home, &["doc", "list", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        2
    );

    assert!(run(&home, &["backup", "restore", &path]).status.success());
    let docs = stdout_json(&run(&home, &["doc", "list", "--json"]));
    assert_eq!(docs.as_array().unwrap().len(), 1);
    assert_eq!(docs[0]["title"], "Keep");
}

#[test]
fn delete_without_confirm_fails_and_keeps_data() {
    let (_dir, home) = fresh_home();
    assert!(run(&home, &["doc", "add", "Keep"]).status.success());
    let denied = run(&home, &["data", "delete"]);
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("error[confirmation_required]"));
    let wrong = run(&home, &["data", "delete", "--confirm", "nope"]);
    assert_eq!(wrong.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("error[confirmation_required]"));
    assert_eq!(
        stdout_json(&run(&home, &["doc", "list", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn errors_exit_nonzero_with_code() {
    let (_dir, home) = fresh_home();
    let missing = home.join("nope");
    let output = run(&home, &["backup", "restore", missing.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("error["));
}

#[test]
fn restore_works_when_open_fails() {
    let (_dir, home) = fresh_home();
    assert!(
        run(&home, &["doc", "add", "Keep", "--body", "alpha"])
            .status
            .success()
    );
    let backup = stdout_json(&run(&home, &["backup", "create", "--json"]));
    let path = backup["path"].as_str().unwrap().to_string();
    std::fs::write(home.join("jobpilot.db"), b"not a database").unwrap();
    assert!(!run(&home, &["status"]).status.success());
    let restored = run(&home, &["backup", "restore", &path]);
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_eq!(
        stdout_json(&run(&home, &["doc", "list", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn restore_with_another_handle_open_reports_in_use() {
    let (_dir, home) = fresh_home();
    assert!(run(&home, &["doc", "add", "Keep"]).status.success());
    let backup = stdout_json(&run(&home, &["backup", "create", "--json"]));
    let path = backup["path"].as_str().unwrap().to_string();

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let output = run(&home, &["backup", "restore", &path]);
    db.close().await;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("error[in_use]"));
}

fn run_env(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
        .env("JOBPILOT_SECRET_SERVICE", "jobpilot-test")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> Output {
    let output = run(home, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        stderr_text(&output)
    );
    output
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/fixtures");
    path.join(name).to_str().unwrap().to_string()
}

#[test]
fn profile_set_and_add_round_trip_through_json() {
    let (_dir, home) = fresh_home();
    ok(
        &home,
        &["profile", "set", "--name", "Ada", "--email", "a@b.co"],
    );
    ok(&home, &["profile", "add", "countries", "pt", "CA"]);
    let shown = stdout_json(&run(&home, &["profile", "show", "--json"]));
    assert_eq!(shown["countries"], serde_json::json!(["PT", "CA"]));
    assert_eq!(shown["name"], "Ada");
    ok(&home, &["profile", "remove", "countries", "PT"]);
    let shown = stdout_json(&run(&home, &["profile", "show", "--json"]));
    assert_eq!(shown["countries"], serde_json::json!(["CA"]));
    ok(&home, &["prefs", "work-mode", "remote,hybrid"]);
    ok(&home, &["prefs", "employment", "add", "CA", "contract"]);
    let shown = stdout_json(&run(&home, &["profile", "show", "--json"]));
    assert_eq!(shown["work_modes"], serde_json::json!(["remote", "hybrid"]));
    assert_eq!(shown["employment_types"][0]["kind"], "contract");
    ok(&home, &["prefs", "employment", "remove", "CA", "contract"]);
}

#[test]
#[ignore = "uses the OS keychain under the test service"]
fn auth_set_stores_only_what_the_user_states() {
    let (_dir, home) = fresh_home();
    ok(&home, &["profile", "add", "countries", "PT", "CA"]);
    ok(
        &home,
        &["profile", "auth", "set", "PT", "--authorized", "yes"],
    );
    let shown = stdout_json(&run(&home, &["profile", "show", "--json"]));
    assert_eq!(shown["authorization"].as_array().unwrap().len(), 1);
    assert_eq!(shown["authorization"][0]["country"], "PT");
    assert_eq!(shown["authorization"][0]["authorized"], "yes");
    assert_eq!(shown["authorization"][0]["needs_sponsorship"], "not_stated");
    ok(&home, &["profile", "auth", "remove", "PT"]);
}

#[test]
fn panel_marker_refuses_recovery_and_delete_commands() {
    let (_dir, home) = fresh_home();
    let out = home.parent().unwrap().join("key.txt");
    let (out, any) = (out.to_str().unwrap(), "x");
    let cases: Vec<Vec<&str>> = vec![
        vec!["data", "recovery-key", "export", "--out", out],
        vec!["data", "recovery-key", "import", any],
        vec!["data", "discard-locked", "--confirm", any],
        vec!["data", "delete", "--confirm", "DELETE"],
        vec!["backup", "restore", any],
        vec!["kb", "reveal", any],
    ];
    for args in cases {
        let output = run_env(&home, &args, &[("JOBPILOT_PANEL", "1")]);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let text = stderr_text(&output);
        assert!(text.starts_with("error[panel_refused]"), "{args:?}: {text}");
        assert!(text.contains("Run this in an ordinary terminal, or use the app."));
    }
    assert!(!home.join("jobpilot.db").exists());
}

#[test]
fn cli_output_strips_terminal_escapes() {
    let (_dir, home) = fresh_home();
    let body = home.parent().unwrap().join("posting.txt");
    std::fs::write(&body, "Build \x1b[31mthings\x1b[0m\x07 now").unwrap();
    let title = "\x1b]0;evil\x07title";
    let pasted = run(
        &home,
        &[
            "job",
            "paste",
            title,
            "--file",
            body.to_str().unwrap(),
            "--json",
        ],
    );
    let id = stdout_json(&pasted)["id"].as_str().unwrap().to_string();
    ok(
        &home,
        &["kb", "add", "note", "Led \x1b[2Jthe team\x1b]0;x\x07"],
    );
    ok(
        &home,
        &[
            "resume",
            "import",
            &fixture("resume.docx"),
            "--title",
            title,
        ],
    );
    let resumes = stdout_json(&run(&home, &["resume", "list", "--json"]));
    let draft = resumes[0]["versions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Text mode: no control byte but newline and tab reaches stdout.
    let text_cases: Vec<Vec<&str>> = vec![
        vec!["job", "show", &id],
        vec!["job", "list"],
        vec!["kb", "list"],
        vec!["resume", "list"],
        vec!["resume", "compare", &draft],
    ];
    for args in &text_cases {
        let stdout = String::from_utf8_lossy(&ok(&home, args).stdout).into_owned();
        assert!(
            !stdout
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "{args:?}: {stdout:?}"
        );
    }
    // JSON mode: the parsed values are clean, not merely escaped on the wire.
    let job = stdout_json(&run(&home, &["job", "show", &id, "--json"]));
    assert_eq!(job["job"]["title"], "title");
    assert!(job["text"].as_str().unwrap().contains("Build things"));
    assert_eq!(
        stdout_json(&run(&home, &["job", "list", "--json"]))[0]["title"],
        "title"
    );
    let items = stdout_json(&run(&home, &["kb", "list", "--json"]));
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["body"] == "Led the team"),
        "{items}"
    );
    assert_eq!(resumes[0]["title"], "title");
    let compare = run(&home, &["resume", "compare", &draft, "--json"]);
    let compare = stdout_json(&compare);
    assert!(!compare.to_string().contains("\\u001b"));
}

#[test]
fn resume_import_generate_compare_export_with_no_ai_is_manual_required() {
    let (_dir, home) = fresh_home();
    ok(&home, &["ai", "disable", "ollama"]);
    let imported = stdout_json(&run(
        &home,
        &["resume", "import", &fixture("resume.docx"), "--json"],
    ));
    let lineage = imported["lineage_id"].as_str().unwrap().to_string();
    let listed = stdout_json(&run(&home, &["resume", "list", "--json"]));
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["lineage_id"], lineage.as_str());
    let versions = stdout_json(&run(&home, &["resume", "versions", &lineage, "--json"]));
    let draft = versions[0]["id"].as_str().unwrap().to_string();

    let job = stdout_json(&run(
        &home,
        &[
            "job",
            "paste",
            "Engineer",
            "--file",
            &fixture("ollama_tags.json"),
            "--json",
        ],
    ));
    let job_id = job["id"].as_str().unwrap();
    let generated = run(&home, &["resume", "generate", job_id]);
    assert_eq!(generated.status.code(), Some(1));
    assert!(stderr_text(&generated).starts_with("error[manual_required]"));
    let letter = run(&home, &["letter", "generate", job_id]);
    assert!(stderr_text(&letter).starts_with("error[manual_required]"));

    let out = home.parent().unwrap().join("out");
    let exported = ok(
        &home,
        &[
            "resume",
            "export",
            &draft,
            "--format",
            "docx",
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert!(!exported.stdout.is_empty());
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    ok(&home, &["resume", "compare", &draft]);
}

/// A `claude` that always answers with `reply` as its structured output. The child gets a
/// scrubbed environment, so the mock's knob lives in a wrapper next to the data folder.
fn claude_saying(home: &Path, reply: &serde_json::Value) -> String {
    let dir = home.parent().unwrap();
    let line = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false, "result": "",
        "structured_output": reply, "permission_denials": [],
    });
    let reply_file = dir.join("reply.json");
    std::fs::write(&reply_file, format!("{line}\n")).unwrap();
    let mocks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-claude");
    if cfg!(windows) {
        let wrapper = dir.join("claude.cmd");
        let script = format!(
            "@echo off\r\nset MOCK_CLAUDE_REPLY={}\r\ncall \"{}\" %*\r\n",
            reply_file.display(),
            mocks.join("claude.cmd").display()
        );
        std::fs::write(&wrapper, script).unwrap();
        return wrapper.to_str().unwrap().to_owned();
    }
    let wrapper = dir.join("claude");
    let script = format!(
        "#!/bin/sh\nexport MOCK_CLAUDE_REPLY='{}'\nexec '{}' \"$@\"\n",
        reply_file.display(),
        mocks.join("claude").display()
    );
    std::fs::write(&wrapper, script).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    wrapper.to_str().unwrap().to_owned()
}

/// Claude first, with consent for these categories, and no Ollama.
fn claude_only(home: &Path, bin: &str, categories: &str) {
    let env = [("JOBPILOT_CLAUDE_BIN", bin)];
    for args in [
        vec!["ai", "disable", "ollama"],
        vec!["ai", "enable", "claude_code"],
        vec!["ai", "order", "claude_code"],
        vec![
            "ai",
            "consent",
            "give",
            "claude_code",
            "--categories",
            categories,
        ],
    ] {
        let output = run_env(home, &args, &env);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            stderr_text(&output)
        );
    }
}

/// A job, evidence the draft can cite, and a reply that cites it.
fn draftable(home: &Path) -> (String, serde_json::Value) {
    let item = stdout_json(&run(
        home,
        &[
            "kb",
            "add",
            "experience",
            "Shipped the Rust compiler",
            "--json",
        ],
    ));
    let posting = home.parent().unwrap().join("posting.txt");
    std::fs::write(&posting, "Rust engineer wanted").unwrap();
    let job = stdout_json(&run(
        home,
        &[
            "job",
            "paste",
            "Engineer",
            "--file",
            posting.to_str().unwrap(),
            "--json",
        ],
    ));
    let reply = serde_json::json!({
        "summary": [],
        "sections": [{"heading": "experience", "entries": [{
            "title": "", "subtitle": "", "dates": "", "evidence_ids": [],
            "bullets": [{"text": "Shipped the Rust compiler", "evidence_ids": [item["id"]]}],
        }]}],
    });
    (job["id"].as_str().unwrap().to_owned(), reply)
}

const DRAFT_CONSENT: &str = "job_text,resume_text,profile_basic";

/// The mock Gemini CLI, saying `reply` as its stream.
fn gemini_saying(home: &Path, reply: &serde_json::Value) -> String {
    let dir = home.parent().unwrap();
    let text = serde_json::json!({"type": "message", "role": "assistant",
        "content": reply.to_string(), "delta": true});
    let end = serde_json::json!({"type": "result", "status": "success", "stats": {}});
    let reply_file = dir.join("stream.jsonl");
    std::fs::write(&reply_file, format!("{text}\n{end}\n")).unwrap();
    let mocks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-gemini");
    if cfg!(windows) {
        let wrapper = dir.join("gemini.cmd");
        let script = format!(
            "@echo off\r\nset MOCK_GEMINI_REPLY={}\r\ncall \"{}\" %*\r\n",
            reply_file.display(),
            mocks.join("gemini.cmd").display()
        );
        std::fs::write(&wrapper, script).unwrap();
        return wrapper.to_str().unwrap().to_owned();
    }
    let wrapper = dir.join("gemini");
    let script = format!(
        "#!/bin/sh\nexport MOCK_GEMINI_REPLY='{}'\nexec '{}' \"$@\"\n",
        reply_file.display(),
        mocks.join("gemini").display()
    );
    std::fs::write(&wrapper, script).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    wrapper.to_str().unwrap().to_owned()
}

#[test]
fn gemini_cli_drafts_through_the_mock() {
    let (_dir, home) = fresh_home();
    let (job, reply) = draftable(&home);
    let bin = gemini_saying(&home, &reply);
    let env = [
        ("JOBPILOT_GEMINI_BIN", bin.as_str()),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    for args in [
        vec!["ai", "disable", "ollama"],
        vec!["ai", "enable", "gemini_cli"],
        vec![
            "ai",
            "consent",
            "give",
            "gemini_cli",
            "--categories",
            DRAFT_CONSENT,
            "--attest",
        ],
    ] {
        let output = run_env(&home, &args, &env);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            stderr_text(&output)
        );
    }

    let made = run_env(&home, &["resume", "generate", &job, "--json"], &env);
    assert!(made.status.success(), "{}", stderr_text(&made));
    let made = stdout_json(&made);
    assert_eq!(made["provider"], "gemini_cli");
    assert_eq!(made["blocked"], 0);
    let shown = stdout_json(&run_env(
        &home,
        &["ai", "consent", "show", "gemini_cli", "--json"],
        &env,
    ));
    assert_eq!(shown["key"], "gemini_cli/cli_login");
    assert_eq!(shown["path"], bin);
}

#[test]
fn gemini_cli_absent_shows_unavailable() {
    let (dir, home) = fresh_home();
    let gone = dir.path().join("gemini");
    let env = [
        ("JOBPILOT_GEMINI_BIN", gone.to_str().unwrap()),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    assert!(
        run_env(&home, &["ai", "enable", "gemini_cli"], &env)
            .status
            .success()
    );
    let status = stdout_json(&run_env(&home, &["ai", "status", "--json"], &env));
    let rows = status["external"].as_array().unwrap();
    let row = rows.iter().find(|row| row["name"] == "gemini_cli").unwrap();
    assert_eq!(row["available"], false);
    assert_eq!(row["reason"], "not_installed");
    assert_eq!(row["path"], serde_json::Value::Null);
}

/// The mock Codex CLI behind a wrapper that sets `vars`, and the file its stdin lands in.
fn codex_with(home: &Path, vars: &[(&str, &str)]) -> (String, PathBuf) {
    let dir = home.parent().unwrap();
    let stdin = dir.join("codex-stdin");
    let mocks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-codex");
    let stdin_var = ("MOCK_CODEX_STDIN", stdin.to_str().unwrap());
    let all = std::iter::once(&stdin_var).chain(vars);
    if cfg!(windows) {
        let wrapper = dir.join("codex.cmd");
        let mut script = String::from("@echo off\r\n");
        for (name, value) in all {
            script.push_str(&format!("set \"{name}={value}\"\r\n"));
        }
        script.push_str(&format!(
            "call \"{}\" %*\r\n",
            mocks.join("codex.cmd").display()
        ));
        std::fs::write(&wrapper, script).unwrap();
        return (wrapper.to_str().unwrap().to_owned(), stdin);
    }
    let wrapper = dir.join("codex");
    let mut script = String::from("#!/bin/sh\n");
    for (name, value) in all {
        script.push_str(&format!("export {name}='{value}'\n"));
    }
    script.push_str(&format!(
        "exec '{}' \"$@\"\n",
        mocks.join("codex").display()
    ));
    std::fs::write(&wrapper, script).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    (wrapper.to_str().unwrap().to_owned(), stdin)
}

#[test]
fn codex_is_skipped_for_a_draft_and_says_why() {
    let (_dir, home) = fresh_home();
    let (job, _reply) = draftable(&home);
    let (bin, stdin) = codex_with(&home, &[]);
    let env = [
        ("JOBPILOT_CODEX_BIN", bin.as_str()),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    for args in [
        vec!["ai", "disable", "ollama"],
        vec!["ai", "enable", "codex_cli"],
        vec![
            "ai",
            "consent",
            "give",
            "codex_cli",
            "--categories",
            DRAFT_CONSENT,
            "--attest",
        ],
    ] {
        let output = run_env(&home, &args, &env);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            stderr_text(&output)
        );
    }
    let made = run_env(&home, &["resume", "generate", &job], &env);
    assert_eq!(made.status.code(), Some(1));
    assert!(
        stderr_text(&made).starts_with("error[manual_required]"),
        "{}",
        stderr_text(&made)
    );
    assert!(!stdin.exists(), "Codex was started for a draft");
    let shown = run_env(&home, &["ai", "consent", "show", "codex_cli"], &env);
    let shown = String::from_utf8_lossy(&shown.stdout).to_string();
    assert!(
        shown.contains("skipped, this provider cannot prove it has no tools"),
        "{shown}"
    );
    assert!(
        shown.contains("Skipped") || shown.contains("skipped"),
        "{shown}"
    );
}

/// The owner's install today: `codex --version` exits 1. Enabling it is what probes it
/// (decision 28), so the row reads unavailable only after that.
#[test]
fn codex_failing_its_version_shows_unavailable() {
    let (_dir, home) = fresh_home();
    let (bin, stdin) = codex_with(&home, &[("MOCK_CODEX_VERSION_EXIT", "1")]);
    let env = [
        ("JOBPILOT_CODEX_BIN", bin.as_str()),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    let row = || {
        let status = stdout_json(&run_env(&home, &["ai", "status", "--json"], &env));
        let rows = status["external"].as_array().unwrap();
        rows.iter()
            .find(|row| row["name"] == "codex_cli")
            .unwrap()
            .clone()
    };
    assert_eq!(row()["available"], true, "off, so never started");
    assert!(
        run_env(&home, &["ai", "enable", "codex_cli"], &env)
            .status
            .success()
    );
    let on = row();
    assert_eq!(on["available"], false, "{on}");
    assert!(on["reason"].is_string(), "{on}");
    assert_eq!(on["runs_job_text"], false, "{on}");
    assert!(!stdin.exists());
}

/// An nvm install under a GUI launch: only the login shell can find it, and the quick look an
/// off row gets does not ask that shell. Turning it on must still reach it.
#[cfg(unix)]
#[test]
fn an_off_cli_only_the_login_shell_finds_comes_up_once_enabled() {
    use std::os::unix::fs::PermissionsExt;
    // A real install in a system folder would be found, and started, instead of the mock.
    let system = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];
    if system
        .iter()
        .any(|dir| Path::new(dir).join("gemini").exists())
    {
        eprintln!("skipped: a real gemini sits in a system folder");
        return;
    }
    let (dir, home) = fresh_home();
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-gemini/gemini");
    let shell = dir.path().join("shell");
    std::fs::write(&shell, format!("#!/bin/sh\necho '{}'\n", mock.display())).unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let empty = empty.to_str().unwrap();
    let env = [
        ("PATH", empty),
        ("HOME", empty),
        ("SHELL", shell.to_str().unwrap()),
        ("NVM_DIR", empty),
        ("VOLTA_HOME", empty),
        ("FNM_DIR", empty),
        ("FNM_MULTISHELL_PATH", empty),
        ("JOBPILOT_GEMINI_BIN", ""),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    let row = || {
        let status = stdout_json(&run_env(&home, &["ai", "status", "--json"], &env));
        let rows = status["external"].as_array().unwrap();
        rows.iter()
            .find(|row| row["name"] == "gemini_cli")
            .unwrap()
            .clone()
    };
    let off = row();
    assert_eq!(off["available"], false, "{off}");
    assert_eq!(off["reason"], "not_searched", "{off}");

    let text = run_env(&home, &["ai", "status"], &env);
    let text = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(text.contains("turn on to look for it"), "{text}");
    assert!(!text.contains("not_searched"), "{text}");

    assert!(
        run_env(&home, &["ai", "enable", "gemini_cli"], &env)
            .status
            .success()
    );
    let on = row();
    assert_eq!(on["available"], true, "{on}");
    assert_eq!(on["reason"], serde_json::Value::Null, "{on}");
    assert_eq!(on["path"], mock.to_str().unwrap(), "{on}");
}

#[test]
fn an_account_the_cli_refuses_gets_the_plan_hint() {
    let (_dir, home) = fresh_home();
    let (job, reply) = draftable(&home);
    let bin = gemini_saying(&home, &reply);
    let wrapper = Path::new(&bin);
    let stderr = home.parent().unwrap().join("stderr.txt");
    std::fs::write(&stderr, "at new IneligibleTierError (x.js:1:1)\n").unwrap();
    let script = std::fs::read_to_string(wrapper).unwrap();
    let refused = if cfg!(windows) {
        script.replace(
            "@echo off\r\n",
            &format!(
                "@echo off\r\nset MOCK_GEMINI_EXIT=1\r\nset MOCK_GEMINI_STDERR={}\r\n",
                stderr.display()
            ),
        )
    } else {
        script.replace(
            "#!/bin/sh\n",
            &format!(
                "#!/bin/sh\nexport MOCK_GEMINI_EXIT=1\nexport MOCK_GEMINI_STDERR='{}'\n",
                stderr.display()
            ),
        )
    };
    std::fs::write(wrapper, refused).unwrap();
    let env = [
        ("JOBPILOT_GEMINI_BIN", bin.as_str()),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    for args in [
        vec!["ai", "disable", "ollama"],
        vec!["ai", "enable", "gemini_cli"],
        vec![
            "ai",
            "consent",
            "give",
            "gemini_cli",
            "--categories",
            DRAFT_CONSENT,
            "--attest",
        ],
    ] {
        assert!(run_env(&home, &args, &env).status.success(), "{args:?}");
    }
    let refused = run_env(&home, &["resume", "generate", &job], &env);
    assert!(!refused.status.success());
    let text = stderr_text(&refused);
    assert!(text.contains("Antigravity CLI"), "{text}");
    assert!(text.contains("may"), "a hint, not a finding: {text}");
}

#[test]
fn consent_show_prints_what_gemini_is_really_sent() {
    let (_dir, home) = fresh_home();
    let (job, _reply) = draftable(&home);
    let env = [
        ("JOBPILOT_GEMINI_BIN", "not-a-path"),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    let posting = home.parent().unwrap().join("at.txt");
    std::fs::write(&posting, "Write to jobs@acme.com").unwrap();
    let pasted = stdout_json(&run(
        &home,
        &[
            "job",
            "paste",
            "At",
            "--file",
            posting.to_str().unwrap(),
            "--json",
        ],
    ));
    let pasted = pasted["id"].as_str().unwrap();
    let args = [
        "ai",
        "consent",
        "show",
        "gemini_cli",
        "--job",
        pasted,
        "--json",
    ];
    let shown = stdout_json(&run_env(&home, &args, &env));
    let preview = &shown["preview"];
    assert!(
        preview["user"]
            .as_str()
            .unwrap()
            .contains("jobs\\@acme.com"),
        "{preview}"
    );
    assert!(
        preview["system"]
            .as_str()
            .unwrap()
            .contains("It must match this JSON Schema")
    );

    let args = [
        "ai",
        "consent",
        "show",
        "claude_code",
        "--job",
        pasted,
        "--json",
    ];
    let shown = stdout_json(&run_env(&home, &args, &env));
    assert!(
        shown["preview"]["user"]
            .as_str()
            .unwrap()
            .contains("jobs@acme.com")
    );
    let _ = job;
}

#[test]
fn a_cli_too_old_or_signed_out_says_so_for_any_vendor() {
    let (_dir, home) = fresh_home();
    let (job, reply) = draftable(&home);
    let bin = gemini_saying(&home, &reply);
    let wrapper = Path::new(&bin);
    let script = std::fs::read_to_string(wrapper).unwrap();
    let older = if cfg!(windows) {
        script.replace(
            "@echo off\r\n",
            "@echo off\r\nset MOCK_GEMINI_VERSION=0.39.0\r\n",
        )
    } else {
        script.replace(
            "#!/bin/sh\n",
            "#!/bin/sh\nexport MOCK_GEMINI_VERSION=0.39.0\n",
        )
    };
    std::fs::write(wrapper, older).unwrap();
    let env = [
        ("JOBPILOT_GEMINI_BIN", bin.as_str()),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    for args in [
        vec!["ai", "disable", "ollama"],
        vec!["ai", "enable", "gemini_cli"],
        vec![
            "ai",
            "consent",
            "give",
            "gemini_cli",
            "--categories",
            DRAFT_CONSENT,
            "--attest",
        ],
    ] {
        assert!(run_env(&home, &args, &env).status.success(), "{args:?}");
    }
    let refused = run_env(&home, &["resume", "generate", &job], &env);
    assert!(!refused.status.success());
    let text = stderr_text(&refused);
    assert!(
        text.contains("The installed CLI is older than the version JobPilot checked."),
        "{text}"
    );

    let older = std::fs::read_to_string(wrapper).unwrap();
    let signed_out = if cfg!(windows) {
        older.replace("0.39.0\r\n", "0.40.0\r\nset MOCK_GEMINI_EXIT=41\r\n")
    } else {
        older.replace("0.39.0\n", "0.40.0\nexport MOCK_GEMINI_EXIT=41\n")
    };
    std::fs::write(wrapper, signed_out).unwrap();
    let refused = run_env(&home, &["resume", "generate", &job], &env);
    let text = stderr_text(&refused);
    assert!(
        text.contains("This CLI is not signed in. Run it once in your own terminal"),
        "{text}"
    );
}

#[test]
fn resume_generate_appears_in_agents_list() {
    let (_dir, home) = fresh_home();
    let (job, reply) = draftable(&home);
    let bin = claude_saying(&home, &reply);
    claude_only(&home, &bin, DRAFT_CONSENT);
    let env = [("JOBPILOT_CLAUDE_BIN", bin.as_str())];

    let made = run_env(&home, &["resume", "generate", &job, "--json"], &env);
    let made = stdout_json(&made);
    assert_eq!(made["provider"], "claude_code");
    assert_eq!(made["blocked"], 0);
    assert!(made["exported"].is_null());
    assert!(made["export_skipped"].is_null());
    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["agent"], "resume");
    assert_eq!(runs[0]["state"], "completed");
    assert_eq!(runs[0]["actor"], "cli");
    assert_eq!(runs[0]["steps"], 2);
}

#[test]
fn resume_generate_export_writes_to_the_exports_folder() {
    let (_dir, home) = fresh_home();
    let (job, reply) = draftable(&home);
    let bin = claude_saying(&home, &reply);
    claude_only(&home, &bin, DRAFT_CONSENT);
    let env = [("JOBPILOT_CLAUDE_BIN", bin.as_str())];

    let made = run_env(
        &home,
        &[
            "resume",
            "generate",
            &job,
            "--export",
            "pdf",
            "--template",
            "compact",
            "--json",
        ],
        &env,
    );
    let made = stdout_json(&made);
    let path = std::path::PathBuf::from(made["exported"].as_str().expect("a path"));
    assert!(path.is_file(), "{path:?}");
    assert_eq!(path.parent().unwrap(), home.join("exports"));
    assert_eq!(path.extension().unwrap(), "pdf");
}

/// The whole app with no provider: every switch off, no CLI to find, and a proxy that goes
/// nowhere, so a stray request would fail a step here and not reach a host.
#[tokio::test]
async fn no_provider_works_end_to_end() {
    let (dir, home) = fresh_home();
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let dead = "http://127.0.0.1:1";
    let step = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jobpilot"));
        command
            .args(args)
            .env("JOBPILOT_HOME", &home)
            .env("JOBPILOT_SECRET_SERVICE", "jobpilot-test")
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
            .env("PATH", &empty)
            .env("HTTP_PROXY", dead)
            .env("HTTPS_PROXY", dead)
            .env("http_proxy", dead)
            .env("https_proxy", dead)
            .env_remove("SHELL")
            // A name that is not there: `ai status` must not find an installed `claude`.
            .env("JOBPILOT_CLAUDE_BIN", "not-a-path")
            .env_remove("JOBPILOT_GEMINI_BIN")
            .env_remove("JOBPILOT_CODEX_BIN")
            .current_dir(dir.path());
        command.output().unwrap()
    };
    let json = |args: &[&str]| {
        let mut all = args.to_vec();
        all.push("--json");
        let output = step(&all);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            stderr_text(&output)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };

    assert_eq!(json(&["status"])["schema_version"], app_schema_version());
    json(&["ai", "disable", "ollama"]);
    let ai = json(&["ai", "status"]);
    let enabled = ai["enabled"].as_object().unwrap();
    assert!(!enabled.is_empty());
    assert!(enabled.values().all(|on| on == false), "{ai}");

    json(&["doc", "add", "Cover note", "--body", "zebra stripes"]);
    assert_eq!(
        json(&["doc", "search", "zebra"]).as_array().unwrap().len(),
        1
    );
    json(&[
        "profile",
        "set",
        "--name",
        "Ada",
        "--email",
        "ada@example.com",
    ]);
    json(&["kb", "add", "experience", "Shipped the Rust compiler"]);
    let posting = fixture("ollama_tags.json");
    let job = json(&["job", "paste", "Rust role", "--file", &posting]);
    let job = job["id"].as_str().unwrap();

    let generated = step(&["resume", "generate", job]);
    assert_eq!(generated.status.code(), Some(1));
    let said = stderr_text(&generated);
    assert!(said.starts_with("error[manual_required]"), "{said}");
    assert!(said.contains("no_ai") || said.contains("No AI"), "{said}");

    let app = json(&["app", "add", "--company", "Acme", "--role", "Dev"]);
    let app = app["id"].as_str().unwrap();
    json(&["app", "status", app, "applied", "--on", "2026-11-01"]);
    json(&["memory", "add-question", "Why this job?"]);
    assert_eq!(
        json(&["memory", "search", "job"]).as_array().unwrap().len(),
        1
    );
    json(&["analytics", "report"]);
    assert!(json(&["backup", "create"])["path"].is_string());
    assert!(json(&["data", "export"])["path"].is_string());

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let reasons: Vec<String> =
        sqlx::query_scalar("SELECT coalesce(reason, outcome) FROM task_runs WHERE provider = ?")
            .bind("no_ai")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert!(reasons.iter().any(|r| r == "no_ai"), "{reasons:?}");
    let calls: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'ai_external_call'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(calls, 0, "nothing left the machine");
    db.close().await;
}

#[test]
fn a_wrong_stored_cli_path_is_named_in_status() {
    let (dir, home) = fresh_home();
    let file = if cfg!(windows) {
        "gemini.cmd"
    } else {
        "gemini"
    };
    let wrong = dir.path().join("wrong").join(file);
    std::fs::create_dir_all(&wrong).unwrap();
    let env = [
        ("PATH", ""),
        ("JOBPILOT_GEMINI_BIN", ""),
        ("JOBPILOT_CLAUDE_BIN", "not-a-path"),
    ];
    let args = ["ai", "cli-path", "gemini_cli", wrong.to_str().unwrap()];
    let stored = run_env(&home, &args, &env);
    assert!(stored.status.success(), "{}", stderr_text(&stored));

    let status = stdout_json(&run_env(&home, &["ai", "status", "--json"], &env));
    let rows = status["external"].as_array().unwrap();
    let row = rows.iter().find(|row| row["name"] == "gemini_cli").unwrap();
    assert_eq!(row["reason"], "wrong_path", "{row}");

    let text = run_env(&home, &["ai", "status"], &env);
    let text = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(
        text.contains("the path set for it is not that program"),
        "{text}"
    );
    assert!(!text.contains("wrong_path"), "{text}");
}

#[test]
fn memory_refresh_with_no_ai_reports_skipped() {
    let (_dir, home) = fresh_home();
    ok(&home, &["ai", "disable", "ollama"]);
    ok(
        &home,
        &["memory", "add-question", "Please certify this is true"],
    );
    let done = stdout_json(&run(&home, &["memory", "refresh", "--json"]));
    assert_eq!(done["classified"]["skipped"], true);
    assert_eq!(done["classified"]["checked"], 0);
    let text = ok(&home, &["memory", "refresh"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Checked 0, raised 0, indexed 0"), "{text}");
    assert!(
        text.contains("No AI took this; classes come from the rules."),
        "{text}"
    );
    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert_eq!(runs[0]["agent"], "career_memory");
    assert_eq!(runs[0]["state"], "completed");
}

#[test]
fn memory_refresh_prints_the_count_and_waits_for_yes() {
    let (_dir, home) = fresh_home();
    let bin = claude_saying(&home, &serde_json::json!({ "class": "sensitive" }));
    claude_only(&home, &bin, "job_text");
    let env = [("JOBPILOT_CLAUDE_BIN", bin.as_str())];
    for i in 0..3 {
        ok(
            &home,
            &[
                "memory",
                "add-question",
                &format!("Describe project number {i}"),
            ],
        );
    }

    let asked = run_env(&home, &["memory", "refresh"], &env);
    assert!(asked.status.success(), "{}", stderr_text(&asked));
    let text = String::from_utf8_lossy(&asked.stdout);
    assert!(
        text.contains(
            "This will make at most 3 paid or subscription calls for 3 questions \
             (a press checks 20 questions at most). \
             Run `jobpilot memory refresh --yes` to go ahead."
        ),
        "{text}"
    );
    let json = stdout_json(&run_env(&home, &["memory", "refresh", "--json"], &env));
    assert_eq!(json["external_calls"], 3);
    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert!(runs.as_array().unwrap().is_empty(), "no run started");

    let done = stdout_json(&run_env(
        &home,
        &["memory", "refresh", "--yes", "--json"],
        &env,
    ));
    assert_eq!(done["classified"]["checked"], 3);
    assert_eq!(done["classified"]["raised"], 3);
    assert_eq!(done["external_calls"], 3, "the count it acted on");
    let text = run_env(&home, &["memory", "refresh", "--yes"], &env);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Checked 0, raised 0, indexed 0"), "{text}");
    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert_eq!(runs[0]["agent"], "career_memory");
}

#[test]
fn ai_status_json_names_both_providers() {
    let (_dir, home) = fresh_home();
    let bin = if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    };
    let mock = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/mock-claude")
        .join(bin);
    // A dead loopback port keeps the test off any real Ollama.
    let first = ok(&home, &["status"]);
    drop(first);
    let db_home = home.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let db = Db::open(&AppPaths::new(&db_home)).await.unwrap();
                jobpilot_core::settings::set(
                    &db,
                    "ai.ollama_url",
                    &serde_json::json!("http://127.0.0.1:1"),
                )
                .await
                .unwrap();
                db.close().await;
            });
    })
    .join()
    .unwrap();
    let env = [("JOBPILOT_CLAUDE_BIN", mock.to_str().unwrap())];
    let output = run_env(&home, &["ai", "status", "--json"], &env);
    let value = stdout_json(&output);
    assert_eq!(value["ollama"]["state"], "not_running");
    assert_eq!(value["claude_code"]["available"], true);
    assert!(value["claude_code"]["path"].is_string());
    assert_eq!(value["order"], serde_json::json!(["ollama"]));
}

#[tokio::test]
async fn consent_give_then_revoke_writes_audit_rows() {
    let (_dir, home) = fresh_home();
    ok(
        &home,
        &[
            "ai",
            "consent",
            "give",
            "claude_code",
            "--categories",
            "job_text,resume_text,profile_basic",
        ],
    );
    let status = stdout_json(&run(&home, &["ai", "status", "--json"]));
    assert_eq!(
        status["claude_code"]["consent"].as_array().unwrap().len(),
        3
    );
    ok(&home, &["ai", "consent", "revoke", "claude_code"]);
    let bad = run(
        &home,
        &[
            "ai",
            "consent",
            "give",
            "claude_code",
            "--categories",
            "bogus",
        ],
    );
    assert!(!bad.status.success());

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let actions: Vec<String> = jobpilot_core::audit::recent(&db, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    db.close().await;
    assert!(
        actions.contains(&"consent_given".to_string()),
        "{actions:?}"
    );
    assert!(
        actions.contains(&"consent_revoked".to_string()),
        "{actions:?}"
    );
}

#[test]
fn consent_show_says_what_is_sent_for_each_provider() {
    let (_dir, home) = fresh_home();
    for (name, words) in [
        ("openai", "Services Agreement 4.2"),
        ("anthropic", "Customer Content"),
        ("google", "billing on"),
        ("openrouter", "Prompt logging"),
        ("endpoint", "up to that server"),
        ("gemini_cli", "free Gemini API key"),
        ("codex_cli", "cannot prove it has no tools"),
        ("claude_code", "Consumer Terms"),
    ] {
        let output = ok(&home, &["ai", "consent", "show", name]);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains(words), "{name}: {text}");
        assert!(text.contains("draft_resume"), "{name}: {text}");
        assert!(text.contains("terms read 2026-11-01"), "{name}: {text}");
    }

    let shown = stdout_json(&run(
        &home,
        &["ai", "consent", "show", "codex_cli", "--json"],
    ));
    assert_eq!(shown["key"], "codex_cli/cli_login");
    assert_eq!(shown["tasks"][0]["skipped_because"], "no_tools");

    let local = run(&home, &["ai", "consent", "show", "ollama"]);
    assert!(!local.status.success());
    let text = stderr_text(&local);
    assert!(text.starts_with("error[validation]"), "{text}");
    assert!(!text.contains("claude_code only"), "{text}");
}

#[test]
fn consent_show_with_a_job_prints_the_prompt() {
    let (_dir, home) = fresh_home();
    ok(&home, &["kb", "add", "note", "Shipped the Rust compiler"]);
    let posting = home.parent().unwrap().join("posting.txt");
    std::fs::write(&posting, "We need a Rust engineer.").unwrap();
    let file = posting.to_str().unwrap();
    ok(&home, &["job", "paste", "Rust role", "--file", file]);
    let jobs = stdout_json(&run(&home, &["job", "list", "--json"]));
    let id = jobs[0]["id"].as_str().unwrap();

    let output = ok(&home, &["ai", "consent", "show", "openai", "--job", id]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("<<<UNTRUSTED_POSTING>>>"), "{text}");
    assert!(text.contains("Shipped the Rust compiler"), "{text}");
}

#[tokio::test]
async fn consent_give_needs_attest_for_google() {
    let (_dir, home) = fresh_home();
    let args = [
        "ai",
        "consent",
        "give",
        "google",
        "--categories",
        "job_text",
    ];
    let refused = run(&home, &args);
    assert!(!refused.status.success());
    let text = stderr_text(&refused);
    assert!(text.contains("billing on"), "{text}");
    assert!(text.contains("logged for a limited period"), "{text}");
    assert!(text.contains("--attest"), "{text}");
    assert!(text.contains("error[refused]"), "{text}");

    let mut attested = args.to_vec();
    attested.push("--attest");
    ok(&home, &attested);

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let given = jobpilot_core::ai::consent::active(&db, "google/paid_key").await;
    db.close().await;
    assert_eq!(given.unwrap().unwrap().len(), 1);
    ok(&home, &["ai", "consent", "revoke", "google"]);
}

#[test]
fn anthropic_and_google_take_a_model_and_consent() {
    let (_dir, home) = fresh_home();
    let env = [("JOBPILOT_CLAUDE_BIN", "not-a-path")];
    let row = |home: &Path, name: &str| {
        let shown = stdout_json(&run_env(home, &["ai", "status", "--json"], &env));
        let rows = shown["external"].as_array().unwrap().clone();
        rows.into_iter().find(|row| row["name"] == name).unwrap()
    };
    for name in ["anthropic", "google"] {
        assert_eq!(row(&home, name)["reason"], "no_api_model");
        ok(&home, &["ai", "model", name, "vendor-model-1"]);
        ok(&home, &["ai", "enable", name]);
        let give = ["ai", "consent", "give", name, "--categories", DRAFT_CONSENT];
        if name == "google" {
            let refused = run(&home, &give);
            assert_eq!(refused.status.code(), Some(1));
            assert!(stderr_text(&refused).contains("--attest"));
            assert!(
                row(&home, name)["consent"].is_null(),
                "no attestation, no record"
            );
            let mut attested = give.to_vec();
            attested.push("--attest");
            ok(&home, &attested);
        } else {
            ok(&home, &give);
        }
        let shown = row(&home, name);
        assert_eq!(shown["model"], "vendor-model-1");
        assert_eq!(shown["reason"], "no_key");
        assert_eq!(shown["consent"].as_array().unwrap().len(), 3);
    }

    let refused = run(&home, &["ai", "model", "google", "models/x"]);
    assert!(stderr_text(&refused).contains("validation"));
    assert_eq!(row(&home, "google")["model"], "vendor-model-1");
}

#[test]
fn panel_refuses_new_ai_commands() {
    let (_dir, home) = fresh_home();
    let panel = [("JOBPILOT_PANEL", "1")];
    for args in [
        vec!["ai", "enable", "openai"],
        vec!["ai", "disable", "openai"],
        vec!["ai", "order", "openai"],
        vec!["ai", "model", "openai", "m"],
        vec!["ai", "endpoint", "http://127.0.0.1:1"],
        vec![
            "ai",
            "consent",
            "give",
            "openai",
            "--categories",
            "job_text",
        ],
    ] {
        let refused = run_env(&home, &args, &panel);
        assert!(!refused.status.success(), "{args:?}");
        assert!(stderr_text(&refused).contains("panel_refused"), "{args:?}");
    }
    for args in [
        vec!["ai", "consent", "show", "openai"],
        vec!["ai", "consent", "revoke", "openai"],
    ] {
        let allowed = run_env(&home, &args, &panel);
        assert!(allowed.status.success(), "{args:?}");
    }
}

#[test]
fn endpoint_drafts_end_to_end_against_a_fake() {
    const PATH: &str = "/v1/chat/completions";
    let (_dir, home) = fresh_home();
    let (job, reply) = draftable(&home);
    let content = reply.to_string();
    let answer = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": content},
                     "finish_reason": "stop"}],
    });
    let fake = Fake::start(vec![(PATH, Canned::json(200, &answer))]);
    let env = [("JOBPILOT_CLAUDE_BIN", "not-a-path")];

    ok(&home, &["ai", "endpoint", &format!("{}/v1/", fake.url)]);
    ok(&home, &["ai", "model", "endpoint", "m"]);
    ok(&home, &["ai", "enable", "endpoint"]);
    ok(&home, &["ai", "disable", "ollama"]);
    let shown = stdout_json(&run_env(&home, &["ai", "status", "--json"], &env));
    let rows = shown["external"].as_array().unwrap();
    let row = rows.iter().find(|row| row["name"] == "endpoint").unwrap();
    assert_eq!(row["available"], true);
    assert_eq!(row["path"], format!("{}/v1", fake.url));
    assert_eq!(row["model"], "m");

    let refused = run(&home, &["resume", "generate", &job]);
    assert!(
        stderr_text(&refused).contains("consent"),
        "no consent, no send"
    );
    assert!(fake.hits(PATH).is_empty());

    ok(
        &home,
        &[
            "ai",
            "consent",
            "give",
            "endpoint",
            "--categories",
            DRAFT_CONSENT,
        ],
    );
    let made = stdout_json(&run(&home, &["resume", "generate", &job, "--json"]));
    assert_eq!(made["provider"], "endpoint");
    assert_eq!(made["blocked"], 0);
    assert_eq!(fake.hits(PATH).len(), 1);
    assert_eq!(fake.hits(PATH)[0]["model"], "m");

    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    let id = runs[0]["id"].as_str().unwrap();
    let log = ok(&home, &["agents", "log", id]);
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(log.contains("endpoint/loopback"), "{log}");

    let moved = Fake::start(vec![(PATH, Canned::json(200, &answer))]);
    ok(&home, &["ai", "endpoint", &format!("{}/v1", moved.url)]);
    let stopped = run(&home, &["resume", "generate", &job]);
    assert!(
        stderr_text(&stopped).contains("endpoint address changed"),
        "{}",
        stderr_text(&stopped)
    );
    assert!(moved.hits(PATH).is_empty());
}

#[test]
fn an_endpoint_refused_or_cleared_leaves_the_row_saying_why() {
    let (_dir, home) = fresh_home();
    let env = [("JOBPILOT_CLAUDE_BIN", "not-a-path")];
    let reason = |home: &Path| {
        let shown = stdout_json(&run_env(home, &["ai", "status", "--json"], &env));
        let rows = shown["external"].as_array().unwrap().clone();
        let row = rows.iter().find(|row| row["name"] == "endpoint").cloned();
        row.unwrap()["reason"].clone()
    };
    assert_eq!(reason(&home), "no_endpoint");

    let refused = run(&home, &["ai", "endpoint", "https://api.example.com/v1"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr_text(&refused).contains("not_loopback"));

    ok(&home, &["ai", "endpoint", "http://127.0.0.1:9/v1"]);
    assert_eq!(reason(&home), "no_api_model");
    let shown = ok(&home, &["ai", "consent", "show", "endpoint"]);
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("Address: http://127.0.0.1:9/v1"), "{shown}");
    assert!(!shown.contains("Program:"), "{shown}");
    ok(&home, &["ai", "endpoint", ""]);
    assert_eq!(reason(&home), "no_endpoint");
}

#[test]
fn ai_model_explains_a_refused_openrouter_model() {
    let (_dir, home) = fresh_home();
    let refused = run(&home, &["ai", "model", "openrouter", "meta-llama/llama-x"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr_text(&refused).contains("not eligible through OpenRouter"),
        "{}",
        stderr_text(&refused)
    );
    ok(&home, &["ai", "model", "openrouter", "anthropic/claude-x"]);
    let refused = run(&home, &["ai", "model", "ollama", "m"]);
    assert!(stderr_text(&refused).contains("validation"));
    let refused = run(&home, &["ai", "model", "openai", "gpt-4o-search-preview"]);
    assert!(stderr_text(&refused).contains("searches by itself"));
}

#[test]
fn every_provider_shows_in_status() {
    let (_dir, home) = fresh_home();
    let env = [("JOBPILOT_CLAUDE_BIN", "not-a-path")];
    let shown = run_env(&home, &["ai", "status"], &env);
    let text = String::from_utf8_lossy(&shown.stdout);
    for label in [
        "OpenAI",
        "Anthropic",
        "Google Gemini",
        "OpenRouter",
        "Codex CLI",
    ] {
        assert!(text.contains(label), "{label}: {text}");
    }
    assert!(
        run_env(&home, &["ai", "enable", "openrouter"], &env)
            .status
            .success()
    );
    let shown = run_env(&home, &["status"], &env);
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(text.contains("OpenRouter"), "{text}");
    assert!(!text.contains("Codex CLI"), "only what is on: {text}");
    let json = stdout_json(&run_env(&home, &["status", "--json"], &env));
    assert_eq!(json["ai"]["external"].as_array().unwrap().len(), 7);
}

#[tokio::test]
async fn status_and_profile_show_never_make_a_key() {
    let (_dir, home) = fresh_home();
    // A service no earlier run has used: a key made anywhere would leave rows behind.
    let service = format!("jobpilot-test-unused-{}", std::process::id());
    let env = [("JOBPILOT_SECRET_SERVICE", service.as_str())];
    let status = stdout_json(&run_env(&home, &["status", "--json"], &env));
    assert_eq!(status["key_state"]["reason"], "no_key_yet");
    assert_eq!(status["swap_interrupted"], false);
    assert!(status["ai"]["claude_code"]["available"].is_boolean());
    let shown = stdout_json(&run_env(&home, &["profile", "show", "--json"], &env));
    assert_eq!(shown["key_state"]["reason"], "no_key_yet");
    assert_eq!(shown["locked"], false);
    for args in [
        vec!["data", "recovery-key", "import", "nowhere"],
        vec!["data", "discard-locked", "--confirm", "x"],
    ] {
        let output = run_env(&home, &args, &env);
        assert_eq!(output.status.code(), Some(1));
        assert!(stderr_text(&output).contains("no key yet"), "{args:?}");
    }
    let key_file = home.parent().unwrap().join("key.txt");
    let export = run_env(
        &home,
        &[
            "data",
            "recovery-key",
            "export",
            "--out",
            key_file.to_str().unwrap(),
        ],
        &env,
    );
    assert_eq!(export.status.code(), Some(1));
    assert!(!key_file.exists());

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM data_keys")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let audited = jobpilot_core::audit::recent(&db, 50).await.unwrap();
    db.close().await;
    assert_eq!(keys, 0);
    assert!(
        audited.iter().all(|e| e.action != "data_key_created"),
        "{audited:?}"
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_cancels_generation_and_leaves_no_child() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let (_dir, home) = fresh_home();
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-claude/claude");
    let pids = home.parent().unwrap().join("pids");
    // The child gets a scrubbed environment, so the mock's knobs live in a wrapper script.
    let wrapper = home.parent().unwrap().join("claude");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport MOCK_CLAUDE_SLEEP=60 MOCK_CLAUDE_PID='{}'\nexec '{}' \"$@\"\n",
            pids.display(),
            mock.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let env = [("JOBPILOT_CLAUDE_BIN", wrapper.to_str().unwrap())];
    ok(&home, &["ai", "disable", "ollama"]);
    ok(&home, &["ai", "enable", "claude_code"]);
    ok(&home, &["ai", "order", "claude_code"]);
    ok(
        &home,
        &[
            "ai",
            "consent",
            "give",
            "claude_code",
            "--categories",
            "job_text,resume_text,profile_basic",
        ],
    );
    ok(
        &home,
        &["kb", "add", "experience", "Shipped the Rust compiler"],
    );
    let posting = home.parent().unwrap().join("posting.txt");
    std::fs::write(&posting, "Rust engineer wanted").unwrap();
    let job = stdout_json(&run(
        &home,
        &[
            "job",
            "paste",
            "Engineer",
            "--file",
            posting.to_str().unwrap(),
            "--json",
        ],
    ));

    let mut child = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(["resume", "generate", job["id"].as_str().unwrap()])
        .env("JOBPILOT_HOME", &home)
        .envs(env)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let child_pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pids)
            .ok()
            .and_then(|t| t.lines().next().map(str::to_string))
            .filter(|p| !p.is_empty())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "the mock claude never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&child_pid));
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );

    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "jobpilot ignored Ctrl-C");
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(
        status.code(),
        Some(1),
        "signal {:?}: {stderr}",
        status.signal()
    );
    assert!(stderr.starts_with("error[cancelled]"), "{stderr}");
    let gone = Instant::now() + Duration::from_secs(5);
    while alive(&child_pid) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(&child_pid), "the claude child survived");
}

#[tokio::test]
async fn kb_reveal_prints_a_text_and_a_sealed_item_lists_empty() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(&home, &["kb", "add", "note", "Plain body", "--json"]));
    let plain = added["id"].as_str().unwrap().to_string();
    let shown = stdout_json(&run(&home, &["kb", "reveal", &plain, "--json"]));
    assert_eq!(shown["body"], "Plain body");

    // A sealed row naming a key this root never had, so no keychain entry can exist.
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let mut connection = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO knowledge_items (id, kind, body, body_enc, key_id, source, approved, \
         marked_sensitive, created_at, updated_at) \
         VALUES ('sealed1', 'note', '', x'00', 'gone', 'user', 0, 1, 't', 't')",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
    drop(connection);
    db.close().await;

    let service = format!("jobpilot-test-unused-{}", std::process::id());
    let env = [("JOBPILOT_SECRET_SERVICE", service.as_str())];
    let listed = stdout_json(&run_env(&home, &["kb", "list", "--json"], &env));
    let sealed = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "sealed1")
        .unwrap();
    assert_eq!(sealed["sealed"], true);
    assert_eq!(sealed["body"], "");
    let text = String::from_utf8_lossy(&run_env(&home, &["kb", "list"], &env).stdout).into_owned();
    assert!(text.contains("sealed"), "{text}");

    let revealed = run_env(&home, &["kb", "reveal", "sealed1"], &env);
    assert_eq!(revealed.status.code(), Some(1));
    assert!(
        stderr_text(&revealed).contains("key_missing"),
        "{}",
        stderr_text(&revealed)
    );
}

#[test]
fn app_add_status_list_and_history_round_trip() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &[
            "app",
            "add",
            "--company",
            "Acme",
            "--role",
            "Engineer",
            "--country",
            "pt",
            "--link",
            "https://example.com/j",
            "--json",
        ],
    ));
    let id = added["id"].as_str().unwrap().to_string();
    assert_eq!(added["status"], "saved");
    assert_eq!(added["country"], "PT");
    let moved = stdout_json(&run(
        &home,
        &[
            "app",
            "status",
            &id,
            "applied",
            "--on",
            "2026-10-02",
            "--note",
            "sent",
            "--json",
        ],
    ));
    assert_eq!(moved["applied_on"], "2026-10-02");
    ok(&home, &["app", "note", &id, "recruiter called"]);
    ok(&home, &["app", "edit", &id, "--role", "Staff Engineer"]);
    let shown = stdout_json(&run(&home, &["app", "show", &id, "--json"]));
    assert_eq!(shown["role"], "Staff Engineer");
    assert_eq!(shown["company"], "Acme");
    assert_eq!(shown["link"], "https://example.com/j");
    let applied = stdout_json(&run(
        &home,
        &["app", "list", "--status", "applied", "--json"],
    ));
    assert_eq!(applied.as_array().unwrap().len(), 1);
    let saved = stdout_json(&run(&home, &["app", "list", "--status", "saved", "--json"]));
    assert!(saved.as_array().unwrap().is_empty());
    let history = stdout_json(&run(&home, &["app", "history", &id, "--json"]));
    let kinds: Vec<_> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["created", "status", "note", "edited"]);
    let text = run(&home, &["app", "history", &id]);
    assert!(String::from_utf8_lossy(&text.stdout).contains("saved -> applied"));
    let bad = run(&home, &["app", "status", &id, "ghosted"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(stderr_text(&bad).starts_with("error[validation]"));
    ok(&home, &["app", "remove", &id]);
    let gone = run(&home, &["app", "show", &id]);
    assert!(stderr_text(&gone).starts_with("error[not_found]"));
}

#[test]
fn panel_runs_tracking_commands() {
    let (_dir, home) = fresh_home();
    let panel = [("JOBPILOT_PANEL", "1")];
    let added = run_env(
        &home,
        &["app", "add", "--company", "Acme", "--role", "Dev", "--json"],
        &panel,
    );
    let id = stdout_json(&added)["id"].as_str().unwrap().to_string();
    for args in [
        vec!["app", "list"],
        vec!["app", "show", &id],
        vec!["app", "status", &id, "applied"],
        vec!["app", "note", &id, "n"],
        vec!["app", "edit", &id, "--role", "r"],
        vec!["app", "history", &id],
        vec!["app", "remove", &id],
    ] {
        let output = run_env(&home, &args, &panel);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            stderr_text(&output)
        );
    }
}

#[test]
fn memory_add_approve_and_list_round_trip() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &["memory", "add-question", "Why this job?", "--json"],
    ));
    let question = added["id"].as_str().unwrap().to_string();
    assert_eq!(added["class"], "normal");
    let item = stdout_json(&run(&home, &["kb", "add", "note", "A note", "--json"]));
    let item = item["id"].as_str().unwrap();
    let answer = stdout_json(&run(
        &home,
        &[
            "memory",
            "add-answer",
            &question,
            "Because",
            "--from-item",
            item,
            "--json",
        ],
    ));
    let id = answer["id"].as_str().unwrap().to_string();
    assert_eq!(answer["approved"], false);
    assert_eq!(answer["source"], "knowledge");

    ok(&home, &["memory", "approve", &id]);
    ok(&home, &["memory", "reviewed", &id]);
    ok(&home, &["memory", "edit-answer", &id, "Since then"]);
    ok(&home, &["memory", "approve", &id]);

    let listed = stdout_json(&run(
        &home,
        &["memory", "list", "--class", "normal", "--json"],
    ));
    let row = &listed[0];
    assert_eq!(row["question"]["text"], "Why this job?");
    assert_eq!(row["answers"][0]["body"], "Since then");
    assert_eq!(row["answers"][0]["approved"], true);
    assert_eq!(row["answers"][0]["version"], 2);
    let text =
        String::from_utf8_lossy(&ok(&home, &["memory", "show", &question]).stdout).into_owned();
    assert!(
        text.contains("Since then") && text.contains("approved"),
        "{text}"
    );
    let shown = stdout_json(&run(&home, &["memory", "reveal", &id, "--json"]));
    assert_eq!(shown["body"], "Since then");

    ok(&home, &["memory", "unapprove", &id]);
    ok(&home, &["memory", "set-class", &question, "normal"]);
    let empty = stdout_json(&run(
        &home,
        &["memory", "list", "--class", "legal", "--json"],
    ));
    assert_eq!(empty, serde_json::json!([]));
    ok(&home, &["memory", "remove-answer", &id]);
    ok(&home, &["memory", "remove-question", &question]);
    let gone = run(&home, &["memory", "show", &question]);
    assert!(stderr_text(&gone).starts_with("error[not_found]"));
}

#[test]
fn memory_refuses_an_answer_on_a_work_authorization_question() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &[
            "memory",
            "add-question",
            "Are you authorized to work here?",
            "--country",
            "ca",
            "--json",
        ],
    ));
    assert_eq!(added["class"], "work_authorization");
    assert_eq!(added["country"], "CA");
    let question = added["id"].as_str().unwrap();

    let refused = run(&home, &["memory", "add-answer", question, "Yes"]);

    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr_text(&refused).starts_with("error[validation]"));
    let row = stdout_json(&run(&home, &["memory", "show", question, "--json"]));
    assert_eq!(row["answers"], serde_json::json!([]));
}

#[test]
fn panel_refuses_what_memory_confirms_and_runs_the_rest() {
    let (_dir, home) = fresh_home();
    let panel = [("JOBPILOT_PANEL", "1")];
    let added = run_env(
        &home,
        &["memory", "add-question", "Why this job?", "--json"],
        &panel,
    );
    let question = stdout_json(&added)["id"].as_str().unwrap().to_string();
    let answer = run_env(
        &home,
        &["memory", "add-answer", &question, "Because", "--json"],
        &panel,
    );
    let answer = stdout_json(&answer)["id"].as_str().unwrap().to_string();

    for args in [
        vec!["memory", "approve", &answer],
        vec!["memory", "reviewed", &answer],
        vec!["memory", "set-class", &question, "normal"],
        vec!["memory", "reveal", &answer],
    ] {
        let refused = run_env(&home, &args, &panel);
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert!(
            stderr_text(&refused).contains("panel_refused"),
            "{args:?}: {}",
            stderr_text(&refused)
        );
    }
    let row = stdout_json(&run(&home, &["memory", "show", &question, "--json"]));
    assert_eq!(row["answers"][0]["approved"], false);

    for args in [
        vec!["memory", "list"],
        vec!["memory", "show", &question],
        vec!["memory", "edit-answer", &answer, "Since then"],
        vec!["memory", "unapprove", &answer],
        vec!["memory", "remove-answer", &answer],
        vec!["memory", "remove-question", &question],
    ] {
        let output = run_env(&home, &args, &panel);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            stderr_text(&output)
        );
    }
}

#[tokio::test]
async fn memory_lists_a_sealed_answer_empty_and_reveal_needs_its_key() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &["memory", "add-question", "Your salary?", "--json"],
    ));
    let question = added["id"].as_str().unwrap().to_string();

    // A sealed row naming a key this root never had, so no keychain entry can exist.
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let mut connection = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, source, \
         reviewed_at, created_at, updated_at) \
         VALUES ('sealed1', ?, '', x'00', 'gone', 'user', 't', 't', 't')",
    )
    .bind(&question)
    .execute(&mut *connection)
    .await
    .unwrap();
    drop(connection);
    db.close().await;

    let service = format!("jobpilot-test-unused-{}", std::process::id());
    let env = [("JOBPILOT_SECRET_SERVICE", service.as_str())];
    let row = stdout_json(&run_env(
        &home,
        &["memory", "show", &question, "--json"],
        &env,
    ));
    assert_eq!(row["answers"][0]["sealed"], true);
    assert_eq!(row["answers"][0]["body"], "");
    let text =
        String::from_utf8_lossy(&run_env(&home, &["memory", "list"], &env).stdout).into_owned();
    assert!(text.contains("[sealed]"), "{text}");

    let revealed = run_env(&home, &["memory", "reveal", "sealed1"], &env);
    assert_eq!(revealed.status.code(), Some(1));
    assert!(
        stderr_text(&revealed).contains("key_missing"),
        "{}",
        stderr_text(&revealed)
    );
}

#[tokio::test]
async fn memory_use_is_refused_until_confirmed_and_prints_the_hint() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &["memory", "add-question", "Your salary?", "--json"],
    ));
    let question = added["id"].as_str().unwrap().to_string();
    let app = stdout_json(&run(
        &home,
        &[
            "app",
            "add",
            "--company",
            "Acme",
            "--role",
            "Engineer",
            "--json",
        ],
    ));
    let app = app["id"].as_str().unwrap().to_string();

    // A sealed, approved row under a key this root never had: the gate refuses before any key.
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let mut connection = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, source, \
         approved, reviewed_at, created_at, updated_at) \
         VALUES ('sealed1', ?, '', x'00', 'gone', 'user', 1, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 't', 't')",
    )
    .bind(&question)
    .execute(&mut *connection)
    .await
    .unwrap();
    drop(connection);
    db.close().await;

    let service = format!("jobpilot-test-unused-{}", std::process::id());
    let env = [("JOBPILOT_SECRET_SERVICE", service.as_str())];
    let refused = run_env(
        &home,
        &["memory", "use", "sealed1", "--application", &app],
        &env,
    );
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.contains("confirmation_required"), "{text}");
    assert!(
        text.contains(
            "Confirm it first in an ordinary terminal: jobpilot memory confirm <answer> \
             --application <application>"
        ),
        "{text}"
    );

    let confirmed = run_env(
        &home,
        &["memory", "confirm", "sealed1", "--application", &app],
        &env,
    );
    assert!(confirmed.status.success(), "{}", stderr_text(&confirmed));
    let listed = stdout_json(&run_env(&home, &["app", "answers", &app, "--json"], &env));
    assert_eq!(listed[0]["answer_id"], "sealed1");
    assert_eq!(listed[0]["current"], true);
}

#[test]
fn memory_use_prints_a_normal_answer_and_authorization_says_not_stated() {
    let (_dir, home) = fresh_home();
    let question = stdout_json(&run(
        &home,
        &["memory", "add-question", "Why this job?", "--json"],
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let answer = stdout_json(&run(
        &home,
        &["memory", "add-answer", &question, "Because", "--json"],
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(&home, &["memory", "approve", &answer]);
    let app = stdout_json(&run(
        &home,
        &[
            "app",
            "add",
            "--company",
            "Acme",
            "--role",
            "Engineer",
            "--json",
        ],
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();

    let used = stdout_json(&run(
        &home,
        &["memory", "use", &answer, "--application", &app, "--json"],
    ));
    assert_eq!(used["text"], "Because");

    let none = stdout_json(&run(&home, &["memory", "authorization", "CA", "--json"]));
    assert_eq!(none["stated"], false);
    assert_eq!(none["authorized"], "not_stated");
    let text =
        String::from_utf8_lossy(&ok(&home, &["memory", "authorization", "ca"]).stdout).into_owned();
    assert!(text.contains("not stated by you"), "{text}");
    let bad = run(&home, &["memory", "authorization", "canada"]);
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn panel_refuses_confirm_use_and_authorization() {
    let (_dir, home) = fresh_home();
    let panel = [("JOBPILOT_PANEL", "1")];
    for args in [
        vec!["memory", "confirm", "a", "--application", "x"],
        vec!["memory", "use", "a", "--application", "x"],
        vec!["memory", "authorization", "PT"],
    ] {
        let refused = run_env(&home, &args, &panel);
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert!(stderr_text(&refused).contains("panel_refused"), "{args:?}");
    }
    let listed = run_env(&home, &["app", "answers", "x"], &panel);
    assert!(
        !stderr_text(&listed).contains("panel_refused"),
        "{}",
        stderr_text(&listed)
    );
}

#[test]
fn memory_search_by_text_with_no_model() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &[
            "memory",
            "add-question",
            "Describe your leadership style",
            "--json",
        ],
    ));
    let question = added["id"].as_str().unwrap().to_string();
    let answer = stdout_json(&run(
        &home,
        &["memory", "add-answer", &question, "I mentor", "--json"],
    ));
    ok(
        &home,
        &["memory", "approve", answer["id"].as_str().unwrap()],
    );
    stdout_json(&run(
        &home,
        &[
            "memory",
            "add-question",
            "Are you authorized to work in the US?",
            "--country",
            "US",
            "--json",
        ],
    ));

    let found = stdout_json(&run(&home, &["memory", "search", "leadership", "--json"]));
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["question_id"], question.as_str());
    assert_eq!(found[0]["via"], "text");
    assert_eq!(found[0]["answer"], "I mentor");
    // Work authorization is never found, whatever the words.
    let none = stdout_json(&run(&home, &["memory", "search", "authorized", "--json"]));
    assert!(none.as_array().unwrap().is_empty());

    // With no model, refresh says so and succeeds.
    let refreshed = run(&home, &["memory", "refresh"]);
    assert!(refreshed.status.success());
    assert!(
        String::from_utf8_lossy(&refreshed.stdout)
            .contains("No embedding model is set; search uses text.")
    );
}

#[test]
fn memory_text_names_the_item_an_answer_came_from() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &["memory", "add-question", "Why this job?", "--json"],
    ));
    let question = added["id"].as_str().unwrap().to_string();
    let item = stdout_json(&run(&home, &["kb", "add", "note", "A note", "--json"]));
    let item = item["id"].as_str().unwrap().to_string();
    let answer = stdout_json(&run(
        &home,
        &[
            "memory",
            "add-answer",
            &question,
            "Because",
            "--from-item",
            &item,
            "--json",
        ],
    ));
    let id = answer["id"].as_str().unwrap().to_string();

    let text =
        String::from_utf8_lossy(&ok(&home, &["memory", "show", &question]).stdout).into_owned();
    assert!(text.contains(&format!("knowledge {item}")), "{text}");

    ok(&home, &["memory", "edit-answer", &id, "My own words"]);
    let text =
        String::from_utf8_lossy(&ok(&home, &["memory", "show", &question]).stdout).into_owned();
    assert!(!text.contains(&item), "{text}");
}

#[test]
fn memory_refuses_lowering_authorization_and_says_to_reword() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &[
            "memory",
            "add-question",
            "Are you authorized to work here?",
            "--json",
        ],
    ));
    let question = added["id"].as_str().unwrap();

    let refused = run(&home, &["memory", "set-class", question, "normal"]);

    assert_eq!(refused.status.code(), Some(1));
    let said = stderr_text(&refused);
    assert!(said.starts_with("error[refused]"), "{said}");
    assert!(
        said.contains("your profile") && said.contains("other words"),
        "{said}"
    );
}

#[tokio::test]
async fn data_export_says_how_many_answers_it_left_out() {
    let (_dir, home) = fresh_home();
    let added = stdout_json(&run(
        &home,
        &["memory", "add-question", "Why this job?", "--json"],
    ));
    let question = added["id"].as_str().unwrap().to_string();
    ok(&home, &["memory", "add-answer", &question, "Because"]);
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    sqlx::query("UPDATE memory_questions SET class = 'sensitive'")
        .execute(&db.pool)
        .await
        .unwrap();
    db.close().await;

    let output = run(&home, &["data", "export", "--json"]);

    assert!(
        stderr_text(&output).contains("1 sealed career answer"),
        "{}",
        stderr_text(&output)
    );
    let report = stdout_json(&output);
    assert_eq!(report["withheld"], 1);
    assert_eq!(report["sensitive_plain"], false);
}

fn tracked_run(home: &Path) -> (String, String) {
    let added = stdout_json(&run(
        home,
        &[
            "app",
            "add",
            "--company",
            "Acme",
            "--role",
            "Engineer",
            "--json",
        ],
    ));
    let app = added["id"].as_str().unwrap().to_owned();
    assert!(
        run(home, &["app", "status", &app, "applied", "--note", "sent"])
            .status
            .success()
    );
    let runs = stdout_json(&run(home, &["agents", "list", "--json"]));
    (app, runs[0]["id"].as_str().unwrap().to_owned())
}

#[test]
fn agents_list_shows_a_tracking_run_after_app_status() {
    let (_dir, home) = fresh_home();
    let (_, id) = tracked_run(&home);
    let runs = stdout_json(&run(&home, &["agents", "list", "--limit", "5", "--json"]));
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["agent"], "tracking");
    assert_eq!(runs[0]["state"], "completed");
    assert_eq!(runs[0]["actor"], "cli");
    assert_eq!(runs[0]["step"], 1);

    let text = run(&home, &["agents", "list"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&id) && text.contains("tracking  completed  1 of 1"),
        "{text}"
    );
    let shown = run(&home, &["agents", "show", &id]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("completed"));
}

#[test]
fn agents_log_prints_states() {
    let (_dir, home) = fresh_home();
    let (_, id) = tracked_run(&home);
    let log = run(&home, &["agents", "log", &id]);
    assert!(log.status.success());
    let text = String::from_utf8_lossy(&log.stdout);
    let states: Vec<&str> = text
        .lines()
        .map(|line| line.split("  ").nth(2).unwrap())
        .collect();
    assert_eq!(
        states,
        ["queued", "checking", "running", "checkpointed", "completed"],
        "{text}"
    );
    assert!(
        !text.contains("sent"),
        "a note stays out of the log: {text}"
    );
    let json = stdout_json(&run(&home, &["agents", "log", &id, "--json"]));
    assert_eq!(json.as_array().unwrap().len(), 5);
}

#[test]
fn agents_cancel_and_resume_refuse_a_finished_run() {
    let (_dir, home) = fresh_home();
    let (_, id) = tracked_run(&home);
    for verb in ["cancel", "resume"] {
        let refused = run(&home, &["agents", verb, &id]);
        assert!(!refused.status.success());
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.contains("error[validation]"), "{stderr}");
    }
    let missing = run(&home, &["agents", "show", "nope"]);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("error[not_found]"));
}

#[tokio::test]
async fn agents_cancel_and_resume_act_on_a_halted_run() {
    let (_dir, home) = fresh_home();
    let (app, id) = tracked_run(&home);
    let halt = |run_id: String, home: std::path::PathBuf| async move {
        let db = Db::open(&AppPaths::new(&home)).await.unwrap();
        sqlx::query(
            "UPDATE agent_runs SET state = 'halted', step = 0, finished_at = NULL WHERE id = ?",
        )
        .bind(run_id)
        .execute(&db.pool)
        .await
        .unwrap();
        db.close().await;
    };
    halt(id.clone(), home.clone()).await;
    let resumed = stdout_json(&run(&home, &["agents", "resume", &id, "--json"]));
    assert_eq!(resumed["state"], "completed");
    assert_eq!(resumed["id"], id.as_str());

    halt(id.clone(), home.clone()).await;
    assert!(run(&home, &["agents", "cancel", &id]).status.success());
    let shown = stdout_json(&run(&home, &["agents", "show", &id, "--json"]));
    assert_eq!(shown["state"], "cancelled");
    let app = stdout_json(&run(&home, &["app", "show", &app, "--json"]));
    assert_eq!(app["status"], "applied");
}

#[test]
fn analytics_show_says_so_with_no_report() {
    let (_dir, home) = fresh_home();
    let shown = run(&home, &["analytics", "show"]);
    assert!(shown.status.success());
    assert_eq!(
        String::from_utf8_lossy(&shown.stdout).trim(),
        "No report yet."
    );
    let json = stdout_json(&run(&home, &["analytics", "show", "--json"]));
    assert!(json.is_null(), "{json}");
}

#[test]
fn analytics_report_after_applying_prints_a_rate() {
    let (_dir, home) = fresh_home();
    tracked_run(&home);
    let report = stdout_json(&run(&home, &["analytics", "report", "--json"]));
    assert_eq!(report["volume"]["total"], 1);
    assert_eq!(report["funnel"]["applied"], 1);
    assert_eq!(report["funnel"]["response_rate"], 0.0);

    let text = run(&home, &["analytics", "report"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Response rate: 0%"), "{text}");
    assert!(text.contains("As of 20"), "{text}");
    assert!(
        text.contains("Median days to a response: not enough data yet"),
        "{text}"
    );
    assert!(!text.contains("Acme"), "{text}");

    let shown = String::from_utf8_lossy(&run(&home, &["analytics", "show"]).stdout).into_owned();
    assert!(shown.contains("Response rate: 0%"), "{shown}");
    let latest = stdout_json(&run(&home, &["analytics", "show", "--json"]));
    let at = latest["created_at"].as_str().unwrap();
    assert!(shown.contains(&format!("As of {at}")), "{shown}");
    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert_eq!(runs[0]["agent"], "analytics");
}

#[test]
fn pause_set_status_clear_round_trip() {
    let (_dir, home) = fresh_home();
    let off = stdout_json(&run(&home, &["pause", "status", "--json"]));
    assert_eq!(off["paused"], false);
    assert!(off["set_at"].is_null());
    assert!(String::from_utf8_lossy(&ok(&home, &["pause", "status"]).stdout).contains("no"));

    let on = stdout_json(&run(&home, &["pause", "set", "--json"]));
    assert_eq!(on["paused"], true);
    assert_eq!(on["set_by"], "cli");
    // A second set changes nothing.
    let again = stdout_json(&run(&home, &["pause", "set", "--json"]));
    assert_eq!(again["set_at"], on["set_at"]);

    // Nothing automated starts, and the error says how to clear it.
    let app = stdout_json(&run(
        &home,
        &["app", "add", "--company", "Acme", "--role", "Dev", "--json"],
    ));
    let id = app["id"].as_str().unwrap();
    let refused = run(&home, &["app", "status", id, "applied"]);
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.starts_with("error[paused]"), "{text}");
    assert!(text.contains("jobpilot pause clear"), "{text}");
    // Editing a field stays open.
    ok(&home, &["app", "edit", id, "--role", "Staff"]);
    let json = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert!(json.as_array().unwrap().is_empty(), "no run started");

    let cleared = stdout_json(&run(&home, &["pause", "clear", "--json"]));
    assert_eq!(cleared["paused"], false);
    ok(&home, &["app", "status", id, "applied"]);
}

#[test]
fn pause_clear_is_refused_in_the_panel() {
    let (_dir, home) = fresh_home();
    let panel = [("JOBPILOT_PANEL", "1")];
    let set = run_env(&home, &["pause", "set"], &panel);
    assert!(set.status.success(), "{}", stderr_text(&set));
    let status = run_env(&home, &["pause", "status"], &panel);
    assert!(status.status.success());
    let refused = run_env(&home, &["pause", "clear"], &panel);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr_text(&refused).starts_with("error[panel_refused]"));
    let still = stdout_json(&run(&home, &["pause", "status", "--json"]));
    assert_eq!(still["paused"], true);
}

#[test]
fn status_prints_the_pause() {
    let (_dir, home) = fresh_home();
    let text = |home: &Path| String::from_utf8_lossy(&ok(home, &["status"]).stdout).into_owned();
    assert!(text(&home).contains("paused: no"), "{}", text(&home));
    assert_eq!(
        stdout_json(&run(&home, &["status", "--json"]))["paused"],
        false
    );
    ok(&home, &["pause", "set"]);
    let on = text(&home);
    assert!(on.contains("paused: yes, set "), "{on}");
    assert!(on.contains(" by cli"), "{on}");
    let json = stdout_json(&run(&home, &["status", "--json"]));
    assert_eq!(json["paused"], true);
    assert_eq!(json["paused_by"], "cli");
}

#[cfg(unix)]
#[test]
fn pause_from_a_second_process_stops_a_running_draft() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let (_dir, home) = fresh_home();
    let dir = home.parent().unwrap();
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/mock-claude/claude");
    let (pids, done) = (dir.join("pids"), dir.join("done"));
    // The child gets a scrubbed environment, so the mock's knobs live in a wrapper script.
    let wrapper = dir.join("claude");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport MOCK_CLAUDE_SLEEP=30 MOCK_CLAUDE_PID='{}' MOCK_CLAUDE_DONE='{}'\n\
             exec '{}' \"$@\"\n",
            pids.display(),
            done.display(),
            mock.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let bin = wrapper.to_str().unwrap();
    claude_only(&home, bin, DRAFT_CONSENT);
    let (job, _) = draftable(&home);

    let mut child = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(["resume", "generate", &job])
        .env("JOBPILOT_HOME", &home)
        .env("JOBPILOT_CLAUDE_BIN", bin)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    // The run row shows running, and the mock has started.
    let child_pid = loop {
        let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
        let running = runs
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["state"] == "running");
        let pid = std::fs::read_to_string(&pids)
            .ok()
            .and_then(|t| t.lines().next().map(str::to_string))
            .filter(|p| !p.is_empty());
        if let (true, Some(pid)) = (running, pid) {
            break pid;
        }
        assert!(Instant::now() < deadline, "the draft never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&child_pid));

    ok(&home, &["pause", "set"]);
    let paused_at = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            paused_at.elapsed() < Duration::from_secs(5),
            "the draft ignored the pause"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(
        status.code(),
        Some(1),
        "signal {:?}: {stderr}",
        status.signal()
    );
    assert!(stderr.starts_with("error[paused]"), "{stderr}");

    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert_eq!(runs[0]["state"], "halted");
    assert_eq!(runs[0]["reason"], "paused");
    assert!(!done.exists(), "the mock finished its work");
    let gone = Instant::now() + Duration::from_secs(5);
    while alive(&child_pid) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(&child_pid), "the claude child survived the pause");
}

/// A stand-in for a loopback Ollama. It lists one local model, and then holds `/api/pull` and
/// `/api/embed` open without ever finishing them, as a long download or a slow model does.
/// `reached` says when a held request has arrived.
fn fake_ollama() -> (String, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let reached = Arc::new(AtomicBool::new(false));
    let flag = reached.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let flag = flag.clone();
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                let mut buf = [0u8; 4096];
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&seen).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                let whole = |body: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                match path.as_str() {
                    "/api/version" => {
                        let _ = stream.write_all(whole(r#"{"version":"0.5.0"}"#).as_bytes());
                    }
                    "/api/tags" => {
                        let tags = r#"{"models":[{"name":"gemma3:4b","size":3338801718,
                            "details":{"family":"gemma3","parameter_size":"4.3B"}}]}"#;
                        let _ = stream.write_all(whole(tags).as_bytes());
                    }
                    _ => {
                        if path == "/api/pull" {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
                            );
                        }
                        flag.store(true, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                }
            });
        }
    });
    (url, reached)
}

/// A pause from a second process ends a download or an index the CLI is running, and the
/// command says `paused`, not `cancelled` and not a manual-work error.
#[tokio::test]
async fn pause_from_a_second_process_stops_ai_pull_and_ai_index() {
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    for args in [vec!["ai", "pull", "gemma3:4b"], vec!["ai", "index"]] {
        let (_dir, home) = fresh_home();
        let (url, reached) = fake_ollama();
        ok(
            &home,
            &["kb", "add", "experience", "Shipped the Rust compiler"],
        );
        let db = Db::open(&AppPaths::new(&home)).await.unwrap();
        jobpilot_core::settings::set(&db, "ai.ollama_url", &serde_json::json!(url))
            .await
            .unwrap();
        jobpilot_core::settings::set(&db, "ai.embedding_model", &serde_json::json!("gemma3:4b"))
            .await
            .unwrap();
        db.close().await;

        let mut child = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
            .args(&args)
            .env("JOBPILOT_HOME", &home)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !reached.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "{args:?} never reached the server"
            );
            std::thread::sleep(Duration::from_millis(25));
        }

        ok(&home, &["pause", "set"]);
        let paused_at = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                paused_at.elapsed() < Duration::from_secs(5),
                "{args:?} ignored the pause"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
        assert_eq!(status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains("error[paused]"), "{args:?}: {stderr}");
    }
}

#[tokio::test]
async fn ai_index_while_paused_says_paused() {
    let (_dir, home) = fresh_home();
    ok(
        &home,
        &["kb", "add", "experience", "Shipped the Rust compiler"],
    );
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    jobpilot_core::settings::set(&db, "ai.embedding_model", &serde_json::json!("gemma3:4b"))
        .await
        .unwrap();
    db.close().await;
    ok(&home, &["pause", "set"]);
    let refused = run(&home, &["ai", "index"]);
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.starts_with("error[paused]"), "{text}");
}

/// A run with `input` piped in, as a script would give a key.
fn run_with_stdin(home: &Path, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A command that refuses before reading closes the pipe, which is no failure here.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

#[test]
fn key_set_refuses_argv_by_shape() {
    let (_dir, home) = fresh_home();
    for args in [
        vec!["ai", "key", "set", "openai", "sk-ZXQ-argv"],
        vec!["ai", "key", "set", "openai", "--key=sk-ZXQ-argv"],
        vec!["ai", "key", "set", "sk-ZXQ-argv"],
        vec!["ai", "--json", "key", "set", "openai", "sk-ZXQ-argv"],
        vec!["--json", "ai", "key", "remove", "openai", "sk-ZXQ-argv"],
    ] {
        let refused = run(&home, &args);
        assert_eq!(refused.status.code(), Some(2), "{args:?}");
        assert!(refused.stdout.is_empty(), "{args:?}");
        let text = stderr_text(&refused);
        assert!(text.starts_with("error[usage]"), "{args:?}: {text}");
        assert!(!text.contains("ZXQ"), "{args:?}: {text}");
    }
    assert!(
        !home.exists(),
        "refused while parsing, before any data opened"
    );
}

#[test]
fn only_a_key_command_hides_what_clap_says() {
    let (_dir, home) = fresh_home();
    let help = run(&home, &["ai", "key", "set", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("Usage"), "{text}");

    let other = run(&home, &["doc", "nosuch"]);
    assert_eq!(other.status.code(), Some(2));
    let text = stderr_text(&other);
    assert!(text.contains("nosuch"), "{text}");
    assert!(!text.contains("error[usage]"), "{text}");
}

/// Refused before stdin is read: the pipe stays open, and nothing is ever written to it.
#[test]
fn a_provider_with_no_key_is_refused_before_any_key_is_asked_for() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let (_dir, home) = fresh_home();
    let mut child = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(["ai", "key", "set", "gemini_cli"])
        .env("JOBPILOT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let held_open = child.stdin.take();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            panic!("waited for a key this provider cannot take");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    drop(held_open);
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.starts_with("error[refused]"), "{stderr}");
}

#[test]
fn key_set_with_a_bad_key_echoes_nothing() {
    let (_dir, home) = fresh_home();
    let refused = run_with_stdin(&home, &["ai", "key", "set", "openai"], "two words here\n");
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.starts_with("error[validation]"), "{text}");
    assert!(!text.contains("two words here"), "{text}");
    assert!(refused.stdout.is_empty());
    let secrets = run(&home, &["ai", "status", "--json"]);
    let status = stdout_json(&secrets);
    assert_eq!(status["external"][0]["key_stored"], false);
}

#[test]
fn key_set_with_nothing_piped_is_not_a_key() {
    let (_dir, home) = fresh_home();
    let refused = run_with_stdin(&home, &["ai", "key", "set", "openai"], "");
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr_text(&refused).starts_with("error[validation]"));
}

#[test]
fn key_set_refuses_a_provider_that_takes_no_key() {
    let (_dir, home) = fresh_home();
    for provider in ["ollama", "claude_code", "gemini_cli", "codex_cli"] {
        let refused = run_with_stdin(&home, &["ai", "key", "set", provider], "sk-abcdefgh\n");
        assert_eq!(refused.status.code(), Some(1), "{provider}");
        let text = stderr_text(&refused);
        assert!(text.starts_with("error[refused]"), "{provider}: {text}");
        assert!(!text.contains("sk-abcdefgh"), "{text}");
    }
}

#[test]
fn key_remove_with_no_key_is_not_found() {
    let (_dir, home) = fresh_home();
    let refused = run(&home, &["ai", "key", "remove", "openai"]);
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.starts_with("error[not_found]"), "{text}");
}

/// The test names the stored value by reading the data folder back through `ai status`.
fn cli_row(home: &Path, name: &str) -> serde_json::Value {
    let status = stdout_json(&run(home, &["ai", "status", "--json"]));
    status["external"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .cloned()
        .unwrap()
}

#[test]
fn cli_path_refuses_another_program() {
    let (dir, home) = fresh_home();
    let program = if cfg!(windows) {
        "gemini.cmd"
    } else {
        "gemini"
    };
    let mock = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/mock-gemini")
        .join(program);
    // Another program, a relative path, and another provider's name are all refused.
    for (provider, path) in [
        ("gemini_cli", "/bin/sh"),
        ("gemini_cli", "gemini"),
        ("gemini_cli", "/x/gemini-evil"),
        ("codex_cli", mock.to_str().unwrap()),
        ("claude_code", mock.to_str().unwrap()),
        ("openai", mock.to_str().unwrap()),
    ] {
        let out = run(&home, &["ai", "cli-path", provider, path]);
        assert!(!out.status.success(), "{provider} {path}");
    }
    let refused = run(&home, &["ai", "cli-path", "gemini_cli", "/bin/sh"]);
    let message = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(
        message.contains("full path to a program named gemini"),
        "{message}"
    );

    // The right program is stored and shows on the row; an empty path clears it.
    let set = run(
        &home,
        &[
            "ai",
            "cli-path",
            "gemini_cli",
            mock.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(stdout_json(&set)["gemini_cli_path"], mock.to_str().unwrap());
    assert_eq!(cli_row(&home, "gemini_cli")["path"], mock.to_str().unwrap());
    let cleared = run(&home, &["ai", "cli-path", "gemini_cli", "", "--json"]);
    assert_eq!(stdout_json(&cleared)["gemini_cli_path"], "");
    drop(dir);
}

// Job commands. Each runs on a temporary home and the test keychain service; none reaches a
// vendor, and no provider is set up.

fn run_stdin(home: &Path, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
        .env("JOBPILOT_SECRET_SERVICE", "jobpilot-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout_of(output: &Output) -> String {
    assert!(output.status.success(), "stderr: {}", stderr_text(output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Pastes a posting through standard input and returns its job.
fn paste(home: &Path, title: &str, extra: &[&str], text: &str) -> serde_json::Value {
    let mut args = vec!["job", "paste", title];
    args.extend_from_slice(extra);
    args.push("--json");
    stdout_json(&run_stdin(home, &args, text))
}

#[test]
fn job_paste_then_list_show_and_resume_generate_reach_the_same_job() {
    let (_dir, home) = fresh_home();
    let job = paste(
        &home,
        "Rust role",
        &["--company", "Acme"],
        "Build Rust services.",
    );
    let id = job["id"].as_str().unwrap();
    let listed = stdout_json(&run(&home, &["job", "list", "--json"]));
    assert_eq!(listed[0]["id"], id);
    let shown = stdout_json(&run(&home, &["job", "show", id, "--json"]));
    assert_eq!(shown["job"]["id"], id);
    assert_eq!(shown["job"]["company"], "Acme");
    // The draft commands take the same id, and reach the same posting: no AI, so manual.
    let drafted = run(&home, &["resume", "generate", id]);
    let text = stderr_text(&drafted);
    assert!(!text.contains("not_found"), "{text}");
    assert!(!text.contains("error[validation]"), "{text}");
    let text = stderr_text(&run(&home, &["letter", "generate", id]));
    assert!(!text.contains("not_found"), "{text}");
    assert_eq!(
        stderr_text(&run(&home, &["resume", "generate", "nosuch"]))
            .split(':')
            .next(),
        Some("error[not_found]")
    );
}

#[test]
fn job_import_a_linkedin_url_with_text_stores_it_and_makes_no_request() {
    let (_dir, home) = fresh_home();
    let url = "https://www.linkedin.com/jobs/view/123";
    let args = ["job", "import", "--url", url, "--title", "Dev", "--json"];
    let job = stdout_json(&run_stdin(&home, &args, "A posting body."));
    assert_eq!(job["connector"], "linkedin");
    assert_eq!(job["url"], url);
    assert!(
        stdout_json(&run(&home, &["agents", "list", "--json"]))
            .as_array()
            .unwrap()
            .is_empty(),
        "an import starts no run"
    );
}

#[test]
fn job_import_a_url_with_no_text_says_paste_the_text() {
    let (_dir, home) = fresh_home();
    let args = [
        "job",
        "import",
        "--url",
        "https://jobs.lever.co/acme/1",
        "--title",
        "Dev",
    ];
    let refused = run_stdin(&home, &args, "");
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.starts_with("error[refused]"), "{text}");
    assert!(text.contains("needs_text"), "{text}");
    assert!(text.contains("paste the posting's text"), "{text}");
}

#[test]
fn job_src_add_linkedin_is_refused_with_the_import_route() {
    let (_dir, home) = fresh_home();
    let refused = run(&home, &["job", "src", "add", "linkedin", "x"]);
    assert_eq!(refused.status.code(), Some(1));
    let text = stderr_text(&refused);
    assert!(text.starts_with("error[refused]"), "{text}");
    assert!(text.contains("Import the posting."), "{text}");
    assert!(text.contains("jobpilot job import"), "{text}");
    let none = stdout_json(&run(&home, &["job", "source", "list", "--json"]));
    assert!(none.as_array().unwrap().is_empty());
}

#[test]
fn job_src_add_greenhouse_says_the_vendor_is_silent() {
    let (_dir, home) = fresh_home();
    let text = stdout_of(&run(&home, &["job", "src", "add", "greenhouse", "acme"]));
    assert!(
        text.contains("Vendor is silent on third-party readers"),
        "{text}"
    );
    let list = stdout_json(&run(&home, &["job", "source", "list", "--json"]));
    assert_eq!(list[0]["connector"], "greenhouse");
    assert_eq!(list[0]["target"], "acme");
    let added = stdout_json(&run(
        &home,
        &["job", "source", "add", "lever", "acme", "--json"],
    ));
    assert!(added["note"].is_null());
    let id = added["id"].as_str().unwrap();
    ok(&home, &["job", "source", "remove", id]);
    assert_eq!(
        stdout_json(&run(&home, &["job", "source", "list", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn job_sources_lists_all_nine_with_status_limit_and_attribution() {
    let (_dir, home) = fresh_home();
    let json = stdout_json(&run(&home, &["job", "sources", "--json"]));
    let all = json.as_array().unwrap();
    assert_eq!(all.len(), 9);
    let text = stdout_of(&run(&home, &["job", "sources"]));
    for source in all {
        assert!(text.contains(source["name"].as_str().unwrap()), "{text}");
    }
    let remoteok = text.lines().find(|l| l.starts_with("remoteok")).unwrap();
    assert!(remoteok.contains("read: allowed"), "{remoteok}");
    assert!(remoteok.contains("Remote OK"), "{remoteok}");
    assert!(remoteok.contains("limit:"), "{remoteok}");
    assert!(
        text.contains("Vendor is silent on third-party readers"),
        "{text}"
    );
    assert!(text.contains("read on: 20"), "{text}");
}

#[test]
fn job_fetch_with_no_sources_says_add_one_and_sends_nothing() {
    let (_dir, home) = fresh_home();
    let text = stdout_of(&run(&home, &["job", "fetch"]));
    assert!(text.contains("job source add"), "{text}");
    assert!(
        stdout_json(&run(&home, &["agents", "list", "--json"]))
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn job_list_filters_are_exact_and_json_matches_text() {
    let (_dir, home) = fresh_home();
    paste(
        &home,
        "Remote dev",
        &[],
        "Fully remote role. Work from anywhere.",
    );
    paste(&home, "Plain dev", &[], "Write code for us.");
    let all = stdout_json(&run(&home, &["job", "list", "--json"]));
    assert_eq!(all.as_array().unwrap().len(), 2);
    let text = stdout_of(&run(&home, &["job", "list"]));
    assert_eq!(text.lines().count(), 2);
    for job in all.as_array().unwrap() {
        assert!(text.contains(job["id"].as_str().unwrap()), "{text}");
    }
    // A job that states another mode is dropped; one that states none stays, marked.
    let hybrid = stdout_json(&run(&home, &["job", "list", "--mode", "hybrid", "--json"]));
    for job in hybrid.as_array().unwrap() {
        assert!(job["work_mode"].is_null() || job["work_mode"] == "hybrid");
    }
    let remote = stdout_json(&run(&home, &["job", "list", "--mode", "remote", "--json"]));
    assert!(remote.as_array().unwrap().len() <= all.as_array().unwrap().len());
    let marked = stdout_of(&run(&home, &["job", "list", "--country", "PT"]));
    assert!(marked.contains("not stated: country"), "{marked}");
    let bad = run(&home, &["job", "list", "--min-pay", "lots"]);
    assert!(stderr_text(&bad).starts_with("error[validation]"));
    let pay = stdout_of(&run(&home, &["job", "list", "--min-pay", "90000:eur:year"]));
    assert_eq!(pay.lines().count(), 2, "unstated pay is kept: {pay}");
}

#[test]
fn job_analyze_with_no_ai_scores_and_prints_reasons() {
    let (_dir, home) = fresh_home();
    ok(&home, &["profile", "set", "--name", "Ada"]);
    let job = paste(&home, "Rust dev", &[], "Remote role using Rust and SQL.");
    let id = job["id"].as_str().unwrap();
    let text = stdout_of(&run(&home, &["job", "analyze", id]));
    assert!(text.contains("scored"), "{text}");
    assert!(text.contains(id), "{text}");
    assert!(text.contains("skills"), "reasons are printed: {text}");
    let json = stdout_json(&run(&home, &["job", "analyze", id, "--json"]));
    assert_eq!(json["jobs"][0]["id"], id);
    assert!(json["jobs"][0]["explanation"]["rows"].is_array());
    let shown = stdout_of(&run(&home, &["job", "show", id]));
    assert!(shown.contains("Score:"), "{shown}");
}

#[test]
fn job_analyze_ai_without_yes_states_calls_and_stops() {
    let (_dir, home) = fresh_home();
    let bin = fresh_claude(&home);
    let job = paste(&home, "Rust dev", &[], "Remote role using Rust.");
    let id = job["id"].as_str().unwrap();
    let env = [("JOBPILOT_CLAUDE_BIN", bin.as_str())];
    let stopped = run_env(&home, &["job", "analyze", id, "--ai"], &env);
    let text = stdout_of(&stopped);
    assert!(text.contains("at most"), "{text}");
    assert!(text.contains("--ai --yes"), "{text}");
    assert!(
        stdout_json(&run(&home, &["agents", "list", "--json"]))
            .as_array()
            .unwrap()
            .is_empty(),
        "no run started"
    );
}

#[test]
fn job_research_while_paused_refuses_before_the_count() {
    let (_dir, home) = fresh_home();
    let bin = claude_saying(&home, &serde_json::json!({}));
    claude_only(&home, &bin, "job_text,company_text");
    let job = paste(&home, "Rust dev", &["--company", "Acme"], "Remote role.");
    let company = job["company_id"].as_str().unwrap();
    let env = [("JOBPILOT_CLAUDE_BIN", bin.as_str())];
    ok(&home, &["pause", "set"]);
    let stopped = run_env(&home, &["job", "research", company], &env);
    assert_eq!(stopped.status.code(), Some(1));
    assert!(stderr_text(&stopped).starts_with("error[paused]"));
}

#[test]
fn job_analyze_ai_with_no_approved_evidence_says_what_to_do() {
    let (_dir, home) = fresh_home();
    let bin = fresh_claude(&home);
    let job = paste(&home, "Rust dev", &[], "Remote role using Rust.");
    let id = job["id"].as_str().unwrap();
    let env = [("JOBPILOT_CLAUDE_BIN", bin.as_str())];
    let text = stdout_of(&run_env(
        &home,
        &["job", "analyze", id, "--ai", "--yes"],
        &env,
    ));
    assert!(!text.contains("no_evidence"), "{text}");
    assert!(text.contains("jobpilot kb add"), "{text}");
    assert!(text.contains("jobpilot kb approve"), "{text}");
}

/// A mock claude that is on and consented. The plan only counts: nothing here runs it.
fn fresh_claude(home: &Path) -> String {
    let bin = claude_saying(home, &serde_json::json!({}));
    claude_only(home, &bin, "job_text,profile_basic,resume_text");
    bin
}

#[test]
fn job_show_reads_not_stated_for_a_missing_country() {
    let (_dir, home) = fresh_home();
    ok(&home, &["profile", "auth", "set", "PT"]);
    let job = paste(&home, "Dev", &[], "Write code.");
    let text = stdout_of(&run(&home, &["job", "show", job["id"].as_str().unwrap()]));
    assert!(text.contains("Country: not stated"), "{text}");
    assert!(text.contains("Work authorization: not stated"), "{text}");
    assert!(text.contains("Source: import"), "{text}");
    assert!(text.contains("Untrusted text:"), "{text}");
    assert!(!text.contains("eligible"), "{text}");
}

#[test]
fn job_show_strips_terminal_escapes_from_a_posting() {
    let (_dir, home) = fresh_home();
    let url = "https://example.com/jobs/1\x1b]0;evil\x07";
    let args = [
        "job",
        "import",
        "--url",
        url,
        "--title",
        "Dev\x1b[2J",
        "--company",
        "Ac\x1b[31mme",
        "--json",
    ];
    let job = stdout_json(&run_stdin(
        &home,
        &args,
        "Build \x1b[31mthings\x1b[0m\x07 now",
    ));
    let id = job["id"].as_str().unwrap();
    let text = stdout_of(&run(&home, &["job", "show", id]));
    assert!(
        !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "{text:?}"
    );
    assert!(text.contains("Build things now"), "{text}");
}

#[test]
fn job_research_with_no_backend_shows_stored_text() {
    let (_dir, home) = fresh_home();
    paste(&home, "Dev", &["--company", "Acme"], "Acme builds anvils.");
    let companies = stdout_json(&run(&home, &["job", "company", "list", "--json"]));
    let company = companies[0]["id"].as_str().unwrap();
    let noted = run_stdin(
        &home,
        &["job", "company", "note", company],
        "Founded long ago.",
    );
    stdout_of(&noted);
    let text = stdout_of(&run(&home, &["job", "research", company]));
    assert!(text.contains("Stored text:"), "{text}");
    assert!(text.contains("Founded long ago."), "{text}");
    let missing = run(&home, &["job", "research", "nosuch"]);
    assert!(stderr_text(&missing).starts_with("error[not_found]"));
}

#[test]
fn a_stale_score_is_marked_until_job_analyze() {
    let (_dir, home) = fresh_home();
    let job = paste(&home, "Dev", &[], "We need Rust. 5+ years of experience.");
    let id = job["id"].as_str().unwrap();
    let show = |home: &Path| stdout_of(&run(home, &["job", "show", id]));
    assert!(show(&home).contains("Score: -- (Not scored yet)"));
    ok(&home, &["job", "analyze"]);
    assert!(!stdout_of(&run(&home, &["job", "list"])).contains("stale"));
    assert!(!show(&home).contains("stale"));

    ok(&home, &["job", "prefs", "set", "--seniority", "senior"]);

    assert!(stdout_of(&run(&home, &["job", "list"])).contains("[stale score]"));
    assert!(show(&home).contains("stale: run `jobpilot job analyze`"));
    ok(&home, &["job", "analyze"]);
    assert!(!stdout_of(&run(&home, &["job", "list"])).contains("stale"));
}

#[test]
fn job_show_says_why_a_scored_job_has_no_score() {
    let (_dir, home) = fresh_home();
    let job = paste(&home, "Dev", &[], "Write code.");
    ok(&home, &["job", "analyze"]);
    let shown = stdout_of(&run(&home, &["job", "show", job["id"].as_str().unwrap()]));
    assert!(
        shown.contains("Score: -- (No score: nothing in your profile to compare)"),
        "{shown}"
    );
}

#[test]
fn job_commands_refuse_while_paused_where_they_write() {
    let (_dir, home) = fresh_home();
    let job = paste(&home, "Dev", &[], "Write code.");
    let id = job["id"].as_str().unwrap();
    ok(&home, &["job", "src", "add", "lever", "acme"]);
    ok(&home, &["pause", "set"]);
    let attempts: Vec<(Vec<&str>, &str)> = vec![
        (vec!["job", "fetch"], ""),
        (
            vec![
                "job",
                "import",
                "--url",
                "https://example.com/a",
                "--title",
                "T",
            ],
            "Body.",
        ),
        (vec!["job", "analyze", id], ""),
        (vec!["job", "analyze"], ""),
    ];
    for (args, input) in attempts {
        let output = run_stdin(&home, &args, input);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            stderr_text(&output).starts_with("error[paused]"),
            "{args:?}"
        );
    }
    let nothing = stdout_json(&run(&home, &["job", "list", "--json"]));
    assert_eq!(nothing.as_array().unwrap().len(), 1);
}

#[test]
fn job_list_and_show_work_while_paused() {
    let (_dir, home) = fresh_home();
    let job = paste(&home, "Dev", &[], "Write code.");
    let id = job["id"].as_str().unwrap();
    ok(&home, &["pause", "set"]);
    assert!(stdout_of(&run(&home, &["job", "list"])).contains(id));
    assert!(stdout_of(&run(&home, &["job", "show", id])).contains("Write code."));
    ok(&home, &["job", "sources"]);
    ok(&home, &["job", "prefs", "show"]);
    ok(&home, &["job", "search", "code"]);
}

#[test]
fn job_prefs_set_then_show_keeps_the_other_value() {
    let (_dir, home) = fresh_home();
    ok(
        &home,
        &["job", "prefs", "set", "--min-pay", "90000:eur:year"],
    );
    let both = stdout_json(&run(
        &home,
        &["job", "prefs", "set", "--seniority", "Senior", "--json"],
    ));
    assert_eq!(both["min_pay"]["amount"], 90000);
    assert_eq!(both["min_pay"]["currency"], "EUR");
    assert_eq!(both["seniority"], "senior");
    let text = stdout_of(&run(&home, &["job", "prefs", "show"]));
    assert!(text.contains("90000 EUR per year"), "{text}");
    let bad = run(&home, &["job", "prefs", "set", "--seniority", "boss"]);
    assert!(stderr_text(&bad).starts_with("error[validation]"));
}

#[tokio::test]
async fn a_cut_job_list_says_how_many_are_shown() {
    use jobpilot_core::jobs::{PastedJob, import_pasted};
    let (_dir, home) = fresh_home();
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    for n in 0..201 {
        let job = PastedJob {
            title: format!("Role {n}"),
            company: format!("Company {n}"),
            url: None,
            text: format!("Work number {n}."),
        };
        import_pasted(&db, job).await.unwrap();
    }
    db.close().await;
    let said = "200 of 201 shown. Narrow with --country, --mode, or --min-pay.";
    let text = stdout_of(&run(&home, &["job", "list"]));
    assert!(text.contains(said), "{text}");
    let json = stdout_json(&run(&home, &["job", "list", "--json"]));
    assert_eq!(
        json.as_array().unwrap().len(),
        200,
        "the shape stays a list"
    );
    let search = stdout_of(&run(&home, &["job", "search", "Work"]));
    assert!(
        search.contains("200 of 201 shown. Narrow the query."),
        "{search}"
    );
    let few = stdout_of(&run(&home, &["job", "search", "199"]));
    assert!(!few.contains(" shown."), "{few}");
}

// ---- Claude Code apply runs ----

/// A saved application with an uploaded resume and letter, and the consent a run needs.
async fn apply_ready_home(home: &Path) -> String {
    use jobpilot_core::ai::claude_code::PROVIDER;
    use jobpilot_core::ai::{DataCategory, consent};
    use jobpilot_core::applications::{self, ApplicationInput};
    use jobpilot_core::drafts;
    use jobpilot_core::render::{Entry, ResumeDoc, Section};
    let db = Db::open(&AppPaths::new(home)).await.unwrap();
    let doc = |title: &str| ResumeDoc {
        name: "Ada Lovelace".into(),
        contact: vec!["ada@example.com".into()],
        summary: Vec::new(),
        sections: vec![Section {
            heading: "Experience".into(),
            entries: vec![Entry {
                bullets: vec![title.into()],
                ..Entry::default()
            }],
        }],
    };
    let mut drafts_made = Vec::new();
    for kind in ["resume", "cover_letter"] {
        let title = format!("Acme {kind}");
        let made = drafts::create_version(
            &db,
            kind,
            None,
            "uploaded",
            &title,
            None,
            None,
            doc(&title),
            Vec::new(),
        )
        .await
        .unwrap();
        drafts_made.push(made.id);
    }
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        link: "https://boards.greenhouse.io/acme/jobs/1".into(),
        resume_draft_id: Some(drafts_made[0].clone()),
        letter_draft_id: Some(drafts_made[1].clone()),
        ..ApplicationInput::default()
    };
    let app = applications::add(&db, "cli", input).await.unwrap().id;
    let categories = [
        DataCategory::JobText,
        DataCategory::ProfileBasic,
        DataCategory::CareerMemory,
    ];
    consent::give(&db, PROVIDER, &categories, "2026-11-01", "cli")
        .await
        .unwrap();
    db.close().await;
    app
}

fn node_bin() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .flat_map(|dir| ["node", "node.exe"].map(|name| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("node is needed for this test")
}

/// The scripted fake `claude` behind a wrapper with its variables, and the fake Playwright
/// server. Returns the wrapper, the fake's record file, and the Playwright entry.
fn fake_apply_claude(dir: &Path, steps: &str) -> (PathBuf, PathBuf, PathBuf) {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests");
    let snapshot = dir.join("snapshot.yml");
    std::fs::copy(core.join("fixtures/forms/snapshots/form.yml"), &snapshot).unwrap();
    let scenario = dir.join("scenario.json");
    std::fs::write(&scenario, steps).unwrap();
    let record = dir.join("record.jsonl");
    let server = core.join("mock-mcp/server.mjs");
    let url = format!("file://{}", server.display()).replace('\\', "/");
    let script = format!(
        "process.env.MOCK_SNAPSHOT = {};\nawait import({});\n",
        serde_json::json!(snapshot.display().to_string()),
        serde_json::json!(url),
    );
    let playwright = dir.join("playwright.mjs");
    std::fs::write(&playwright, script).unwrap();
    let vars = [
        ("JOBPILOT_FAKE_SCENARIO", scenario.display().to_string()),
        ("JOBPILOT_FAKE_RECORD", record.display().to_string()),
        (
            "JOBPILOT_FAKE_REPLIES",
            dir.join("replies.jsonl").display().to_string(),
        ),
        ("JOBPILOT_PLAYWRIGHT_CLI", playwright.display().to_string()),
        ("JOBPILOT_NODE_BIN", node_bin().display().to_string()),
        ("JOBPILOT_SECRET_SERVICE", "jobpilot-test".to_string()),
    ];
    #[cfg(unix)]
    let bin = {
        use std::os::unix::fs::PermissionsExt;
        let mut text = String::from("#!/bin/sh\n");
        for (name, value) in &vars {
            text.push_str(&format!("export {name}='{value}'\n"));
        }
        text.push_str(&format!(
            "exec '{}' '{}' \"$@\"\n",
            node_bin().display(),
            core.join("mock-claude-apply/claude").display()
        ));
        let bin = dir.join("claude");
        std::fs::write(&bin, text).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    };
    #[cfg(windows)]
    let bin = {
        let mut text = String::from("@echo off\r\n");
        for (name, value) in &vars {
            text.push_str(&format!("set \"{name}={value}\"\r\n"));
        }
        text.push_str(&format!(
            "call \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
            core.join("mock-claude-apply/claude.cmd").display()
        ));
        let bin = dir.join("claude.cmd");
        std::fs::write(&bin, text).unwrap();
        bin
    };
    (bin, record, playwright)
}

#[tokio::test]
async fn apply_start_runs_a_scripted_session_from_an_ordinary_terminal() {
    let (dir, home) = fresh_home();
    let app = apply_ready_home(&home).await;
    let steps = r#"[{"tool": "apply_next"}, {"tool": "browser_snapshot"},
        {"tool": "browser_click", "args": {"target": "e14"}},
        {"tool": "apply_result", "args": {"outcome": "submitted"}},
        {"tool": "apply_next"}]"#;
    let (bin, record, playwright) = fake_apply_claude(dir.path(), steps);
    let bin = bin.display().to_string();
    let playwright = playwright.display().to_string();
    let env = [
        ("JOBPILOT_CLAUDE_BIN", bin.as_str()),
        ("JOBPILOT_PLAYWRIGHT_CLI", playwright.as_str()),
    ];

    let output = run_env(&home, &["apply", "start", &app], &env);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(text.contains("browser runtime: ready"), "{text}");
    assert!(
        text.contains("1. boards.greenhouse.io: submitted"),
        "{text}"
    );
    assert!(text.contains(": ended"), "{text}");
    let shown = stdout_json(&run(&home, &["app", "show", &app, "--json"]));
    assert_eq!(shown["status"], "applied", "{shown}");
    let status = stdout_json(&run(&home, &["apply", "status", "--json"]));
    assert_eq!(status["state"], "ended");
    let runs = home.join("apply");
    let left: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["session"], "only the session folder stays");
    let recorded = std::fs::read_to_string(record).unwrap();
    assert!(recorded.contains("\"purge\""), "{recorded}");
}

#[tokio::test]
async fn apply_resolve_ends_a_pause_from_the_cli() {
    let (_dir, home) = fresh_home();
    let app = apply_ready_home(&home).await;
    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let run_id = jobpilot_core::apply::start(&db, "cli", std::slice::from_ref(&app), false)
        .await
        .unwrap();
    let view = jobpilot_core::apply::view(&db, &run_id).await.unwrap();
    let pause: String = sqlx::query_scalar(
        "INSERT INTO apply_pauses (id, item_id, kind, origin, excerpt, created_at) \
         VALUES (lower(hex(randomblob(16))), ?, 'login', 'page', 'Sign in', \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) RETURNING id",
    )
    .bind(&view.items[0].id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    db.close().await;

    let shown = stdout_of(&run(&home, &["apply", "show", &run_id]));
    assert!(shown.contains(&format!("pause {pause} (login)")), "{shown}");
    ok(&home, &["apply", "resolve", &pause, "continue"]);
    let again = run(&home, &["apply", "resolve", &pause, "take-over"]);
    assert!(
        stderr_text(&again).starts_with("error[validation]"),
        "{}",
        stderr_text(&again)
    );
    let status = stdout_json(&run(&home, &["apply", "show", &run_id, "--json"]));
    assert!(status["active_pause"].is_null(), "{status}");

    let db = Db::open(&AppPaths::new(&home)).await.unwrap();
    let resolution: String = sqlx::query_scalar("SELECT resolution FROM apply_pauses WHERE id = ?")
        .bind(&pause)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    db.close().await;
    assert_eq!(resolution, "continue");
    ok(&home, &["apply", "cancel", &run_id]);
    let status = stdout_json(&run(&home, &["apply", "status", "--json"]));
    assert_eq!(status["state"], "ended");
}

#[test]
fn apply_start_session_and_mcp_are_refused_in_the_panel() {
    let (_dir, home) = fresh_home();
    let panel = [("JOBPILOT_PANEL", "1")];
    for args in [
        vec!["apply", "start", "x"],
        vec!["apply", "start", "x", "--release-sealed"],
        vec!["apply", "session", "x"],
        vec!["apply-mcp", "--dir", "/x/apply/y"],
    ] {
        let refused = run_env(&home, &args, &panel);
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert!(
            stderr_text(&refused).starts_with("error[panel_refused]"),
            "{args:?}: {}",
            stderr_text(&refused)
        );
        assert!(refused.stdout.is_empty(), "{args:?}");
    }
    let allowed = run_env(&home, &["apply", "status"], &panel);
    assert!(allowed.status.success(), "{}", stderr_text(&allowed));
}

/// A closed terminal or a quitting app sends the host SIGHUP. It winds up as a Ctrl-C does:
/// the run ends, its folder goes, and so does every process the session started.
#[cfg(unix)]
#[tokio::test]
async fn apply_start_winds_up_on_sighup() {
    use std::time::{Duration, Instant};
    let (dir, home) = fresh_home();
    let app = apply_ready_home(&home).await;
    let steps = r#"[{"tool": "apply_next"}, {"tool": "browser_snapshot"}]"#;
    let (bin, record, playwright) = fake_apply_claude(dir.path(), steps);
    let mut host = Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(["apply", "start", &app])
        .env("JOBPILOT_HOME", &home)
        .env("JOBPILOT_SECRET_SERVICE", "jobpilot-test")
        .env("JOBPILOT_CLAUDE_BIN", &bin)
        .env("JOBPILOT_PLAYWRIGHT_CLI", &playwright)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let replies = dir.path().join("replies.jsonl");
    let begun = Instant::now();
    while !std::fs::read_to_string(&replies)
        .unwrap_or_default()
        .contains("\"browser_snapshot\"")
    {
        assert!(begun.elapsed() < Duration::from_secs(60), "a snapshot");
        std::thread::sleep(Duration::from_millis(50));
    }
    let session: serde_json::Value = std::fs::read_to_string(&record)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|entry| {
            entry["argv"]
                .as_array()
                .is_some_and(|a| a.contains(&"--mcp-config".into()))
        })
        .unwrap();
    let claude = session["pid"].as_u64().unwrap().to_string();

    // Only the host this test started is signalled.
    let sent = Command::new("kill")
        .args(["-HUP", &host.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let begun = Instant::now();
    while host.try_wait().unwrap().is_none() {
        assert!(begun.elapsed() < Duration::from_secs(30), "the host ends");
        std::thread::sleep(Duration::from_millis(50));
    }

    let status = stdout_json(&run(&home, &["apply", "status", "--json"]));
    assert_eq!(status["state"], "ended", "{status}");
    assert_eq!(status["items"][0]["state"], "skipped", "{status}");
    let left: Vec<String> = std::fs::read_dir(home.join("apply"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["session"], "only the session folder stays");
    let probe = Command::new("kill").args(["-0", &claude]).status().unwrap();
    assert!(!probe.success(), "claude is gone");
}

#[tokio::test]
async fn apply_material_and_record_are_manual_mode_from_the_cli() {
    let (_dir, home) = fresh_home();
    let app = apply_ready_home(&home).await;

    let text = stdout_of(&run(&home, &["apply", "material", &app]));
    assert!(text.contains("manual: available."), "{text}");
    assert!(
        text.contains("copilot: available. No clause found for this site. You review and submit."),
        "{text}"
    );
    assert!(text.contains("resume: "), "{text}");
    assert!(
        text.contains("work authorization: not stated by you (no country)"),
        "{text}"
    );
    let json = stdout_json(&run(&home, &["apply", "material", &app, "--json"]));
    assert_eq!(json["modes"].as_array().unwrap().len(), 4);
    assert!(json["resume"].as_str().unwrap().ends_with(".pdf"));

    let saved = stdout_json(&run(
        &home,
        &[
            "apply",
            "record",
            &app,
            "--outcome",
            "applied",
            "--on",
            "2026-11-02",
            "--json",
        ],
    ));
    assert_eq!(saved["status"], "applied");
    assert_eq!(saved["applied_on"], "2026-11-02");
    let runs = stdout_json(&run(&home, &["apply", "status", "--json"]));
    assert!(runs.is_null(), "Manual mode starts no run: {runs}");
}
