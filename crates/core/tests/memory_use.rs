//! Confirmed use of answers, and work authorization by exact country. The `#[ignore]`d tests
//! use the real OS keychain under the service from `.cargo/config.toml`. The rest never reach
//! it: a sealed row here is seeded under a key made of sevens.

use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::crypto::{self, DataKey, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::memory::{self, Class, FRESH_DAYS};
use jobpilot_core::profile::{self, Stated, WorkAuthorization};
use jobpilot_core::{AppPaths, Db, Error, audit};
use tempfile::TempDir;

const KEY_ID: &str = "00112233aabbccdd";
const CANARY: &str = "ZXQ-CANARY-ANSWER";

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn key() -> DataKey {
    DataKey::from_bytes(KEY_ID, [7; 32])
}

async fn application(db: &Db, company: &str) -> String {
    let input = ApplicationInput {
        company: company.into(),
        role: "Engineer".into(),
        ..ApplicationInput::default()
    };
    applications::add(db, "cli", input).await.unwrap().id
}

async fn question_of(db: &Db, text: &str, class: Class) -> String {
    let question = memory::add_question(db, text, None).await.unwrap();
    sqlx::query("UPDATE memory_questions SET class = ?, class_source = 'user' WHERE id = ?")
        .bind(class.as_str())
        .bind(&question.id)
        .execute(&db.pool)
        .await
        .unwrap();
    question.id
}

/// An approved answer of `class` whose body is sealed under [`key`] when the class is above
/// normal, and plain otherwise. No keychain is asked.
async fn answer_of(db: &Db, class: Class) -> String {
    let question = question_of(db, "Why this job?", class).await;
    let answer = "a1";
    if class.sealed() {
        let check = key()
            .encrypt("data_keys", "key_check", KEY_ID, crypto::key_check_text())
            .unwrap();
        sqlx::query(
            "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', ?, 0) \
             ON CONFLICT(key_id) DO NOTHING",
        )
        .bind(KEY_ID)
        .bind(check)
        .execute(&db.pool)
        .await
        .unwrap();
        let sealed = key()
            .encrypt("memory_answers", "body_enc", answer, CANARY.as_bytes())
            .unwrap();
        sqlx::query(
            "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, source, \
             approved, reviewed_at, created_at, updated_at) \
             VALUES (?, ?, '', ?, ?, 'user', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 't', 't')",
        )
        .bind(answer)
        .bind(&question)
        .bind(sealed)
        .bind(KEY_ID)
        .execute(&db.pool)
        .await
        .unwrap();
    } else {
        let added = memory::add_answer(db, &question, CANARY, None)
            .await
            .unwrap();
        sqlx::query("UPDATE memory_answers SET id = ? WHERE id = ?")
            .bind(answer)
            .bind(&added.id)
            .execute(&db.pool)
            .await
            .unwrap();
        memory::approve(db, "cli", answer, true).await.unwrap();
    }
    answer.into()
}

fn code<T: std::fmt::Debug>(result: Result<T, Error>) -> &'static str {
    result.unwrap_err().code()
}

fn refused<T: std::fmt::Debug>(result: Result<T, Error>, reason: &str) {
    match result {
        Err(Error::Refused(said)) if said == reason => {}
        other => panic!("{other:?}"),
    }
}

async fn void_by_edit(db: &Db, answer: &str) {
    sqlx::query("UPDATE memory_answers SET version = version + 1, approved = 1 WHERE id = ?")
        .bind(answer)
        .execute(&db.pool)
        .await
        .unwrap();
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

#[tokio::test]
async fn a_sealed_answer_needs_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;

    let result = memory::use_answer(&db, "cli", &answer, &application).await;

    assert_eq!(code(result), "confirmation_required");
    db.close().await;
}

#[tokio::test]
async fn a_confirmation_covers_one_application() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Legal).await;
    let first = application(&db, "Acme").await;
    let second = application(&db, "Globex").await;

    memory::confirm_use(&db, "cli", &answer, &first)
        .await
        .unwrap();
    let result = memory::use_answer(&db, "cli", &answer, &second).await;

    assert_eq!(code(result), "confirmation_required");
    db.close().await;
}

