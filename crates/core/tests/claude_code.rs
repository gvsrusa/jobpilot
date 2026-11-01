//! Claude Code adapter, against a mock CLI. Only `real_claude_has_no_tools_smoke` calls the
//! real one, and only when asked twice: `--ignored` and `JOBPILOT_REAL_CLAUDE_SMOKE=1`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jobpilot_core::ai::claude_code::{ClaudeCode, PROVIDER, TERMS_DATE};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::validate::MAX_OUTPUT_BYTES;
use jobpilot_core::ai::{
    Availability, CancellationToken, Capability, DataCategory, Outcome, Prompt, Provider,
    ProviderId, TaskType, consent, spec,
};
use jobpilot_core::{AppPaths, Db, Error, settings};
use serde_json::{Value, json};
use tempfile::TempDir;

const SENTINEL: &str = "ZXQ-POSTING-SENTINEL";
const SCHEMA: &str = r#"{"properties":{"ok":{"type":"boolean"}},"type":"object"}"#;
const JOB: &str = r#"{"title":"Engineer","company":"Acme","location":"Oslo","remote":"hybrid",
    "skills":["Rust"],"seniority":"senior","compensation":""}"#;

/// Per attempt. The router tries twice, so `WITHIN` allows for both.
#[cfg(unix)]
const LIMIT: Duration = Duration::from_millis(200);
#[cfg(windows)]
const LIMIT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const WITHIN: Duration = Duration::from_secs(1);
#[cfg(windows)]
const WITHIN: Duration = Duration::from_secs(6);

/// Longer than `LIMIT`, so no attempt can finish before it is killed.
#[cfg(unix)]
const SLEEP: &str = "2";
#[cfg(windows)]
const SLEEP: &str = "8";
#[cfg(unix)]
const OUTLIVE: Duration = Duration::from_secs(3);
#[cfg(windows)]
const OUTLIVE: Duration = Duration::from_secs(9);

/// A wrapper script in a temp dir with the values inlined, because `child_env` strips every
/// `MOCK_CLAUDE_*`. It stays a process of its own, so the mock is a grandchild of the app.
struct MockCli {
    dir: TempDir,
    bin: PathBuf,
}

impl MockCli {
    fn new(vars: &[(&str, &str)]) -> MockCli {
        let dir = tempfile::tempdir().unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-claude");
        let mut all: Vec<(String, String)> = ["PID", "ARGS", "STDIN", "ENV", "CWD", "DONE"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name.to_lowercase());
                (format!("MOCK_CLAUDE_{name}"), path.display().to_string())
            })
            .collect();
        for (name, value) in vars {
            all.push(((*name).to_string(), (*value).to_string()));
        }
        let bin = write_wrapper(dir.path(), &mock, &all);
        MockCli { dir, bin }
    }

    fn with_reply(reply: &str, vars: &[(&str, &str)]) -> MockCli {
        let holder = tempfile::tempdir().unwrap().keep();
        let path = holder.join("reply.json");
        fs::write(&path, reply).unwrap();
        let path = path.display().to_string();
        let mut all = vec![("MOCK_CLAUDE_REPLY", path.as_str())];
        all.extend_from_slice(vars);
        MockCli::new(&all)
    }

    fn claude(&self) -> ClaudeCode {
        ClaudeCode::with_binary(self.bin.clone(), None)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.file(name)).unwrap_or_default()
    }
}

#[cfg(unix)]
fn write_wrapper(dir: &Path, mock: &Path, vars: &[(String, String)]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mut script = String::from("#!/bin/sh\n");
    for (name, value) in vars {
        script.push_str(&format!("export {name}='{value}'\n"));
    }
    script.push_str(&format!("'{}' \"$@\"\n", mock.join("claude").display()));
    let path = dir.join("claude-wrapper");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(windows)]
fn write_wrapper(dir: &Path, mock: &Path, vars: &[(String, String)]) -> PathBuf {
    let mut script = String::from("@echo off\r\n");
    for (name, value) in vars {
        script.push_str(&format!("set \"{name}={value}\"\r\n"));
    }
    let target = mock.join("claude.cmd");
    script.push_str(&format!(
        "call \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        target.display()
    ));
    let path = dir.join("claude-wrapper.cmd");
    fs::write(&path, script).unwrap();
    path
}

