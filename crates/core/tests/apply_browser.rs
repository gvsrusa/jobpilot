//! Apply mode against the real thing: the scripted fake `claude` in `mock-claude-apply` drives
//! the real `jobpilot apply-mcp`, the real pinned Playwright server, and a real, visible
//! Chromium, on the fixture forms in `fixtures/forms`, served by `fake_http` on 127.0.0.1. No
//! test reaches any other host, and none starts a real `claude`.
//!
//! Every test is ignored and needs `JOBPILOT_E2E_RUNTIME`: the `browser-runtime` folder of a
//! temporary root that `jobpilot browser setup` filled. Run `cargo build -p jobpilot-cli`
//! first, then, one test at a time:
//!
//! ```text
//! JOBPILOT_E2E_RUNTIME=<root>/browser-runtime cargo test -p jobpilot-core --test apply_browser \
//!     -- --ignored --test-threads=1 --skip serve_fixtures
//! ```

mod fake_http;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use fake_http::Fake as Site;
use jobpilot_core::agents::{claude_apply_with, copilot};
use jobpilot_core::ai::claude_code::{PATH_SETTING, PROVIDER};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{CancellationToken, DataCategory, consent};
use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::apply::session::HostKnobs;
use jobpilot_core::apply::{self, PauseView, Resolution, RunView};
use jobpilot_core::browser::sweep::kill_marked;
use jobpilot_core::profile::{self, ProfileBasic};
use jobpilot_core::render::{Entry, ResumeDoc, Section};
use jobpilot_core::{AppPaths, Db, Error, drafts, memory, pause, settings};
use serde_json::{Value, json};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};
use tempfile::TempDir;
use tokio::task::JoinHandle;

const BASE: [DataCategory; 3] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::CareerMemory,
];
const NAME: &str = "Ada Lovelace";
const EMAIL: &str = "ada@example.com";
/// The approved answer holds a legal phrase on purpose: typed and echoed back, the owner's
/// own words must not stop every application.
const WHY: &str = "I agree with your mission and enjoy building reliable tools.";
/// What the done page says, after a form posts.
const RECEIVED: &str = "Your application was received.";
/// Every form's send button.
const SUBMIT: &str = "Submit application";
/// The legal fixture's declaration, a checkbox.
const CERTIFY: &str = "I certify that my answers are true and complete.";
/// A real browser is slow to start, and a run of eight forms takes a while.
const WAIT: Duration = Duration::from_secs(300);
/// The emergency pause's bound, from the flag to every process gone.
const KILL: Duration = Duration::from_secs(2);

// ---- The world the runs happen in ----

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forms")
}

/// The runtime folder `jobpilot browser setup` filled, from `JOBPILOT_E2E_RUNTIME`.
fn runtime() -> PathBuf {
    let dir = std::env::var_os("JOBPILOT_E2E_RUNTIME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .expect(
            "set JOBPILOT_E2E_RUNTIME to the browser-runtime folder of a root that \
             `jobpilot browser setup` filled",
        );
    let dir = std::path::absolute(dir).unwrap();
    assert!(
        dir.join("node_modules/@playwright/mcp/cli.js").is_file() && dir.join("browsers").is_dir(),
        "{} holds no browser runtime: run `jobpilot browser setup` on its root",
        dir.display()
    );
    dir
}

/// The test root's `browser-runtime` is the shared one, so each root needs no download.
#[cfg(unix)]
fn link_runtime(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// A junction, which needs no privilege, unlike a symbolic link.
#[cfg(windows)]
fn link_runtime(target: &Path, link: &Path) {
    // `mklink` reads a forward slash as a switch.
    let target = target.to_string_lossy().replace('/', "\\");
    let link = link.to_string_lossy().replace('/', "\\");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
}

fn mock_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}

/// The fakes and the runtime are named through the database and the root, so a variable
/// from the shell must not override them.
fn environment_is_clean() {
    for name in ["JOBPILOT_CLAUDE_BIN", "JOBPILOT_PLAYWRIGHT_CLI"] {
        assert!(
            std::env::var_os(name).is_none_or(|value| value.is_empty()),
            "unset {name} to run these tests"
        );
    }
}

struct World {
    tmp: TempDir,
    db: Arc<Db>,
    site: Site,
}

