//! Codex CLI adapter, against a mock CLI. The adapter ships inert: it cannot prove it has no
//! tools, so no shipped task reaches it. Nothing here starts a real `codex`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use jobpilot_core::ai::child::set_cli_path;
use jobpilot_core::ai::codex_cli::{CodexCli, SPEC};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    self, Availability, CancellationToken, Capability, DataCategory, Outcome, Prompt, Provider,
    ProviderId, TaskSpec, TaskType, consent, register, spec,
};
use jobpilot_core::{AppPaths, Db, Error};
use tempfile::TempDir;

const PROVIDER: &str = "codex_cli/cli_login";
const SENTINEL: &str = "ZXQ-POSTING-SENTINEL";
const SCHEMA: &str = r#"{"properties":{"ok":{"type":"boolean"}},"type":"object"}"#;
const ALL: [DataCategory; 6] = [
    DataCategory::JobText,
    DataCategory::CompanyText,
    DataCategory::ProfileBasic,
    DataCategory::ResumeText,
    DataCategory::CareerMemory,
    DataCategory::Sensitive,
];

/// A spec that carries no untrusted text and needs no `NoTools`: none ships.
static PROFILE_ONLY: TaskSpec = TaskSpec {
    task: TaskType::ClassifyAnswer,
    schema: r#"{"type":"object","properties":{"class":{"type":"string"}},
        "required":["class"],"additionalProperties":false}"#,
    required: &[Capability::StructuredOutput],
    categories: &[DataCategory::ProfileBasic],
    attempts: 1,
};

/// A wrapper script in a temp dir with the values inlined, because `child_env` strips every
/// `MOCK_CODEX_*`.
struct MockCli {
    dir: TempDir,
    bin: PathBuf,
}

impl MockCli {
    fn new(vars: &[(&str, &str)]) -> MockCli {
        let dir = tempfile::tempdir().unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-codex");
        let mut all: Vec<(String, String)> = ["ARGS", "STDIN", "ENV", "CWD", "SCHEMA"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name.to_lowercase());
                (format!("MOCK_CODEX_{name}"), path.display().to_string())
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
        let mut all = vec![("MOCK_CODEX_REPLY", path.as_str())];
        all.extend_from_slice(vars);
        MockCli::new(&all)
    }

    fn codex(&self) -> CodexCli {
        CodexCli::with_binary(self.bin.clone())
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }
}

