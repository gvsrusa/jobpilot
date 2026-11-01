//! The `#[ignore]`d tests use the real OS keychain, under the service from
//! `.cargo/config.toml`. The rest never depend on what the keychain answers.

use jobpilot_core::crypto::{self, DataKey, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{AppPaths, Db, Error, audit};
use tempfile::TempDir;

const KEY_ID: &str = "00112233aabbccdd";
const OTHER_KEY_ID: &str = "99887766ffeeddcc";

fn temp_root() -> (TempDir, AppPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    (tmp, paths)
}

fn key() -> DataKey {
    DataKey::from_bytes(KEY_ID, [7; 32])
}

fn entry(key_id: &str) -> keyring::Entry {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    keyring::Entry::new(&service, &format!("{KEY_ACCOUNT_PREFIX}{key_id}")).unwrap()
}

/// Removes a key's keychain entry even when an assertion fails first.
struct EntryGuard(String);

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let _ = entry(&self.0).delete_credential();
    }
}

async fn key_rows(db: &Db) -> Vec<(String, Vec<u8>, i64)> {
    sqlx::query_as("SELECT key_id, key_check, active FROM data_keys ORDER BY key_id")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

/// A root with its first key, which the keychain must have accepted.
async fn unlocked_root(paths: &AppPaths) -> (Db, String, EntryGuard) {
    let db = Db::open(paths).await.unwrap();
    let KeyState::Unlocked { key_id } = db.key_state() else {
        panic!("{:?}", db.key_state());
    };
    let guard = EntryGuard(key_id.clone());
    (db, key_id, guard)
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn first_open_creates_one_active_key_and_audits() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;

    let rows = key_rows(&db).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, key_id);
    assert_eq!(rows[0].2, 1);
    assert_eq!(key_id.len(), 16);
    assert!(
        key_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );

    let hex = entry(&key_id).get_password().unwrap();
    assert_eq!(hex.len(), 64);
    assert!(
        crypto::key_state_of(&key_id, &rows[0].1)
            == KeyState::Unlocked {
                key_id: key_id.clone()
            }
    );

    let log = audit::recent(&db, 10).await.unwrap();
    let created: Vec<_> = log
        .iter()
        .filter(|e| e.action == "data_key_created")
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].detail, serde_json::json!({ "key_id": key_id }));
    assert!(!created[0].detail.to_string().contains(&hex));
    db.close().await;

    let db = Db::open(&paths).await.unwrap();
    assert_eq!(db.key_state(), KeyState::Unlocked { key_id });
    assert_eq!(key_rows(&db).await.len(), 1, "a second open adds nothing");
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn open_never_creates_a_second_key_while_ciphertext_exists() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    let before = key_rows(&db).await;
    db.close().await;

    entry(&key_id).delete_credential().unwrap();
    let db = Db::open(&paths).await.unwrap();

    assert_eq!(
        db.key_state(),
        KeyState::KeyMissing {
            key_id: key_id.clone()
        }
    );
    assert_eq!(key_rows(&db).await, before);
    assert!(matches!(
        crypto::unlocked(&db).await,
        Err(Error::KeyMissing { key_id: id }) if id == key_id
    ));
    assert_eq!(
        key_rows(&db).await,
        before,
        "asking for the key adds nothing"
    );
    assert!(matches!(
        entry(&key_id).get_password(),
        Err(keyring::Error::NoEntry)
    ));
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn wrong_key_in_store_is_mismatch() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    let before = key_rows(&db).await;
    db.close().await;

    let wrong = "ab".repeat(32);
    entry(&key_id).set_password(&wrong).unwrap();
    let db = Db::open(&paths).await.unwrap();

    assert_eq!(
        db.key_state(),
        KeyState::KeyMismatch {
            key_id: key_id.clone()
        }
    );
    assert_eq!(key_rows(&db).await, before);
    assert_eq!(
        entry(&key_id).get_password().unwrap(),
        wrong,
        "the entry is left as it is"
    );
    db.close().await;
}

// A value that is not a key at all is still "the store returned a key that fails the
// check", never `KeyMissing`, and never reaches a message.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn malformed_stored_value_is_mismatch_and_stays_private() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;

    for value in [
        "hunter2",
        "",
        &"ab".repeat(31),
        &"ab".repeat(33),
        &format!("+f{}", "ab".repeat(31)),
        &"é".repeat(32),
    ] {
        entry(&key_id).set_password(value).unwrap();
        let state = crypto::state(&db).await.unwrap();
        assert_eq!(
            state,
            KeyState::KeyMismatch {
                key_id: key_id.clone()
            },
            "{value:?}"
        );
        assert_eq!(db.key_state(), state);
        let error = crypto::unlocked(&db).await.err().unwrap();
        assert!(matches!(error, Error::KeyMismatch { .. }), "{error:?}");
    }
    assert!(!format!("{:?}", db.key_state()).contains("hunter2"));
    db.close().await;
}

