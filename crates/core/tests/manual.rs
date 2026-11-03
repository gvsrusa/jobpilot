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
use jobpilot_core::{AppPaths, Db, Error};
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
