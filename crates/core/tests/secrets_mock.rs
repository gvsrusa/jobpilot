//! Keychain paths on keyring's in-memory mock, so they run on every build and reach no OS
//! store. The mock keeps entries by name for the whole process, so each test here uses its own
//! provider. Its own binary, as the default store is process-wide.

#[path = "fake_http/mod.rs"]
mod fake_http;

use std::sync::Once;

use fake_http::{Canned, Fake};
use jobpilot_core::ai::api::{self, ApiProvider, KeySource};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    CancellationToken, DataCategory, Outcome, Prompt, Provider, TaskType, consent,
};
use jobpilot_core::{AppPaths, Db, backup, secrets};
use serde_json::json;

fn mock_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        // The first `Entry` installs the OS store, so that happens first, then this replaces it.
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}

#[tokio::test]
async fn a_restored_row_whose_key_is_gone_is_dropped() {
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    secrets::set_api_key(&db, "openai", "sk-abcdefgh", "test")
        .await
        .unwrap();
    secrets::set(&db, "my-token", "v", "test").await.unwrap();
    let saved = backup::create(&db, "test").await.unwrap();
    secrets::delete_api_key(&db, "openai", "test")
        .await
        .unwrap();

    let restored = backup::restore(db, &saved.path, "test").await.unwrap();

    assert_eq!(secrets::names(&restored.db).await.unwrap(), ["my-token"]);
    restored.db.close().await;
}

/// `secret_index` said a key is stored, and the entry is gone. A send reads the keychain, finds
/// nothing, and stops before the socket.
#[tokio::test]
async fn a_keychain_entry_that_is_gone_is_no_key_and_sends_nothing() {
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    consent::give_for(&db, "openrouter", &[DataCategory::JobText], false, "test")
        .await
        .unwrap();
    let fake = Fake::start(vec![(
        "/chat/completions",
        Canned::json(200, &json!({"choices": []})),
    )]);
    let key = KeySource::Keychain("openrouter".into());
    let model = "openai/gpt-x";
    let provider = ApiProvider::for_test("openrouter", &fake.url, model, key).unwrap();
    let router = Router::with_providers(vec![Box::new(provider)]);
    let prompt = Prompt {
        system: "s".into(),
        user: "u".into(),
        schema: "",
    };

    let outcome = router
        .run(
            &db,
            "test",
            TaskType::ParseJob,
            prompt,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        Outcome::ManualRequired {
            reason: "no_key".into()
        }
    );
    assert!(fake.paths().is_empty());
    db.close().await;
}

/// The whole road for a stored key: `secret_index` names it, and the send reads it from the
/// store on each attempt.
#[tokio::test]
async fn a_stored_endpoint_key_is_read_at_each_send() {
    const PATH: &str = "/chat/completions";
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let reply = json!({"choices": [{"message": {"content": "{}"}}]});
    let fake = Fake::start(vec![(PATH, Canned::json(200, &reply))]);
    api::set_endpoint(&db, &fake.url).await.unwrap();
    api::set_model(&db, "endpoint", "m").await.unwrap();
    secrets::set_api_key(&db, "endpoint", "sk-first-key", "test")
        .await
        .unwrap();
    let endpoint = ApiProvider::from_settings(&db, "endpoint")
        .await
        .unwrap()
        .unwrap();
    let prompt = Prompt {
        system: "s".into(),
        user: "u".into(),
        schema: "{}",
    };

    endpoint
        .complete(&prompt, CancellationToken::new())
        .await
        .unwrap();
    secrets::set_api_key(&db, "endpoint", "sk-second-key", "test")
        .await
        .unwrap();
    endpoint
        .complete(&prompt, CancellationToken::new())
        .await
        .unwrap();

    let headers = fake.headers(PATH);
    let bearers: Vec<&String> = headers
        .iter()
        .filter(|line| line.starts_with("authorization"))
        .collect();
    assert_eq!(
        bearers,
        [
            "authorization: bearer sk-first-key",
            "authorization: bearer sk-second-key"
        ]
    );
    db.close().await;
}
