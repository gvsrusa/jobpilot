//! Manual mode: the material for one application and the outcome a person records. Nothing
//! here opens a browser, starts a run, or reaches a site. Keychain paths run on keyring's
//! in-memory mock, so no OS store is reached. Its own binary, as the default store is
//! process-wide.

use std::sync::Once;

use jobpilot_core::ai::CancellationToken;
use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::apply::{self, Material};
use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::memory::{self, Class};
use jobpilot_core::profile::{self, Stated, WorkAuthorization};
use jobpilot_core::render::{Entry, ResumeDoc, Section};
use jobpilot_core::{AppPaths, Db, Error, audit, drafts};
use tempfile::TempDir;

const BLOCKED: &str = "The site's terms forbid this. JobPilot will not do it, even if you ask.";
const UNKNOWN: &str =
    "The site has not said whether tools may do this. Treated as blocked until it answers.";
const NO_CLAUSE: &str = "No clause found for this site. You review and submit.";
const NO_AUTONOMOUS: &str = "No site or connector qualifies today. Needs a site rule of allowed, verified permission, and your explicit authorization.";

fn mock_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}

async fn world() -> (TempDir, Db) {
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let KeyState::Unlocked { .. } = crypto::state(&db).await.unwrap() else {
        panic!("the mock keychain did not unlock");
    };
    (tmp, db)
}

async fn add_app(db: &Db, link: &str, country: &str) -> String {
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        link: link.into(),
        country: country.into(),
        ..ApplicationInput::default()
    };
    applications::add(db, "cli", input).await.unwrap().id
}

async fn answer(db: &Db, question: &str, class: Class, text: &str) -> String {
    let question = memory::add_question(db, question, None).await.unwrap();
    sqlx::query("UPDATE memory_questions SET class = ?, class_source = 'user' WHERE id = ?")
        .bind(class.as_str())
        .bind(&question.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let added = memory::add_answer(db, &question.id, text, None)
        .await
        .unwrap();
    memory::approve(db, "cli", &added.id, true).await.unwrap();
    added.id
}

fn note<'a>(material: &'a Material, mode: &str) -> (bool, &'a str) {
    let choice = material
        .modes
        .iter()
        .find(|choice| choice.mode == mode)
        .unwrap_or_else(|| panic!("no {mode} row"));
    (choice.available, choice.note.as_str())
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&db.pool).await.unwrap()
}

#[tokio::test]
async fn manual_makes_no_request_and_records_the_outcome() {
    let (_tmp, db) = world().await;
    let app = add_app(&db, "https://jobs.lever.co/acme/1", "PT").await;

    let material = apply::material(&db, &app).await.unwrap();
    assert!(note(&material, "manual").0);
    assert!(material.resume.is_none() && material.letter.is_none());

    let saved = apply::record_outcome(
        &db,
        "cli",
        &app,
        "applied",
        Some("2026-11-02"),
        "",
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(saved.status, "applied");
    assert_eq!(saved.applied_on.as_deref(), Some("2026-11-02"));

    let detail: String = sqlx::query_scalar(
        "SELECT detail FROM audit_log WHERE action = 'application_submitted' AND target = ?",
    )
    .bind(&app)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["mode"], "manual");
    assert_eq!(detail["agent"], "manual");

    // No run of any browser mode, and the only agent that ran is tracking.
    assert_eq!(count(&db, "SELECT count(*) FROM apply_runs").await, 0);
    assert_eq!(count(&db, "SELECT count(*) FROM apply_items").await, 0);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM agent_runs WHERE agent <> 'tracking'"
        )
        .await,
        0
    );

    // Another outcome moves the status and claims no submission.
    apply::record_outcome(
        &db,
        "cli",
        &app,
        "screening",
        None,
        "a call booked",
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM audit_log WHERE action = 'application_submitted'"
        )
        .await,
        1
    );

    // Only a person records an outcome.
    let refused = apply::record_outcome(
        &db,
        "system",
        &app,
        "offer",
        None,
        "",
        CancellationToken::new(),
    )
    .await;
    assert!(refused.is_err(), "the system actor recorded {refused:?}");
}