#[cfg(unix)]
fn write_wrapper(dir: &Path, mock: &Path, vars: &[(String, String)]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mut script = String::from("#!/bin/sh\n");
    for (name, value) in vars {
        script.push_str(&format!("export {name}='{value}'\n"));
    }
    script.push_str(&format!("'{}' \"$@\"\n", mock.join("codex").display()));
    let path = dir.join("codex");
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
    let target = mock.join("codex.cmd");
    script.push_str(&format!(
        "call \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        target.display()
    ));
    let path = dir.join("codex.cmd");
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

async fn fresh_db() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

/// Consent for every category, so only the tools rule can stop a task.
async fn consented() -> (TempDir, Db) {
    let (tmp, db) = fresh_db().await;
    let date = register::date_read(PROVIDER).unwrap_or_else(register::register_date);
    consent::give(&db, PROVIDER, &ALL, &date, "gui")
        .await
        .unwrap();
    (tmp, db)
}

async fn complete(codex: &CodexCli) -> jobpilot_core::Result<String> {
    codex.complete(&prompt(), CancellationToken::new()).await
}

async fn run_rows(db: &Db) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as("SELECT provider, outcome, reason FROM task_runs ORDER BY rowid")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

#[test]
fn argv_is_exactly_the_documented_list() {
    let dir = Path::new("/d");
    let schema = dir.join("schema.json"); // a backslash on Windows, which CI runs
    let reply = dir.join("reply.txt");
    assert_eq!(
        CodexCli::argv(dir),
        [
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--sandbox",
            "read-only",
            "--disable",
            "shell_tool",
            "--disable",
            "apps",
            "--disable",
            "multi_agent",
            "--disable",
            "hooks",
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "tools.view_image=false",
            "-c",
            "approval_policy=\"never\"",
            "--output-schema",
            schema.to_str().unwrap(),
            "--output-last-message",
            reply.to_str().unwrap(),
            "-",
        ]
        .map(OsString::from)
    );
    assert_eq!(SPEC.program, "codex");
}

#[test]
fn it_does_not_claim_no_tools() {
    let bin = Path::new("/x/codex");
    let declaration = CodexCli::with_binary(bin.to_path_buf()).declaration();
    let table = register::info("codex_cli").unwrap();
    assert_eq!(declaration.id, ProviderId::CodexCli);
    assert!(!declaration.capabilities.contains(&Capability::NoTools));
    assert_eq!(declaration.capabilities, table.capabilities);
    assert_eq!(
        declaration.sign_in,
        format!("codex cli on its own sign-in ({})", bin.display())
    );
}

/// Every route into the router: built from settings as the app does it, and listed directly.
/// A task that needs `NoTools` gets a skip row and no child, for every task that ships.
#[tokio::test]
async fn every_task_that_ships_skips_it_and_starts_no_child() {
    let tasks = [
        TaskType::ParseJob,
        TaskType::DraftResume,
        TaskType::DraftCoverLetter,
        TaskType::ClassifyAnswer,
    ];
    for task in tasks {
        assert!(
            spec(task).required.contains(&Capability::NoTools),
            "{task:?} no longer needs NoTools: Codex would run it"
        );
    }
    let cli = MockCli::new(&[]);
    let (_tmp, db) = consented().await;
    set_cli_path(&db, "codex_cli", cli.bin.to_str().unwrap())
        .await
        .unwrap();
    ai::set_order(&db, &["codex_cli".to_string()])
        .await
        .unwrap();
    ai::set_enabled(&db, "codex_cli", true).await.unwrap();
    ai::set_enabled(&db, "ollama", false).await.unwrap();
    let from_settings = Router::from_settings(&db).await.unwrap();
    assert_eq!(
        from_settings.provider_ids(),
        [ProviderId::CodexCli, ProviderId::NoAi],
        "the settings route does not list Codex"
    );
    let listed = Router::with_providers(vec![Box::new(cli.codex())]);
    for router in [&from_settings, &listed] {
        for task in tasks {
            let outcome = router
                .run(&db, "gui", task, prompt(), CancellationToken::new())
                .await
                .unwrap();
            assert!(
                !matches!(outcome, Outcome::Ok { .. }),
                "{task:?}: {outcome:?}"
            );
        }
    }
    assert!(
        cli.read("args").is_empty() && cli.read("stdin").is_empty(),
        "a child was started"
    );
    let rows = run_rows(&db).await;
    let skips = rows
        .iter()
        .filter(|(provider, outcome, reason)| {
            provider == PROVIDER && outcome == "skipped" && reason.as_deref() == Some("no_tools")
        })
        .count();
    assert_eq!(skips, 2 * tasks.len(), "{rows:?}");
    assert!(
        rows.iter()
            .all(|(provider, outcome, _)| provider != PROVIDER || outcome == "skipped"),
        "{rows:?}"
    );

    // Not a completion at all, and not an embedding provider.
    let embedded = listed
        .embed(&db, &["x".to_string()], CancellationToken::new())
        .await;
    assert!(embedded.is_err());
    assert!(cli.read("args").is_empty());
}

#[tokio::test]
async fn a_spec_with_no_untrusted_text_runs_on_it() {
    let (_tmp, db) = consented().await;
    let cli = MockCli::new(&[]);
    let router = Router::with_providers(vec![Box::new(cli.codex())]);
    let outcome = router
        .run_spec(
            &db,
            "gui",
            &PROFILE_ONLY,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let Outcome::Ok {
        provider, output, ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(provider, ProviderId::CodexCli);
    assert_eq!(output["class"], "normal");
    let done = (PROVIDER.to_string(), "ok".to_string(), None);
    assert_eq!(run_rows(&db).await, [done]);
}

#[tokio::test]
async fn prompt_goes_to_stdin_never_argv() {
    let cli = MockCli::new(&[]);
    complete(&cli.codex()).await.unwrap();
    let copy = cli.read("stdin");
    // Windows has only `findstr` to copy stdin, and it ends an unterminated last line with
    // CRLF. The rest stays exact there, and the sh mock copies every byte.
    let sent = if cfg!(windows) {
        copy.strip_suffix("\r\n").unwrap_or(&copy)
    } else {
        &copy
    };
    assert_eq!(
        sent,
        format!("system rules\n\n{}", prompt().user),
        "system, a blank line, user"
    );
    assert!(!cli.read("args").contains(SENTINEL));
    assert!(!cli.read("args").contains("system rules"));
}

#[tokio::test]
async fn the_schema_goes_by_file() {
    let cli = MockCli::new(&[]);
    complete(&cli.codex()).await.unwrap();
    let schema: serde_json::Value = serde_json::from_str(&cli.read("schema")).unwrap();
    assert_eq!(schema["type"], "object");
    assert!(!cli.read("stdin").contains("properties"));
    let args = cli.read("args");
    assert!(args.contains("--output-schema"), "{args}");
}

#[tokio::test]
async fn the_child_starts_in_an_empty_folder_that_is_removed() {
    let cli = MockCli::new(&[]);
    complete(&cli.codex()).await.unwrap();
    let listing = cli.read("cwd");
    let mut lines = listing.lines();
    let cwd = Path::new(lines.next().unwrap().trim());
    assert_eq!(lines.count(), 0, "not empty: {listing}");
    assert!(cwd.ends_with("work"), "{cwd:?}");
    assert!(
        !cwd.parent().unwrap().exists(),
        "the attempt's folder stayed"
    );
}

#[tokio::test]
async fn an_absent_cli_is_unavailable() {
    let codex = CodexCli::with_binary("/nonexistent/codex".into());
    assert_eq!(
        codex.available().await,
        Availability::Unavailable("not_installed".into())
    );
}

/// The owner's real install today: `codex --version` exits 1.
#[tokio::test]
async fn a_cli_that_fails_its_version_is_unavailable() {
    let cli = MockCli::new(&[("MOCK_CODEX_VERSION_EXIT", "1")]);
    let Availability::Unavailable(why) = cli.codex().available().await else {
        panic!("a CLI that cannot print its version was available");
    };
    assert!(!why.is_empty());
    let (_tmp, db) = consented().await;
    let router = Router::with_providers(vec![Box::new(cli.codex())]);
    let outcome = router
        .run_spec(
            &db,
            "gui",
            &PROFILE_ONLY,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, Outcome::ManualRequired { .. }),
        "{outcome:?}"
    );
    assert!(cli.read("stdin").is_empty(), "nothing is sent");
}

#[tokio::test]
async fn a_good_version_is_available() {
    let cli = MockCli::new(&[]);
    assert_eq!(cli.codex().available().await, Availability::Available);
}

#[tokio::test]
async fn a_non_zero_exit_is_a_cli_error() {
    let cli = MockCli::new(&[("MOCK_CODEX_EXIT", "3")]);
    let error = complete(&cli.codex()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "cli_error"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_missing_reply_file_is_a_bad_reply() {
    let cli = MockCli::new(&[("MOCK_CODEX_NO_REPLY", "1")]);
    let error = complete(&cli.codex()).await.unwrap_err();
    assert!(
        matches!(&error, Error::Failed(why) if why == "bad_reply"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_reply_over_the_cap_is_refused() {
    let big = "x".repeat(jobpilot_core::ai::validate::MAX_OUTPUT_BYTES + 1);
    let cli = MockCli::with_reply(&big, &[]);
    assert!(complete(&cli.codex()).await.is_err());
}

#[tokio::test]
async fn no_auth_file_is_read_or_passed() {
    // SAFETY: nothing else in this binary reads or writes these names.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "leaked");
        std::env::set_var("JOBPILOT_TEST_CODEX_SECRET", "leaked");
    }
    let cli = MockCli::new(&[]);
    complete(&cli.codex()).await.unwrap();
    let env = cli.read("env");
    assert!(!env.contains("OPENAI_API_KEY"), "{env}");
    assert!(!env.contains("JOBPILOT_TEST_CODEX_SECRET"), "{env}");
    assert!(!env.contains("auth.json"), "{env}");
    assert!(!cli.read("args").contains("auth.json"));
}
