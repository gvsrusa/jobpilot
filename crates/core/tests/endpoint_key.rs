//! A key belongs to the address it was stored for. On keyring's in-memory mock, so no test here
//! reaches an OS store. The mock's store is process-wide and keeps entries by name, so each test
//! holds the lock for its whole run.

#[path = "fake_http/mod.rs"]
mod fake_http;

use std::sync::Once;

use fake_http::{Canned, Fake};
use jobpilot_core::ai::api::{set_endpoint, set_model};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    self, CancellationToken, DataCategory, Outcome, Prompt, TaskType, consent,
};
use jobpilot_core::{AppPaths, Db, backup, secrets};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};

const NAME: &str = "ai-key-endpoint";
const PATH: &str = "/chat/completions";
const JOB: &str = r#"{"title":"Engineer","company":"Acme","location":"Oslo","remote":"hybrid",
    "skills":["Rust"],"seniority":"senior","compensation":""}"#;

static ONE_AT_A_TIME: Mutex<()> = Mutex::const_new(());

async fn mock_store() -> MutexGuard<'static, ()> {
    static SET: Once = Once::new();
    SET.call_once(|| {
        // The first `Entry` installs the OS store, so that happens first, then this replaces it.
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
    ONE_AT_A_TIME.lock().await
}

async fn db() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

async fn store_key(db: &Db) {
    secrets::set_api_key(db, "endpoint", "sk-first-address", "test")
        .await
        .unwrap();
    assert_eq!(secrets::names(db).await.unwrap(), [NAME]);
}

fn gone(db_names: &[String]) -> bool {
    !db_names.iter().any(|name| name == NAME) && secrets::get(NAME).unwrap().is_none()
}

fn reply() -> Canned {
    let choice = json!({"message": {"content": JOB}, "finish_reason": "stop"});
    Canned::json(200, &json!({"choices": [choice]}))
}

#[tokio::test]
async fn a_changed_address_drops_the_key() {
    let _held = mock_store().await;
    let (_tmp, db) = db().await;
    set_endpoint(&db, "http://127.0.0.1:1234/v1").await.unwrap();
    store_key(&db).await;

    set_endpoint(&db, "http://127.0.0.1:1234/v1/")
        .await
        .unwrap();
    assert_eq!(
        secrets::names(&db).await.unwrap(),
        [NAME],
        "the same address"
    );

    set_endpoint(&db, "http://127.0.0.1:5678/v1").await.unwrap();
    assert!(gone(&secrets::names(&db).await.unwrap()));
    db.close().await;
}

#[tokio::test]
async fn clearing_the_address_drops_the_key() {
    let _held = mock_store().await;
    let (_tmp, db) = db().await;
    set_endpoint(&db, "http://127.0.0.1:1234/v1").await.unwrap();
    store_key(&db).await;

    set_endpoint(&db, "").await.unwrap();

    assert!(gone(&secrets::names(&db).await.unwrap()));
    db.close().await;
}

/// Through the whole stack: the second server sees no key, then no request at all until fresh
/// consent, and then a request that carries none.
#[tokio::test]
async fn the_second_address_never_sees_the_first_key() {
    let _held = mock_store().await;
    let (_tmp, db) = db().await;
    let first = Fake::start(vec![(PATH, reply())]);
    let second = Fake::start(vec![(PATH, reply())]);
    set_endpoint(&db, &first.url).await.unwrap();
    set_model(&db, "endpoint", "m").await.unwrap();
    store_key(&db).await;
    ai::set_enabled(&db, "ollama", false).await.unwrap();
    ai::set_enabled(&db, "endpoint", true).await.unwrap();
    consent::give_for(&db, "endpoint", &[DataCategory::JobText], false, "test")
        .await
        .unwrap();
    let run = async |db: &Db| {
        let prompt = Prompt {
            system: "s".into(),
            user: "u".into(),
            schema: "",
        };
        let cancel = CancellationToken::new();
        let router = Router::from_settings(db).await.unwrap();
        router
            .run(db, "test", TaskType::ParseJob, prompt, cancel)
            .await
            .unwrap()
    };
    assert!(matches!(run(&db).await, Outcome::Ok { .. }));
    let bearer = "authorization: bearer sk-first-address".to_string();
    assert!(first.headers(PATH).contains(&bearer));

    set_endpoint(&db, &second.url).await.unwrap();
    assert!(!matches!(run(&db).await, Outcome::Ok { .. }));
    assert!(second.paths().is_empty());

    consent::give_for(&db, "endpoint", &[DataCategory::JobText], false, "test")
        .await
        .unwrap();
    assert!(matches!(run(&db).await, Outcome::Ok { .. }));
    let headers = second.headers(PATH);
    assert!(!headers.iter().any(|line| line.starts_with("authorization")));
    db.close().await;
}

/// A backup can carry another address with no dialog. Consent was already revoked by every
/// restore, so what is left to prove is the key.
#[tokio::test]
async fn a_restore_that_changes_the_address_drops_the_key() {
    let _held = mock_store().await;
    let (_tmp, db) = db().await;
    set_endpoint(&db, "http://127.0.0.1:5678/v1").await.unwrap();
    let saved = backup::create(&db, "test").await.unwrap();
    set_endpoint(&db, "http://127.0.0.1:1234/v1").await.unwrap();
    store_key(&db).await;
    consent::give_for(&db, "endpoint", &[DataCategory::JobText], false, "test")
        .await
        .unwrap();

    let restored = backup::restore(db, &saved.path, "test").await.unwrap().db;

    assert!(gone(&secrets::names(&restored).await.unwrap()));
    assert_eq!(
        consent::active(&restored, "endpoint/loopback")
            .await
            .unwrap(),
        None
    );
    restored.close().await;
}

#[tokio::test]
async fn a_restore_of_the_same_address_keeps_the_key() {
    let _held = mock_store().await;
    let (_tmp, db) = db().await;
    set_endpoint(&db, "http://127.0.0.1:1234/v1").await.unwrap();
    store_key(&db).await;
    let saved = backup::create(&db, "test").await.unwrap();

    let restored = backup::restore(db, &saved.path, "test").await.unwrap().db;

    assert_eq!(secrets::names(&restored).await.unwrap(), [NAME]);
    assert!(secrets::get(NAME).unwrap().is_some());
    secrets::delete_api_key(&restored, "endpoint", "test")
        .await
        .unwrap();
    restored.close().await;
}
