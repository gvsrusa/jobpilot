//! The MCP client and server helpers, and the sweep. Only a fake node server runs: no
//! browser, no real `claude`, no network.

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jobpilot_core::Error;
use jobpilot_core::ai::CancellationToken;
use jobpilot_core::ai::child::system_env;
use jobpilot_core::browser::mcp::{
    Incoming, McpClient, error_reply, read_message, reply, tool_text,
};
use jobpilot_core::browser::sweep::{kill_marked, windows_fold};
use serde_json::{Value, json};

fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for name in ["node", "node.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!("node is needed for these tests");
}

fn server() -> OsString {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/mock-mcp/server.mjs")
        .into_os_string()
}

/// `PATH` and, on Windows, the system names node needs to start, as the product's
/// `child_env` carries them; then `extra`.
fn env_with(extra: &[(&str, String)]) -> Vec<(String, String)> {
    let mut env = vec![(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    )];
    for name in system_env(cfg!(windows)) {
        if let Ok(value) = std::env::var(name) {
            env.push((name.to_string(), value));
        }
    }
    for (name, value) in extra {
        env.push((name.to_string(), value.clone()));
    }
    env
}

fn env_of(extra: &[(&str, &Path)]) -> Vec<(String, String)> {
    let extra: Vec<(&str, String)> = extra
        .iter()
        .map(|(name, value)| (*name, value.display().to_string()))
        .collect();
    env_with(&extra)
}

async fn start(extra: &[(&str, &Path)], cwd: &Path) -> McpClient {
    McpClient::start(&node(), &[server()], &env_of(extra), cwd)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_client_lists_and_calls_tools_on_a_server() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot = tmp.path().join("snapshot.txt");
    let calls = tmp.path().join("calls.jsonl");
    fs::write(&snapshot, "- heading \"Apply\"").unwrap();
    let mut client = start(
        &[("MOCK_SNAPSHOT", &snapshot), ("MOCK_CALLS", &calls)],
        tmp.path(),
    )
    .await;
    let tools = client.tools().await.unwrap();
    assert_eq!(tools.len(), 25);
    assert!(tools.iter().any(|tool| tool["name"] == "browser_snapshot"));

    let cancel = CancellationToken::new();
    let seen = client
        .call("browser_snapshot", json!({}), &cancel)
        .await
        .unwrap();
    assert_eq!(seen.text, "- heading \"Apply\"");
    assert!(!seen.is_error);
    let failed = client.call("fail", json!({"a": 1}), &cancel).await.unwrap();
    assert!(failed.is_error);
    assert_eq!(failed.raw["isError"], true);

    let log = fs::read_to_string(&calls).unwrap();
    let names: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        // The server also logs its own start.
        .filter(|entry| entry.get("started").is_none())
        .map(|entry| entry["name"].clone())
        .collect();
    assert_eq!(names, ["browser_snapshot", "fail"]);
    client.close().await;
}

#[tokio::test]
async fn a_server_that_exits_is_browser_gone_not_a_hang() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = McpClient::start(
        &node(),
        &[server()],
        &env_with(&[("MOCK_EXIT_AFTER", "1".into())]),
        tmp.path(),
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    client.call("one", json!({}), &cancel).await.unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        client.call("two", json!({}), &cancel),
    )
    .await
    .expect("must not hang");
    assert!(matches!(outcome, Err(Error::Failed(ref why)) if why == "browser_gone"));
    client.close().await;
}

#[tokio::test]
async fn a_cancel_returns_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = start(&[], tmp.path()).await;
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        later.cancel();
    });
    let begun = Instant::now();
    // The fake never answers a call named `slow`.
    let outcome = client.call("slow", json!({}), &cancel).await;
    assert!(matches!(outcome, Err(Error::Cancelled)));
    assert!(begun.elapsed() < Duration::from_secs(3));
    client.close().await;
}

