use std::path::Path;
use std::process::{Command, Output};

use jobpilot_core::{AppPaths, Db, app_schema_version, documents};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
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
    assert_eq!(job["title"], "title");
    assert!(job["normalized"].as_str().unwrap().contains("Build things"));
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
