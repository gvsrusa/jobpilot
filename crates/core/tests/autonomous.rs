//! Autonomous mode in front of the fake Playwright server. It ships as a gate: with the real
//! register and `VERIFIED` nothing is eligible, and with a doctored register, a verified site,
//! and a stored grant it fills, checks, and clicks submit exactly once. No browser, no model,
//! no network. Keychain paths run on keyring's in-memory mock, so this is its own binary.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use jobpilot_core::agents::autonomous_with;
use jobpilot_core::ai::CancellationToken;
use jobpilot_core::ai::register::REGISTER;
use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::apply::proxy::Knobs;
use jobpilot_core::apply::{self, PauseView, Resolution, RunView};
use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::permissions::{self, Site, VERIFIED};
use jobpilot_core::profile::{self, ProfileBasic};
use jobpilot_core::render::{Entry, ResumeDoc, Section};
use jobpilot_core::{AppPaths, Db, Error, Result, drafts, memory};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(30);
const LINK: &str = "https://jobs.lever.co/acme/1";
const FORM_URL: &str = "https://jobs.lever.co/acme/1/apply";
const WHY: &str = "I enjoy building reliable tools, and I agree with your mission.";
const PHONE: &str = "+351 555 0100";
const SUBMIT: &str = "e14";
const THANKS: &str = "### Page\n- Page URL: https://jobs.lever.co/acme/1/thanks\n### Snapshot\n\
                      ```yaml\n- heading \"Thank you for applying\" [ref=e1]\n```\n";

/// Lever's submit row read as `allowed`: what a register would say once a vendor allowed it.
const ALLOWED_LEVER: &str = "| Integration | Operation | Primary source URL | Date read | Quoted \
clause | Verdict | Consequence |\n|---|---|---|---|---|---|---|\n| Lever | Submit the hosted \
form with no human | https://example.invalid | 2026-11-01 | No clause found. | `allowed` | x |\n";
const LEVER: &[Site] = &[Site::Lever];

fn mock_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}

/// The form as `browser_snapshot` replies with it. `values` are what its text fields show.
fn page(url: &str, values: [&str; 3], extra: &str) -> String {
    let shown = |value: &str| match value.is_empty() {
        true => String::new(),
        false => format!(": {value}"),
    };
    format!(
        "### Page
- Page URL: {url}
- Page Title: Apply
### Snapshot
```yaml
- generic [ref=e1]:
  - textbox \"Full name *\" [ref=e3]{}
  - textbox \"Email *\" [ref=e4]{}
  - textbox \"Why this role?\" [ref=e5]{}
  - button \"Resume\" [ref=e12]
  - button \"Cover letter\" [ref=e15]
{extra}  - button \"Submit application\" [ref={SUBMIT}]
```
",
        shown(values[0]),
        shown(values[1]),
        shown(values[2]),
    )
}

/// Before the fill: the email is pre-filled by the site.
fn blank(extra: &str) -> String {
    page(FORM_URL, ["", "prefill@x.io", ""], extra)
}

/// After the fill: the page shows what the run wrote.
fn filled(extra: &str) -> String {
    page(FORM_URL, ["Ada Lovelace", "ada@example.com", WHY], extra)
}

const PHONE_FIELD: &str = "  - textbox \"Phone\" [ref=e6]\n";

struct World {
    tmp: TempDir,
    db: Arc<Db>,
}