fn prompt() -> Prompt {
    Prompt {
        system: "system rules".into(),
        user: format!("<<<UNTRUSTED_POSTING>>>{SENTINEL}<<<END_UNTRUSTED_POSTING>>>"),
        schema: SCHEMA,
    }
}

fn job_prompt() -> Prompt {
    Prompt {
        schema: spec(TaskType::ParseJob).schema,
        ..prompt()
    }
}

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    consent::give(&db, PROVIDER, &[DataCategory::JobText], TERMS_DATE, "gui")
        .await
        .unwrap();
    (tmp, db)
}

async fn complete(claude: &ClaudeCode) -> jobpilot_core::Result<String> {
    claude.complete(&prompt(), CancellationToken::new()).await
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

async fn reasons(db: &Db) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT outcome, reason FROM task_runs WHERE provider = ? ORDER BY rowid")
        .bind(PROVIDER)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

/// A zombie still answers `kill -0`, so ask `ps` for the state.
#[cfg(unix)]
fn running(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

/// Every process the mock started must end, not only the one the app spawned directly.
#[cfg(unix)]
async fn assert_pids_gone(cli: &MockCli) {
    let pids = cli.read("pid");
    assert!(!pids.trim().is_empty(), "the mock never started");
    for _ in 0..50 {
        if !pids.lines().any(running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("still running: {pids}");
}

#[cfg(windows)]
async fn assert_pids_gone(_cli: &MockCli) {}

/// The mock writes `done` when its sleep ends, so a survivor shows on every OS.
async fn assert_never_finished(cli: &MockCli, sleep: Duration) {
    tokio::time::sleep(sleep).await;
    assert!(!cli.file("done").exists(), "the mock outlived the kill");
}

#[tokio::test]
async fn argv_disables_every_tool_and_setting_source() {
    let cli = MockCli::new(&[]);
    complete(&cli.claude()).await.unwrap();
    let args = cli.read("args");

    #[cfg(unix)]
    {
        let lines: Vec<&str> = args.lines().collect();
        let after = |flag: &str| {
            let at = lines.iter().position(|arg| *arg == flag).unwrap();
            lines[at + 1]
        };
        assert_eq!(after("--tools"), "");
        assert_eq!(after("--allowedTools"), "StructuredOutput");
        assert_eq!(after("--setting-sources"), "");
        assert_eq!(after("--output-format"), "json");
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert_eq!(after("--json-schema"), SCHEMA);
        for flag in ["-p", "--strict-mcp-config", "--no-session-persistence"] {
            assert!(lines.contains(&flag), "{flag} missing");
        }
    }
    #[cfg(windows)]
    for part in [
        "--tools \"\"",
        "--allowedTools StructuredOutput",
        "--setting-sources \"\"",
        "--strict-mcp-config",
    ] {
        assert!(args.contains(part), "{part} missing from {args}");
    }

    assert!(
        !args.contains("--disallowedTools"),
        "this denies StructuredOutput"
    );
    assert!(!args.contains("--bare"));
    assert!(!args.contains("--dangerously-skip-permissions"));
    assert!(!args.contains("--allow-dangerously-skip-permissions"));
    assert!(!args.contains("bypassPermissions"));
}

#[test]
fn argv_is_exactly_the_verified_list() {
    let claude = ClaudeCode::with_binary("/x/claude".into(), None);
    assert_eq!(
        claude.argv(SCHEMA, "rules"),
        os(&[
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            SCHEMA,
            "--tools",
            "",
            "--allowedTools",
            "StructuredOutput",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
            "--system-prompt",
            "rules",
        ])
    );

    let named = ClaudeCode::with_binary("/x/claude".into(), Some("sonnet".into()));
    let args = named.argv(SCHEMA, "rules");
    assert_eq!(args[args.len() - 2..], os(&["--model", "sonnet"]));
}

/// Windows refuses a line break in a batch-file argument, and cmd caps the line at 8191
/// characters, so the shipped multi-line schema files must reach argv on one line.
#[test]
fn argv_keeps_schema_and_batch_system_prompt_on_one_line() {
    let pretty = "{\n  \"type\": \"object\"\n}\n";
    let claude = ClaudeCode::with_binary("/x/claude".into(), None);
    let args = claude.argv(pretty, "line one\nline two");
    assert_eq!(args[4], "{\"type\":\"object\"}");
    assert_eq!(args.last().unwrap(), "line one\nline two");

    let shim = ClaudeCode::with_binary(r"C:\npm\claude.CMD".into(), None);
    let args = shim.argv(pretty, "line one\r\nline two\nline three");
    assert_eq!(args.last().unwrap(), "line one line two line three");
}

#[test]
fn declaration_is_external_no_tools_and_names_the_binary() {
    let bin = PathBuf::from("/opt/bin/claude");
    let declaration = ClaudeCode::with_binary(bin, None).declaration();
    assert_eq!(declaration.id, ProviderId::ClaudeCode);
    assert_eq!(declaration.id.to_string(), PROVIDER);
    assert_eq!(
        declaration.capabilities,
        [Capability::StructuredOutput, Capability::NoTools]
    );
    assert!(!declaration.local);
    assert_eq!(declaration.timeout, Duration::from_secs(60));
    assert_eq!(
        declaration.sign_in,
        "claude cli on its own login (/opt/bin/claude)"
    );
}

#[tokio::test]
async fn prompt_goes_to_stdin_not_argv() {
    let cli = MockCli::new(&[]);
    complete(&cli.claude()).await.unwrap();
    assert_eq!(cli.read("stdin").trim_end(), prompt().user);
    assert!(!cli.read("args").contains(SENTINEL));
}

#[tokio::test]
async fn working_directory_is_fresh_empty_and_removed() {
    let cli = MockCli::new(&[]);
    complete(&cli.claude()).await.unwrap();
    let listing = cli.read("cwd");
    let mut lines = listing.lines();
    let cwd = PathBuf::from(lines.next().unwrap().trim());
    assert_eq!(lines.next(), None, "working directory was not empty");
    assert!(!cwd.exists(), "working directory was not removed");
}

#[tokio::test]
async fn structured_output_is_returned_and_result_is_fallback() {
    let cli = MockCli::with_reply(
        r#"{"subtype":"success","is_error":false,"result":"ignored","structured_output":{"a":1}}"#,
        &[],
    );
    let text = complete(&cli.claude()).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"a":1})
    );

    for reply in [
        r#"{"subtype":"success","is_error":false,"result":"{\"b\":2}"}"#,
        r#"{"subtype":"success","is_error":false,"result":"{\"b\":2}","structured_output":null}"#,
    ] {
        let cli = MockCli::with_reply(reply, &[]);
        assert_eq!(complete(&cli.claude()).await.unwrap(), r#"{"b":2}"#);
    }
}

#[tokio::test]
async fn error_result_is_refused_not_ok() {
    let cli = MockCli::with_reply(r#"{"is_error":true,"subtype":"error_max_turns"}"#, &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(matches!(&error, Error::Refused(why) if why == "error_max_turns"));

    // `is_error` alone, then `subtype` alone, then neither word at all.
    for (reply, why) in [
        (
            r#"{"is_error":true,"subtype":"success","result":"x"}"#,
            "success",
        ),
        (
            r#"{"is_error":false,"subtype":"error_during_execution"}"#,
            "error_during_execution",
        ),
        (r#"{"result":"x"}"#, "unknown"),
    ] {
        let cli = MockCli::with_reply(reply, &[]);
        let error = complete(&cli.claude()).await.unwrap_err();
        assert!(
            matches!(&error, Error::Refused(got) if got == why),
            "{error}"
        );
    }
}

/// The real failure under `--disallowedTools "*"`: `success`, no error, and free text.
#[tokio::test]
async fn a_permission_denial_is_refused_even_on_success() {
    let denial = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"ok\":true}",
        "permission_denials":[{"tool_name":"StructuredOutput"}]}"#;
    let cli = MockCli::with_reply(denial, &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why == "permission_denied"),
        "{error}"
    );

    // Fails closed on a shape it does not know.
    let odd = r#"{"subtype":"success","is_error":false,"result":"x","permission_denials":"Bash"}"#;
    let cli = MockCli::with_reply(odd, &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(matches!(error, Error::Refused(_)), "{error}");

    let empty = r#"{"subtype":"success","is_error":false,"result":"x","permission_denials":[]}"#;
    let cli = MockCli::with_reply(empty, &[]);
    assert_eq!(complete(&cli.claude()).await.unwrap(), "x");
}

/// The recorded smoke run got an array through the adapter's own argv, with no `--verbose`.
#[tokio::test]
async fn an_event_array_is_read_from_its_last_result_event() {
    let events = r#"[{"type":"system","subtype":"init","tools":["StructuredOutput"]},
        {"type":"result","subtype":"success","structured_output":{"from":"earlier"}},
        {"type":"result","subtype":"success","is_error":false,"structured_output":{"a":1},
         "permission_denials":[]}]"#;
    let cli = MockCli::with_reply(events, &[]);
    assert_eq!(complete(&cli.claude()).await.unwrap(), r#"{"a":1}"#);

    let failed = r#"[{"type":"system","subtype":"init"},
        {"type":"result","subtype":"error_max_turns","is_error":true}]"#;
    let cli = MockCli::with_reply(failed, &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why == "error_max_turns"),
        "{error}"
    );

    let no_result = r#"[{"type":"system","subtype":"init"}]"#;
    let cli = MockCli::with_reply(no_result, &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "bad_reply"),
        "{error}"
    );
}

