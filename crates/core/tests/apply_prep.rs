//! Apply-run preparation: preflight, the sealed release at start, and the brief. Keychain
//! paths run on keyring's in-memory mock, so no OS store is reached. Its own binary, as the
//! default store is process-wide.

use std::path::{Path, PathBuf};
use std::sync::Once;

use jobpilot_core::ai::claude_code::PROVIDER;
use jobpilot_core::ai::{DataCategory, consent};
use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::apply::brief::{self, Brief};
use jobpilot_core::apply::{self, AuthorizationState, DocState, Item, MAX_RUN_ITEMS, Resolution};
use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::drafts::{self, ClaimInput};
use jobpilot_core::memory::{self, Class, FRESH_DAYS};
use jobpilot_core::profile::{self, ProfileBasic, Stated, WorkAuthorization};
use jobpilot_core::render::{Entry, ResumeDoc, Section};
use jobpilot_core::{AppPaths, Db, Error, pause};
use tempfile::TempDir;

const BASE: [DataCategory; 3] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::CareerMemory,
];
const ALL: [DataCategory; 4] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::CareerMemory,
    DataCategory::Sensitive,
];

fn mock_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}

struct World {
    tmp: TempDir,
    db: Db,
    key_id: String,
}

async fn world() -> World {
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let KeyState::Unlocked { key_id } = crypto::state(&db).await.unwrap() else {
        panic!("the mock keychain did not unlock");
    };
    let basic = ProfileBasic {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        cities: vec!["Lisbon".into(), "Porto".into()],
        countries: vec!["PT".into()],
        languages: vec!["en".into(), "pt".into()],
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();
    World { tmp, db, key_id }
}

/// Forgets the data key, so any later keychain read fails.
fn drop_key(key_id: &str) {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE").unwrap();
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    keyring::Entry::new(&service, &format!("{KEY_ACCOUNT_PREFIX}{key_id}"))
        .unwrap()
        .delete_credential()
        .unwrap();
}

async fn consent_to(db: &Db, categories: &[DataCategory]) {
    consent::give(db, PROVIDER, categories, "2026-11-01", "cli")
        .await
        .unwrap();
}

fn doc(name: &str, bullet: &str) -> ResumeDoc {
    ResumeDoc {
        name: name.into(),
        contact: vec!["ada@example.com".into()],
        summary: Vec::new(),
        sections: vec![Section {
            heading: "Experience".into(),
            entries: vec![Entry {
                bullets: vec![bullet.into()],
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
        doc("Ada Lovelace", title),
        Vec::new(),
    )
    .await
    .unwrap()
    .id
}

struct App<'a> {
    company: &'a str,
    link: &'a str,
    country: &'a str,
    docs: bool,
}

impl<'a> App<'a> {
    fn new(company: &'a str) -> App<'a> {
        App {
            company,
            link: "https://boards.greenhouse.io/acme/jobs/1",
            country: "",
            docs: true,
        }
    }
}

async fn add_app(db: &Db, app: App<'_>) -> String {
    let (resume, letter) = if app.docs {
        let resume = uploaded(db, "resume", &format!("{} resume", app.company)).await;
        let letter = uploaded(db, "cover_letter", &format!("{} letter", app.company)).await;
        (Some(resume), Some(letter))
    } else {
        (None, None)
    };
    let input = ApplicationInput {
        company: app.company.into(),
        role: "Engineer".into(),
        link: app.link.into(),
        country: app.country.into(),
        resume_draft_id: resume,
        letter_draft_id: letter,
        ..ApplicationInput::default()
    };
    applications::add(db, "cli", input).await.unwrap().id
}

async fn app(db: &Db, company: &str) -> String {
    add_app(db, App::new(company)).await
}

/// An approved answer of `class` to a new question.
async fn answer(db: &Db, question: &str, class: Class, text: &str) -> String {
    scoped_answer(db, question, None, class, text).await
}

/// As [`answer`], with the question scoped to `country`.
async fn scoped_answer(
    db: &Db,
    question: &str,
    country: Option<&str>,
    class: Class,
    text: &str,
) -> String {
    let question = memory::add_question(db, question, country).await.unwrap();
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

async fn reviewed_days_ago(db: &Db, answer: &str, days: i64) {
    sqlx::query(
        "UPDATE memory_answers SET reviewed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?) \
         WHERE id = ?",
    )
    .bind(format!("-{days} days"))
    .bind(answer)
    .execute(&db.pool)
    .await
    .unwrap();
}

fn files_dir(w: &World) -> PathBuf {
    w.tmp.path().join("files")
}

async fn item_of(db: &Db, run: &str, application: &str) -> Item {
    apply::view(db, run)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.application_id.as_deref() == Some(application))
        .unwrap()
}

async fn brief_for(w: &World, run: &str, application: &str) -> Brief {
    let item = item_of(&w.db, run, application).await;
    brief::build(&w.db, "cli", &item, &files_dir(w))
        .await
        .unwrap()
}

fn json(brief: &Brief) -> String {
    serde_json::to_string(brief).unwrap()
}

fn refused<T: std::fmt::Debug>(result: Result<T, Error>, reason: &str) {
    match result {
        Err(Error::Refused(said)) if said == reason => {}
        other => panic!("expected refused {reason}, got {other:?}"),
    }
}

fn ids(list: &[&String]) -> Vec<String> {
    list.iter().map(|id| id.to_string()).collect()
}

#[tokio::test]
async fn a_brief_holds_approved_fresh_answers_and_nothing_else() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let fresh = "ZXQ-FRESH-ANSWER";
    let draft = "ZXQ-DRAFT-ANSWER";
    let stale = "ZXQ-STALE-ANSWER";
    answer(&w.db, "Why this job?", Class::Normal, fresh).await;
    let unapproved = answer(&w.db, "Why this team?", Class::Normal, draft).await;
    memory::approve(&w.db, "cli", &unapproved, false)
        .await
        .unwrap();
    let old = answer(&w.db, "Why this city?", Class::Normal, stale).await;
    reviewed_days_ago(&w.db, &old, FRESH_DAYS + 1).await;
    let a = app(&w.db, "Acme").await;

    let check = apply::preflight(&w.db, std::slice::from_ref(&a))
        .await
        .unwrap();
    assert_eq!(check.items[0].answers, 1);
    assert_eq!(check.items[0].stale_left_out, 1);
    assert!(
        check.items[0].blocks.is_empty(),
        "{:?}",
        check.items[0].blocks
    );
    assert!(check.consent_missing.is_empty());

    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();
    let brief = brief_for(&w, &run, &a).await;

    assert_eq!(brief.answers.len(), 1);
    assert_eq!(brief.answers[0].question, "Why this job?");
    assert_eq!(brief.answers[0].answer, fresh);
    assert_eq!(brief.company, "Acme");
    let text = json(&brief);
    assert!(!text.contains(draft), "{text}");
    assert!(!text.contains(stale), "{text}");
    let fact = |name: &str| {
        brief
            .facts
            .iter()
            .find(|fact| fact.name == name)
            .map(|fact| fact.value.clone())
    };
    assert_eq!(fact("name").as_deref(), Some("Ada Lovelace"));
    assert_eq!(fact("email").as_deref(), Some("ada@example.com"));
    assert_eq!(fact("cities").as_deref(), Some("Lisbon, Porto"));
    assert_eq!(fact("regions"), None, "an empty list is left out");
    assert!(brief.work_authorization.is_none());
    let allowed = brief::allowed_values(&brief);
    for value in ["Ada Lovelace", "Lisbon", "Porto", "pt", fresh] {
        assert!(allowed.iter().any(|v| v == value), "{value}: {allowed:?}");
    }
    assert!(!allowed.iter().any(|v| v == draft || v == stale));
    w.db.close().await;
}

#[tokio::test]
async fn a_legal_answer_is_never_in_a_brief_even_when_confirmed() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let legal_text = "ZXQ-LEGAL-ANSWER";
    let legal = answer(&w.db, "Do you certify this?", Class::Legal, legal_text).await;
    let a = app(&w.db, "Acme").await;
    memory::confirm_use(&w.db, "cli", &legal, &a).await.unwrap();

    let run = apply::start(&w.db, "cli", &ids(&[&a]), true).await.unwrap();
    let brief = brief_for(&w, &run, &a).await;

    assert!(brief.answers.is_empty());
    assert!(!json(&brief).contains(legal_text));
    assert!(
        !brief::allowed_values(&brief)
            .iter()
            .any(|v| v == legal_text)
    );
    w.db.close().await;
}

#[tokio::test]
async fn a_sealed_answer_needs_the_release_at_start() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let salary = "ZXQ-SALARY-ANSWER";
    let sealed = answer(&w.db, "What is your salary?", Class::Sensitive, salary).await;
    let a = app(&w.db, "Acme").await;
    let b = app(&w.db, "Globex").await;

    let check = apply::preflight(&w.db, &ids(&[&a, &b])).await.unwrap();
    assert_eq!(check.items[0].sealed, 1);
    assert_eq!(check.sealed_total, 2);

    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();
    let brief = brief_for(&w, &run, &a).await;
    assert!(brief.answers.is_empty());
    assert!(!json(&brief).contains(salary));
    apply::cancel(&w.db, "cli", &run).await.unwrap();

    let run = apply::start(&w.db, "cli", &ids(&[&a]), true).await.unwrap();
    let brief = brief_for(&w, &run, &a).await;
    assert_eq!(brief.answers.len(), 1);
    assert_eq!(brief.answers[0].answer, salary);
    assert_eq!(
        memory::use_answer(&w.db, "cli", &sealed, &b)
            .await
            .unwrap_err()
            .code(),
        "confirmation_required",
        "only the application in the run is released"
    );
    w.db.close().await;
}

#[tokio::test]
async fn release_covers_only_the_applications_of_this_run() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let salary = "ZXQ-SALARY-ANSWER";
    answer(&w.db, "What is your salary?", Class::Sensitive, salary).await;
    let a = app(&w.db, "Acme").await;
    let b = app(&w.db, "Globex").await;

    let first = apply::start(&w.db, "cli", &ids(&[&a]), true).await.unwrap();
    apply::cancel(&w.db, "cli", &first).await.unwrap();
    assert!(
        memory::application_answers(&w.db, &b)
            .await
            .unwrap()
            .is_empty()
    );

    let second = apply::start(&w.db, "cli", &ids(&[&b]), false)
        .await
        .unwrap();
    let brief = brief_for(&w, &second, &b).await;
    assert!(brief.answers.is_empty());
    assert!(!json(&brief).contains(salary));
    w.db.close().await;
}

#[tokio::test]
async fn work_authorization_is_the_exact_country_or_nothing() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    profile::set_authorization(
        &w.db,
        WorkAuthorization {
            country: "CA".into(),
            authorized: Stated::Yes,
            needs_sponsorship: Stated::No,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    let us = add_app(
        &w.db,
        App {
            country: "US",
            ..App::new("Acme")
        },
    )
    .await;
    let ca = add_app(
        &w.db,
        App {
            country: "CA",
            ..App::new("Globex")
        },
    )
    .await;
    let blank = app(&w.db, "Initech").await;

    let check = apply::preflight(&w.db, &ids(&[&us, &ca, &blank]))
        .await
        .unwrap();
    assert_eq!(check.items[0].authorization, AuthorizationState::NotStated);
    assert_eq!(check.items[1].authorization, AuthorizationState::Stated);
    assert_eq!(check.items[2].authorization, AuthorizationState::NoCountry);
    assert_eq!(check.sealed_total, 1, "the CA row");

    let run = apply::start(&w.db, "cli", &ids(&[&us, &ca, &blank]), true)
        .await
        .unwrap();
    assert!(!item_of(&w.db, &run, &us).await.auth_released);
    assert!(item_of(&w.db, &run, &ca).await.auth_released);

    assert!(brief_for(&w, &run, &us).await.work_authorization.is_none());
    let brief = brief_for(&w, &run, &ca).await;
    let fact = brief.work_authorization.as_ref().unwrap();
    assert_eq!(fact.country, "CA");
    assert_eq!(fact.authorized, "yes");
    assert_eq!(fact.needs_sponsorship, "no");
    let allowed = brief::allowed_values(&brief);
    assert!(allowed.iter().any(|v| v == "yes") && allowed.iter().any(|v| v == "no"));

    // With the key gone, any keychain read would fail.
    drop_key(&w.key_id);
    let brief = brief_for(&w, &run, &blank).await;
    assert!(brief.work_authorization.is_none());
    let check = apply::preflight(&w.db, std::slice::from_ref(&blank))
        .await
        .unwrap();
    assert_eq!(check.items[0].authorization, AuthorizationState::NoCountry);
    w.db.close().await;
}

#[tokio::test]
async fn stage_files_replaces_the_last_applications_files() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let a = app(&w.db, "Acme").await;
    let b = app(&w.db, "Globex").await;
    let run = apply::start(&w.db, "cli", &ids(&[&a, &b]), false)
        .await
        .unwrap();
    let item_a = item_of(&w.db, &run, &a).await;
    let item_b = item_of(&w.db, &run, &b).await;
    let files = files_dir(&w);

    brief::stage_files(&item_a, &files).unwrap();
    std::fs::write(files.join("stray.txt"), "left over").unwrap();
    let staged = brief::stage_files(&item_b, &files).unwrap();

    let mut names: Vec<String> = std::fs::read_dir(&files)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["cover-letter.pdf", "resume.pdf"]);
    let read = |path: &Path| std::fs::read(path).unwrap();
    assert_eq!(
        read(&files.join("resume.pdf")),
        read(item_b.resume_path.as_ref().unwrap())
    );
    assert_eq!(
        read(&files.join("cover-letter.pdf")),
        read(item_b.letter_path.as_ref().unwrap())
    );
    assert_ne!(
        read(item_a.resume_path.as_ref().unwrap()),
        read(item_b.resume_path.as_ref().unwrap())
    );
    assert_eq!(staged.len(), 2);
    for file in &staged {
        let path = Path::new(&file.path);
        assert!(path.is_absolute());
        assert_eq!(path, std::fs::canonicalize(path).unwrap());
    }
    assert!(staged.iter().any(|file| file.kind == "resume"));

    #[cfg(unix)]
    {
        let link = w.tmp.path().join("link.pdf");
        std::os::unix::fs::symlink(item_a.resume_path.as_ref().unwrap(), &link).unwrap();
        let mut linked = item_a.clone();
        linked.resume_path = Some(link);
        assert!(brief::stage_files(&linked, &files).is_err());
    }
    w.db.close().await;
}