// Retry: a repaired store moves the state back, and `state` updates what `key_state` says.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn state_requeries_the_store() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    let hex = entry(&key_id).get_password().unwrap();

    entry(&key_id).delete_credential().unwrap();
    assert_eq!(
        db.key_state(),
        KeyState::Unlocked {
            key_id: key_id.clone()
        },
        "read at open"
    );
    let missing = KeyState::KeyMissing {
        key_id: key_id.clone(),
    };
    assert_eq!(crypto::state(&db).await.unwrap(), missing);
    assert_eq!(db.key_state(), missing);

    entry(&key_id).set_password(&hex).unwrap();
    let unlocked = KeyState::Unlocked {
        key_id: key_id.clone(),
    };
    assert_eq!(crypto::state(&db).await.unwrap(), unlocked);
    assert_eq!(db.key_state(), unlocked);
    let key = crypto::unlocked(&db).await.unwrap();
    assert_eq!(key.key_id(), key_id);
    db.close().await;
}

// Threat model section 9: rows under a key the key-check does not cover are a mismatch.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn row_under_another_key_id_is_mismatch() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', x'00', 0)",
    )
    .bind(OTHER_KEY_ID)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) VALUES ('PT', x'00', ?, 't')")
        .bind(OTHER_KEY_ID)
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::KeyMismatch {
            key_id: OTHER_KEY_ID.into()
        }
    );

    sqlx::query("DELETE FROM work_authorization")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::Unlocked { key_id }
    );
    db.close().await;
}

// Two handles may open a new root at once. Only one key may survive, and the loser's
// keychain entry must go, not the winner's.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn racing_first_creations_leave_one_key() {
    let (_tmp, paths) = temp_root();
    let (db, first_id, _guard) = unlocked_root(&paths).await;
    sqlx::query("DELETE FROM data_keys")
        .execute(&db.pool)
        .await
        .unwrap();

    let (a, b, c) = tokio::join!(crypto::state(&db), crypto::state(&db), crypto::state(&db));

    let rows = key_rows(&db).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let winner = rows[0].0.clone();
    let _winner_guard = EntryGuard(winner.clone());
    assert_ne!(winner, first_id);
    for state in [a, b, c] {
        assert_eq!(
            state.unwrap(),
            KeyState::Unlocked {
                key_id: winner.clone()
            }
        );
    }
    assert_eq!(entry(&winner).get_password().unwrap().len(), 64);
    db.close().await;
}

// A restore can leave rows with none active. Rows exist, so no key may be made, whatever
// the keychain says, and open still succeeds.
#[tokio::test]
async fn rows_without_an_active_key_never_create_one() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let opened: Vec<_> = key_rows(&db).await;
    let _guards: Vec<_> = opened.iter().map(|row| EntryGuard(row.0.clone())).collect();
    sqlx::query("UPDATE data_keys SET active = 0")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, '9999', x'00', 0)")
        .bind(OTHER_KEY_ID)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = key_rows(&db).await;
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    assert_eq!(
        db.key_state(),
        KeyState::KeyMissing {
            key_id: OTHER_KEY_ID.into()
        }
    );
    assert_eq!(key_rows(&db).await, before);
    db.close().await;
}

#[tokio::test]
async fn export_lists_key_ids_and_no_key_material() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let _guards: Vec<_> = key_rows(&db)
        .await
        .iter()
        .map(|row| EntryGuard(row.0.clone()))
        .collect();
    sqlx::query("INSERT OR IGNORE INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', x'0102', 0)")
        .bind(OTHER_KEY_ID)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) VALUES ('PT', x'0102', ?, 't')")
        .bind(OTHER_KEY_ID)
        .execute(&db.pool)
        .await
        .unwrap();

    let dir = jobpilot_core::transfer::export_all(&db, "cli")
        .await
        .unwrap();

    let text = std::fs::read_to_string(dir.join("data.json")).unwrap();
    let data: serde_json::Value = serde_json::from_str(&text).unwrap();
    let keys = data["tables"]["data_keys"].as_array().unwrap();
    assert!(keys.iter().any(|row| row["key_id"] == OTHER_KEY_ID));
    assert!(keys.iter().all(|row| row.get("key_check").is_none()));
    let rows = data["tables"]["work_authorization"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["country"], "PT");
    assert!(rows[0].get("ciphertext").is_none());
    db.close().await;
}

#[test]
fn encrypt_decrypt_round_trip_with_fresh_nonce() {
    let key = key();
    let first = key.encrypt("t", "c", "a", b"secret").unwrap();
    let second = key.encrypt("t", "c", "a", b"secret").unwrap();

    assert_ne!(first, second);
    assert_eq!(key.decrypt("t", "c", "a", &first).unwrap(), b"secret");
    assert_eq!(key.decrypt("t", "c", "a", &second).unwrap(), b"secret");
    assert_eq!(key.key_id(), KEY_ID);
}