async fn world() -> World {
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let KeyState::Unlocked { .. } = crypto::state(&db).await.unwrap() else {
        panic!("the mock keychain did not unlock");
    };
    let basic = ProfileBasic {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();
    for (question, text) in [
        ("Why this role?", WHY),
        ("What is your phone number?", PHONE),
    ] {
        let question = memory::add_question(&db, question, None).await.unwrap();
        let answer = memory::add_answer(&db, &question.id, text, None)
            .await
            .unwrap();
        memory::approve(&db, "cli", &answer.id, true).await.unwrap();
    }
    World {
        tmp,
        db: Arc::new(db),
    }
}

fn doc(title: &str) -> ResumeDoc {
    ResumeDoc {
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

async fn add_app(db: &Db, link: &str) -> String {
    let resume = uploaded(db, "resume", "Acme resume").await;
    let letter = uploaded(db, "cover_letter", "Acme letter").await;
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        link: link.into(),
        resume_draft_id: Some(resume),
        letter_draft_id: Some(letter),
        ..ApplicationInput::default()
    };
    applications::add(db, "cli", input).await.unwrap().id
}

/// The fake server, through a wrapper that sets its variables: `command_for` passes only an
/// allow-listed environment.
struct Mock {
    calls: PathBuf,
    cli: PathBuf,
    /// The page from the fill on, which a test may change while a stop is open.
    after: PathBuf,
}

/// `before` until the fill, `after` from the fill, `thanks` once the submit ref is clicked.
/// With `die`, the server exits on that click with no reply.
fn mock(w: &World, before: &str, after: &str, die: bool) -> Mock {
    mock_with(
        w,
        Pages {
            before,
            after,
            die,
            ..Pages::default()
        },
    )
}

/// What the fake server shows: `late` from the `n`th snapshot after the fill on.
#[derive(Default)]
struct Pages<'a> {
    before: &'a str,
    after: &'a str,
    thanks: Option<&'a str>,
    late: Option<(&'a str, u32)>,
    die: bool,
}

fn mock_with(w: &World, pages: Pages<'_>) -> Mock {
    let Pages {
        before,
        after,
        thanks,
        late,
        die,
    } = pages;
    let dir = w.tmp.path().join("mock");
    std::fs::create_dir_all(&dir).unwrap();
    let snapshot = dir.join("snapshot.yml");
    let after_fill = dir.join("after_fill.yml");
    let after_click = dir.join("after_click.yml");
    let calls = dir.join("calls.jsonl");
    std::fs::write(&snapshot, before).unwrap();
    std::fs::write(&after_fill, after).unwrap();
    std::fs::write(&after_click, thanks.unwrap_or(THANKS)).unwrap();
    std::fs::write(&calls, "").unwrap();
    let late_file = dir.join("late.yml");
    let late = match late {
        Some((page, at)) => {
            std::fs::write(&late_file, page).unwrap();
            format!(
                "process.env.MOCK_LATE = {};\nprocess.env.MOCK_LATE_AT = \"{at}\";\n",
                json!(late_file.display().to_string())
            )
        }
        None => String::new(),
    };
    let server = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-mcp/server.mjs");
    let url = format!("file://{}", server.display()).replace('\\', "/");
    let die = match die {
        true => "process.env.MOCK_DIE_ON_CLICK = \"1\";\n",
        false => "",
    };
    let script = format!(
        "process.env.MOCK_SNAPSHOT = {};\nprocess.env.MOCK_CALLS = {};\n\
         process.env.MOCK_AFTER_FILL = {};\nprocess.env.MOCK_AFTER_CLICK = {};\n\
         process.env.MOCK_CLICK_REF = {};\n{die}{late}await import({});\n",
        json!(snapshot.display().to_string()),
        json!(calls.display().to_string()),
        json!(after_fill.display().to_string()),
        json!(after_click.display().to_string()),
        json!(SUBMIT),
        json!(url),
    );
    let cli = dir.join("wrapper.mjs");
    std::fs::write(&cli, script).unwrap();
    Mock {
        calls,
        cli,
        after: after_fill,
    }
}

impl Mock {
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.calls)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call.get("name").is_some())
            .collect()
    }

    fn submit_clicks(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call["name"] == "browser_click" && call["args"]["target"] == SUBMIT)
            .count()
    }

    /// Every click's target, in order.
    fn clicks(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter(|call| call["name"] == "browser_click")
            .map(|call| call["args"]["target"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    fn knobs(&self) -> Knobs {
        Knobs {
            hold: None,
            playwright_cli: Some(self.cli.clone()),
        }
    }
}

fn run(
    w: &World,
    register: &'static str,
    verified: &'static [Site],
    app: &str,
    m: &Mock,
) -> JoinHandle<Result<RunView>> {
    let db = w.db.clone();
    let app = app.to_owned();
    let knobs = m.knobs();
    tokio::spawn(async move {
        autonomous_with(
            &db,
            register,
            verified,
            "cli",
            &app,
            CancellationToken::new(),
            &knobs,
        )
        .await
    })
}

async fn ended(task: JoinHandle<Result<RunView>>) -> Result<RunView> {
    tokio::time::timeout(WAIT, task)
        .await
        .expect("the run ends")
        .unwrap()
}

async fn open_pause(db: &Db) -> PauseView {
    let begun = Instant::now();
    loop {
        if let Some(run) = apply::latest(db).await.unwrap()
            && let Some(pause) = run.active_pause
        {
            return pause;
        }
        assert!(begun.elapsed() < WAIT, "no pause opened");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn status(db: &Db, app: &str) -> String {
    applications::get(db, app).await.unwrap().status
}

fn refused(result: &Result<RunView>) -> Option<&str> {
    match result {
        Err(Error::Refused(why)) => Some(why.as_str()),
        _ => None,
    }
}

async fn latest(db: &Db) -> RunView {
    apply::latest(db).await.unwrap().expect("a run")
}

// ---- Tests ----

#[tokio::test]
async fn autonomous_is_refused_on_every_site_today_with_no_child_started() {
    for link in [
        "https://boards.greenhouse.io/acme/jobs/1",
        LINK,
        "https://jobs.ashbyhq.com/acme/1",
        "https://acme.wd1.myworkdayjobs.com/en-US/careers/job/1",
        "https://www.indeed.com/viewjob?jk=1",
        "https://www.linkedin.com/jobs/view/1",
        "https://careers.example.com/jobs/1",
    ] {
        let w = world().await;
        // Even with every connector granted.
        for connector in [
            "greenhouse",
            "lever",
            "ashby",
            "workday",
            "indeed",
            "linkedin",
        ] {
            permissions::grant(&w.db, "cli", connector).await.unwrap();
        }
        let app = add_app(&w.db, link).await;
        let m = mock(&w, &blank(""), &filled(""), false);
        let result = ended(run(&w, REGISTER, VERIFIED, &app, &m)).await;
        assert!(
            refused(&result).is_some_and(|why| why.starts_with("policy_")),
            "{link}: {result:?}"
        );
        // Nothing started: no server was spoken to, no run row, no run folder.
        assert!(
            std::fs::read_to_string(&m.calls).unwrap().is_empty(),
            "{link}"
        );
        assert!(apply::latest(&w.db).await.unwrap().is_none(), "{link}");
        assert!(
            std::fs::read_dir(w.db.paths().apply())
                .map(|entries| entries.count() == 0)
                .unwrap_or(true),
            "{link}"
        );
        assert_eq!(status(&w.db, &app).await, "saved", "{link}");
    }
}

#[tokio::test]
async fn a_grant_alone_does_not_make_a_site_eligible() {
    let w = world().await;
    let app = add_app(&w.db, LINK).await;
    let m = mock(&w, &blank(""), &filled(""), false);
    // A grant, but the register and the verified list as they are.
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let result = ended(run(&w, REGISTER, VERIFIED, &app, &m)).await;
    assert_eq!(refused(&result), Some("policy_unknown"), "{result:?}");
    // A grant and an allowed row, but no verification.
    let result = ended(run(&w, ALLOWED_LEVER, &[], &app, &m)).await;
    assert_eq!(refused(&result), Some("policy_unknown"), "{result:?}");
    // A row and verification, but the grant is gone.
    permissions::revoke(&w.db, "cli", "lever").await.unwrap();
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &m)).await;
    assert_eq!(refused(&result), Some("not_authorized"), "{result:?}");
    // Another connector's grant does not carry Lever.
    permissions::grant(&w.db, "cli", "ashby").await.unwrap();
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &m)).await;
    assert_eq!(refused(&result), Some("not_authorized"), "{result:?}");
    assert!(m.calls().is_empty());
    assert!(apply::latest(&w.db).await.unwrap().is_none());
    // Only a known connector can be granted.
    let unknown = permissions::grant(&w.db, "cli", "acme").await.unwrap_err();
    assert!(matches!(unknown, Error::Validation(_)), "{unknown:?}");
}

