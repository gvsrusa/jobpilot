//! Real credential-store behaviour that unit tests cannot show. The tests are
//! `#[ignore]`d: they use the OS store under `JOBPILOT_SECRET_SERVICE` from
//! `.cargo/config.toml`, never the app's own service.

use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{AppPaths, Db, secrets};

fn service() -> String {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    service
}

/// With the entry gone, the answer is `KeyMissing` on every OS: absent is not locked.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn no_entry_is_missing_on_this_os() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let KeyState::Unlocked { key_id } = crypto::state(&db).await.unwrap() else {
        panic!("first use should make a key");
    };
    let name = format!("{KEY_ACCOUNT_PREFIX}{key_id}");
    let entry = keyring::Entry::new(&service(), &name).unwrap();
    entry.delete_credential().unwrap();

    assert!(secrets::get(&name).unwrap().is_none());
    let state = crypto::state(&db).await.unwrap();
    assert_eq!(state, KeyState::KeyMissing { key_id });
}

/// An over-long entry name makes CredRead fail with something other than NotFound.
/// That must stay an error (`Secret`), so `crypto` reports it as unavailable and
/// never as a missing entry.
#[cfg(target_os = "windows")]
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn credential_manager_error_maps_to_unavailable() {
    let name = "x".repeat(2000);
    let error = secrets::get(&name).unwrap_err();
    assert!(
        matches!(error, jobpilot_core::Error::Secret(_)),
        "{error:?}"
    );
}