impl World {
    fn root(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    /// A second handle, as another process holds one.
    async fn other(&self) -> Db {
        Db::open(&AppPaths::new(self.root())).await.unwrap()
    }
}

/// A failed test must not leave a browser open: everything a run of this root started holds
/// `<root>/apply` on its command line.
impl Drop for World {
    fn drop(&mut self) {
        kill_marked(&self.root().join("apply"));
    }
}

async fn world() -> World {
    let runtime = runtime();
    environment_is_clean();
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    let link = paths.browser_runtime();
    if link.is_dir() {
        std::fs::remove_dir(&link).unwrap();
    }
    link_runtime(&runtime, &link);

    let basic = ProfileBasic {
        name: NAME.into(),
        email: EMAIL.into(),
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();
    consent::give(&db, PROVIDER, &BASE, "2026-11-01", "cli")
        .await
        .unwrap();
    let question = memory::add_question(&db, "Why this role?", None)
        .await
        .unwrap();
    let answer = memory::add_answer(&db, &question.id, WHY, None)
        .await
        .unwrap();
    memory::approve(&db, "cli", &answer.id, true).await.unwrap();
    World {
        tmp,
        db: Arc::new(db),
        site: Site::pages(&fixtures()),
    }
}

fn doc(title: &str) -> ResumeDoc {
    ResumeDoc {
        name: NAME.into(),
        contact: vec![EMAIL.into()],
        summary: Vec::new(),
        sections: vec![Section {
            heading: "Experience".into(),
            entries: vec![Entry {
                bullets: vec![title.into()],
                ..Entry::default()
            }],
        }],
    }
}

async fn uploaded(db: &Db, kind: &str, title: &str) -> String {
    drafts::create_version(
        db,
        kind,
        None,
        "uploaded",
        title,
        None,
        None,
        doc(title),
        Vec::new(),
    )
    .await
    .unwrap()
    .id
}

/// An application whose link is the fixture page `page`.
async fn add_app(w: &World, page: &str) -> String {
    let resume = uploaded(&w.db, "resume", &format!("{page} resume")).await;
    let letter = uploaded(&w.db, "cover_letter", &format!("{page} letter")).await;
    let input = ApplicationInput {
        company: format!("{page} company"),
        role: "Engineer".into(),
        link: format!("{}/{page}", w.site.url),
        resume_draft_id: Some(resume),
        letter_draft_id: Some(letter),
        ..ApplicationInput::default()
    };
    applications::add(&w.db, "cli", input).await.unwrap().id
}

async fn run_of(w: &World, pages: &[&str]) -> (String, Vec<String>) {
    let mut apps = Vec::new();
    for page in pages {
        apps.push(add_app(w, page).await);
    }
    let run = apply::start(&w.db, "cli", &apps, false).await.unwrap();
    (run, apps)
}

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

/// The CLI the fake `claude` starts as its MCP server.
fn sidecar() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join(format!("jobpilot{}", std::env::consts::EXE_SUFFIX));
    assert!(
        bin.is_file(),
        "run `cargo build -p jobpilot-cli` first: {} is missing",
        bin.display()
    );
    bin
}

fn mock_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-claude-apply")
}

#[cfg(unix)]
fn write_wrapper(dir: &Path, vars: &[(String, String)]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mut script = String::from("#!/bin/sh\n");
    for (name, value) in vars {
        script.push_str(&format!("export {name}='{value}'\n"));
    }
    script.push_str(&format!(
        "exec '{}' '{}' \"$@\"\n",
        node().display(),
        mock_dir().join("claude").display()
    ));
    // This name, because a stored `ai.claude_path` must be a program named claude.
    let path = dir.join("claude");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(windows)]
fn write_wrapper(dir: &Path, vars: &[(String, String)]) -> PathBuf {
    let mut script = String::from("@echo off\r\n");
    for (name, value) in vars {
        script.push_str(&format!("set \"{name}={value}\"\r\n"));
    }
    script.push_str(&format!(
        "call \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        mock_dir().join("claude.cmd").display()
    ));
    let path = dir.join("claude.cmd");
    std::fs::write(&path, script).unwrap();
    path
}

/// One scripted session in front of the real server.
struct Script {
    dir: TempDir,
    knobs: HostKnobs,
}