#[tokio::test]
async fn with_row_verification_and_grant_it_submits_once() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let m = mock(&w, &blank(""), &filled(""), false);
    let view = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &m))
        .await
        .unwrap();

    assert_eq!(m.submit_clicks(), 1, "{:?}", m.calls());
    assert_eq!(view.mode, "autonomous");
    assert_eq!(view.state, "ended");
    let item = &view.items[0];
    assert_eq!(item.state, "submitted");
    assert_eq!(item.submit_clicks, 1);
    assert!(item.status_written);
    assert!(
        item.confirmation_text
            .as_deref()
            .unwrap_or("")
            .contains("Thank you for applying")
    );
    assert_eq!(
        item.confirmation_url.as_deref(),
        Some("https://jobs.lever.co/acme/1/thanks")
    );
    assert_eq!(status(&w.db, &app).await, "applied");
    // The fill came before the click, with the rules and the approved answer.
    let names: Vec<String> = m
        .calls()
        .iter()
        .map(|call| call["name"].as_str().unwrap().to_owned())
        .collect();
    let fill = names.iter().position(|n| n == "browser_fill_form").unwrap();
    let click = m
        .calls()
        .iter()
        .position(|call| call["name"] == "browser_click" && call["args"]["target"] == SUBMIT)
        .unwrap();
    assert!(fill < click);
    assert_eq!(
        names.iter().filter(|n| *n == "browser_file_upload").count(),
        2
    );
    let detail: String = sqlx::query_scalar(
        "SELECT detail FROM audit_log WHERE action = 'application_submitted' AND target = ?",
    )
    .bind(&app)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["mode"], "autonomous");
    assert_eq!(detail["agent"], "browser_application");
    let created: String = sqlx::query_scalar(
        "SELECT json_extract(detail, '$.mode') FROM audit_log WHERE action = 'apply_run_created'",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(created, "autonomous");
    assert!(!apply::run_dir(&w.db, &view.id).exists());
}

