//! Application tracking works with no AI and never asks the keychain.

use jobpilot_core::applications::{self, ApplicationInput, STATUSES};
use jobpilot_core::crypto::KeyState;
use jobpilot_core::{AppPaths, Db, Error, audit, knowledge};
use tempfile::TempDir;

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn input(company: &str, role: &str) -> ApplicationInput {
    ApplicationInput {
        company: company.into(),
        role: role.into(),
        ..ApplicationInput::default()
    }
}

async fn counts(db: &Db) -> (i64, i64) {
    let a: (i64,) = sqlx::query_as("SELECT count(*) FROM applications")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let e: (i64,) = sqlx::query_as("SELECT count(*) FROM application_events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    (a.0, e.0)
}

#[tokio::test]
async fn add_starts_saved_with_a_created_event() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    assert_eq!(app.status, "saved");
    assert_eq!(app.applied_on, None);
    let events = applications::history(&db, &app.id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "created");
    assert_eq!(events[0].actor, "cli");
    assert_eq!(events[0].to_status.as_deref(), Some("saved"));
    assert_eq!(STATUSES.len(), 8);
}

#[tokio::test]
async fn status_change_records_from_and_to_in_one_event() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "gui", input("Acme", "Engineer"))
        .await
        .unwrap();
    let app = applications::set_status(&db, "gui", &app.id, "applied", None, "sent")
        .await
        .unwrap();
    assert_eq!(app.status, "applied");
    let events = applications::history(&db, &app.id).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "status");
    assert_eq!(events[1].from_status.as_deref(), Some("saved"));
    assert_eq!(events[1].to_status.as_deref(), Some("applied"));
    assert_eq!(events[1].note, "sent");
}

#[tokio::test]
async fn first_applied_sets_applied_on_and_a_later_one_keeps_it() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    let app = applications::set_status(&db, "cli", &app.id, "applied", Some("2026-10-02"), "")
        .await
        .unwrap();
    assert_eq!(app.applied_on.as_deref(), Some("2026-10-02"));
    applications::set_status(&db, "cli", &app.id, "screening", None, "")
        .await
        .unwrap();
    let app = applications::set_status(&db, "cli", &app.id, "applied", Some("2026-10-09"), "")
        .await
        .unwrap();
    assert_eq!(app.applied_on.as_deref(), Some("2026-10-02"));

    let other = applications::add(&db, "cli", input("Beta", "Dev"))
        .await
        .unwrap();
    let other = applications::set_status(&db, "cli", &other.id, "applied", None, "")
        .await
        .unwrap();
    let today: (String,) = sqlx::query_as("SELECT strftime('%Y-%m-%d', 'now')")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(other.applied_on.as_deref(), Some(today.0.as_str()));
}

#[tokio::test]
async fn the_same_status_twice_writes_no_event() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    applications::set_status(&db, "cli", &app.id, "applied", None, "")
        .await
        .unwrap();
    applications::set_status(&db, "cli", &app.id, "applied", None, "  ")
        .await
        .unwrap();
    assert_eq!(applications::history(&db, &app.id).await.unwrap().len(), 2);
    // A typed note is kept, as a note and not a status change.
    let same = applications::set_status(&db, "cli", &app.id, "applied", None, "again")
        .await
        .unwrap();
    assert_eq!(same.status, "applied");
    let events = applications::history(&db, &app.id).await.unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].kind, "note");
    assert_eq!(events[2].note, "again");
    assert_eq!(events[2].from_status, None);
}

#[tokio::test]
async fn any_status_can_be_corrected() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    applications::set_status(&db, "cli", &app.id, "rejected", None, "")
        .await
        .unwrap();
    let app = applications::set_status(&db, "cli", &app.id, "interviewing", None, "my mistake")
        .await
        .unwrap();
    assert_eq!(app.status, "interviewing");
    let events = applications::history(&db, &app.id).await.unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.from_status.as_deref(), Some("rejected"));
    assert_eq!(last.to_status.as_deref(), Some("interviewing"));
}

#[tokio::test]
async fn bad_input_is_refused() {
    let (_tmp, db) = setup().await;
    let good = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    let before = counts(&db).await;
    let mut cases: Vec<ApplicationInput> = Vec::new();
    cases.push(input("", "Engineer"));
    cases.push(input("   ", "Engineer"));
    cases.push(input(&"c".repeat(301), "Engineer"));
    cases.push(input("Acme", ""));
    cases.push(input("Acme", &"r".repeat(301)));
    cases.push(ApplicationInput {
        country: "USA".into(),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        country: "1a".into(),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        notes: "n".repeat(20_001),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        link: "javascript:x".into(),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        link: format!("https://{}", "a".repeat(2_000)),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        job_text_id: Some("nope".into()),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        resume_draft_id: Some("nope".into()),
        ..input("Acme", "Engineer")
    });
    cases.push(ApplicationInput {
        letter_draft_id: Some("nope".into()),
        ..input("Acme", "Engineer")
    });
    for case in cases {
        let added = applications::add(&db, "cli", case.clone()).await;
        assert!(matches!(added, Err(Error::Validation(_))), "{case:?}");
        let updated = applications::update(&db, "cli", &good.id, case.clone()).await;
        assert!(matches!(updated, Err(Error::Validation(_))), "{case:?}");
    }
    for status in ["ghosted", "", "Applied"] {
        let result = applications::set_status(&db, "cli", &good.id, status, None, "").await;
        assert!(matches!(result, Err(Error::Validation(_))), "{status}");
    }
    for on in [
        "02/10/2026",
        "2026-13-01",
        "2026-02-30",
        "2026-1-1",
        "yesterday",
    ] {
        let result = applications::set_status(&db, "cli", &good.id, "applied", Some(on), "").await;
        assert!(matches!(result, Err(Error::Validation(_))), "{on}");
    }
    let long = "x".repeat(20_001);
    let result = applications::set_status(&db, "cli", &good.id, "applied", None, &long).await;
    assert!(matches!(result, Err(Error::Validation(_))));
    let result = applications::add_note(&db, "cli", &good.id, "  ").await;
    assert!(matches!(result, Err(Error::Validation(_))));
    let result = applications::add_note(&db, "cli", &good.id, &long).await;
    assert!(matches!(result, Err(Error::Validation(_))));
    assert_eq!(counts(&db).await, before);
    assert_eq!(
        applications::get(&db, &good.id).await.unwrap().status,
        "saved"
    );
}