#[tokio::test]
async fn a_sealed_answer_needs_confirmation_before_it_is_supplied() {
    let (_tmp, db) = world().await;
    let app = add_app(&db, "https://jobs.lever.co/acme/1", "PT").await;
    let normal = answer(&db, "Notice period?", Class::Normal, "Four weeks").await;
    let salary = answer(&db, "Salary expectation?", Class::Sensitive, "90000 EUR").await;

    let material = apply::material(&db, &app).await.unwrap();
    let row = |id: &str| {
        material
            .answers
            .iter()
            .find(|row| row.answer_id == id)
            .unwrap()
            .clone()
    };
    assert!(!row(&normal).needs_confirmation);
    assert_eq!(row(&normal).class, "normal");
    assert!(row(&salary).needs_confirmation);
    assert_eq!(row(&salary).class, "sensitive");
    assert_eq!(row(&salary).question, "Salary expectation?");
    // The material holds no answer text.
    let text = serde_json::to_string(&material).unwrap();
    assert!(!text.contains("90000") && !text.contains("Four weeks"));

    assert!(matches!(
        memory::use_answer(&db, "cli", &salary, &app).await,
        Err(Error::ConfirmationRequired)
    ));
    memory::confirm_use(&db, "cli", &salary, &app)
        .await
        .unwrap();
    let material = apply::material(&db, &app).await.unwrap();
    let confirmed = material
        .answers
        .iter()
        .find(|row| row.answer_id == salary)
        .unwrap();
    assert!(!confirmed.needs_confirmation);
    let used = memory::use_answer(&db, "cli", &salary, &app).await.unwrap();
    assert_eq!(used.text, "90000 EUR");

    // A confirmation is per application.
    let other = add_app(&db, "https://jobs.lever.co/acme/2", "PT").await;
    let material = apply::material(&db, &other).await.unwrap();
    assert!(
        material
            .answers
            .iter()
            .find(|row| row.answer_id == salary)
            .unwrap()
            .needs_confirmation
    );
}