#[tokio::test]
async fn a_lost_reply_after_the_click_is_uncertain_and_never_clicks_again() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let m = mock(&w, &blank(""), &filled(""), true);
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &m)).await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(m.submit_clicks(), 1);
    let view = latest(&w.db).await;
    assert_eq!(view.state, "ended");
    let item = &view.items[0];
    assert_eq!(item.state, "uncertain");
    assert_eq!(item.submit_clicks, 1);
    assert!(item.status_written);
    // No status from doubt: a note tells the owner to check the site.
    assert_eq!(status(&w.db, &app).await, "saved");
    let noted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_events WHERE application_id = ? \
         AND note = 'Check the site. JobPilot will not try again.'",
    )
    .bind(&app)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(noted, 1);

    // A new run on the same application does not click again.
    let again = mock(&w, &blank(""), &filled(""), false);
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &again)).await;
    assert_eq!(refused(&result), Some("already_submitted"), "{result:?}");
    assert!(again.calls().is_empty());
}

#[tokio::test]
async fn a_changed_page_before_submit_pauses() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    // The page shows another email than the one written.
    let changed = page(FORM_URL, ["Ada Lovelace", "someone@else.io", WHY], "");
    let m = mock(&w, &blank(""), &changed, false);
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    let pause = open_pause(&w.db).await;
    assert_eq!(pause.kind, "other");
    assert_eq!(pause.origin, "guard");
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    assert_eq!(m.submit_clicks(), 0);
    apply::resolve_pause(&w.db, "cli", &pause.id, Resolution::TakeOver)
        .await
        .unwrap();
    let view = ended(task).await.unwrap();
    assert_eq!(view.items[0].state, "manual_required");
    assert_eq!(view.items[0].submit_clicks, 0);
    assert_eq!(m.submit_clicks(), 0);
    assert_eq!(status(&w.db, &app).await, "saved");

    // A field the page added after the fill is a change too.
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let added = filled("  - textbox \"Referral code\" [ref=e20]\n");
    let m = mock(&w, &blank(""), &added, false);
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    apply::resolve_pause(&w.db, "cli", &pause.id, Resolution::TakeOver)
        .await
        .unwrap();
    ended(task).await.unwrap();
    assert_eq!(m.submit_clicks(), 0);
}

#[tokio::test]
async fn an_unmapped_field_pauses_and_nothing_submits() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let m = mock(&w, &blank(PHONE_FIELD), &filled(PHONE_FIELD), false);
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    let pause = open_pause(&w.db).await;
    assert_eq!(pause.kind, "other");
    assert!(pause.excerpt.contains("Phone"), "{}", pause.excerpt);
    assert_eq!(m.submit_clicks(), 0);
    apply::resolve_pause(&w.db, "cli", &pause.id, Resolution::TakeOver)
        .await
        .unwrap();
    let view = ended(task).await.unwrap();
    assert_eq!(view.items[0].state, "manual_required");
    assert_eq!(m.submit_clicks(), 0);
    assert_eq!(status(&w.db, &app).await, "saved");
}

#[tokio::test]
async fn autonomous_calls_no_model() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    // "Phone" has an approved answer only a model could match ("What is your phone number?").
    // Autonomous has no router: the field goes to a person, and its answer is not typed.
    let m = mock(&w, &blank(PHONE_FIELD), &filled(PHONE_FIELD), false);
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("Phone"), "{}", pause.excerpt);
    // The person fills Phone in the browser and says so: the run submits.
    apply::resolve_pause(&w.db, "cli", &pause.id, Resolution::Continue)
        .await
        .unwrap();
    let view = ended(task).await.unwrap();
    assert_eq!(view.items[0].state, "submitted");
    assert_eq!(m.submit_clicks(), 1);
    assert!(!std::fs::read_to_string(&m.calls).unwrap().contains(PHONE));

    let agent_run: String = sqlx::query_scalar("SELECT agent_run_id FROM apply_runs WHERE id = ?")
        .bind(&view.id)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    let logged: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT step, provider, data_categories FROM agent_run_log WHERE run_id = ? ORDER BY seq",
    )
    .bind(&agent_run)
    .fetch_all(&w.db.pool)
    .await
    .unwrap();
    assert!(!logged.is_empty());
    for (step, provider, categories) in &logged {
        assert_eq!(provider, &None, "{step}");
        assert_eq!(categories, "[]", "{step}");
    }
    let steps: Vec<&str> = logged.iter().map(|(step, _, _)| step.as_str()).collect();
    for step in ["open", "detect", "fill", "check", "submit", "confirm"] {
        assert!(steps.contains(&step), "{step}: {steps:?}");
    }
}