#[test]
fn ciphertext_has_the_documented_layout() {
    let sealed = key().encrypt("t", "c", "a", b"secret").unwrap();

    assert_eq!(&sealed[..4], b"JPK1");
    assert_eq!(&sealed[4..20], KEY_ID.as_bytes());
    assert_eq!(sealed.len(), 4 + 16 + 12 + b"secret".len() + 16);
    assert!(!sealed.windows(6).any(|w| w == b"secret"));
}

#[test]
fn empty_plaintext_round_trips() {
    let sealed = key().encrypt("t", "c", "a", b"").unwrap();
    assert!(key().decrypt("t", "c", "a", &sealed).unwrap().is_empty());
}

#[test]
fn swapped_row_id_fails_to_decrypt() {
    let sealed = key().encrypt("t", "c", "a", b"secret").unwrap();

    let result = key().decrypt("t", "c", "b", &sealed);

    assert!(matches!(result, Err(Error::Validation(_))), "{result:?}");
}

#[test]
fn swapped_table_or_column_fails_to_decrypt() {
    let sealed = key().encrypt("t", "c", "a", b"secret").unwrap();

    assert!(matches!(
        key().decrypt("x", "c", "a", &sealed),
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        key().decrypt("t", "x", "a", &sealed),
        Err(Error::Validation(_))
    ));
}

#[test]
fn foreign_key_id_header_is_reported() {
    let sealed = DataKey::from_bytes(OTHER_KEY_ID, [7; 32])
        .encrypt("t", "c", "a", b"secret")
        .unwrap();

    let result = key().decrypt("t", "c", "a", &sealed);

    assert!(
        matches!(&result, Err(Error::KeyMismatch { key_id }) if key_id == OTHER_KEY_ID),
        "{result:?}"
    );
}

// The key id is in the associated data, so rewriting the header cannot pass as the
// reader's own ciphertext even under the same key bytes.
#[test]
fn rewritten_key_id_header_fails_to_decrypt() {
    let mut sealed = DataKey::from_bytes(OTHER_KEY_ID, [7; 32])
        .encrypt("t", "c", "a", b"secret")
        .unwrap();
    sealed[4..20].copy_from_slice(KEY_ID.as_bytes());

    assert!(matches!(
        key().decrypt("t", "c", "a", &sealed),
        Err(Error::Validation(_))
    ));
}

#[test]
fn wrong_key_bytes_fail_to_decrypt() {
    let sealed = key().encrypt("t", "c", "a", b"secret").unwrap();
    let wrong = DataKey::from_bytes(KEY_ID, [8; 32]);

    assert!(matches!(
        wrong.decrypt("t", "c", "a", &sealed),
        Err(Error::Validation(_))
    ));
}

#[test]
fn damaged_ciphertext_is_validation_not_a_panic() {
    let sealed = key().encrypt("t", "c", "a", b"secret").unwrap();
    let mut flipped = sealed.clone();
    *flipped.last_mut().unwrap() ^= 1;
    let mut bad_magic = sealed.clone();
    bad_magic[0] = b'X';
    let mut bad_id = sealed.clone();
    bad_id[4] = 0xff;

    for input in [
        Vec::new(),
        b"JPK1".to_vec(),
        sealed[..sealed.len() - 7].to_vec(),
        sealed[..4 + 16 + 12 + 15].to_vec(),
        flipped,
        bad_magic,
        bad_id,
    ] {
        let result = key().decrypt("t", "c", "a", &input);
        assert!(
            matches!(result, Err(Error::Validation(_))),
            "{input:?}: {result:?}"
        );
    }
}

// The header can only hold 16 ascii hex characters, so another id must not be sealed
// into something `decrypt` would refuse for good.
#[test]
fn encrypt_refuses_a_key_id_the_header_cannot_hold() {
    for id in ["", "short", "00112233AABBCCDD", "00112233aabbccdd0"] {
        let result = DataKey::from_bytes(id, [7; 32]).encrypt("t", "c", "a", b"secret");
        assert!(matches!(result, Err(Error::Validation(_))), "{id}");
    }
}

#[test]
fn key_check_text_is_fixed() {
    assert_eq!(crypto::key_check_text(), b"jobpilot-key-check-v1");
}

#[test]
fn key_state_serializes_with_a_state_tag() {
    let value = serde_json::to_value(KeyState::KeyMissing { key_id: "k".into() }).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "state": "key_missing", "key_id": "k" })
    );
    let value = serde_json::to_value(KeyState::KeyUnavailable {
        key_id: "k".into(),
        reason: "r".into(),
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "state": "key_unavailable", "key_id": "k", "reason": "r" })
    );
}