#[tokio::test]
async fn a_draft_with_a_flagged_claim_blocks_at_preflight() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let bullet = "Led a team of nine";
    let flagged = drafts::create_version(
        &w.db,
        "resume",
        None,
        "generated",
        "Tailored resume",
        None,
        Some("ollama"),
        doc("Ada Lovelace", bullet),
        vec![ClaimInput {
            text: bullet.into(),
            evidence_ids: Vec::new(),
        }],
    )
    .await
    .unwrap();
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        link: "https://jobs.lever.co/acme/1".into(),
        resume_draft_id: Some(flagged.id),
        ..ApplicationInput::default()
    };
    let a = applications::add(&w.db, "cli", input).await.unwrap().id;

    let check = apply::preflight(&w.db, std::slice::from_ref(&a))
        .await
        .unwrap();
    assert_eq!(check.items[0].resume, DocState::Blocked("claims_blocked"));
    assert_eq!(check.items[0].letter, DocState::Missing);
    assert!(check.items[0].blocks.contains(&"resume_blocked"));

    refused(
        apply::start(&w.db, "cli", std::slice::from_ref(&a), false).await,
        "resume_blocked",
    );
    w.db.close().await;
}

#[tokio::test]
async fn start_refuses_a_second_run_more_than_25_and_a_non_https_link() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let mut many = Vec::new();
    for n in 0..=MAX_RUN_ITEMS {
        many.push(
            add_app(
                &w.db,
                App {
                    docs: false,
                    ..App::new(&format!("Company {n}"))
                },
            )
            .await,
        );
    }
    let code = |result: jobpilot_core::Result<String>| result.unwrap_err().code();
    assert_eq!(
        code(apply::start(&w.db, "cli", &many, false).await),
        "validation"
    );
    assert_eq!(
        code(apply::start(&w.db, "cli", &[], false).await),
        "validation"
    );

    let plain = add_app(
        &w.db,
        App {
            link: "http://example.com/jobs/1",
            ..App::new("Plain")
        },
    )
    .await;
    let check = apply::preflight(&w.db, std::slice::from_ref(&plain))
        .await
        .unwrap();
    assert_eq!(check.items[0].blocks, ["link_not_https"]);
    refused(
        apply::start(&w.db, "cli", std::slice::from_ref(&plain), false).await,
        "link_not_https",
    );

    let applied = app(&w.db, "Applied").await;
    applications::set_status(&w.db, "cli", &applied, "applied", None, "")
        .await
        .unwrap();
    refused(
        apply::start(&w.db, "cli", std::slice::from_ref(&applied), false).await,
        "not_saved",
    );

    let first = apply::start(&w.db, "cli", &many[..MAX_RUN_ITEMS], false)
        .await
        .unwrap();
    assert_eq!(
        apply::view(&w.db, &first).await.unwrap().items.len(),
        MAX_RUN_ITEMS
    );
    refused(
        apply::start(&w.db, "cli", &many[MAX_RUN_ITEMS..], false).await,
        "apply_run_active",
    );
    let check = apply::preflight(&w.db, &many[..1]).await.unwrap();
    assert!(check.items[0].blocks.contains(&"already_in_a_run"));
    w.db.close().await;
}