#[tokio::test]
async fn good_fields_are_stored_and_normalised() {
    let (_tmp, db) = setup().await;
    let job = knowledge::import_job_text(&db, "Role", "We need a person.")
        .await
        .unwrap();
    let app = applications::add(
        &db,
        "cli",
        ApplicationInput {
            link: "https://example.com/jobs/1".into(),
            country: "pt".into(),
            job_text_id: Some(job.id.clone()),
            ..input("  Acme  ", " Engineer ")
        },
    )
    .await
    .unwrap();
    assert_eq!(app.company, "Acme");
    assert_eq!(app.role, "Engineer");
    assert_eq!(app.country, "PT");
    assert_eq!(app.job_text_id.as_deref(), Some(job.id.as_str()));
    assert!(matches!(
        applications::get(&db, "missing").await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn list_filters_by_status_newest_first() {
    let (_tmp, db) = setup().await;
    let a = applications::add(&db, "cli", input("A", "r"))
        .await
        .unwrap();
    let b = applications::add(&db, "cli", input("B", "r"))
        .await
        .unwrap();
    let c = applications::add(&db, "cli", input("C", "r"))
        .await
        .unwrap();
    applications::set_status(&db, "cli", &b.id, "applied", None, "")
        .await
        .unwrap();
    let all = applications::list(&db, None).await.unwrap();
    let ids: Vec<_> = all.iter().map(|x| x.id.as_str()).collect();
    assert_eq!(ids, [b.id.as_str(), c.id.as_str(), a.id.as_str()]);
    let saved = applications::list(&db, Some("saved")).await.unwrap();
    let ids: Vec<_> = saved.iter().map(|x| x.id.as_str()).collect();
    assert_eq!(ids, [c.id.as_str(), a.id.as_str()]);
    assert!(matches!(
        applications::list(&db, Some("ghosted")).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn update_keeps_status_and_records_an_edit() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    applications::set_status(&db, "cli", &app.id, "interviewing", None, "")
        .await
        .unwrap();
    let app = applications::update(&db, "gui", &app.id, input("Acme Inc", "Staff Engineer"))
        .await
        .unwrap();
    assert_eq!(app.status, "interviewing");
    assert_eq!(app.company, "Acme Inc");
    let events = applications::history(&db, &app.id).await.unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.kind, "edited");
    assert_eq!(last.actor, "gui");
    assert!(matches!(
        applications::update(&db, "gui", "missing", input("A", "b")).await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn a_note_lands_in_the_history() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    applications::add_note(&db, "cli", &app.id, "recruiter called")
        .await
        .unwrap();
    let events = applications::history(&db, &app.id).await.unwrap();
    assert_eq!(events.last().unwrap().kind, "note");
    assert_eq!(events.last().unwrap().note, "recruiter called");
    assert!(matches!(
        applications::add_note(&db, "cli", "missing", "x").await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        applications::history(&db, "missing").await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn remove_audits_and_takes_the_events_with_it() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("SecretCorp", "Hidden Role"))
        .await
        .unwrap();
    applications::set_status(&db, "cli", &app.id, "applied", None, "")
        .await
        .unwrap();
    applications::remove(&db, "gui", &app.id).await.unwrap();
    assert_eq!(counts(&db).await, (0, 0));
    let entry = audit::recent(&db, 5)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.action == "application_removed")
        .expect("audit row");
    assert_eq!(entry.actor, "gui");
    assert_eq!(entry.target.as_deref(), Some(app.id.as_str()));
    let text = entry.detail.to_string();
    assert!(
        !text.contains("SecretCorp") && !text.contains("Hidden"),
        "{text}"
    );
    assert!(matches!(
        applications::remove(&db, "gui", &app.id).await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn tracking_works_with_no_ai_and_no_keychain() {
    let (_tmp, db) = setup().await;
    let app = applications::add(&db, "cli", input("Acme", "Engineer"))
        .await
        .unwrap();
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    applications::get(&db, &app.id).await.unwrap();
    applications::list(&db, None).await.unwrap();
    applications::update(&db, "cli", &app.id, input("Acme", "Dev"))
        .await
        .unwrap();
    applications::set_status(&db, "cli", &app.id, "offer", None, "")
        .await
        .unwrap();
    applications::add_note(&db, "cli", &app.id, "n")
        .await
        .unwrap();
    applications::history(&db, &app.id).await.unwrap();
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    applications::remove(&db, "cli", &app.id).await.unwrap();
    assert_eq!(db.key_state(), KeyState::NotLoaded);
}