#[tokio::test]
async fn a_line_over_the_cap_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = McpClient::start(
        &node(),
        &[server()],
        &env_with(&[("MOCK_BIG", "1".into())]),
        tmp.path(),
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let outcome = client.call("browser_snapshot", json!({}), &cancel).await;
    assert!(
        matches!(outcome, Err(Error::TooLarge { .. })),
        "{outcome:?}"
    );
    client.close().await;
}

#[test]
fn read_message_sorts_the_four_methods_and_keeps_string_and_number_ids() {
    let line = r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}"#;
    assert!(matches!(read_message(line).unwrap(),
        Incoming::Initialize { id } if id == json!(7)));
    let line = r#"{"jsonrpc":"2.0","id":"a-1","method":"tools/list"}"#;
    assert!(matches!(read_message(line).unwrap(),
        Incoming::ToolsList { id } if id == json!("a-1")));
    let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"x","arguments":{"k":1}}}"#;
    match read_message(line).unwrap() {
        Incoming::ToolCall { id, name, args } => {
            assert_eq!(id, json!(3));
            assert_eq!(name, "x");
            assert_eq!(args, json!({"k": 1}));
        }
        other => panic!("{other:?}"),
    }
    let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    assert!(matches!(
        read_message(line).unwrap(),
        Incoming::Notification
    ));
    let line = r#"{"jsonrpc":"2.0","id":"z","method":"ping"}"#;
    assert!(matches!(read_message(line).unwrap(),
        Incoming::Other { id } if id == json!("z")));
    assert!(read_message("not json").is_err());

    let ok: Value = serde_json::from_str(&reply(&json!("a-1"), json!({"fine": true}))).unwrap();
    assert_eq!(ok["id"], "a-1");
    assert_eq!(ok["result"]["fine"], true);
    let bad: Value = serde_json::from_str(&error_reply(&json!(2), -32601, "nope")).unwrap();
    assert_eq!(bad["error"]["code"], -32601);
    assert_eq!(bad["error"]["message"], "nope");
    let text = tool_text("hi", true);
    assert_eq!(text["content"][0]["text"], "hi");
    assert_eq!(text["isError"], true);
    assert!(!reply(&json!(1), json!({})).contains('\n'));
}

/// A node process that ignores SIGTERM and says when its handler is in place.
fn stubborn(marker: &str) -> std::process::Child {
    let script =
        "process.on('SIGTERM', () => {}); console.log('ready'); setInterval(() => {}, 1000);";
    let mut child = Command::new(node())
        .arg("-e")
        .arg(script)
        .arg(marker)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "ready");
    child
}

fn gone_within(child: &mut std::process::Child, wait: Duration) -> bool {
    let begun = Instant::now();
    while begun.elapsed() < wait {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn sweep_kills_a_process_that_ignores_sigterm() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run-sweep-marker-a1b2c3");
    let mut marked = stubborn(&run.display().to_string());
    let mut other = stubborn("some-other-run-folder-d4e5f6");

    let killed = kill_marked(&run);
    assert_eq!(killed, 1);
    assert!(gone_within(&mut marked, Duration::from_secs(2)));
    assert!(!gone_within(&mut other, Duration::from_millis(300)));
    let _ = other.kill();
    let _ = other.wait();
}

/// Chromium can start a helper on its way out, after a sweep has read the process table: the
/// sweep looks again until a pass finds nothing new. Here an unmarked watcher stands in for
/// Chromium: it starts a marked process when its marked child dies. The marker reaches it
/// through the environment, so the watcher itself is not swept.
#[test]
fn sweep_kills_a_process_started_by_one_it_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run-sweep-late-c7d8e9");
    let marker = run.display().to_string();
    let script = r#"
        const { spawn } = require("node:child_process");
        const marker = process.env.SWEEP_MARKER;
        const keep = "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000);";
        const first = spawn(process.execPath, ["-e", keep, marker], { stdio: "ignore" });
        first.on("spawn", () => console.log("ready"));
        first.on("exit", () => {
          const late = spawn(process.execPath, ["-e", keep, marker], {
            stdio: "ignore",
            detached: true,
          });
          late.unref();
          console.log("late " + late.pid);
        });
    "#;
    let mut watcher = Command::new(node())
        .arg("-e")
        .arg(script)
        .env("SWEEP_MARKER", &marker)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(watcher.stdout.take().unwrap()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "ready");
    // The child's command line is in place once node has started it.
    std::thread::sleep(Duration::from_millis(200));

    let killed = kill_marked(&run);
    let late: u32 = lines
        .next()
        .unwrap()
        .unwrap()
        .strip_prefix("late ")
        .unwrap()
        .parse()
        .unwrap();
    let begun = Instant::now();
    let mut alive = still_running(late);
    while alive && begun.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(50));
        alive = still_running(late);
    }
    if alive {
        // Only the process this test started.
        let mut system = sysinfo::System::new().unwrap();
        let pid = sysinfo::Pid::from_u32(late);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        if let Some(process) = system.process(pid) {
            process.kill();
        }
    }
    let _ = watcher.wait();
    assert!(!alive, "the late process outlived the sweep");
    assert_eq!(killed, 2);
}