/// The CLI's real wording, read from its 2.1.316 binary: `Not logged in · Please run /login`.
/// It arrives as a result with `subtype: success` and `is_error: true`.
#[tokio::test]
async fn not_logged_in_is_reported_from_json_and_from_plain_text() {
    let json_reply =
        r#"{"subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#;
    for (reply, exit) in [
        (json_reply, "1"),
        ("Not logged in · Please run /login", "1"),
    ] {
        let cli = MockCli::with_reply(reply, &[("MOCK_CLAUDE_EXIT", exit)]);
        let error = complete(&cli.claude()).await.unwrap_err();
        assert!(
            matches!(&error, Error::Refused(why) if why == "not_logged_in"),
            "{error}"
        );
    }
}

/// A posting can say anything, and a good reply can quote it.
#[tokio::test]
async fn a_good_reply_quoting_the_login_message_is_not_a_login_failure() {
    let cli = MockCli::with_reply(
        r#"{"subtype":"success","is_error":false,"structured_output":{"note":"Not logged in"}}"#,
        &[],
    );
    let text = complete(&cli.claude()).await.unwrap();
    assert!(text.contains("Not logged in"));
}

#[tokio::test]
async fn exit_and_garbage_are_failures_with_codes() {
    let cli = MockCli::with_reply("boom", &[("MOCK_CLAUDE_EXIT", "3")]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "exit_3"),
        "{error}"
    );

    let cli = MockCli::with_reply("boom", &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "bad_reply"),
        "{error}"
    );

    let cli = MockCli::with_reply("[1]", &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "bad_reply"),
        "{error}"
    );

    let ok = r#"{"subtype":"success","is_error":false,"result":"{}"}"#;
    let cli = MockCli::with_reply(ok, &[("MOCK_CLAUDE_EXIT", "4")]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "exit_4"),
        "{error}"
    );
}

