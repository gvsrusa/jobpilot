use std::path::Path;
use std::process::{Command, Output};

use jobpilot_core::{AppPaths, Db, app_schema_version, documents};

/// Its own keychain service: on macOS an entry made by one binary makes another binary
/// wait on a system prompt, and these tests open a root from both.
fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jobpilot"))
        .args(args)
        .env("JOBPILOT_HOME", home)
        .env("JOBPILOT_SECRET_SERVICE", "jobpilot-test-cli")
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