#[tokio::test]
async fn a_revoked_grant_stops_the_next_step() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let m = mock(&w, &blank(PHONE_FIELD), &filled(PHONE_FIELD), false);
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    let pause = open_pause(&w.db).await;
    // Revoked while the run waits for the person: the submit step is refused.
    permissions::revoke(&w.db, "cli", "lever").await.unwrap();
    apply::resolve_pause(&w.db, "cli", &pause.id, Resolution::Continue)
        .await
        .unwrap();
    let result = ended(task).await;
    assert_eq!(refused(&result), Some("not_authorized"), "{result:?}");
    assert_eq!(m.submit_clicks(), 0);
    let view = latest(&w.db).await;
    assert_eq!(view.state, "ended");
    assert_eq!(view.items[0].submit_clicks, 0);
    assert_eq!(view.items[0].state, "skipped");
    assert_eq!(status(&w.db, &app).await, "saved");
}

// ---- Fix round 1 ----

/// The submit-time snapshot: the fill's snapshot, two upload snapshots, and the check's
/// come first.
const AT_SUBMIT: u32 = 5;

/// A run with a grant, a doctored register, and Lever verified, on `pages`.
async fn eligible(pages: Pages<'_>) -> (World, String, Mock, JoinHandle<Result<RunView>>) {
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let m = mock_with(&w, pages);
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    (w, app, m, task)
}

async fn answer(db: &Db, pause: &PauseView, resolution: Resolution) {
    apply::resolve_pause(db, "cli", &pause.id, resolution)
        .await
        .unwrap();
}

/// A pause other than `seen`.
async fn next_pause(db: &Db, seen: &str) -> PauseView {
    let begun = Instant::now();
    loop {
        if let Some(run) = apply::latest(db).await.unwrap()
            && let Some(pause) = run.active_pause
            && pause.id != seen
        {
            return pause;
        }
        assert!(begun.elapsed() < WAIT, "no new pause opened");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn ended_item(task: JoinHandle<Result<RunView>>, db: &Db) -> apply::Item {
    let _ = ended(task).await;
    latest(db).await.items.remove(0)
}

#[tokio::test]
async fn a_submission_seen_in_any_mode_blocks_every_mode() {
    // P1: a Claude Code item the model said was submitted, with no click seen.
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    sqlx::query(
        "INSERT INTO apply_runs (id, mode, actor, state, created_at) \
         VALUES ('r0', 'claude_code', 'cli', 'ended', '2026-11-01T00:00:00Z')",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO apply_items (id, apply_run_id, application_id, position, host, state, \
         reason) VALUES ('i0', 'r0', ?, 0, 'jobs.lever.co', 'uncertain', 'no_submit_seen')",
    )
    .bind(&app)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let m = mock(&w, &blank(""), &filled(""), false);
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &m)).await;
    assert_eq!(refused(&result), Some("already_submitted"), "{result:?}");
    assert!(m.calls().is_empty());
    // The preflight every mode runs says the same.
    let checked = apply::preflight(&w.db, std::slice::from_ref(&app))
        .await
        .unwrap();
    assert!(
        checked.items[0].blocks.contains(&"already_submitted"),
        "{:?}",
        checked.items[0].blocks
    );

    // An audit row of a submission alone blocks, as does a status past saved.
    let w = world().await;
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    sqlx::query(
        "INSERT INTO audit_log (at, actor, action, target, detail) \
         VALUES ('2026-11-01T00:00:00Z', 'cli', 'application_submitted', ?, '{}')",
    )
    .bind(&app)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let m = mock(&w, &blank(""), &filled(""), false);
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &app, &m)).await;
    assert_eq!(refused(&result), Some("already_submitted"), "{result:?}");
    let other = add_app(&w.db, LINK).await;
    sqlx::query("UPDATE applications SET status = 'applied' WHERE id = ?")
        .bind(&other)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let result = ended(run(&w, ALLOWED_LEVER, LEVER, &other, &m)).await;
    assert_eq!(refused(&result), Some("already_submitted"), "{result:?}");
    assert!(m.calls().is_empty());
}