#[tokio::test]
async fn start_needs_consent_for_the_categories_and_sensitive_only_on_release() {
    let w = world().await;
    let a = app(&w.db, "Acme").await;
    let one = std::slice::from_ref(&a);

    let check = apply::preflight(&w.db, one).await.unwrap();
    assert_eq!(check.consent_missing, BASE);
    refused(
        apply::start(&w.db, "cli", one, false).await,
        "consent_required",
    );

    consent_to(&w.db, &[DataCategory::JobText, DataCategory::ProfileBasic]).await;
    let check = apply::preflight(&w.db, one).await.unwrap();
    assert_eq!(check.consent_missing, [DataCategory::CareerMemory]);
    refused(
        apply::start(&w.db, "cli", one, false).await,
        "consent_required",
    );

    consent_to(&w.db, &BASE).await;
    refused(
        apply::start(&w.db, "cli", one, true).await,
        "consent_required",
    );
    let run = apply::start(&w.db, "cli", one, false).await.unwrap();
    apply::cancel(&w.db, "cli", &run).await.unwrap();

    consent_to(&w.db, &ALL).await;
    apply::start(&w.db, "cli", one, true).await.unwrap();
    let log = jobpilot_core::audit::recent(&w.db, 50).await.unwrap();
    let call = log
        .iter()
        .find(|row| row.action == "ai_external_call")
        .unwrap();
    assert_eq!(call.actor, "cli");
    assert_eq!(call.detail["provider"], PROVIDER);
    assert_eq!(
        call.detail["categories"],
        serde_json::json!(["job_text", "profile_basic", "career_memory", "sensitive"])
    );
    let created = log
        .iter()
        .find(|row| row.action == "apply_run_created")
        .unwrap();
    assert_eq!(created.detail["items"], 1);
    assert_eq!(
        created.detail["hosts"],
        serde_json::json!(["boards.greenhouse.io"])
    );
    w.db.close().await;
}