impl Script {
    /// Each tool reply the fake got, in order, with the args it sent.
    fn replies(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("replies.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|entry| entry.get("tool").is_some())
            .collect()
    }

    /// How many calls of `tool` were answered.
    fn answered(&self, tool: &str) -> usize {
        self.replies()
            .iter()
            .filter(|entry| entry["tool"] == tool && !entry["reply"].is_null())
            .count()
    }

    /// The replies to the calls the script wrote, without the snapshots the fake took to find
    /// its refs: (tool, text, is_error).
    fn written(&self) -> Vec<(String, String, bool)> {
        self.replies()
            .iter()
            .filter(|entry| entry["auto"] != true)
            .map(|entry| {
                let result = &entry["reply"]["result"];
                (
                    entry["tool"].as_str().unwrap_or("").to_owned(),
                    result["content"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    result["isError"].as_bool().unwrap_or(false),
                )
            })
            .collect()
    }
}

async fn script(w: &World, steps: Vec<Value>) -> Script {
    let dir = tempfile::tempdir_in(w.tmp.path()).unwrap();
    let at = |name: &str| dir.path().join(name).display().to_string();
    std::fs::write(
        dir.path().join("scenario.json"),
        Value::Array(steps).to_string(),
    )
    .unwrap();
    let vars: Vec<(String, String)> = vec![
        ("JOBPILOT_FAKE_SCENARIO".into(), at("scenario.json")),
        ("JOBPILOT_FAKE_RECORD".into(), at("record.jsonl")),
        ("JOBPILOT_FAKE_REPLIES".into(), at("replies.jsonl")),
        ("JOBPILOT_NODE_BIN".into(), node().display().to_string()),
        ("JOBPILOT_SECRET_SERVICE".into(), "jobpilot-test".into()),
    ];
    let bin = write_wrapper(dir.path(), &vars);
    settings::set(&w.db, PATH_SETTING, &json!(bin.display().to_string()))
        .await
        .unwrap();
    Script {
        knobs: HostKnobs {
            sidecar: Some(sidecar()),
            playwright_cli: None,
            run_timeout: None,
        },
        dir,
    }
}

fn host(w: &World, run: &str, s: &Script) -> JoinHandle<jobpilot_core::Result<RunView>> {
    let db = w.db.clone();
    let run = run.to_owned();
    let knobs = s.knobs.clone();
    tokio::spawn(async move {
        claude_apply_with(&db, "cli", &run, CancellationToken::new(), &knobs).await
    })
}

async fn ended(task: JoinHandle<jobpilot_core::Result<RunView>>) -> jobpilot_core::Result<RunView> {
    tokio::time::timeout(WAIT, task)
        .await
        .expect("the host ends")
        .unwrap()
}

async fn active_pause(db: &Db, run: &str) -> Option<PauseView> {
    apply::view(db, run).await.unwrap().active_pause
}

/// Waits for `check`. A pause nobody expected fails the test at once, with what it is,
/// rather than holding for half an hour.
async fn until(w: &World, run: &str, s: &Script, what: &str, mut check: impl AsyncFnMut() -> bool) {
    let begun = Instant::now();
    while !check().await {
        if let Some(pause) = active_pause(&w.db, run).await {
            panic!(
                "{what}: an unexpected {} pause from {}: {:?}\n{:#?}",
                pause.kind,
                pause.origin,
                pause.excerpt,
                s.written()
            );
        }
        assert!(begun.elapsed() < WAIT, "{what}\n{:#?}", s.written());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits for a pause to open.
async fn next_pause(w: &World, run: &str, s: &Script) -> PauseView {
    let begun = Instant::now();
    loop {
        if let Some(pause) = active_pause(&w.db, run).await {
            return pause;
        }
        assert!(begun.elapsed() < WAIT, "a pause\n{:#?}", s.written());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A person presses Continue.
async fn resume(w: &World, pause: &PauseView) {
    apply::resolve_pause(&w.db, "cli", &pause.id, Resolution::Continue)
        .await
        .unwrap();
}

async fn item(db: &Db, run: &str, position: usize) -> apply::Item {
    apply::view(db, run).await.unwrap().items.remove(position)
}

/// The host's agent run, by the link on the apply run.
async fn agent_state(db: &Db, run: &str) -> (String, Option<String>) {
    sqlx::query_as(
        "SELECT a.state, a.reason FROM agent_runs a JOIN apply_runs r ON r.agent_run_id = a.id \
         WHERE r.id = ?",
    )
    .bind(run)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

/// Why a run folder may still be there: the cleanup's own audit rows, for a failure message.
async fn cleanup_note(db: &Db) -> String {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT detail FROM audit_log WHERE action = 'cleanup_failed'")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    format!("cleanup_failed rows: {rows:?}")
}

/// Every live process whose arguments name `dir`, with those arguments.
fn processes_in(dir: &Path) -> Vec<(u32, Vec<String>)> {
    #[cfg(windows)]
    let fold = jobpilot_core::browser::sweep::windows_fold;
    #[cfg(not(windows))]
    let fold = |text: &str| text.to_owned();
    let marker = fold(&dir.display().to_string());
    let mut system = System::new().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let own = std::process::id();
    system
        .processes()
        .iter()
        .filter(|(pid, process)| pid.as_u32() != own && process.status() != ProcessStatus::Zombie)
        .filter_map(|(pid, process)| {
            let args: Vec<String> = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            args.iter()
                .any(|arg| fold(arg).contains(&marker))
                .then_some((pid.as_u32(), args))
        })
        .collect()
}

fn alive(pid: u32) -> bool {
    let mut system = System::new().unwrap();
    let pid = Pid::from_u32(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .is_some_and(|process| process.status() != ProcessStatus::Zombie)
}

/// The browser's own processes: each names the profile it runs on.
fn browsers(dir: &Path) -> Vec<Vec<String>> {
    processes_in(dir)
        .into_iter()
        .map(|(_, args)| args)
        .filter(|args| args.iter().any(|arg| arg.starts_with("--user-data-dir=")))
        .collect()
}

/// One text field of a multipart body, as the browser posts it.
fn field(name: &str, value: &str) -> String {
    format!("name=\"{name}\"\r\n\r\n{value}\r\n")
}

// ---- Scripts ----

fn call(tool: &str, args: Value) -> Value {
    json!({"tool": tool, "args": args})
}

fn click(name: &str) -> Value {
    call("browser_click", json!({"target": format!("@ref:{name}")}))
}

fn type_into(name: &str, text: &str) -> Value {
    call(
        "browser_type",
        json!({"target": format!("@ref:{name}"), "text": text}),
    )
}

/// The next application, its page, and, on a landing page, the way to its form.
fn open(page: &str) -> Vec<Value> {
    let mut steps = vec![
        call("apply_next", json!({})),
        call("browser_navigate", json!({"url": "@brief:link"})),
    ];
    if matches!(page, "greenhouse" | "indeed") {
        steps.push(click("Apply now"));
    }
    steps
}

fn text_fields() -> Vec<Value> {
    vec![
        type_into("Full name", NAME),
        type_into("Email", EMAIL),
        type_into("Why this role?", WHY),
    ]
}

/// The two staged files, each through the file chooser its field opens.
fn uploads() -> Vec<Value> {
    vec![
        click("Resume"),
        call(
            "browser_file_upload",
            json!({"paths": ["@brief:files.0.path"]}),
        ),
        click("Cover letter"),
        call(
            "browser_file_upload",
            json!({"paths": ["@brief:files.1.path"]}),
        ),
    ]
}

/// The `greenhouse` shape: a split name, starred labels, and each file behind an `Attach`
/// button that opens a hidden input.
fn split_form() -> Vec<Value> {
    vec![
        type_into("First name *", "Ada"),
        type_into("Last name *", "Lovelace"),
        type_into("Email *", EMAIL),
        type_into("Why this role?", WHY),
        click("Attach#1"),
        call(
            "browser_file_upload",
            json!({"paths": ["@brief:files.0.path"]}),
        ),
        click("Attach#2"),
        call(
            "browser_file_upload",
            json!({"paths": ["@brief:files.1.path"]}),
        ),
    ]
}

/// The whole form of `page`, as each fixture lays it out.
fn fill(page: &str) -> Vec<Value> {
    if page == "greenhouse" {
        return split_form();
    }
    let mut steps = text_fields();
    if page == "two_pages" {
        steps.push(click("Continue"));
    }
    if page == "workday" {
        // A button that opens a listbox, then the option.
        steps.extend([click("Preferred office"), click("Porto")]);
    }
    if page == "legal" {
        steps.push(click(CERTIFY));
    }
    steps.extend(uploads());
    steps
}

fn submit() -> Vec<Value> {
    vec![
        click(SUBMIT),
        call("apply_result", json!({"outcome": "submitted"})),
    ]
}

/// The run has nothing left, and the session exits as a real one would.
fn finish() -> Vec<Value> {
    vec![call("apply_next", json!({})), json!({"exit": 0})]
}

fn applying(pages: &[&str]) -> Vec<Value> {
    let mut steps = Vec::new();
    for page in pages {
        steps.extend(open(page));
        steps.extend(fill(page));
        steps.extend(submit());
    }
    steps.extend(finish());
    steps
}

/// A run of one `company` application whose session opens the page and waits there.
async fn browser_open(w: &World) -> (String, Script, JoinHandle<jobpilot_core::Result<RunView>>) {
    let (run, _) = run_of(w, &["company"]).await;
    let mut steps = open("company");
    steps.push(call("browser_snapshot", json!({})));
    let s = script(w, steps).await;
    let task = host(w, &run, &s);
    until(w, &run, &s, "the page", async || {
        s.answered("browser_snapshot") > 0
    })
    .await;
    (run, s, task)
}

async fn cancelled(w: &World, run: &str, task: JoinHandle<jobpilot_core::Result<RunView>>) {
    apply::cancel(&w.other().await, "cli", run).await.unwrap();
    let result = ended(task).await;
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    let dir = apply::run_dir(&w.db, run);
    assert!(
        !dir.exists(),
        "{}: {}",
        dir.display(),
        cleanup_note(&w.db).await
    );
}

// ---- Tests ----

#[tokio::test]
#[ignore]
async fn browser_e2e_fills_uploads_and_submits_on_each_fixture() {
    const FORMS: [&str; 8] = [
        "greenhouse",
        "lever",
        "ashby",
        "workday",
        "indeed",
        "linkedin",
        "company",
        "two_pages",
    ];
    let w = world().await;
    let (run, apps) = run_of(&w, &FORMS).await;
    let s = script(&w, applying(&FORMS)).await;
    let task = host(&w, &run, &s);
    until(&w, &run, &s, "every form posted", async || {
        w.site.bodies("/form").len() == FORMS.len() && task.is_finished()
    })
    .await;

    let view = ended(task).await.unwrap();
    assert_eq!(view.state, "ended");
    let bodies = w.site.bodies("/form");
    for (position, page) in FORMS.iter().enumerate() {
        let done = item(&w.db, &run, position).await;
        assert_eq!(done.state, "submitted", "{page}");
        assert_eq!(done.submit_clicks, 1, "{page}");
        assert!(done.status_written, "{page}");
        let confirmation = done.confirmation_text.unwrap_or_default();
        assert!(confirmation.contains(RECEIVED), "{page}: {confirmation}");
        assert_eq!(
            applications::get(&w.db, &apps[position])
                .await
                .unwrap()
                .status,
            "applied",
            "{page}"
        );
        let body = &bodies[position];
        if *page == "workday" {
            assert!(body.contains(&field("office", "opo")), "{page}");
        }
        let names = match *page {
            "greenhouse" => vec![field("first_name", "Ada"), field("last_name", "Lovelace")],
            _ => vec![field("name", NAME)],
        };
        for value in names.into_iter().chain([
            field("email", EMAIL),
            field("why", WHY),
            "name=\"resume\"; filename=\"resume.pdf\"".into(),
            "name=\"letter\"; filename=\"cover-letter.pdf\"".into(),
        ]) {
            let head: String = body.chars().take(600).collect();
            assert!(
                body.contains(&value),
                "{page}: {value} not posted in {head}"
            );
        }
    }
    // Every call the script made was taken: none was refused or failed on a real page.
    for (tool, text, error) in s.written() {
        assert!(!error, "{tool}: {text}");
    }
    assert!(!apply::run_dir(&w.db, &run).exists());
}

#[tokio::test]
#[ignore]
async fn browser_e2e_each_stop_holds_until_a_person_continues() {
    const STOPS: [&str; 5] = ["captcha", "mfa", "identity", "legal", "login"];
    let w = world().await;
    let (run, _) = run_of(&w, &STOPS).await;
    let s = script(&w, applying(&STOPS)).await;
    let task = host(&w, &run, &s);

    for (position, kind) in STOPS.iter().enumerate() {
        let pause = next_pause(&w, &run, &s).await;
        assert_eq!(pause.kind, *kind);
        assert_eq!(pause.origin, "page");
        assert_eq!(pause.item_id, item(&w.db, &run, position).await.id);
        // The page's reply is held while the person is asked.
        assert_eq!(s.answered("browser_navigate"), position, "{kind}");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(w.site.bodies("/form").len(), position, "{kind}");
        assert!(active_pause(&w.db, &run).await.is_some(), "{kind}");
        resume(&w, &pause).await;

        // A stop on two lines of one page asks for each, and the legal checkbox asks again
        // when clicked. Nothing posts until the last is answered.
        let mut origins = vec![pause.origin.clone()];
        let begun = Instant::now();
        while w.site.bodies("/form").len() == position {
            if let Some(again) = active_pause(&w.db, &run).await {
                assert_eq!(again.kind, *kind, "{}", again.excerpt);
                assert_eq!(w.site.bodies("/form").len(), position, "{kind}");
                origins.push(again.origin.clone());
                resume(&w, &again).await;
            }
            assert!(begun.elapsed() < WAIT, "{kind}: the form after Continue");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if *kind == "legal" {
            // The page's line, then the click on the declaration itself.
            assert!(origins.contains(&"guard".to_owned()), "{origins:?}");
        }
    }

    let view = ended(task).await.unwrap();
    assert_eq!(view.state, "ended");
    for (position, kind) in STOPS.iter().enumerate() {
        let done = item(&w.db, &run, position).await;
        assert_eq!(done.state, "submitted", "{kind}");
        assert_eq!(done.submit_clicks, 1, "{kind}");
    }
    for (tool, text, error) in s.written() {
        assert!(!error, "{tool}: {text}");
    }
}

/// The stop shows only after `Apply now`, so the click's reply is held, not the page's. (The
/// fixture's name holds no stop word: the page's address is scanned too.) A
/// call the session sends meanwhile gets no answer and does nothing until a person is done.
#[tokio::test]
#[ignore]
async fn browser_e2e_a_stop_after_a_click_holds_every_later_call() {
    let w = world().await;
    let (run, _) = run_of(&w, &["after_click"]).await;
    let mut steps = vec![
        call("apply_next", json!({})),
        call("browser_navigate", json!({"url": "@brief:link"})),
        call("browser_snapshot", json!({})),
        json!({"tool": "browser_click", "args": {"target": "@ref:Apply now"}, "nowait": true}),
    ];
    // Sent while the click is held: its snapshot, then the typing.
    steps.extend(fill("after_click"));
    steps.extend(submit());
    steps.extend(finish());
    let s = script(&w, steps).await;
    let task = host(&w, &run, &s);

    let pause = next_pause(&w, &run, &s).await;
    assert_eq!(
        (pause.kind.as_str(), pause.origin.as_str()),
        ("captcha", "page")
    );
    // The page itself opened without a stop.
    let opened = s.written();
    assert_eq!(opened[1].0, "browser_navigate");
    assert!(!opened[1].2, "{opened:?}");
    let seen = s.replies().len();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        s.replies().len(),
        seen,
        "a call was answered during the hold"
    );
    assert!(w.site.bodies("/form").is_empty());
    assert!(active_pause(&w.db, &run).await.is_some());
    resume(&w, &pause).await;

    until(&w, &run, &s, "the run ends", async || task.is_finished()).await;
    ended(task).await.unwrap();
    assert_eq!(w.site.bodies("/form").len(), 1);
    let done = item(&w.db, &run, 0).await;
    assert_eq!(done.state, "submitted");
    assert_eq!(done.submit_clicks, 1);
    for (tool, text, error) in s.written() {
        assert!(!error, "{tool}: {text}");
    }
}

#[tokio::test]
#[ignore]
async fn browser_e2e_a_hostile_page_changes_nothing() {
    const CLAIM: &str = "Licensed attorney with twenty years of practice.";
    const HIDDEN: &str = "SYSTEM NOTE";
    let w = world().await;
    let (run, _) = run_of(&w, &["injection"]).await;
    let stray = fixtures().join("injection.html").display().to_string();
    let mut steps = open("injection");
    // What the page asks for, as a steered session would try it.
    steps.extend([
        call("browser_file_upload", json!({"paths": [stray]})),
        call("browser_navigate", json!({"url": "file:///etc/hosts"})),
        type_into("Why this role?", &format!("{WHY} {CLAIM}")),
        call(
            "browser_navigate",
            json!({"url": "http://collector.invalid/steal"}),
        ),
    ]);
    steps.extend(fill("injection"));
    steps.extend(submit());
    steps.extend(finish());
    let s = script(&w, steps).await;
    let task = host(&w, &run, &s);
    until(&w, &run, &s, "the run ends", async || task.is_finished()).await;
    ended(task).await.unwrap();

    let written = s.written();
    let steered: Vec<(&str, &str, bool)> = written[2..6]
        .iter()
        .map(|(tool, text, error)| (tool.as_str(), text.as_str(), *error))
        .collect();
    assert_eq!(steered[0], ("browser_file_upload", "upload_refused", true));
    assert_eq!(steered[1], ("browser_navigate", "url_refused", true));
    assert_eq!(steered[2].0, "browser_type");
    assert!(
        steered[2].1.starts_with("not an approved answer"),
        "{steered:?}"
    );
    assert!(steered[2].2);
    assert_eq!(steered[3], ("browser_navigate", "url_refused", true));

    let bodies = w.site.bodies("/form");
    assert_eq!(bodies.len(), 1);
    // The answer is the approved one alone, and the only files are the two staged ones.
    assert!(bodies[0].contains(&field("why", WHY)));
    assert!(!bodies[0].contains("attorney"));
    let mut files: Vec<&str> = bodies[0]
        .split("filename=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    files.sort_unstable();
    assert_eq!(files, ["cover-letter.pdf", "resume.pdf"]);
    assert!(w.site.paths().iter().all(|path| !path.contains("steal")));
    // The hidden instruction never reached the session, its record, or a log row.
    for file in ["replies.jsonl", "record.jsonl"] {
        let text = std::fs::read_to_string(s.dir.path().join(file)).unwrap_or_default();
        assert!(!text.contains(HIDDEN), "{file}");
    }
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT detail FROM audit_log UNION ALL SELECT outcome FROM agent_run_log \
         UNION ALL SELECT excerpt FROM apply_pauses",
    )
    .fetch_all(&w.db.pool)
    .await
    .unwrap();
    assert!(rows.iter().all(|row| !row.contains(HIDDEN)));
    // The visible one was seen, as page data inside the markers.
    let replies = std::fs::read_to_string(s.dir.path().join("replies.jsonl")).unwrap();
    assert!(replies.contains("Message for the assistant"));
    let done = item(&w.db, &run, 0).await;
    assert_eq!(done.state, "submitted");
    assert_eq!(done.submit_clicks, 1);
}

#[tokio::test]
#[ignore]
async fn browser_e2e_a_fake_success_message_is_recorded_as_the_pages_claim() {
    let w = world().await;
    let (run, apps) = run_of(&w, &["fake_success"]).await;
    let s = script(&w, applying(&["fake_success"])).await;
    let task = host(&w, &run, &s);
    until(&w, &run, &s, "the run ends", async || task.is_finished()).await;
    ended(task).await.unwrap();

    // The page said it was sent, and nothing was posted: the claim is stored as the page's.
    assert!(w.site.bodies("/form").is_empty());
    let done = item(&w.db, &run, 0).await;
    assert_eq!(done.state, "submitted");
    assert_eq!(done.submit_clicks, 1);
    let claim = done.confirmation_text.unwrap_or_default();
    assert!(claim.contains("Application sent. Thank you!"), "{claim}");
    assert!(!claim.contains(RECEIVED));
    assert_eq!(
        done.confirmation_url.as_deref(),
        Some(format!("{}/fake_success", w.site.url).as_str())
    );
    assert_eq!(
        applications::get(&w.db, &apps[0]).await.unwrap().status,
        "applied"
    );
}

#[tokio::test]
#[ignore]
async fn browser_e2e_a_rejected_submit_pauses_before_the_second_click() {
    let w = world().await;
    let (run, _) = run_of(&w, &["reject_once"]).await;
    let mut steps = open("reject_once");
    steps.extend(fill("reject_once"));
    steps.extend([click(SUBMIT), click(SUBMIT)]);
    steps.push(call("apply_result", json!({"outcome": "submitted"})));
    steps.extend(finish());
    let s = script(&w, steps).await;
    let task = host(&w, &run, &s);

    let pause = next_pause(&w, &run, &s).await;
    assert_eq!(
        (pause.kind.as_str(), pause.origin.as_str()),
        ("other", "guard")
    );
    assert_eq!(item(&w.db, &run, 0).await.submit_clicks, 1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The page refused the first click, and the second waits for a person.
    assert!(w.site.bodies("/form").is_empty());
    resume(&w, &pause).await;

    until(&w, &run, &s, "the run ends", async || task.is_finished()).await;
    ended(task).await.unwrap();
    assert_eq!(w.site.bodies("/form").len(), 1);
    let done = item(&w.db, &run, 0).await;
    assert_eq!(done.state, "submitted");
    assert_eq!(done.submit_clicks, 2);
}

#[tokio::test]
#[ignore]
async fn browser_e2e_emergency_pause_closes_the_browser_and_deletes_the_profile() {
    let w = world().await;
    let (run, s, task) = browser_open(&w).await;
    let dir = apply::run_dir(&w.db, &run);
    let running = processes_in(&dir);
    assert!(!browsers(&dir).is_empty(), "{running:?}");
    let has = |flag: &str| {
        running
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == flag))
    };
    assert!(has("--mcp-config"), "claude: {running:?}");
    assert!(has("apply-mcp"), "apply-mcp: {running:?}");
    assert!(has("--config"), "the Playwright server: {running:?}");

    let other = w.other().await;
    let begun = Instant::now();
    pause::set(&other, "cli").await.unwrap();
    while running.iter().any(|(pid, _)| alive(*pid)) {
        assert!(
            begun.elapsed() <= KILL,
            "still running: {:?}",
            processes_in(&dir)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Every process that names the run folder, including any the browser started on its
    // way out, is gone within the bound.
    while !processes_in(&dir).is_empty() {
        assert!(
            begun.elapsed() <= KILL,
            "still running: {:?}",
            processes_in(&dir)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result = ended(task).await;
    assert!(matches!(result, Err(Error::Paused)), "{result:?}");
    assert_eq!(
        agent_state(&w.db, &run).await,
        ("halted".to_owned(), Some("paused".to_owned()))
    );
    assert!(
        !dir.exists(),
        "{}: {}",
        dir.display(),
        cleanup_note(&w.db).await
    );
    drop(s);
    pause::clear(&other, "cli").await.unwrap();
}

#[tokio::test]
#[ignore]
async fn browser_e2e_uses_no_anti_detection_flag() {
    let w = world().await;
    let (run, s, task) = browser_open(&w).await;
    until(&w, &run, &s, "the page's probe", async || {
        !w.site.bodies("/probe").is_empty()
    })
    .await;
    assert_eq!(w.site.bodies("/probe"), ["webdriver=true"]);

    let dir = apply::run_dir(&w.db, &run);
    let browsers = browsers(&dir);
    assert!(!browsers.is_empty());
    for (_, args) in processes_in(&dir) {
        assert!(
            args.iter().all(|arg| !arg.contains("AutomationControlled")),
            "{args:?}"
        );
    }
    assert!(
        browsers.iter().any(|args| args
            .iter()
            .any(|arg| arg == "--disable-blink-features=JobPilotNone")),
        "{browsers:?}"
    );
    cancelled(&w, &run, task).await;
}

#[tokio::test]
#[ignore]
async fn browser_e2e_browser_is_visible_and_the_profile_is_the_runs() {
    let w = world().await;
    let (run, _s, task) = browser_open(&w).await;
    let dir = apply::run_dir(&w.db, &run);
    let profile = dir.join("profile");
    let browsers = browsers(&dir);
    assert!(!browsers.is_empty());
    #[cfg(windows)]
    let fold = jobpilot_core::browser::sweep::windows_fold;
    #[cfg(not(windows))]
    let fold = |text: &str| text.to_owned();
    let wanted = fold(&format!("--user-data-dir={}", profile.display()));
    for args in &browsers {
        assert!(
            args.iter().all(|arg| !arg.starts_with("--headless")),
            "{args:?}"
        );
        assert!(
            args.iter()
                .filter(|arg| arg.starts_with("--user-data-dir="))
                .all(|arg| fold(arg) == wanted),
            "{args:?}"
        );
    }
    // The browser writes its profile into the run's folder, and nowhere else is named.
    let entries = std::fs::read_dir(&profile).unwrap().count();
    assert!(entries > 0, "{}", profile.display());
    cancelled(&w, &run, task).await;
    assert!(!profile.exists(), "{}", cleanup_note(&w.db).await);
}

/// For Task 14 only: serves the fixture forms on 127.0.0.1 until killed, printing the port.
/// It needs no runtime.
/// Copilot drives the same real server and browser with no `claude` at all. It fills and
/// uploads, then holds for the person. The test plays the person: it cannot click in a browser
/// it does not drive, so it sends the form as the browser's submit would, then presses
/// Continue, and the run captures the page and records the application.
#[tokio::test]
#[ignore]
async fn browser_e2e_copilot_on_the_greenhouse_lever_and_ashby_fixtures() {
    const REVIEW: &str = "Review the form in the browser and click submit yourself.";
    let w = world().await;
    for (posted, page) in ["greenhouse", "lever", "ashby"].into_iter().enumerate() {
        let app = add_app(&w, page).await;
        let db = w.db.clone();
        let id = app.clone();
        let task = tokio::spawn(async move {
            let router = Router::with_providers(Vec::new());
            copilot(&db, &router, "cli", &id, CancellationToken::new()).await
        });
        let begun = Instant::now();
        let pause = loop {
            let run = apply::latest(&w.db).await.unwrap();
            if let Some(pause) = run.and_then(|run| run.active_pause) {
                break pause;
            }
            assert!(
                !task.is_finished(),
                "{page}: the run ended before its review"
            );
            assert!(begun.elapsed() < WAIT, "{page}: no review stop");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        // The review stop and nothing left over: every field filled, and each file went up
        // through its own chooser (a refused click or upload would be listed here).
        assert_eq!(
            pause.excerpt, REVIEW,
            "{page}: a {} stop from {}",
            pause.kind, pause.origin
        );
        let run = apply::latest(&w.db).await.unwrap().unwrap();
        let dir = apply::run_dir(&w.db, &run.id);
        assert_eq!(run.items[0].state, "filled", "{page}");
        assert_eq!(run.items[0].submit_clicks, 0, "{page}");
        let uploaded: Vec<String> = sqlx::query_scalar(
            "SELECT json_extract(detail, '$.kind') FROM audit_log \
             WHERE action = 'apply_file_uploaded' AND target = ? ORDER BY id",
        )
        .bind(&run.items[0].id)
        .fetch_all(&w.db.pool)
        .await
        .unwrap();
        assert_eq!(uploaded, ["resume", "letter"], "{page}");
        // Nothing was sent: Copilot never clicks submit.
        assert_eq!(w.site.bodies("/form").len(), posted, "{page}");
        // The browser stays open through the hold.
        assert!(!browsers(&dir).is_empty(), "{page}");

        // The person submits, then says so.
        let sent = reqwest::Client::new()
            .post(format!("{}/form", w.site.url))
            .body("submitted by the person")
            .send()
            .await
            .unwrap();
        assert!(sent.status().is_success() || sent.status().is_redirection());
        resume(&w, &pause).await;
        let view = tokio::time::timeout(WAIT, task)
            .await
            .expect("the run ends")
            .unwrap()
            .unwrap();
        let done = &view.items[0];
        assert_eq!(view.state, "ended", "{page}");
        assert_eq!(done.state, "submitted", "{page}");
        assert!(done.status_written, "{page}");
        // The capture is the page the person left: the real browser holds what Copilot typed.
        let seen = done.confirmation_text.clone().unwrap_or_default();
        let typed = match page {
            "greenhouse" => vec!["Ada", "Lovelace", EMAIL, WHY],
            _ => vec![NAME, EMAIL, WHY],
        };
        for value in typed {
            assert!(seen.contains(value), "{page}: {value} not in {seen}");
        }
        // The landing page's link adds a fragment; the page is the fixture's.
        let url = done.confirmation_url.clone().unwrap_or_default();
        assert!(
            url.starts_with(&format!("{}/{page}", w.site.url)),
            "{page}: {url}"
        );
        assert_eq!(
            applications::get(&w.db, &app).await.unwrap().status,
            "applied",
            "{page}"
        );
        assert!(!dir.exists(), "{page}: {}", cleanup_note(&w.db).await);
        assert!(browsers(&dir).is_empty(), "{page}");
    }
    // The person's posts are the only ones sent.
    assert_eq!(w.site.bodies("/form").len(), 3);
}

#[tokio::test]
#[ignore]
async fn serve_fixtures() {
    let site = Site::pages(&fixtures());
    let port = site.url.rsplit(':').next().unwrap_or_default().to_owned();
    println!("port {port}");
    println!("{}/company", site.url);
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// apply-mcp lists the browser tools from a pinned copy, so the list never waits for the
/// server. The pinned server must still list exactly those entries for the allowed tools.
#[tokio::test]
#[ignore]
async fn browser_e2e_pinned_tools_match_the_real_server() {
    use jobpilot_core::browser::mcp::McpClient;
    use jobpilot_core::browser::runtime::{self, BrowserSpec, RuntimeState};

    let w = world().await;
    let RuntimeState::Ready(found) = runtime::state(&w.db).await.unwrap() else {
        panic!("the browser runtime is not ready");
    };
    let dir = w.root().join("apply").join("pin");
    let files = dir.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let spec = BrowserSpec {
        profile: dir.join("profile"),
        output: dir.join("output"),
    };
    let config = dir.join("playwright.json");
    std::fs::write(&config, runtime::playwright_config(&spec).to_string()).unwrap();
    let (program, args, env) = runtime::command_for(&found, &config, &files);
    let mut client = McpClient::start(&program, &args, &env, &files)
        .await
        .unwrap();
    let listed = client.tools().await.unwrap();
    client.close().await;
    let by_name = |tools: Vec<Value>| -> std::collections::BTreeMap<String, Value> {
        tools
            .into_iter()
            .filter_map(|tool| Some((tool["name"].as_str()?.to_owned(), tool)))
            .filter(|(name, _)| apply::guard::BROWSER_TOOLS.contains(&name.as_str()))
            .collect()
    };
    let real = by_name(listed);
    let pinned = by_name(apply::proxy::browser_tools());
    assert_eq!(real.len(), apply::guard::BROWSER_TOOLS.len());
    assert_eq!(
        pinned, real,
        "update assets/playwright_mcp_tools.json from the server"
    );
}