#[tokio::test]
async fn an_autonomous_uncertain_item_blocks_a_claude_code_run() {
    let (w, app, m, task) = eligible(Pages {
        before: &blank(""),
        after: &filled(""),
        die: true,
        ..Pages::default()
    })
    .await;
    assert!(ended(task).await.is_err());
    assert_eq!(m.submit_clicks(), 1);
    let checked = apply::preflight(&w.db, std::slice::from_ref(&app))
        .await
        .unwrap();
    assert!(checked.items[0].blocks.contains(&"already_submitted"));
    let started = apply::start(&w.db, "cli", std::slice::from_ref(&app), false).await;
    assert!(started.is_err(), "{started:?}");
}

#[tokio::test]
async fn a_page_off_the_postings_site_before_submit_ends_skipped() {
    // P2, and C7: the filled page is on another origin.
    let off = page(
        "https://other.example/apply",
        ["Ada Lovelace", "ada@example.com", WHY],
        "",
    );
    let (w, app, m, task) = eligible(Pages {
        before: &blank(""),
        after: &off,
        ..Pages::default()
    })
    .await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "skipped");
    assert_eq!(item.reason.as_deref(), Some("origin_changed"));
    assert_eq!(m.submit_clicks(), 0);
    assert_eq!(status(&w.db, &app).await, "saved");

    // The page moves off the site while a changed-form stop is open.
    let changed = page(FORM_URL, ["Ada Lovelace", "someone@else.io", WHY], "");
    let (w, _, m, task) = eligible(Pages {
        before: &blank(""),
        after: &changed,
        ..Pages::default()
    })
    .await;
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    std::fs::write(&m.after, &off).unwrap();
    answer(&w.db, &pause, Resolution::Continue).await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "skipped");
    assert_eq!(item.reason.as_deref(), Some("origin_changed"));
    assert_eq!(m.submit_clicks(), 0);
}

#[tokio::test]
async fn continue_on_a_changed_form_authorizes_only_the_page_it_showed() {
    // P3: the page changes again while the stop is open: a new stop, no click.
    let changed = page(FORM_URL, ["Ada Lovelace", "someone@else.io", WHY], "");
    let (w, app, m, task) = eligible(Pages {
        before: &blank(""),
        after: &changed,
        ..Pages::default()
    })
    .await;
    let first = open_pause(&w.db).await;
    let again = page(
        FORM_URL,
        ["Ada Byron", "someone@else.io", WHY],
        "  - textbox \"Referral code\" [ref=e20]\n",
    );
    std::fs::write(&m.after, &again).unwrap();
    answer(&w.db, &first, Resolution::Continue).await;
    let second = next_pause(&w.db, &first.id).await;
    assert!(second.excerpt.contains("changed"), "{}", second.excerpt);
    assert_eq!(m.submit_clicks(), 0);
    // C3: Continue on the page as the stop showed it submits it, once.
    answer(&w.db, &second, Resolution::Continue).await;
    let view = ended(task).await.unwrap();
    assert_eq!(view.items[0].state, "submitted");
    assert_eq!(m.submit_clicks(), 1);
    assert_eq!(status(&w.db, &app).await, "applied");
}

#[tokio::test]
async fn the_page_is_read_again_just_before_the_click() {
    // A change between the check and the click stops the run before it clicks.
    let late = page(FORM_URL, ["Ada Lovelace", "late@change.io", WHY], "");
    let (w, _, m, task) = eligible(Pages {
        before: &blank(""),
        after: &filled(""),
        late: Some((&late, AT_SUBMIT)),
        ..Pages::default()
    })
    .await;
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    assert_eq!(m.submit_clicks(), 0);
    answer(&w.db, &pause, Resolution::TakeOver).await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "manual_required");
    assert_eq!(m.submit_clicks(), 0);

    // C8: a submit button renamed is a change.
    let renamed = filled("").replace("Submit application", "Submit and share my data");
    let (w, _, m, task) = eligible(Pages {
        before: &blank(""),
        after: &filled(""),
        late: Some((&renamed, AT_SUBMIT)),
        ..Pages::default()
    })
    .await;
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    answer(&w.db, &pause, Resolution::TakeOver).await;
    ended(task).await.unwrap();
    assert!(m.clicks().iter().all(|t| t != SUBMIT));

    // A file button gone is a change.
    let no_letter = filled("").replace("  - button \"Cover letter\" [ref=e15]\n", "");
    let (w, _, m, task) = eligible(Pages {
        before: &blank(""),
        after: &filled(""),
        late: Some((&no_letter, AT_SUBMIT)),
        ..Pages::default()
    })
    .await;
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    answer(&w.db, &pause, Resolution::TakeOver).await;
    ended(task).await.unwrap();
    assert_eq!(m.submit_clicks(), 0);
}