#[tokio::test]
async fn output_over_the_limit_is_too_large() {
    let cli = MockCli::with_reply(&"x".repeat(MAX_OUTPUT_BYTES + 2), &[]);
    let error = complete(&cli.claude()).await.unwrap_err();
    assert!(matches!(error, Error::TooLarge { .. }), "{error}");
}

#[tokio::test]
async fn timeout_kills_the_child() {
    let (_tmp, db) = setup().await;
    let cli = MockCli::new(&[("MOCK_CLAUDE_SLEEP", SLEEP)]);
    let router = Router::with_providers(vec![Box::new(cli.claude())]).with_timeout(LIMIT);

    let clock = Instant::now();
    let outcome = router
        .run(
            &db,
            "gui",
            TaskType::ParseJob,
            job_prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(clock.elapsed() < WITHIN, "took {:?}", clock.elapsed());
    assert!(matches!(outcome, Outcome::ManualRequired { .. }));

    let failed = ("failed".to_string(), Some("timeout".to_string()));
    assert_eq!(reasons(&db).await, [failed.clone(), failed]);
    assert_pids_gone(&cli).await;
    assert_never_finished(&cli, OUTLIVE).await;
}

#[tokio::test]
async fn cancel_kills_the_child_and_its_children() {
    let (_tmp, db) = setup().await;
    let cli = MockCli::new(&[("MOCK_CLAUDE_SLEEP", SLEEP)]);
    let router = Router::with_providers(vec![Box::new(cli.claude())]);

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(LIMIT * 2).await;
        trigger.cancel();
    });
    let clock = Instant::now();
    let outcome = router
        .run(&db, "gui", TaskType::ParseJob, job_prompt(), cancel)
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(
        clock.elapsed() < WITHIN + WITHIN,
        "took {:?}",
        clock.elapsed()
    );
    assert_pids_gone(&cli).await;
    assert_never_finished(&cli, OUTLIVE).await;
}

