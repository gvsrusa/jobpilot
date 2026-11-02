//! Keychain paths on keyring's in-memory mock, so they run on every build and reach no OS
//! store. The mock keeps nothing past its entry, so every read finds no key, which is the
//! state these tests want. Its own binary, as the default store is process-wide.

use std::sync::Once;

use jobpilot_core::{AppPaths, Db, backup, secrets};

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