fn still_running(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessStatus, ProcessesToUpdate, System};
    let mut system = System::new().unwrap();
    let pid = Pid::from_u32(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .is_some_and(|process| process.status() != ProcessStatus::Zombie)
}

#[test]
fn sweep_never_kills_its_caller() {
    let tmp = tempfile::tempdir().unwrap();
    // Our own argv holds the test binary's path, so this marker matches the caller.
    let own = std::env::current_exe().unwrap();
    assert_eq!(kill_marked(&own), 0);

    // An empty or short marker would match everything, so it kills nothing.
    let mut bystander = stubborn("bystander-run-folder-0a9b8c");
    for marker in ["", "/", "a", "node", "/tmp"] {
        assert_eq!(kill_marked(Path::new(marker)), 0, "marker {marker:?}");
    }
    assert!(!gone_within(&mut bystander, Duration::from_millis(300)));
    let _ = bystander.kill();
    let _ = bystander.wait();
    drop(tmp);
}

#[test]
fn sweep_matches_a_whole_folder_and_refuses_a_shallow_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let apply = tmp.path().join("apply");
    let mut sibling = stubborn(&apply.join("abcd").display().to_string());
    let mut inside = stubborn(&format!("out={}/shots", apply.join("abc").display()));

    assert_eq!(kill_marked(&apply.join("abc")), 1);
    assert!(gone_within(&mut inside, Duration::from_secs(2)));
    assert!(!gone_within(&mut sibling, Duration::from_millis(300)));

    assert_eq!(kill_marked(Path::new("/Users/x")), 0);
    assert_eq!(kill_marked(Path::new("relative/dir/with/parts")), 0);
    assert!(!gone_within(&mut sibling, Duration::from_millis(300)));
    let _ = sibling.kill();
    let _ = sibling.wait();
}

#[test]
fn windows_paths_fold_to_one_form() {
    assert_eq!(windows_fold(r"C:\Users\Me\Run-A"), "c:/users/me/run-a");
    assert_eq!(windows_fold("c:/USERS/me/run-a"), "c:/users/me/run-a");
}

#[tokio::test]
async fn the_client_answers_a_ping_and_skips_noise_and_a_stray_string_id() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.jsonl");
    let mut client = McpClient::start(
        &node(),
        &[server()],
        &env_with(&[
            ("MOCK_NOISE", "1".into()),
            ("MOCK_CALLS", calls.display().to_string()),
        ]),
        tmp.path(),
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let seen = client
        .call("browser_tabs", json!({}), &cancel)
        .await
        .unwrap();
    assert_eq!(seen.text, "ok browser_tabs");
    // The next call gives the server time to log the pong it was sent.
    client
        .call("browser_tabs", json!({}), &cancel)
        .await
        .unwrap();
    let log = fs::read_to_string(&calls).unwrap();
    assert!(log.contains(r#"{"pong":"p-1"}"#), "{log}");
    client.close().await;
}