#[tokio::test]
async fn missing_binary_is_unavailable() {
    let claude = ClaudeCode::with_binary("/nonexistent/claude".into(), None);
    assert!(matches!(
        claude.available().await,
        Availability::Unavailable(_)
    ));
}

#[tokio::test]
async fn version_failure_is_unavailable_and_success_is_available() {
    assert_eq!(
        MockCli::new(&[]).claude().available().await,
        Availability::Available
    );
    let cli = MockCli::new(&[("MOCK_CLAUDE_VERSION_EXIT", "1")]);
    assert!(matches!(
        cli.claude().available().await,
        Availability::Unavailable(_)
    ));
}

#[tokio::test]
async fn hung_version_probe_is_unavailable_within_its_timeout() {
    let cli = MockCli::new(&[("MOCK_CLAUDE_VERSION_SLEEP", "30")]);
    let clock = Instant::now();
    assert!(matches!(
        cli.claude().available().await,
        Availability::Unavailable(_)
    ));
    assert!(
        clock.elapsed() < Duration::from_secs(7),
        "{:?}",
        clock.elapsed()
    );
}

#[tokio::test]
async fn embed_is_refused() {
    let claude = ClaudeCode::with_binary("/nonexistent/claude".into(), None);
    let inputs = vec!["x".to_string()];
    let result = claude.embed("m", &inputs, CancellationToken::new()).await;
    assert!(matches!(result, Err(Error::Refused(_))));
}

async fn ids(db: &Db) -> Vec<ProviderId> {
    Router::from_settings(db).await.unwrap().provider_ids()
}