#[tokio::test]
async fn work_authorization_reads_not_stated_for_an_unstated_country() {
    let (_tmp, db) = world().await;
    profile::set_authorization(
        &db,
        WorkAuthorization {
            country: "PT".into(),
            authorized: Stated::Yes,
            needs_sponsorship: Stated::No,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    let de = add_app(&db, "https://jobs.lever.co/acme/1", "DE").await;
    let pt = add_app(&db, "https://jobs.lever.co/acme/2", "PT").await;
    let blank = add_app(&db, "https://jobs.lever.co/acme/3", "").await;

    let unstated = apply::material(&db, &de).await.unwrap().authorization;
    assert_eq!(unstated.country, "DE");
    assert!(!unstated.stated);
    assert_eq!(unstated.authorized, Stated::NotStated);
    assert_eq!(unstated.needs_sponsorship, Stated::NotStated);

    let stated = apply::material(&db, &pt).await.unwrap().authorization;
    assert!(stated.stated);
    assert_eq!(stated.authorized, Stated::Yes);

    let none = apply::material(&db, &blank).await.unwrap().authorization;
    assert_eq!(none.country, "");
    assert!(!none.stated);
    assert_eq!(none.authorized, Stated::NotStated);

    // A work authorization question never appears as an answer to copy.
    let material = apply::material(&db, &de).await.unwrap();
    assert!(
        material
            .answers
            .iter()
            .all(|row| row.class != "work_authorization")
    );
}

#[tokio::test]
async fn each_mode_note_is_the_fixed_copy_for_each_site() {
    let (_tmp, db) = world().await;
    let indeed = add_app(&db, "https://www.indeed.com/viewjob?jk=1", "").await;
    let workday = add_app(
        &db,
        "https://acme.wd5.myworkdayjobs.com/en-US/careers/job/1",
        "",
    )
    .await;
    let lever = add_app(&db, "https://jobs.lever.co/acme/1", "").await;
    let company = add_app(&db, "https://careers.example.com/jobs/1", "").await;
    let plain = add_app(&db, "http://jobs.lever.co/acme/1", "").await;

    let indeed = apply::material(&db, &indeed).await.unwrap();
    assert_eq!(note(&indeed, "copilot"), (false, BLOCKED));
    assert_eq!(note(&indeed, "autonomous"), (false, BLOCKED));

    let workday = apply::material(&db, &workday).await.unwrap();
    assert_eq!(note(&workday, "copilot"), (false, UNKNOWN));
    assert_eq!(note(&workday, "autonomous"), (false, NO_AUTONOMOUS));

    let lever = apply::material(&db, &lever).await.unwrap();
    assert_eq!(note(&lever, "copilot"), (true, NO_CLAUSE));
    assert_eq!(note(&lever, "autonomous"), (false, NO_AUTONOMOUS));

    // A site with no register row reads as unknown, so Manual only.
    let company = apply::material(&db, &company).await.unwrap();
    assert_eq!(note(&company, "copilot"), (false, UNKNOWN));
    assert_eq!(note(&company, "autonomous"), (false, NO_AUTONOMOUS));
    let plain = apply::material(&db, &plain).await.unwrap();
    assert_eq!(note(&plain, "copilot"), (false, UNKNOWN));

    for material in [&indeed, &workday, &lever, &company, &plain] {
        let modes: Vec<&str> = material.modes.iter().map(|choice| choice.mode).collect();
        assert_eq!(modes, ["manual", "copilot", "autonomous", "claude_code"]);
        assert_eq!(
            note(material, "manual"),
            (
                true,
                "You fill and submit in your own browser. JobPilot opens the link, gives you files and answers, and records what happened."
            )
        );
        let (_, claude) = note(material, "claude_code");
        assert!(
            claude == apply::CLAUDE_CODE_NOTE || claude == apply::CLAUDE_CODE_SETUP_NOTE,
            "{claude}"
        );
    }
    assert_eq!(
        apply::CLAUDE_CODE_NOTE,
        "Fills and submits with Claude Code in a visible browser. Add it to a run from Applications."
    );
}

async fn submissions(db: &Db, app: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT json_extract(detail, '$.mode') FROM audit_log \
         WHERE action = 'application_submitted' AND target = ? ORDER BY rowid",
    )
    .bind(app)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

async fn record(db: &Db, app: &str, outcome: &str) -> Result<String, Error> {
    apply::record_outcome(db, "cli", app, outcome, None, "", CancellationToken::new())
        .await
        .map(|saved| saved.status)
}

#[tokio::test]
async fn a_submission_is_audited_once_per_application() {
    let (_tmp, db) = world().await;
    let app = add_app(&db, "https://jobs.lever.co/acme/1", "PT").await;
    record(&db, &app, "applied").await.unwrap();
    record(&db, &app, "applied").await.unwrap();
    record(&db, &app, "interviewing").await.unwrap();
    // A move back to applied writes the status and claims no new submission.
    assert_eq!(record(&db, &app, "applied").await.unwrap(), "applied");
    assert_eq!(submissions(&db, &app).await, ["manual"]);

    // A Claude Code run already recorded this one: recording it by hand adds nothing.
    let run = add_app(&db, "https://jobs.lever.co/acme/2", "PT").await;
    audit::record(
        &db,
        "cli",
        "application_submitted",
        Some(&run),
        serde_json::json!({ "mode": "claude_code" }),
    )
    .await
    .unwrap();
    record(&db, &run, "applied").await.unwrap();
    assert_eq!(submissions(&db, &run).await, ["claude_code"]);
}

#[tokio::test]
async fn recording_refuses_an_application_in_a_run_that_has_not_ended() {
    let (_tmp, db) = world().await;
    let app = add_app(&db, "https://jobs.lever.co/acme/1", "PT").await;
    for statement in [
        "INSERT INTO apply_runs (id, mode, actor, state, created_at) \
         VALUES ('run1', 'claude_code', 'cli', 'running', '2026-11-02T10:00:00.000Z')",
        "INSERT INTO apply_items (id, apply_run_id, application_id, position, host) \
         SELECT 'item1', 'run1', id, 0, 'jobs.lever.co' FROM applications",
    ] {
        sqlx::query(statement).execute(&db.pool).await.unwrap();
    }
    match record(&db, &app, "applied").await {
        Err(Error::Refused(reason)) if reason == "in_active_run" => {}
        other => panic!("expected in_active_run, got {other:?}"),
    }
    let status: String = sqlx::query_scalar("SELECT status FROM applications")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "saved");

    sqlx::query("UPDATE apply_runs SET state = 'ended', ended_at = created_at")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(record(&db, &app, "applied").await.unwrap(), "applied");
}

#[tokio::test]
async fn the_status_and_the_submission_row_land_together() {
    let (_tmp, db) = world().await;
    let app = add_app(&db, "https://jobs.lever.co/acme/1", "PT").await;
    sqlx::query(
        "CREATE TRIGGER no_submission BEFORE INSERT ON audit_log \
         WHEN NEW.action = 'application_submitted' BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(record(&db, &app, "applied").await.is_err());
    let status: String = sqlx::query_scalar("SELECT status FROM applications")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "saved", "no status without its submission row");
}

async fn uploaded(db: &Db, kind: &str, bullet: &str) -> drafts::Draft {
    let doc = ResumeDoc {
        name: "Ada Lovelace".into(),
        contact: vec!["ada@example.com".into()],
        summary: Vec::new(),
        sections: vec![Section {
            heading: "Experience".into(),
            entries: vec![Entry {
                bullets: vec![bullet.into()],
                ..Entry::default()
            }],
        }],
    };
    drafts::create_version(
        db,
        kind,
        None,
        "uploaded",
        "Acme",
        None,
        None,
        doc,
        Vec::new(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn material_renders_a_document_only_when_it_is_missing_or_its_version_changed() {
    let (_tmp, db) = world().await;
    let resume = uploaded(&db, "resume", "Built things").await;
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        link: "https://jobs.lever.co/acme/1".into(),
        resume_draft_id: Some(resume.id.clone()),
        ..ApplicationInput::default()
    };
    let app = applications::add(&db, "cli", input.clone())
        .await
        .unwrap()
        .id;
    let exports = || {
        count(
            &db,
            "SELECT count(*) FROM audit_log WHERE action = 'export'",
        )
    };

    let first = apply::material(&db, &app).await.unwrap().resume.unwrap();
    assert!(first.is_file());
    assert_eq!(exports().await, 1);
    let again = apply::material(&db, &app).await.unwrap().resume.unwrap();
    assert_eq!(again, first);
    assert_eq!(exports().await, 1, "a second read renders nothing");

    // A removed file is rendered again.
    std::fs::remove_file(&first).unwrap();
    apply::material(&db, &app).await.unwrap();
    assert!(first.is_file());
    assert_eq!(exports().await, 2);

    // Another document is rendered once, even where its file name is the same.
    let newer = uploaded(&db, "resume", "Built more").await;
    let input = ApplicationInput {
        resume_draft_id: Some(newer.id),
        ..input
    };
    applications::update(&db, "cli", &app, input).await.unwrap();
    let second = apply::material(&db, &app).await.unwrap().resume.unwrap();
    assert!(second.is_file());
    assert_eq!(exports().await, 3);
    apply::material(&db, &app).await.unwrap();
    assert_eq!(exports().await, 3);
}

/// Run by [`the_review_screen_starts_no_child_process`] in a child of this binary with a
/// clean environment. On its own it does nothing.
#[tokio::test]
#[ignore]
async fn no_child_process_probe() {
    if std::env::var_os("JOBPILOT_PROBE_MARKER").is_none() {
        return;
    }
    let (_tmp, db) = world().await;
    let app = add_app(&db, "https://jobs.lever.co/acme/1", "PT").await;
    apply::material(&db, &app).await.unwrap();
    record(&db, &app, "applied").await.unwrap();
}

#[cfg(unix)]
#[test]
fn the_review_screen_starts_no_child_process() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("spawned");
    // Any login shell, node, or claude lookup that runs a program runs this, which leaves a mark.
    let program = tmp.path().join("probe");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\necho \"$0 $*\" >> '{}'\necho v99.0.0\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let empty = tmp.path().join("bin");
    std::fs::create_dir(&empty).unwrap();

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "no_child_process_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("JOBPILOT_PROBE_MARKER", &marker)
        .env("SHELL", &program)
        .env("JOBPILOT_NODE_BIN", &program)
        .env("PATH", &empty)
        .env("HOME", tmp.path())
        .env_remove("JOBPILOT_CLAUDE_BIN")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the probe did not run"
    );
    let spawned = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(spawned.is_empty(), "started: {spawned}");
}
