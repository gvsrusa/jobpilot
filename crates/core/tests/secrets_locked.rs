//! A locked keychain must read as `KeyUnavailable` (temporary), never `KeyMissing`.
//!
//! This test swaps the default keychain, so it has its own binary and runs only when
//! `JOBPILOT_ALLOW_KEYCHAIN_SWAP=1` (CI sets it; do not set it on a machine whose
//! keychain matters unless you accept the swap). It never touches the login keychain's
//! contents: it makes a throwaway keychain file, makes that the default, and a drop
//! guard restores the earlier default and deletes the throwaway even on failure. A
//! killed test process can still leave the throwaway as default; `security
//! default-keychain -s <old>` repairs that.
//!
//! Observed on macOS before any change in this task: a locked keychain already mapped
//! to `Error::Secret`, then `KeyUnavailable` carrying the key id, so Step 3's mapping
//! needed no code change and this test pins it.

#![cfg(target_os = "macos")]

use std::process::Command;

use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::{AppPaths, Db};

fn security(args: &[&str]) -> String {
    let out = Command::new("security").args(args).output().unwrap();
    assert!(out.status.success(), "security {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_matches('"')
        .to_owned()
}

struct Swap {
    previous: String,
    temp: String,
}

impl Drop for Swap {
    fn drop(&mut self) {
        let _ = Command::new("security")
            .args(["default-keychain", "-s", &self.previous])
            .status();
        let _ = Command::new("security")
            .args(["delete-keychain", &self.temp])
            .status();
    }
}

#[tokio::test]
#[ignore = "swaps the default keychain"]
async fn locked_keychain_is_unavailable_not_missing() {
    if std::env::var("JOBPILOT_ALLOW_KEYCHAIN_SWAP").as_deref() != Ok("1") {
        eprintln!("skipped: set JOBPILOT_ALLOW_KEYCHAIN_SWAP=1 to swap the default keychain");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let temp = tmp.path().join("locked.keychain-db");
    let temp = temp.to_str().unwrap().to_owned();
    let previous = security(&["default-keychain"]);

    security(&["create-keychain", "-p", "pw", &temp]);
    let _swap = Swap {
        previous,
        temp: temp.clone(),
    };
    security(&["default-keychain", "-s", &temp]);
    security(&["unlock-keychain", "-p", "pw", &temp]);

    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let KeyState::Unlocked { key_id } = crypto::state(&db).await.unwrap() else {
        panic!("first use should make a key in the throwaway keychain");
    };

    security(&["lock-keychain", &temp]);
    let state = crypto::state(&db).await.unwrap();
    assert!(
        matches!(&state, KeyState::KeyUnavailable { key_id: id, .. } if *id == key_id),
        "{state:?}"
    );
}