#[tokio::test]
async fn start_refuses_while_paused() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let a = app(&w.db, "Acme").await;
    pause::set(&w.db, "cli").await.unwrap();

    let result = apply::start(&w.db, "cli", std::slice::from_ref(&a), false).await;

    assert!(matches!(result, Err(Error::Paused)), "{result:?}");
    assert!(apply::latest(&w.db).await.unwrap().is_none());
    w.db.close().await;
}

#[tokio::test]
async fn a_ready_run_can_be_cancelled_and_a_new_run_starts() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let a = app(&w.db, "Acme").await;
    let b = app(&w.db, "Globex").await;
    let run = apply::start(&w.db, "cli", &ids(&[&a, &b]), false)
        .await
        .unwrap();
    let agent_run: Option<String> =
        sqlx::query_scalar("SELECT agent_run_id FROM apply_runs WHERE id = ?")
            .bind(&run)
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
    assert_eq!(agent_run, None);
    let view = apply::latest(&w.db).await.unwrap().unwrap();
    assert_eq!(view.id, run);
    assert_eq!(view.state, "ready");
    assert_eq!(view.mode, "claude_code");
    assert!(view.items.iter().all(|item| item.state == "queued"));
    assert!(
        view.items
            .iter()
            .all(|item| item.policy_note.as_deref() == Some("owner decision 2026-11-01"))
    );
    assert_eq!(view.items[0].host, "boards.greenhouse.io");
    assert_eq!(apply::run_dir(&w.db, &run), w.db.paths().apply().join(&run));

    refused(apply::cancel(&w.db, "system", &run).await, "only_a_person");
    apply::cancel(&w.db, "cli", &run).await.unwrap();

    let view = apply::view(&w.db, &run).await.unwrap();
    assert_eq!(view.state, "ended");
    assert!(view.ended_at.is_some());
    assert!(view.items.iter().all(|item| item.state == "skipped"));
    assert!(
        view.items
            .iter()
            .all(|item| item.reason.as_deref() == Some("run_ended"))
    );
    assert_eq!(
        apply::cancel(&w.db, "cli", &run).await.unwrap_err().code(),
        "validation"
    );
    let log = jobpilot_core::audit::recent(&w.db, 50).await.unwrap();
    assert!(log.iter().any(|row| row.action == "apply_run_cancelled"));

    apply::start(&w.db, "cli", &ids(&[&a, &b]), false)
        .await
        .unwrap();
    w.db.close().await;
}

