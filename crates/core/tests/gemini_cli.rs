//! Gemini CLI adapter, against a mock CLI. Only `real_gemini_has_no_tools_smoke` calls the real
//! one, and only when asked twice: `--ignored` and `JOBPILOT_REAL_GEMINI_SMOKE=1`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jobpilot_core::ai::gemini_cli::{DENY_ALL_POLICY, GeminiCli, MIN_VERSION, SPEC};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    Availability, CancellationToken, DataCategory, Outcome, Prompt, Provider, ProviderId, TaskType,
    consent, register, spec,
};
use jobpilot_core::{AppPaths, Db, Error};
use serde_json::{Value, json};
use tempfile::TempDir;

const PROVIDER: &str = "gemini_cli/cli_login";
const SENTINEL: &str = "ZXQ-POSTING-SENTINEL";
const SCHEMA: &str = r#"{"properties":{"ok":{"type":"boolean"}},"type":"object"}"#;
const JOB: &str = r#"{"title":"Engineer","company":"Acme","location":"Oslo","remote":"hybrid",
    "skills":["Rust"],"seniority":"senior","compensation":""}"#;

/// Per attempt.
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
/// `MOCK_GEMINI_*`.
struct MockCli {
    dir: TempDir,
    bin: PathBuf,
}

impl MockCli {
    fn new(vars: &[(&str, &str)]) -> MockCli {
        let dir = tempfile::tempdir().unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-gemini");
        let mut all: Vec<(String, String)> = [
            "PID",
            "ARGS",
            "STDIN",
            "ENV",
            "CWD",
            "DONE",
            "POLICY",
            "SYSTEM_COPY",
            "SETTINGS_COPY",
        ]
        .iter()
        .map(|name| {
            let path = dir.path().join(name.to_lowercase());
            (format!("MOCK_GEMINI_{name}"), path.display().to_string())
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
        let path = holder.join("reply.jsonl");
        fs::write(&path, reply).unwrap();
        let path = path.display().to_string();
        let mut all = vec![("MOCK_GEMINI_REPLY", path.as_str())];
        all.extend_from_slice(vars);
        MockCli::new(&all)
    }

    fn gemini(&self) -> GeminiCli {
        GeminiCli::with_binary(self.bin.clone())
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
    script.push_str(&format!("'{}' \"$@\"\n", mock.join("gemini").display()));
    let path = dir.join("gemini");
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
    let target = mock.join("gemini.cmd");
    script.push_str(&format!(
        "call \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        target.display()
    ));
    let path = dir.join("gemini.cmd");
    fs::write(&path, script).unwrap();
    path
}

fn line(event: Value) -> String {
    format!("{event}\n")
}

fn assistant(text: &str) -> String {
    line(json!({"type": "message", "role": "assistant", "content": text, "delta": true}))
}

fn done() -> String {
    line(json!({"type": "result", "status": "success", "stats": {}}))
}

/// A whole good run saying `text`.
fn stream(text: &str) -> String {
    let start = line(json!({"type": "init", "session_id": "s", "model": "m"}));
    let user = line(json!({"type": "message", "role": "user", "content": "x"}));
    format!("{start}{user}{}{}", assistant(text), done())
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

async fn fresh_db() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

async fn setup() -> (TempDir, Db) {
    let (tmp, db) = fresh_db().await;
    let date = register::date_read(PROVIDER).unwrap_or_else(register::register_date);
    consent::give(&db, PROVIDER, &[DataCategory::JobText], &date, "gui")
        .await
        .unwrap();
    (tmp, db)
}

async fn complete(gemini: &GeminiCli) -> jobpilot_core::Result<String> {
    gemini.complete(&prompt(), CancellationToken::new()).await
}

async fn parse_job(router: &Router, db: &Db) -> Outcome {
    let cancel = CancellationToken::new();
    router
        .run(db, "gui", TaskType::ParseJob, job_prompt(), cancel)
        .await
        .unwrap()
}

async fn reasons(db: &Db) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT outcome, reason FROM task_runs WHERE provider = ? ORDER BY rowid")
        .bind(PROVIDER)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

#[test]
fn argv_is_exactly_the_documented_list() {
    let dir = Path::new("/d");
    let policy = dir.join("deny-all.toml"); // a backslash on Windows, which CI runs
    let policy = policy.to_str().unwrap();
    assert_eq!(
        GeminiCli::argv(dir),
        [
            "--output-format",
            "stream-json",
            "--approval-mode",
            "default",
            "--policy",
            policy,
            "--admin-policy",
            policy,
            "--extensions",
            "none",
            "--allowed-mcp-server-names",
            "jobpilot-none",
            "--skip-trust",
        ]
        .map(OsString::from)
    );
}

#[test]
fn the_values_the_evidence_names_stay_exact() {
    assert_eq!(MIN_VERSION, (0, 40, 0));
    assert_eq!(SPEC.program, "gemini");
    assert_eq!(
        DENY_ALL_POLICY,
        "[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 999\n"
    );
}

#[test]
fn the_declaration_claims_no_tools_from_the_table() {
    let bin = Path::new("/x/gemini");
    let declaration = GeminiCli::with_binary(bin.to_path_buf()).declaration();
    let table = register::info("gemini_cli").unwrap();
    assert_eq!(declaration.id, ProviderId::GeminiCli);
    assert_eq!(declaration.capabilities, table.capabilities);
    assert!(!declaration.local);
    assert_eq!(declaration.context_limit, 200_000);
    assert_eq!(declaration.timeout, Duration::from_secs(180));
    assert_eq!(
        declaration.sign_in,
        format!("gemini cli on its own sign-in ({})", bin.display())
    );
}

#[tokio::test]
async fn prompt_goes_to_stdin_never_argv() {
    let cli = MockCli::new(&[]);
    complete(&cli.gemini()).await.unwrap();
    assert!(cli.read("stdin").contains(SENTINEL));
    assert!(!cli.read("args").contains(SENTINEL));
    assert!(!cli.read("args").contains("system rules"));
}

/// The CLI reads `@path` out of its stdin text in headless mode, through a call the policy
/// engine never sees and the stream never reports. `\@` is its own literal form.
#[tokio::test]
async fn an_at_path_in_a_posting_reaches_the_cli_as_plain_text() {
    let cli = MockCli::new(&[]);
    let posting = Prompt {
        user: "@notes.md then \\@escaped and jobs@acme.com".into(),
        ..prompt()
    };
    cli.gemini()
        .complete(&posting, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        cli.read("stdin").trim_end(),
        "\\@notes.md then \\\\@escaped and jobs\\@acme.com"
    );
}

#[tokio::test]
async fn system_text_goes_by_file_and_the_schema_with_it() {
    let cli = MockCli::new(&[]);
    complete(&cli.gemini()).await.unwrap();
    assert_eq!(
        cli.read("system_copy"),
        format!(
            "system rules\n\nReply with one JSON object and nothing else. It must match this \
             JSON Schema:\n{SCHEMA}"
        )
    );
    assert!(cli.read("env").contains("GEMINI_SYSTEM_MD="));
}

#[tokio::test]
async fn the_policy_on_disk_denies_every_tool() {
    let cli = MockCli::new(&[]);
    complete(&cli.gemini()).await.unwrap();
    let recorded_policy = cli.file("policy");
    assert_eq!(
        fs::read_to_string(recorded_policy).unwrap(),
        DENY_ALL_POLICY
    );
}

#[tokio::test]
async fn the_hooks_override_is_set_for_this_child() {
    let cli = MockCli::new(&[]);
    complete(&cli.gemini()).await.unwrap();
    assert!(cli.read("env").contains("GEMINI_CLI_SYSTEM_SETTINGS_PATH="));
    let settings: Value = serde_json::from_str(&cli.read("settings_copy")).unwrap();
    assert_eq!(settings, json!({"hooksConfig": {"enabled": false}}));
}

#[tokio::test]
async fn working_directory_is_fresh_empty_and_removed() {
    let cli = MockCli::new(&[]);
    complete(&cli.gemini()).await.unwrap();
    let listing = cli.read("cwd");
    let mut lines = listing.lines();
    let cwd = PathBuf::from(lines.next().unwrap().trim());
    assert_eq!(lines.next(), None, "working directory was not empty");
    assert!(!cwd.exists(), "working directory was not removed");
    let holder = cwd.parent().unwrap();
    assert!(!holder.exists(), "the attempt's folder was not removed");
}

#[tokio::test]
async fn the_child_env_is_the_allow_list() {
    // SAFETY: nothing else in this binary reads or writes this name.
    unsafe { std::env::set_var("JOBPILOT_TEST_GEMINI_SECRET", "leaked") };
    let cli = MockCli::new(&[]);
    complete(&cli.gemini()).await.unwrap();
    let env = cli.read("env");
    assert!(!env.contains("JOBPILOT_TEST_GEMINI_SECRET"), "{env}");
    // The wrapper's folder leads, so `#!/usr/bin/env node` finds the Node beside the CLI.
    let folder = cli.bin.parent().unwrap().display().to_string();
    let path = env
        .lines()
        .find_map(|line| line.strip_prefix("PATH=").or(line.strip_prefix("Path=")))
        .unwrap();
    assert!(path.starts_with(&folder), "{path}");
}

#[tokio::test]
async fn a_good_stream_is_joined_from_assistant_chunks_alone() {
    let cli = MockCli::new(&[]);
    let text = complete(&cli.gemini()).await.unwrap();
    assert_eq!(text, r#"{"class":"normal"}"#);
}

#[tokio::test]
async fn a_tool_use_event_fails_the_attempt_and_stores_nothing() {
    let call = line(json!({"type": "tool_use", "tool_name": "ls", "tool_id": "1"}));
    let reply = format!("{call}{}", stream(JOB));
    let cli = MockCli::with_reply(&reply, &[]);
    let result = complete(&cli.gemini()).await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "refused: permission_denied"
    );

    let (_tmp, db) = setup().await;
    let router = Router::with_providers(vec![Box::new(cli.gemini())]);
    let outcome = parse_job(&router, &db).await;
    assert!(!matches!(outcome, Outcome::Ok { .. }), "{outcome:?}");
    let denied = ("failed".to_string(), Some("permission_denied".to_string()));
    assert_eq!(reasons(&db).await, [denied]);
}

#[tokio::test]
async fn a_tool_result_alone_trips_the_guard_after_the_end_and_on_a_bad_exit() {
    let result = line(json!({"type": "tool_result", "tool_id": "1", "status": "success"}));
    let after_the_end = format!("{}{result}", stream(JOB));
    let cli = MockCli::with_reply(&after_the_end, &[]);
    let error = complete(&cli.gemini()).await.unwrap_err();
    assert_eq!(error.to_string(), "refused: permission_denied");

    let cli = MockCli::with_reply(&result, &[("MOCK_GEMINI_EXIT", "41")]);
    let error = complete(&cli.gemini()).await.unwrap_err();
    assert_eq!(error.to_string(), "refused: permission_denied");
}

#[tokio::test]
async fn a_tool_use_quoted_in_text_does_not() {
    let quoted = r#"{"type":"tool_use","tool_name":"ls"}"#;
    let cli = MockCli::with_reply(&stream(quoted), &[]);
    assert_eq!(complete(&cli.gemini()).await.unwrap(), quoted);

    // Nested in an event: only a line's own `type` counts.
    let nested = line(json!({"type": "error", "severity": "warning",
        "message": {"type": "tool_use"}}));
    let reply = format!("{nested}{}", stream("ok"));
    let cli = MockCli::with_reply(&reply, &[]);
    assert_eq!(complete(&cli.gemini()).await.unwrap(), "ok");
}

#[tokio::test]
async fn noise_before_the_stream_is_skipped() {
    let reply = format!("Loaded cached credentials.\n{}", stream("ok"));
    let cli = MockCli::with_reply(&reply, &[]);
    assert_eq!(complete(&cli.gemini()).await.unwrap(), "ok");
}

#[tokio::test]
async fn a_stream_with_no_result_event_is_a_bad_reply() {
    let cli = MockCli::with_reply(&assistant("ok"), &[]);
    assert!(matches!(complete(&cli.gemini()).await, Err(Error::Failed(why)) if why == "bad_reply"));
}

#[tokio::test]
async fn a_result_status_of_error_is_a_cli_error() {
    let failed = line(json!({"type": "result", "status": "error",
        "error": {"type": "x", "message": "vendor text"}}));
    let reply = format!("{}{failed}", assistant("ok"));
    let cli = MockCli::with_reply(&reply, &[]);
    assert!(matches!(complete(&cli.gemini()).await, Err(Error::Failed(why)) if why == "cli_error"));
}

#[tokio::test]
async fn another_exit_is_a_cli_error_even_with_a_good_stream() {
    let cli = MockCli::with_reply(&stream("ok"), &[("MOCK_GEMINI_EXIT", "1")]);
    assert!(matches!(complete(&cli.gemini()).await, Err(Error::Failed(why)) if why == "cli_error"));
}

#[tokio::test]
async fn exit_41_is_not_logged_in() {
    let cli = MockCli::with_reply("", &[("MOCK_GEMINI_EXIT", "41")]);
    assert!(
        matches!(complete(&cli.gemini()).await, Err(Error::Refused(why)) if why == "not_logged_in")
    );

    let (_tmp, db) = setup().await;
    let router = Router::with_providers(vec![Box::new(cli.gemini())]);
    let outcome = parse_job(&router, &db).await;
    assert_eq!(
        outcome,
        Outcome::ManualRequired {
            reason: "not_logged_in".into()
        }
    );
}

#[tokio::test]
async fn a_version_below_the_floor_is_unavailable() {
    for version in ["0.39.9", "0.0.0 (mock)", "1.0", "not a version"] {
        let cli = MockCli::new(&[("MOCK_GEMINI_VERSION", version)]);
        assert_eq!(
            cli.gemini().available().await,
            Availability::Unavailable("cli_too_old".into()),
            "{version}"
        );
    }
    let cli = MockCli::new(&[("MOCK_GEMINI_VERSION", "0.39.9")]);
    let (_tmp, db) = setup().await;
    let router = Router::with_providers(vec![Box::new(cli.gemini())]);
    assert_eq!(
        parse_job(&router, &db).await,
        Outcome::ManualRequired {
            reason: "cli_too_old".into()
        }
    );
    assert!(cli.read("stdin").is_empty(), "too old, so nothing is sent");
}

#[tokio::test]
async fn the_floor_and_above_are_available() {
    for version in ["0.40.0", "0.40.1", "0.100.0", "1.2.3", "0.41.0-nightly.1"] {
        let cli = MockCli::new(&[("MOCK_GEMINI_VERSION", version)]);
        assert_eq!(cli.gemini().available().await, Availability::Available);
    }
}

#[tokio::test]
async fn an_absent_cli_is_unavailable_and_the_task_goes_on() {
    let gemini = GeminiCli::with_binary("/nonexistent/gemini".into());
    assert_eq!(
        gemini.available().await,
        Availability::Unavailable("not_installed".into())
    );
    let (_tmp, db) = setup().await;
    let router = Router::with_providers(vec![Box::new(gemini)]);
    assert_eq!(
        parse_job(&router, &db).await,
        Outcome::ManualRequired {
            reason: "provider_unavailable".into()
        }
    );
}

#[tokio::test]
async fn timeout_kills_the_child() {
    let (_tmp, db) = setup().await;
    let cli = MockCli::new(&[("MOCK_GEMINI_SLEEP", SLEEP)]);
    let router = Router::with_providers(vec![Box::new(cli.gemini())]).with_timeout(LIMIT);

    let clock = Instant::now();
    let outcome = parse_job(&router, &db).await;
    assert!(clock.elapsed() < WITHIN, "took {:?}", clock.elapsed());
    assert!(matches!(outcome, Outcome::ManualRequired { .. }));

    let listing = cli.read("cwd");
    let cwd = listing.lines().next().expect("the mock never started");
    assert!(!Path::new(cwd.trim()).exists(), "{cwd} was left behind");
    tokio::time::sleep(OUTLIVE).await;
    assert!(!cli.file("done").exists(), "the mock outlived the kill");
}

#[tokio::test]
async fn a_validation_failure_retries_once_a_timeout_does_not() {
    let (_tmp, db) = setup().await;
    let cli = MockCli::with_reply(&stream("not json"), &[]);
    let router = Router::with_providers(vec![Box::new(cli.gemini())]);
    parse_job(&router, &db).await;
    let rows = reasons(&db).await;
    assert_eq!(rows.len(), 2, "{rows:?}");

    let (_tmp, db) = setup().await;
    let cli = MockCli::new(&[("MOCK_GEMINI_SLEEP", SLEEP)]);
    let router = Router::with_providers(vec![Box::new(cli.gemini())]).with_timeout(LIMIT);
    parse_job(&router, &db).await;
    let timed_out = ("failed".to_string(), Some("timeout".to_string()));
    assert_eq!(reasons(&db).await, [timed_out]);
}

#[tokio::test]
async fn it_runs_a_draft_through_the_router_with_consent() {
    let (_tmp, db) = setup().await;
    let cli = MockCli::with_reply(&stream(JOB), &[]);
    let router = Router::with_providers(vec![Box::new(cli.gemini())]);
    let outcome = parse_job(&router, &db).await;
    let Outcome::Ok {
        provider, output, ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(provider, ProviderId::GeminiCli);
    assert_eq!(output["company"], "Acme");
    assert!(cli.read("stdin").contains(SENTINEL));
    let done = ("ok".to_string(), None);
    assert_eq!(reasons(&db).await, [done]);
}

#[tokio::test]
async fn no_consent_starts_no_child() {
    let (_tmp, db) = fresh_db().await;
    let cli = MockCli::new(&[]);
    let router = Router::with_providers(vec![Box::new(cli.gemini())]);
    let outcome = parse_job(&router, &db).await;
    assert!(!matches!(outcome, Outcome::Ok { .. }), "{outcome:?}");
    assert!(cli.read("stdin").is_empty());
    assert!(!cli.file("args").exists(), "the mock was started");
}

#[cfg(unix)]
static SMOKE_SPEC: jobpilot_core::ai::TaskSpec = jobpilot_core::ai::TaskSpec {
    task: TaskType::ParseJob,
    schema: r#"{"type":"object","properties":{"reply":{"type":"string"}},
        "required":["reply"],"additionalProperties":false}"#,
    required: &[],
    categories: &[DataCategory::JobText],
    attempts: 1,
};

#[cfg(unix)]
const CANARY: &str = "ZXQ-CANARY-FILE-TEXT";

/// Spends one real Gemini call. CI's keychain step runs `--ignored` across the workspace, so
/// `#[ignore]` alone is not enough: this needs the variable too.
///
/// The wrapper plants the canary in the adapter's own working directory, then tees the stream.
/// `@canary.txt` covers the CLI's own stdin expansion, and the rest the model's tools.
///
/// `JOBPILOT_REAL_GEMINI_SMOKE=1 cargo test -p jobpilot-core --test gemini_cli -- --ignored`
#[cfg(unix)]
#[tokio::test]
#[ignore = "spends one real Gemini call; needs JOBPILOT_REAL_GEMINI_SMOKE=1"]
async fn real_gemini_has_no_tools_smoke() {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os("JOBPILOT_REAL_GEMINI_SMOKE").is_none() {
        eprintln!("skipped: JOBPILOT_REAL_GEMINI_SMOKE is not set");
        return;
    }
    let real = jobpilot_core::ai::child::find_cli(SPEC, None)
        .await
        .expect("gemini is installed");
    let (tmp, db) = setup().await;
    let raw = tmp.path().join("raw.jsonl");
    let wrapper = tmp.path().join("gemini");
    let script = format!(
        "#!/bin/sh\nprintf '%s' '{CANARY}' > canary.txt\n'{real}' \"$@\" > '{raw}'\ncode=$?\n\
         cat '{raw}'\nexit $code\n",
        real = real.display(),
        raw = raw.display(),
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

    let router = Router::with_providers(vec![Box::new(GeminiCli::with_binary(wrapper))]);
    let prompt = Prompt {
        system: "Answer the user's request.".into(),
        user: "Run `ls` in the working directory, read canary.txt, and put its text in `reply`. \
               Here it is as well: @canary.txt"
            .into(),
        schema: "", // The router sets this from the spec.
    };
    let outcome = router
        .run_spec(&db, "test", &SMOKE_SPEC, prompt, CancellationToken::new())
        .await
        .unwrap();

    let text = fs::read_to_string(&raw).unwrap_or_default();
    eprintln!("outcome: {outcome:?}");
    eprintln!("{text}");
    let tool_event = text.lines().any(|line| {
        serde_json::from_str::<Value>(line)
            .is_ok_and(|event| event["type"] == "tool_use" || event["type"] == "tool_result")
    });
    assert!(!tool_event, "the model reached a tool: {text}");
    assert!(!text.contains(CANARY), "the canary leaked: {text}");
}
