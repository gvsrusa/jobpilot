//! Credential Manager mishandles overlapping calls, so the product makes them one at a time.
//! This runs on a store that measures overlap, so it shows on every OS, with no real keychain.
//! Its own binary, as the default store is process-wide.

use std::any::Any;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use jobpilot_core::{AppPaths, Db, crypto, secrets};
use keyring_core::api::{CredentialApi, CredentialStoreApi};
use keyring_core::{Credential, Entry, Result};

static RUNNING: AtomicUsize = AtomicUsize::new(0);
static MOST_AT_ONCE: AtomicUsize = AtomicUsize::new(0);

/// Holds each call open long enough that any two allowed to overlap will.
fn measured<T>(call: impl FnOnce() -> T) -> T {
    let running = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
    MOST_AT_ONCE.fetch_max(running, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(10));
    let result = call();
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    result
}

struct Measured(Entry);

impl CredentialApi for Measured {
    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        measured(|| self.0.set_secret(secret))
    }

    fn get_secret(&self) -> Result<Vec<u8>> {
        measured(|| self.0.get_secret())
    }

    fn delete_credential(&self) -> Result<()> {
        measured(|| self.0.delete_credential())
    }

    fn get_credential(&self) -> Result<Option<Arc<Credential>>> {
        Ok(None)
    }

    fn get_specifiers(&self) -> Option<(String, String)> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct MeasuredStore(Arc<keyring_core::mock::Store>);

impl CredentialStoreApi for MeasuredStore {
    fn vendor(&self) -> String {
        "measured".into()
    }

    fn id(&self) -> String {
        "measured".into()
    }

    fn build(
        &self,
        service: &str,
        user: &str,
        modifiers: Option<&HashMap<&str, &str>>,
    ) -> Result<Entry> {
        let entry = self.0.build(service, user, modifiers)?;
        Ok(Entry::new_with_credential(Arc::new(Measured(entry))))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn measured_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        // The first `Entry` installs the OS store, so that happens first, then this replaces it.
        let _ = keyring::Entry::store_status();
        let mock = keyring_core::mock::Store::new().unwrap();
        keyring_core::set_default_store(Arc::new(MeasuredStore(mock)));
    });
}

// The exact calls of a Windows CI failure: first keys stored and losers' entries removed at
// once on one root, beside a provider key's write, read and removal.
#[tokio::test]
async fn no_two_keychain_calls_overlap() {
    measured_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();

    let (a, b, c, set) = tokio::join!(
        crypto::state(&db),
        crypto::state(&db),
        crypto::state(&db),
        secrets::set_api_key(&db, "openai", "sk-abcdefgh", "test"),
    );
    set.unwrap();
    let (first, second, third) = (a.unwrap(), b.unwrap(), c.unwrap());
    assert_eq!(first, second);
    assert_eq!(first, third);
    assert!(
        matches!(first, crypto::KeyState::Unlocked { .. }),
        "{first:?}"
    );
    let (d, e, gone) = tokio::join!(
        crypto::state(&db),
        crypto::state(&db),
        secrets::delete_api_key(&db, "openai", "test"),
    );
    assert_eq!(d.unwrap(), first);
    assert_eq!(e.unwrap(), first);
    gone.unwrap();

    assert_eq!(MOST_AT_ONCE.load(Ordering::SeqCst), 1);
    db.close().await;
}

// The app and the CLI in its terminal are two processes on one store.
#[tokio::test]
async fn a_keychain_call_waits_for_another_process() {
    measured_store();
    let held = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(std::env::temp_dir().join("jobpilot-keychain.lock"))
        .unwrap();
    held.lock().unwrap();

    let call = tokio::task::spawn_blocking(|| secrets::get("ai-key-held-elsewhere"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!call.is_finished(), "the call did not wait for the lock");

    held.unlock().unwrap();
    assert_eq!(call.await.unwrap().unwrap(), None);
}