#[tokio::test]
async fn a_run_row_holds_no_answer_text() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let texts = [
        "ZXQ-NORMAL-ANSWER",
        "ZXQ-SENSITIVE-ANSWER",
        "ZXQ-LEGAL-ANSWER",
    ];
    answer(&w.db, "Why this job?", Class::Normal, texts[0]).await;
    answer(&w.db, "What is your salary?", Class::Sensitive, texts[1]).await;
    answer(&w.db, "Do you certify this?", Class::Legal, texts[2]).await;
    let a = app(&w.db, "Acme").await;
    let run = apply::start(&w.db, "cli", &ids(&[&a]), true).await.unwrap();
    let item = item_of(&w.db, &run, &a).await;
    sqlx::query(
        "INSERT INTO apply_pauses (id, item_id, kind, origin, excerpt, created_at) \
         VALUES ('p1', ?, 'legal', 'page', 'Do you certify this?', 't')",
    )
    .bind(&item.id)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let brief = brief_for(&w, &run, &a).await;
    assert_eq!(
        brief.answers.len(),
        2,
        "normal and the released sensitive one"
    );

    for table in [
        "apply_runs",
        "apply_items",
        "apply_pauses",
        "audit_log",
        "agent_run_log",
    ] {
        let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info(?)")
            .bind(table)
            .fetch_all(&w.db.pool)
            .await
            .unwrap();
        assert!(!columns.is_empty(), "{table}");
        for column in columns {
            for text in texts {
                let sql = format!(
                    "SELECT count(*) FROM {table} WHERE instr(CAST(\"{column}\" AS TEXT), ?) > 0"
                );
                let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                    .bind(text)
                    .fetch_one(&w.db.pool)
                    .await
                    .unwrap();
                assert_eq!(found, 0, "{table}.{column} holds {text}");
            }
        }
    }
    w.db.close().await;
}