#[tokio::test]
async fn refs_are_not_trusted_and_the_click_goes_to_the_fresh_ref() {
    // The submit button's ref moves between the check and the click: matched by role and
    // name, the page is unchanged, and the click goes to the ref the last snapshot gave,
    // never to the one the check read.
    let checked = filled("").replace("[ref=e14]", "[ref=e99]");
    let (w, _, m, task) = eligible(Pages {
        before: &blank(""),
        after: &checked,
        late: Some((&filled(""), AT_SUBMIT)),
        ..Pages::default()
    })
    .await;
    let view = ended(task).await.unwrap();
    assert_eq!(view.items[0].state, "submitted");
    assert_eq!(m.submit_clicks(), 1);
    assert!(m.clicks().iter().all(|t| t != "e99"), "{:?}", m.clicks());

    // Two controls with one role and name: ambiguous, so a person submits.
    let twice = "  - textbox \"Why this role?\" [ref=e30]\n";
    let (w2, _, m2, task) = eligible(Pages {
        before: &blank(twice),
        after: &filled(twice),
        ..Pages::default()
    })
    .await;
    let item = ended_item(task, &w2.db).await;
    assert_eq!(item.state, "manual_required");
    assert_eq!(item.reason.as_deref(), Some("ambiguous_form"));
    assert_eq!(m2.submit_clicks(), 0);
    drop(w);
}

#[tokio::test]
async fn checkbox_state_and_choices_are_read_back() {
    // A box the person ticks at the stop is the accepted state; a flip after it is a change.
    let updates = "  - checkbox \"Send me job alerts\" [ref=e21]\n";
    let ticked = "  - checkbox \"Send me job alerts\" [checked] [ref=e21]\n";
    let (w, _, m, task) = eligible(Pages {
        before: &blank(updates),
        after: &filled(updates),
        late: Some((&filled(ticked), AT_SUBMIT)),
        ..Pages::default()
    })
    .await;
    let left = open_pause(&w.db).await;
    assert!(
        left.excerpt.contains("Send me job alerts"),
        "{}",
        left.excerpt
    );
    answer(&w.db, &left, Resolution::Continue).await;
    let changed = next_pause(&w.db, &left.id).await;
    assert!(changed.excerpt.contains("changed"), "{}", changed.excerpt);
    assert_eq!(m.submit_clicks(), 0);
    answer(&w.db, &changed, Resolution::TakeOver).await;
    ended(task).await.unwrap();
}

/// A form with a country choice the rules fill from the profile.
fn with_country(values: [&str; 3], country: &str) -> String {
    let field = format!(
        "  - combobox \"Country\" [ref=e7]{country}\n    - option \"PT\"\n    - option \"US\"\n"
    );
    page(FORM_URL, values, &field)
}

async fn country_run(after: &str) -> (World, Mock, JoinHandle<Result<RunView>>) {
    let w = world().await;
    let mut basic = profile::basic(&w.db).await.unwrap();
    basic.countries = vec!["PT".into()];
    profile::set_basic(&w.db, basic).await.unwrap();
    permissions::grant(&w.db, "cli", "lever").await.unwrap();
    let app = add_app(&w.db, LINK).await;
    let m = mock_with(
        &w,
        Pages {
            before: &with_country(["", "prefill@x.io", ""], ""),
            after,
            ..Pages::default()
        },
    );
    let task = run(&w, ALLOWED_LEVER, LEVER, &app, &m);
    (w, m, task)
}

#[tokio::test]
async fn a_choice_is_submitted_only_when_read_back() {
    let values = ["Ada Lovelace", "ada@example.com", WHY];
    // Read back as written: submitted.
    let (w, m, task) = country_run(&with_country(values, ": PT")).await;
    let view = ended(task).await.unwrap();
    assert_eq!(view.items[0].state, "submitted");
    assert_eq!(m.submit_clicks(), 1);
    let wrote = &m
        .calls()
        .into_iter()
        .find(|c| c["name"] == "browser_fill_form")
        .unwrap()["args"]["fields"];
    assert!(wrote.to_string().contains("\"PT\""), "{wrote}");
    drop(w);

    // Another choice shown: a change.
    let (w, m, task) = country_run(&with_country(values, ": US")).await;
    let pause = open_pause(&w.db).await;
    assert!(pause.excerpt.contains("changed"), "{}", pause.excerpt);
    answer(&w.db, &pause, Resolution::TakeOver).await;
    ended(task).await.unwrap();
    assert_eq!(m.submit_clicks(), 0);

    // No selection the snapshot shows: it cannot be checked, so a person submits.
    let (w, m, task) = country_run(&with_country(values, "")).await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "manual_required");
    assert_eq!(item.reason.as_deref(), Some("choice_unverified"));
    assert_eq!(m.submit_clicks(), 0);
}

