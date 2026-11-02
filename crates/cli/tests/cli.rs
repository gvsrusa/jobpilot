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
        "doc": {
            "name": "x", "contact": [], "summary": [],
            "sections": [{"heading": "Experience", "entries": [{
                "title": "", "subtitle": "", "dates": "",
                "bullets": ["Shipped the Rust compiler"],
            }]}],
        },
        "claims": [{"text": "Shipped the Rust compiler", "evidence_ids": [item["id"]]}],
    });
    (job["id"].as_str().unwrap().to_owned(), reply)
}

const DRAFT_CONSENT: &str = "job_text,resume_text,profile_basic";

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
            "This will make up to 3 Claude calls, limit 20. \
             Run `jobpilot memory refresh --yes` to go ahead."
        ),
        "{text}"
    );
    let json = stdout_json(&run_env(&home, &["memory", "refresh", "--json"], &env));
    assert_eq!(json["claude_calls"], 3);
    let runs = stdout_json(&run(&home, &["agents", "list", "--json"]));
    assert!(runs.as_array().unwrap().is_empty(), "no run started");

    let done = stdout_json(&run_env(
        &home,
        &["memory", "refresh", "--yes", "--json"],
        &env,
    ));
    assert_eq!(done["classified"]["checked"], 3);
    assert_eq!(done["classified"]["raised"], 3);
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