#[tokio::test]
async fn resolving_a_pause_is_a_persons_act_and_the_system_actor_cannot() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let a = app(&w.db, "Acme").await;
    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();
    let item = item_of(&w.db, &run, &a).await;
    sqlx::query(
        "INSERT INTO apply_pauses (id, item_id, kind, origin, excerpt, created_at) \
         VALUES ('p1', ?, 'mfa', 'page', 'Enter the code', 't')",
    )
    .bind(&item.id)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let pause = apply::view(&w.db, &run)
        .await
        .unwrap()
        .active_pause
        .unwrap();
    assert_eq!(pause.id, "p1");
    assert_eq!(pause.kind, "mfa");
    assert_eq!(pause.item_id, item.id);

    refused(
        apply::resolve_pause(&w.db, "system", "p1", Resolution::Continue).await,
        "only_a_person",
    );
    assert!(
        apply::view(&w.db, &run)
            .await
            .unwrap()
            .active_pause
            .is_some()
    );

    apply::resolve_pause(&w.db, "gui", "p1", Resolution::TakeOver)
        .await
        .unwrap();
    let (by, resolution): (String, String) =
        sqlx::query_as("SELECT resolved_by, resolution FROM apply_pauses WHERE id = 'p1'")
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
    assert_eq!((by.as_str(), resolution.as_str()), ("gui", "take_over"));
    assert!(
        apply::view(&w.db, &run)
            .await
            .unwrap()
            .active_pause
            .is_none()
    );
    assert!(
        apply::resolve_pause(&w.db, "gui", "p1", Resolution::Continue)
            .await
            .is_err(),
        "a resolved pause stays resolved"
    );
    assert_eq!(
        apply::resolve_pause(&w.db, "gui", "nope", Resolution::Continue)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    w.db.close().await;
}

#[tokio::test]
async fn a_form_question_keeps_form_provenance_and_is_unapproved() {
    let w = world().await;

    let question = memory::add_form_question(&w.db, "What is your expected salary?", None)
        .await
        .unwrap();
    assert_eq!(question.source, "form");
    assert_eq!(question.class, Class::Sensitive, "classed by the rules");
    let again = memory::add_form_question(&w.db, "  What is your EXPECTED salary? ", None)
        .await
        .unwrap();
    assert_eq!(again.id, question.id, "the same key returns the row");
    let hidden = memory::add_form_question(&w.db, "Why\u{200b} us?", Some("pt"))
        .await
        .unwrap();
    assert_eq!(hidden.country.as_deref(), Some("PT"));
    assert!(!hidden.findings.is_empty(), "what was hidden is reported");

    let row = memory::get(&w.db, &question.id).await.unwrap();
    assert!(row.answers.is_empty());
    let approved = memory::usage::approved_for(&w.db).await.unwrap();
    assert!(approved.is_empty());

    let id = answer(&w.db, "Why this job?", Class::Normal, "ZXQ-ANSWER").await;
    let approved = memory::usage::approved_for(&w.db).await.unwrap();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].answer_id, id);
    assert_eq!(approved[0].question, "Why this job?");
    assert_eq!(approved[0].class, Class::Normal);
    assert!(!approved[0].stale);
    assert!(!format!("{approved:?}").contains("ZXQ-ANSWER"));
    w.db.close().await;
}