#[tokio::test]
async fn editing_an_answer_voids_its_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;
    memory::confirm_use(&db, "cli", &answer, &application)
        .await
        .unwrap();

    void_by_edit(&db, &answer).await;

    let result = memory::use_answer(&db, "cli", &answer, &application).await;
    assert_eq!(code(result), "confirmation_required");
    let listed = memory::application_answers(&db, &application)
        .await
        .unwrap();
    assert!(!listed[0].current);
    db.close().await;
}

#[tokio::test]
async fn a_stale_sealed_answer_needs_a_fresh_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;
    memory::confirm_use(&db, "cli", &answer, &application)
        .await
        .unwrap();

    reviewed_days_ago(&db, &answer, FRESH_DAYS + 1).await;

    let result = memory::use_answer(&db, "cli", &answer, &application).await;
    assert_eq!(code(result), "confirmation_required");
    let listed = memory::application_answers(&db, &application)
        .await
        .unwrap();
    assert!(!listed[0].current);
    db.close().await;
}

#[tokio::test]
async fn an_unapproved_answer_is_refused() {
    let (_tmp, db) = open().await;
    let application = application(&db, "Acme").await;
    for class in [Class::Normal, Class::Sensitive, Class::Legal] {
        sqlx::query("DELETE FROM memory_answers")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM memory_questions")
            .execute(&db.pool)
            .await
            .unwrap();
        let answer = answer_of(&db, class).await;
        sqlx::query("UPDATE memory_answers SET approved = 0")
            .execute(&db.pool)
            .await
            .unwrap();

        refused(
            memory::confirm_use(&db, "cli", &answer, &application).await,
            "answer_not_approved",
        );
        refused(
            memory::use_answer(&db, "cli", &answer, &application).await,
            "answer_not_approved",
        );
    }
    db.close().await;
}

#[tokio::test]
async fn a_person_must_confirm_and_use() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;

    refused(
        memory::confirm_use(&db, "system", &answer, &application).await,
        "only_a_person",
    );
    refused(
        memory::use_answer(&db, "system", &answer, &application).await,
        "only_a_person",
    );
    db.close().await;
}

#[tokio::test]
async fn a_work_authorization_answer_cannot_be_used() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;
    sqlx::query("UPDATE memory_questions SET class = 'work_authorization'")
        .execute(&db.pool)
        .await
        .unwrap();

    refused(
        memory::confirm_use(&db, "cli", &answer, &application).await,
        "use_profile_by_country",
    );
    refused(
        memory::use_answer(&db, "cli", &answer, &application).await,
        "use_profile_by_country",
    );
    db.close().await;
}

#[tokio::test]
async fn an_unknown_answer_or_application_is_not_found() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Normal).await;
    let application = application(&db, "Acme").await;

    assert_eq!(
        code(memory::use_answer(&db, "cli", "nope", &application).await),
        "not_found"
    );
    assert_eq!(
        code(memory::use_answer(&db, "cli", &answer, "nope").await),
        "not_found"
    );
    assert_eq!(
        code(memory::confirm_use(&db, "cli", &answer, "nope").await),
        "not_found"
    );
    db.close().await;
}

#[tokio::test]
async fn a_normal_answer_needs_no_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Normal).await;
    let application = application(&db, "Acme").await;

    let used = memory::use_answer(&db, "gui", &answer, &application)
        .await
        .unwrap();

    assert_eq!(used.text, CANARY);
    assert_eq!(used.class, Class::Normal);
    assert!(!used.stale);
    assert_eq!(used.answer_id, answer);
    assert_eq!(used.application_id, application);
    let listed = memory::application_answers(&db, &application)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].used_at.is_some());
    let log = audit::recent(&db, 20).await.unwrap();
    let row = log.iter().find(|e| e.action == "answer_used").unwrap();
    assert_eq!(row.actor, "gui");
    assert_eq!(row.target.as_deref(), Some(answer.as_str()));
    assert_eq!(
        row.detail,
        serde_json::json!({ "application": application, "class": "normal" })
    );
    assert!(!format!("{log:?}").contains(CANARY));
    db.close().await;
}

#[tokio::test]
async fn a_stale_normal_answer_is_returned_marked_stale() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Normal).await;
    let application = application(&db, "Acme").await;
    reviewed_days_ago(&db, &answer, FRESH_DAYS + 1).await;

    let used = memory::use_answer(&db, "cli", &answer, &application)
        .await
        .unwrap();

    assert!(used.stale);
    assert_eq!(used.text, CANARY);
    db.close().await;
}

