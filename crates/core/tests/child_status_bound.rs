//! One test in this binary, so its `set_var` runs with no other test thread. Scripts stand in
//! for the CLIs; each records its own starts in a file, because the child's environment is
//! cleared and a count has to be baked in.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jobpilot_core::ai;
use jobpilot_core::ai::child::set_cli_path;
use jobpilot_core::db::Db;
use jobpilot_core::paths::AppPaths;

fn script(path: &Path, body: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

fn set(name: &str, value: &std::ffi::OsStr) {
    // SAFETY: the only test in this binary, and it spawns no thread of its own.
    unsafe { std::env::set_var(name, value) };
}

fn lines(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

async fn rows(db: &Db) -> (Vec<(bool, Option<String>, Option<String>)>, Duration) {
    let started = Instant::now();
    let status = ai::status(db).await.unwrap();
    let rows = ["gemini_cli", "codex_cli"]
        .map(|name| status.external.iter().find(|r| r.name == name).unwrap())
        .map(|r| (r.available, r.reason.clone(), r.path.clone()));
    (rows.to_vec(), started.elapsed())
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn status_is_bounded_spawns_once_and_gives_a_probe_its_programs_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let db = Db::open(&AppPaths::new(root.join("data"))).await.unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    // A refused port: the default address can sit for the probe's whole 3 s.
    jobpilot_core::settings::set(
        &db,
        "ai.ollama_url",
        &serde_json::json!("http://127.0.0.1:1"),
    )
    .await
    .unwrap();
    // A GUI launch: nothing useful on PATH. Claude is made absent so no login shell runs for it.
    set("PATH", root.join("empty").as_os_str());
    set("HOME", root.join("home").as_os_str());
    set("USERPROFILE", root.join("home").as_os_str());
    set("JOBPILOT_CLAUDE_BIN", root.join("claude").as_os_str());
    // SAFETY: as in `set`.
    unsafe {
        std::env::remove_var("JOBPILOT_GEMINI_BIN");
        std::env::remove_var("JOBPILOT_CODEX_BIN");
        for manager in ["NVM_DIR", "FNM_DIR", "FNM_MULTISHELL_PATH", "VOLTA_HOME"] {
            std::env::remove_var(manager);
        }
    }

    // Off and not found: the login shell is never asked, and the row says so quickly.
    let shell_runs = root.join("shell-runs");
    let shell = script(
        &root.join("shell"),
        &format!("echo x >> '{}'; /bin/sleep 30", shell_runs.display()),
    );
    set("SHELL", shell.as_os_str());
    // The first status pays a one-time start-up cost that is not the CLIs'.
    rows(&db).await;
    let (both, took) = rows(&db).await;
    assert_eq!(both[0].1.as_deref(), Some("not_installed"));
    assert_eq!(both[1].1.as_deref(), Some("not_installed"));
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert_eq!(
        lines(&shell_runs),
        0,
        "an off CLI never reaches the login shell"
    );

    // Two enabled programs that never answer: probed together, killed with their children,
    // started once each, and the screen waits for one timeout, not two.
    let mut pids = Vec::new();
    let mut counts = Vec::new();
    for (name, provider) in [("gemini", "gemini_cli"), ("codex", "codex_cli")] {
        let count = root.join(format!("{name}-runs"));
        let pidfile = root.join(format!("{name}-pid"));
        let bin = script(
            &root.join("hang").join(name).join(name),
            &format!(
                "echo x >> '{}'; /bin/sleep 60 & echo $! > '{}'; wait",
                count.display(),
                pidfile.display()
            ),
        );
        set_cli_path(&db, provider, bin.to_str().unwrap())
            .await
            .unwrap();
        ai::set_enabled(&db, provider, true).await.unwrap();
        counts.push(count);
        pids.push(pidfile);
    }
    let (both, took) = rows(&db).await;
    for row in &both {
        assert_eq!(row.1.as_deref(), Some("no_answer"), "{row:?}");
    }
    assert!(
        took >= Duration::from_secs(9),
        "the 10 s timeout applies: {took:?}"
    );
    assert!(
        took < Duration::from_secs(14),
        "probes run together: {took:?}"
    );
    for count in &counts {
        assert_eq!(lines(count), 1, "{}", count.display());
    }
    for pidfile in &pids {
        let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
        for _ in 0..30 {
            if !alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!alive(pid), "the probe's grandchild {pid} outlived it");
    }

    // A program that needs the `node` beside it, as a GUI launch has no other one.
    let folder = root.join("npm-bin");
    script(&folder.join("node"), "echo v22.0.0");
    let codex = script(&folder.join("codex"), "node --version");
    set_cli_path(&db, "codex_cli", codex.to_str().unwrap())
        .await
        .unwrap();
    let (both, _) = rows(&db).await;
    assert!(both[1].0, "{:?}", both[1]);
}