/// Claude with no binary stays listed: the user enabled it, so the run log and the AI screen
/// show it as unavailable rather than it silently vanishing. That also makes this test
/// independent of whether the machine has `claude`.
#[tokio::test]
async fn from_settings_honours_order_and_enabled() {
    let (_tmp, db) = setup().await;

    assert_eq!(
        ids(&db).await,
        [ProviderId::Ollama, ProviderId::NoAi],
        "default"
    );

    settings::set(&db, "ai.order", &json!(["claude_code", "ollama"]))
        .await
        .unwrap();
    assert_eq!(
        ids(&db).await,
        [ProviderId::Ollama, ProviderId::NoAi],
        "claude stays off until enabled"
    );

    let both = json!({"ollama": true, "claude_code": true});
    settings::set(&db, "ai.enabled", &both).await.unwrap();
    assert_eq!(
        ids(&db).await,
        [ProviderId::ClaudeCode, ProviderId::Ollama, ProviderId::NoAi]
    );

    settings::set(&db, "ai.order", &json!(["ollama", "claude_code"]))
        .await
        .unwrap();
    assert_eq!(
        ids(&db).await,
        [ProviderId::Ollama, ProviderId::ClaudeCode, ProviderId::NoAi]
    );

    let claude_only = json!({"ollama": false, "claude_code": true});
    settings::set(&db, "ai.enabled", &claude_only)
        .await
        .unwrap();
    assert_eq!(ids(&db).await, [ProviderId::ClaudeCode, ProviderId::NoAi]);

    let neither = json!({"ollama": false, "claude_code": false});
    settings::set(&db, "ai.enabled", &neither).await.unwrap();
    assert_eq!(ids(&db).await, [ProviderId::NoAi]);

    // Names outside the allow-list and repeats do not add a provider or a second turn.
    settings::set(&db, "ai.enabled", &both).await.unwrap();
    let odd = json!(["openai", "claude_code", "claude_code", "no_ai"]);
    settings::set(&db, "ai.order", &odd).await.unwrap();
    assert_eq!(ids(&db).await, [ProviderId::ClaudeCode, ProviderId::NoAi]);
}

/// No-AI must survive a bad setting: a remote URL drops Ollama, not the router.
#[tokio::test]
async fn from_settings_drops_ollama_on_a_non_loopback_url() {
    let (_tmp, db) = setup().await;
    settings::set(&db, "ai.ollama_url", &json!("http://example.com:11434"))
        .await
        .unwrap();
    let router = Router::from_settings(&db).await.unwrap();
    assert_eq!(router.provider_ids(), [ProviderId::NoAi]);
}