#[tokio::test]
async fn confirming_marks_it_reviewed_and_audits_with_no_text() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;
    reviewed_days_ago(&db, &answer, FRESH_DAYS + 1).await;
    assert!(memory::find_answer(&db, &answer).await.unwrap().1.stale);

    memory::confirm_use(&db, "gui", &answer, &application)
        .await
        .unwrap();

    assert!(!memory::find_answer(&db, &answer).await.unwrap().1.stale);
    let log = audit::recent(&db, 20).await.unwrap();
    let row = log.iter().find(|e| e.action == "answer_confirmed").unwrap();
    assert_eq!(row.actor, "gui");
    assert_eq!(row.target.as_deref(), Some(answer.as_str()));
    assert_eq!(
        row.detail,
        serde_json::json!({ "application": application, "class": "sensitive" })
    );
    assert!(!format!("{log:?}").contains(CANARY));
    let listed = memory::application_answers(&db, &application)
        .await
        .unwrap();
    assert!(listed[0].current);
    assert!(listed[0].confirmed_at.is_some());
    db.close().await;
}

#[tokio::test]
async fn application_answers_lists_no_text_and_flags_a_voided_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;
    assert!(
        memory::application_answers(&db, &application)
            .await
            .unwrap()
            .is_empty()
    );
    memory::confirm_use(&db, "cli", &answer, &application)
        .await
        .unwrap();

    let listed = memory::application_answers(&db, &application)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].answer_id, answer);
    assert_eq!(listed[0].question, "Why this job?");
    assert_eq!(listed[0].class, Class::Sensitive);
    assert!(listed[0].current);
    assert!(!format!("{listed:?}").contains(CANARY));

    void_by_edit(&db, &answer).await;
    let listed = memory::application_answers(&db, &application)
        .await
        .unwrap();
    assert!(!listed[0].current);
    assert!(listed[0].confirmed_at.is_some(), "the old time still shows");
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    db.close().await;
}

fn authorization_row(country: &str) -> WorkAuthorization {
    WorkAuthorization {
        country: country.into(),
        authorized: Stated::Yes,
        needs_sponsorship: Stated::No,
        note: "a note".into(),
    }
}

/// A US row sealed under [`key`], with no keychain involved.
async fn seed_us_row(db: &Db) {
    let check = key()
        .encrypt("data_keys", "key_check", KEY_ID, crypto::key_check_text())
        .unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', ?, 0)",
    )
    .bind(KEY_ID)
    .bind(check)
    .execute(&db.pool)
    .await
    .unwrap();
    let plain = serde_json::to_vec(&authorization_row("US")).unwrap();
    let sealed = key()
        .encrypt("work_authorization", "row", "US", &plain)
        .unwrap();
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES ('US', ?, ?, 't')",
    )
    .bind(sealed)
    .bind(KEY_ID)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn canada_never_gets_the_us_row() {
    let (_tmp, db) = open().await;
    seed_us_row(&db).await;

    let answer = memory::authorization_for(&db, "CA").await.unwrap();

    assert!(!answer.stated);
    assert_eq!(answer.country, "CA");
    assert_eq!(answer.authorized, Stated::NotStated);
    assert_eq!(answer.needs_sponsorship, Stated::NotStated);
    assert_eq!(answer.note, "");
    db.close().await;
}

#[tokio::test]
async fn not_stated_asks_the_keychain_nothing() {
    let (_tmp, db) = open().await;

    let answer = memory::authorization_for(&db, "pt").await.unwrap();
    assert!(!answer.stated);
    assert_eq!(answer.country, "PT");
    seed_us_row(&db).await;
    memory::authorization_for(&db, "CA").await.unwrap();

    assert_eq!(db.key_state(), KeyState::NotLoaded);
    db.close().await;
}

#[tokio::test]
async fn a_bad_country_is_refused() {
    let (_tmp, db) = open().await;

    let result = memory::authorization_for(&db, "canada").await;

    assert_eq!(code(result), "validation");
    db.close().await;
}

// ---- keychain --------------------------------------------------------------------------