async fn audit_count(db: &Db, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = ?")
        .bind(action)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_release_over_two_applications_puts_the_answer_in_each_brief() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let salary = "ZXQ-SALARY-ANSWER";
    let sealed = answer(&w.db, "What is your salary?", Class::Sensitive, salary).await;
    let apps = [
        app(&w.db, "Acme").await,
        app(&w.db, "Globex").await,
        app(&w.db, "Initech").await,
    ];

    let run = apply::start(&w.db, "cli", &apps, true).await.unwrap();

    for application in &apps {
        let item = item_of(&w.db, &run, application).await;
        assert_eq!(item.released_answers, std::slice::from_ref(&sealed));
        let brief = brief_for(&w, &run, application).await;
        assert_eq!(brief.answers.len(), 1, "{application}");
        assert_eq!(brief.answers[0].answer, salary);
    }
    assert_eq!(audit_count(&w.db, "sensitive_released").await, 1);
    assert_eq!(
        audit_count(&w.db, "answer_confirmed").await,
        0,
        "no confirmation is written"
    );
    let log = jobpilot_core::audit::recent(&w.db, 50).await.unwrap();
    let row = log
        .iter()
        .find(|row| row.action == "sensitive_released")
        .unwrap();
    assert_eq!(row.detail["applications"], serde_json::json!(apps));
    assert_eq!(row.detail["answers"], 3);
    assert!(!row.detail.to_string().contains(salary));
    w.db.close().await;
}

#[tokio::test]
async fn a_run_with_no_release_holds_no_sensitive_answer_even_when_confirmed() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let salary = "ZXQ-SALARY-ANSWER";
    let sealed = answer(&w.db, "What is your salary?", Class::Sensitive, salary).await;
    let a = app(&w.db, "Acme").await;
    memory::confirm_use(&w.db, "cli", &sealed, &a)
        .await
        .unwrap();

    let first = apply::start(&w.db, "cli", &ids(&[&a]), true).await.unwrap();
    apply::cancel(&w.db, "cli", &first).await.unwrap();
    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();

    let item = item_of(&w.db, &run, &a).await;
    assert!(item.released_answers.is_empty());
    let brief = brief_for(&w, &run, &a).await;
    assert!(brief.answers.is_empty());
    assert!(!json(&brief).contains(salary));
    w.db.close().await;
}