/// Claude in `ai.order` needs a binary to do anything; with one, a task really goes through.
#[tokio::test]
async fn router_runs_a_task_through_the_cli_and_writes_the_audit_row() {
    let (_tmp, db) = setup().await;
    let reply = json!({"subtype": "success", "is_error": false,
        "structured_output": serde_json::from_str::<Value>(JOB).unwrap()});
    let cli = MockCli::with_reply(&reply.to_string(), &[]);
    let router = Router::with_providers(vec![Box::new(cli.claude())]);
    let outcome = router
        .run(
            &db,
            "gui",
            TaskType::ParseJob,
            job_prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            Outcome::Ok {
                provider: ProviderId::ClaudeCode,
                attempts: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'ai_external_call'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
    // The shipped schema file is multi-line: it must reach the CLI in one argument.
    assert!(cli.read("args").contains("\"title\""));
    assert!(!cli.read("args").contains(SENTINEL));
}

#[cfg(unix)]
const SMOKE_SCHEMA: &str = r#"{"type":"object","properties":{"tool_names":{"type":"array","items":{"type":"string"}}},"required":["tool_names"]}"#;
#[cfg(unix)]
const SMOKE_PROMPT: &str =
    "List the names of every tool you can call right now. If you have none, return an empty array.";
/// One attempt: this spends the owner's subscription, so a bad reply must not cost a second.
#[cfg(unix)]
static SMOKE_SPEC: jobpilot_core::ai::TaskSpec = jobpilot_core::ai::TaskSpec {
    task: TaskType::ParseJob,
    schema: SMOKE_SCHEMA,
    required: &[Capability::StructuredOutput, Capability::NoTools],
    categories: &[DataCategory::JobText],
    attempts: 1,
};

/// Keys and types only, plus the fields that say how the call went. No ids, no costs.
#[cfg(unix)]
fn shape(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let kept = [
                        "type",
                        "subtype",
                        "is_error",
                        "result",
                        "structured_output",
                        "permission_denials",
                        "tools",
                        "mcp_servers",
                        "terminal_reason",
                    ];
                    let value = if kept.contains(&key.as_str()) {
                        value.clone()
                    } else {
                        shape(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().take(1).map(shape).collect()),
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
    }
}

/// Spends the owner's subscription, once. CI's keychain step runs `--ignored` across the
/// workspace, so `#[ignore]` alone is not enough: this needs the variable too.
///
/// Shape on 2.1.316. The wrapper below adds `--verbose`, so stdout is an array of events,
/// which also reached the adapter without it in the first recorded run:
/// - first `{type: "system", subtype: "init", tools: [..], mcp_servers: [], model, cwd,
///   permissionMode, session_id, plugins, skills, agents, slash_commands, ..}`;
/// - last `{type: "result", subtype: "success", is_error: false, result: "..",
///   structured_output: {..}, permission_denials: []}`.
///
/// `init.tools` is the proof: exactly `["StructuredOutput"]`, as the controller saw with
/// `--tools "" --allowedTools StructuredOutput`. The model's own list is only a second look.
///
/// `JOBPILOT_REAL_CLAUDE_SMOKE=1 cargo test -p jobpilot-core --test claude_code -- --ignored`
///
/// Appends the reply's real shape to the Task 13 report before asserting anything.
#[cfg(unix)]
#[tokio::test]
#[ignore = "calls the real claude CLI on the owner's subscription"]
async fn real_claude_has_no_tools_smoke() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os("JOBPILOT_REAL_CLAUDE_SMOKE").is_none() {
        eprintln!("skipped: JOBPILOT_REAL_CLAUDE_SMOKE is not set");
        return;
    }
    let real = ClaudeCode::locate().expect("claude is on PATH");
    let real = real.declaration().sign_in;
    let real = real
        .trim_start_matches("claude cli on its own login (")
        .trim_end_matches(')');

    let (tmp, db) = setup().await;
    let raw = tmp.path().join("raw.json");
    let wrapper = tmp.path().join("claude-tee");
    let script = format!(
        "#!/bin/sh\n'{real}' \"$@\" --verbose > '{raw}'\ncode=$?\ncat '{raw}'\nexit $code\n",
        raw = raw.display()
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

    let router = Router::with_providers(vec![Box::new(ClaudeCode::with_binary(wrapper, None))]);
    let prompt = Prompt {
        system: "Answer the user's question through the required output format.".into(),
        user: SMOKE_PROMPT.into(),
        schema: SMOKE_SCHEMA,
    };
    let outcome = router
        .run_spec(&db, "test", &SMOKE_SPEC, prompt, CancellationToken::new())
        .await
        .unwrap();

    let text = fs::read_to_string(&raw).unwrap_or_default();
    let recorded = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(events)) => {
            let shapes: Vec<Value> = events.iter().map(shape).collect();
            serde_json::to_string_pretty(&shapes).unwrap()
        }
        Ok(value) => serde_json::to_string_pretty(&shape(&value)).unwrap(),
        Err(_) => format!("not JSON: {}", text.chars().take(400).collect::<String>()),
    };
    let report = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.superpowers/sdd/2026-11-01-jobpilot-release-0.2/task-13-report.md");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(report)
        .unwrap();
    writeln!(
        file,
        "\n## Real smoke output\n\nOutcome: `{outcome:?}`\nRuns: `{:?}`\n\n```json\n{recorded}\n```",
        reasons(&db).await
    )
    .unwrap();

    let events: Value = serde_json::from_str(&text).expect("an array of events");
    let events = events.as_array().expect("--verbose gives an array");
    let init = events
        .iter()
        .find(|e| e["subtype"] == "init")
        .expect("init");
    assert_eq!(
        init["tools"],
        json!(["StructuredOutput"]),
        "any other tool fails"
    );
    assert_eq!(init["mcp_servers"], json!([]));
    let result = events
        .iter()
        .rfind(|e| e["type"] == "result")
        .expect("result");
    assert_eq!(result["permission_denials"], json!([]));
    assert_eq!(result["is_error"], json!(false));
    assert_eq!(result["subtype"], json!("success"));
    assert!(result["structured_output"].is_object());

    let Outcome::Ok { output, .. } = outcome else {
        panic!("no reply: {outcome:?}");
    };
    let named = output["tool_names"].as_array().expect("tool_names");
    assert!(
        named.iter().all(|name| name == "StructuredOutput"),
        "{named:?}"
    );
}