#[tokio::test]
async fn a_legal_declaration_is_never_submitted_by_the_run() {
    // P4b: one Continue on the legal stop before the fill does not lead to a submit.
    let line = "  - paragraph [ref=e40]: By submitting, I certify that all information is true\n";
    let (w, _, m, task) = eligible(Pages {
        before: &blank(line),
        after: &filled(line),
        ..Pages::default()
    })
    .await;
    let legal = open_pause(&w.db).await;
    assert_eq!(legal.kind, "legal");
    answer(&w.db, &legal, Resolution::Continue).await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "manual_required");
    assert_eq!(item.reason.as_deref(), Some("legal_declaration"));
    assert_eq!(m.submit_clicks(), 0);

    // A legal field is never filled, and its page is never submitted.
    let field = "  - checkbox \"I certify that the information I provided is true\" [ref=e41]\n";
    let (w, _, m, task) = eligible(Pages {
        before: &blank(field),
        after: &filled(field),
        ..Pages::default()
    })
    .await;
    // Every stop it opens is continued, as a hurried person might.
    let begun = Instant::now();
    loop {
        if let Some(run) = apply::latest(&w.db).await.unwrap() {
            if run.state == "ended" {
                break;
            }
            if let Some(pause) = run.active_pause {
                answer(&w.db, &pause, Resolution::Continue).await;
            }
        }
        assert!(begun.elapsed() < WAIT, "the run did not end");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "manual_required");
    assert_eq!(m.submit_clicks(), 0);
    assert!(m.clicks().iter().all(|t| t != "e41"));
    for fill in m
        .calls()
        .iter()
        .filter(|c| c["name"] == "browser_fill_form")
    {
        assert!(!fill.to_string().contains("e41"), "{fill}");
    }
}

#[tokio::test]
async fn doubt_after_the_click_is_uncertain() {
    // C4: the form is still there after the click.
    let (w, app, m, task) = eligible(Pages {
        before: &blank(""),
        after: &filled(""),
        thanks: Some(&filled("")),
        ..Pages::default()
    })
    .await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "uncertain");
    assert_eq!(m.submit_clicks(), 1);
    assert_eq!(status(&w.db, &app).await, "saved");

    // C10: a stop on the page after the click.
    let captcha = format!("{THANKS}- checkbox \"I'm not a robot\" [ref=e9]\n");
    let (w, app, m, task) = eligible(Pages {
        before: &blank(""),
        after: &filled(""),
        thanks: Some(&captcha),
        ..Pages::default()
    })
    .await;
    let item = ended_item(task, &w.db).await;
    assert_eq!(item.state, "uncertain");
    assert_eq!(m.submit_clicks(), 1);
    assert_eq!(status(&w.db, &app).await, "saved");
}

#[tokio::test]
async fn a_landing_page_with_only_a_button_is_not_clicked() {
    let landing = format!(
        "### Page\n- Page URL: {FORM_URL}\n### Snapshot\n```yaml\n- button \"Apply\" [ref=e2]\n```\n"
    );
    let (w, _, m, task) = eligible(Pages {
        before: &landing,
        after: &landing,
        ..Pages::default()
    })
    .await;
    let pause = open_pause(&w.db).await;
    assert!(
        pause.excerpt.contains("Open the application form"),
        "{}",
        pause.excerpt
    );
    answer(&w.db, &pause, Resolution::TakeOver).await;
    ended(task).await.unwrap();
    assert!(m.clicks().is_empty(), "{:?}", m.clicks());
}

#[tokio::test]
async fn connectors_are_folded_and_an_unknown_revoke_is_refused() {
    let w = world().await;
    permissions::grant(&w.db, "cli", "Lever").await.unwrap();
    assert_eq!(permissions::grants(&w.db).await.unwrap(), ["lever"]);
    permissions::revoke(&w.db, "cli", "LEVER").await.unwrap();
    assert!(permissions::grants(&w.db).await.unwrap().is_empty());
    let unknown = permissions::revoke(&w.db, "cli", "acme").await.unwrap_err();
    assert!(matches!(unknown, Error::Validation(_)), "{unknown:?}");
}