#[tokio::test]
async fn a_failed_start_releases_nothing() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    answer(
        &w.db,
        "What is your salary?",
        Class::Sensitive,
        "ZXQ-SALARY",
    )
    .await;
    let a = app(&w.db, "Acme").await;
    let b = app(&w.db, "Globex").await;

    // The second item's insert fails inside the transaction, after the release is computed.
    sqlx::query(
        "CREATE TRIGGER fail_second BEFORE INSERT ON apply_items WHEN NEW.position = 1 \
         BEGIN SELECT RAISE(ABORT, 'refused by the test'); END",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    assert!(
        apply::start(&w.db, "cli", &ids(&[&a, &b]), true)
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER fail_second")
        .execute(&w.db.pool)
        .await
        .unwrap();

    // The second-run refusal.
    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();
    refused(
        apply::start(&w.db, "cli", &ids(&[&b]), true).await,
        "apply_run_active",
    );

    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM apply_runs")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "only the run with no release");
    let released: i64 =
        sqlx::query_scalar("SELECT count(*) FROM apply_items WHERE released_answers <> '[]'")
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
    assert_eq!(released, 0);
    assert_eq!(audit_count(&w.db, "sensitive_released").await, 0);
    assert_eq!(audit_count(&w.db, "apply_run_created").await, 1);
    for application in [&a, &b] {
        assert!(
            memory::application_answers(&w.db, application)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert!(item_of(&w.db, &run, &a).await.released_answers.is_empty());
    w.db.close().await;
}

#[tokio::test]
async fn an_answer_scoped_to_a_country_serves_only_that_country() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let us_text = "ZXQ-US-ANSWER";
    let sealed_text = "ZXQ-US-SALARY";
    scoped_answer(&w.db, "Why this job?", Some("US"), Class::Normal, us_text).await;
    let sealed = scoped_answer(
        &w.db,
        "What is your salary?",
        Some("us"),
        Class::Sensitive,
        sealed_text,
    )
    .await;
    let us = add_app(
        &w.db,
        App {
            country: "US",
            ..App::new("Acme")
        },
    )
    .await;
    let ca = add_app(
        &w.db,
        App {
            country: "CA",
            ..App::new("Globex")
        },
    )
    .await;
    let blank = app(&w.db, "Initech").await;

    let check = apply::preflight(&w.db, &ids(&[&us, &ca, &blank]))
        .await
        .unwrap();
    assert_eq!((check.items[0].answers, check.items[0].sealed), (1, 1));
    assert_eq!((check.items[1].answers, check.items[1].sealed), (0, 0));
    assert_eq!((check.items[2].answers, check.items[2].sealed), (0, 0));

    let run = apply::start(&w.db, "cli", &ids(&[&us, &ca, &blank]), true)
        .await
        .unwrap();
    assert_eq!(item_of(&w.db, &run, &us).await.released_answers, [sealed]);
    assert!(item_of(&w.db, &run, &ca).await.released_answers.is_empty());

    let brief = json(&brief_for(&w, &run, &us).await);
    assert!(
        brief.contains(us_text) && brief.contains(sealed_text),
        "{brief}"
    );
    for other in [&ca, &blank] {
        let brief = json(&brief_for(&w, &run, other).await);
        assert!(
            !brief.contains(us_text) && !brief.contains(sealed_text),
            "{brief}"
        );
    }
    w.db.close().await;
}

#[tokio::test]
async fn a_stale_sensitive_answer_is_never_released() {
    let w = world().await;
    consent_to(&w.db, &ALL).await;
    let salary = "ZXQ-SALARY-ANSWER";
    let sealed = answer(&w.db, "What is your salary?", Class::Sensitive, salary).await;
    reviewed_days_ago(&w.db, &sealed, FRESH_DAYS + 1).await;
    let a = app(&w.db, "Acme").await;

    let check = apply::preflight(&w.db, std::slice::from_ref(&a))
        .await
        .unwrap();
    assert_eq!(check.items[0].sealed, 0);
    assert_eq!(check.items[0].stale_left_out, 1);

    let run = apply::start(&w.db, "cli", &ids(&[&a]), true).await.unwrap();
    assert!(item_of(&w.db, &run, &a).await.released_answers.is_empty());
    assert!(!json(&brief_for(&w, &run, &a).await).contains(salary));
    assert!(
        memory::find_answer(&w.db, &sealed).await.unwrap().1.stale,
        "the release does not refresh it"
    );
    w.db.close().await;
}

#[tokio::test]
async fn a_file_outside_the_root_or_unresolved_is_refused() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    let a = app(&w.db, "Acme").await;
    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();
    let item = item_of(&w.db, &run, &a).await;
    let outside = w.tmp.path().join("outside.pdf");
    std::fs::write(&outside, "x").unwrap();

    for path in [outside, w.tmp.path().join("no-such-folder/resume.pdf")] {
        let mut moved = item.clone();
        moved.resume_path = Some(path);
        refused(
            brief::build(&w.db, "cli", &moved, &files_dir(&w)).await,
            "file_outside_root",
        );
    }
    w.db.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn answers_are_marked_used_only_after_the_files_are_staged() {
    let w = world().await;
    consent_to(&w.db, &BASE).await;
    answer(&w.db, "Why this job?", Class::Normal, "ZXQ-ANSWER").await;
    let a = app(&w.db, "Acme").await;
    let run = apply::start(&w.db, "cli", &ids(&[&a]), false)
        .await
        .unwrap();
    let mut item = item_of(&w.db, &run, &a).await;
    let link = w.db.paths().root().join("linked.pdf");
    std::os::unix::fs::symlink(item.resume_path.as_ref().unwrap(), &link).unwrap();
    item.resume_path = Some(link);

    assert!(
        brief::build(&w.db, "cli", &item, &files_dir(&w))
            .await
            .is_err()
    );

    assert_eq!(audit_count(&w.db, "answer_used").await, 0);
    assert!(
        memory::application_answers(&w.db, &a)
            .await
            .unwrap()
            .is_empty()
    );
    w.db.close().await;
}