fn entry(key_id: &str) -> keyring::Entry {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    keyring::Entry::new(&service, &format!("{KEY_ACCOUNT_PREFIX}{key_id}")).unwrap()
}

struct EntryGuard(String);

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let _ = entry(&self.0).delete_credential();
    }
}

async fn unlocked(db: &Db) -> EntryGuard {
    let KeyState::Unlocked { key_id } = crypto::state(db).await.unwrap() else {
        panic!("keychain did not unlock");
    };
    EntryGuard(key_id)
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn a_confirmed_sealed_answer_returns_its_text() {
    let (_tmp, db) = open().await;
    let _guard = unlocked(&db).await;
    let question = memory::add_question(&db, "Your salary?", None)
        .await
        .unwrap();
    let added = memory::add_answer(&db, &question.id, CANARY, None)
        .await
        .unwrap();
    memory::approve(&db, "cli", &added.id, true).await.unwrap();
    let application = application(&db, "Acme").await;

    memory::confirm_use(&db, "cli", &added.id, &application)
        .await
        .unwrap();
    let used = memory::use_answer(&db, "cli", &added.id, &application)
        .await
        .unwrap();

    assert_eq!(used.text, CANARY);
    assert_eq!(used.class, Class::Sensitive);
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn authorization_for_opens_only_the_named_country() {
    let (_tmp, db) = open().await;
    let _guard = unlocked(&db).await;
    profile::set_authorization(&db, authorization_row("US"))
        .await
        .unwrap();
    let mut portugal = authorization_row("PT");
    portugal.authorized = Stated::No;
    portugal.needs_sponsorship = Stated::Yes;
    profile::set_authorization(&db, portugal).await.unwrap();

    let answer = memory::authorization_for(&db, "pt").await.unwrap();

    assert!(answer.stated);
    assert_eq!(answer.country, "PT");
    assert_eq!(answer.authorized, Stated::No);
    assert_eq!(answer.needs_sponsorship, Stated::Yes);
    assert_eq!(answer.note, "a note");
    db.close().await;
}

#[tokio::test]
async fn a_review_for_another_application_does_not_revive_a_stale_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let a = application(&db, "Acme").await;
    let b = application(&db, "Globex").await;
    memory::confirm_use(&db, "cli", &answer, &b).await.unwrap();
    // B was confirmed 300 days ago, and the answer was reviewed 181 days ago.
    sqlx::query(
        "UPDATE application_answers SET confirmed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-300 days')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    reviewed_days_ago(&db, &answer, FRESH_DAYS + 1).await;
    memory::confirm_use(&db, "cli", &answer, &a).await.unwrap();

    // The answer is fresh again, through A only.
    assert_eq!(
        code(memory::use_answer(&db, "cli", &answer, &b).await),
        "confirmation_required"
    );
    assert!(!memory::application_answers(&db, &b).await.unwrap()[0].current);
    assert!(memory::application_answers(&db, &a).await.unwrap()[0].current);
    // Confirming on B itself makes B current: the same-application path still works.
    memory::confirm_use(&db, "cli", &answer, &b).await.unwrap();
    assert!(memory::application_answers(&db, &b).await.unwrap()[0].current);
    db.close().await;
}

#[tokio::test]
async fn a_class_change_or_unapproval_voids_a_confirmation() {
    let (_tmp, db) = open().await;
    let answer = answer_of(&db, Class::Sensitive).await;
    let application = application(&db, "Acme").await;
    memory::confirm_use(&db, "cli", &answer, &application)
        .await
        .unwrap();
    let current = || async {
        memory::application_answers(&db, &application)
            .await
            .unwrap()[0]
            .current
    };
    assert!(current().await);

    sqlx::query("UPDATE memory_questions SET class = 'legal', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','+1 seconds')")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        !current().await,
        "a raise to legal voids it, though approved"
    );
    assert_eq!(
        code(memory::use_answer(&db, "cli", &answer, &application).await),
        "confirmation_required"
    );

    sqlx::query("UPDATE memory_questions SET updated_at = '0'")
        .execute(&db.pool)
        .await
        .unwrap();
    memory::confirm_use(&db, "cli", &answer, &application)
        .await
        .unwrap();
    assert!(current().await);
    memory::approve(&db, "cli", &answer, false).await.unwrap();
    assert!(!current().await, "an unapproved answer is not current");
    db.close().await;
}
