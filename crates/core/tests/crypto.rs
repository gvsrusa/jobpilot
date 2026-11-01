//! The `#[ignore]`d tests use the real OS keychain, under the service from
//! `.cargo/config.toml`. The rest never depend on what the keychain answers.

use jobpilot_core::crypto::{self, DataKey, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{AppPaths, Db, Error, audit, secrets};
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
    let state = crypto::state(&db).await.unwrap();
    let KeyState::Unlocked { key_id } = state else {
        panic!("{state:?}");
    };
    let guard = EntryGuard(key_id.clone());
    (db, key_id, guard)
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn first_use_creates_one_active_key_and_audits() {
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
    assert_eq!(
        db.key_state(),
        KeyState::NotLoaded,
        "open never asks the store"
    );
    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::Unlocked { key_id }
    );
    assert_eq!(key_rows(&db).await.len(), 1, "a second use adds nothing");
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
        crypto::state(&db).await.unwrap(),
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
        crypto::state(&db).await.unwrap(),
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

async fn add_key_row(db: &Db, key_id: &str, created_at: &str, key_check: &[u8]) {
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, ?, ?, 0)",
    )
    .bind(key_id)
    .bind(created_at)
    .bind(key_check)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// The key-check of a key made of sevens. Keychain tests run in parallel against one
/// service, so each needs an id of its own.
fn sealed_key_check(key_id: &str) -> Vec<u8> {
    DataKey::from_bytes(key_id, [7; 32])
        .encrypt("data_keys", "key_check", key_id, crypto::key_check_text())
        .unwrap()
}

fn reactivated_rows(log: &[audit::AuditEntry]) -> usize {
    let rows = log.iter().filter(|e| e.action == "data_key_reactivated");
    rows.count()
}

// A restore can leave rows with none active. Rows exist, so no key may be made, and the
// store decides what they mean. `crypto::tests` pins the store-error cases.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn rows_without_an_active_key_never_create_one() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    const OLDER: &str = "a000000000000001";
    const NEWER: &str = "a000000000000002";
    let check = sealed_key_check(OLDER);
    add_key_row(&db, OLDER, "1000", &check).await;
    add_key_row(&db, NEWER, "9999", b"\0").await;
    let before = key_rows(&db).await;

    let state = crypto::state(&db).await.unwrap();

    assert_eq!(
        state,
        KeyState::KeyMissing {
            key_id: NEWER.into()
        },
        "a clean no-entry for every id names the newest"
    );
    assert_eq!(key_rows(&db).await, before);
    assert_eq!(reactivated_rows(&audit::recent(&db, 10).await.unwrap()), 0);

    let _guard = EntryGuard(OLDER.into());
    entry(OLDER).set_password(&"07".repeat(32)).unwrap();

    let state = crypto::state(&db).await.unwrap();

    assert_eq!(
        state,
        KeyState::Unlocked {
            key_id: OLDER.into()
        }
    );
    let rows = key_rows(&db).await;
    assert_eq!(rows.len(), 2, "reactivated, not created");
    assert_eq!(rows.iter().filter(|row| row.2 == 1).count(), 1);
    assert_eq!(rows.iter().find(|row| row.0 == OLDER).unwrap().2, 1);

    let key = crypto::unlocked(&db).await.unwrap();
    assert_eq!(key.key_id(), OLDER);
    let log = audit::recent(&db, 10).await.unwrap();
    assert_eq!(reactivated_rows(&log), 1, "the second use finds it active");
    let row = log
        .iter()
        .find(|e| e.action == "data_key_reactivated")
        .unwrap();
    assert_eq!(row.detail, serde_json::json!({ "key_id": OLDER }));
    db.close().await;
}

// The key-check proves the key, so nothing about these fields is locked. Calling this a
// mismatch would offer an irreversible discard with the right key in the store.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn correct_key_with_no_active_row_is_reactivated_over_its_ciphertext() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    const HELD: &str = "a000000000000003";
    let check = sealed_key_check(HELD);
    add_key_row(&db, HELD, "1000", &check).await;
    add_orphan_ciphertext(&db, HELD).await;
    let _guard = EntryGuard(HELD.into());
    entry(HELD).set_password(&"07".repeat(32)).unwrap();

    let state = crypto::state(&db).await.unwrap();

    assert_eq!(
        state,
        KeyState::Unlocked {
            key_id: HELD.into()
        }
    );
    assert_eq!(key_rows(&db).await[0].2, 1);
    let log = audit::recent(&db, 10).await.unwrap();
    assert_eq!(reactivated_rows(&log), 1);
    db.close().await;
}

/// Ciphertext under `key_id`, under a key made of sevens, that a trial decrypt can open.
async fn add_sealed_ciphertext(db: &Db, key_id: &str, country: &str) {
    let sealed = DataKey::from_bytes(key_id, [7; 32])
        .encrypt("work_authorization", "row", country, b"{}")
        .unwrap();
    let mut raw = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *raw)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES (?, ?, ?, 't')",
    )
    .bind(country)
    .bind(sealed)
    .bind(key_id)
    .execute(&mut *raw)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *raw)
        .await
        .unwrap();
}

// K1 is proven and K2 is not, so the state names K2 and K1 stays out of the name.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn mixed_ids_name_the_unproven_one() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    // Proven sorts after unproven on the id, and before it on age.
    const PROVEN: &str = "a000000000000006";
    const UNPROVEN: &str = "a000000000000005";
    add_key_row(&db, PROVEN, "2000", &sealed_key_check(PROVEN)).await;
    add_key_row(&db, UNPROVEN, "1000", &sealed_key_check(UNPROVEN)).await;
    add_sealed_ciphertext(&db, PROVEN, "PT").await;
    add_sealed_ciphertext(&db, UNPROVEN, "ES").await;
    let _guard = EntryGuard(PROVEN.into());
    entry(PROVEN).set_password(&"07".repeat(32)).unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::KeyMismatch {
            key_id: UNPROVEN.into()
        }
    );
    assert!(key_rows(&db).await.iter().all(|row| row.2 == 0));

    let _guard = EntryGuard(UNPROVEN.into());
    entry(UNPROVEN).set_password(&"07".repeat(32)).unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::Unlocked {
            key_id: PROVEN.into()
        },
        "every held id is proven, and the newest becomes active"
    );
    db.close().await;
}

// K2's entry was never asked for before, so the right key beside it read as a mismatch.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn a_proven_key_beside_active_k1_is_read_and_made_active() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    const SECOND: &str = "a000000000000007";
    add_key_row(&db, SECOND, "1000", &sealed_key_check(SECOND)).await;
    add_sealed_ciphertext(&db, SECOND, "PT").await;
    let _second = EntryGuard(SECOND.into());
    entry(SECOND).set_password(&"07".repeat(32)).unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::Unlocked {
            key_id: SECOND.into()
        },
        "the active key holds nothing, so the one that holds the fields replaces it"
    );
    let rows = key_rows(&db).await;
    assert_eq!(rows.iter().find(|row| row.0 == key_id).unwrap().2, 0);
    assert_eq!(rows.iter().find(|row| row.0 == SECOND).unwrap().2, 1);
    assert_eq!(reactivated_rows(&audit::recent(&db, 10).await.unwrap()), 1);
    db.close().await;
}

// Both keys are proven and the active one holds fields of its own, so it stays.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn two_proven_ids_keep_the_active_key() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    const SECOND: &str = "a000000000000008";
    add_key_row(&db, SECOND, "1000", &sealed_key_check(SECOND)).await;
    add_sealed_ciphertext(&db, SECOND, "ES").await;
    add_orphan_ciphertext(&db, &key_id).await;
    let _second = EntryGuard(SECOND.into());
    entry(SECOND).set_password(&"07".repeat(32)).unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::Unlocked {
            key_id: key_id.clone()
        }
    );
    assert_eq!(reactivated_rows(&audit::recent(&db, 10).await.unwrap()), 0);
    db.close().await;
}

// No row means no key-check, and a trial decrypt of the ciphertext proves the entry.
// The row it gains makes the key active like any other.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn a_trial_decrypt_proves_a_key_with_no_row() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    const ORPHAN: &str = "a000000000000009";
    add_sealed_ciphertext(&db, ORPHAN, "PT").await;
    let _guard = EntryGuard(ORPHAN.into());
    entry(ORPHAN).set_password(&"07".repeat(32)).unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::Unlocked {
            key_id: ORPHAN.into()
        }
    );
    let rows = key_rows(&db).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0.as_str(), rows[0].2), (ORPHAN, 1));
    assert_eq!(
        crypto::key_state_of(ORPHAN, &rows[0].1),
        KeyState::Unlocked {
            key_id: ORPHAN.into()
        },
        "the new row carries a working key-check"
    );

    let key = crypto::unlocked(&db).await.unwrap();
    assert_eq!(key.key_id(), ORPHAN);
    db.close().await;
}

/// The foreign key refuses this, so it takes a damaged file.
async fn add_orphan_ciphertext(db: &Db, key_id: &str) {
    let mut raw = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *raw)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES ('PT', x'00', ?, 't')",
    )
    .bind(key_id)
    .execute(&mut *raw)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *raw)
        .await
        .unwrap();
}

// Ciphertext blocks a new key whichever table lost its row. The ids it lists are read
// from the store: an entry that fails the trial decrypt is a mismatch, and only a clean
// no-entry is missing. `crypto::tests` pins the store-error case.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn ciphertext_without_a_key_row_never_creates_one() {
    const ORPHAN: &str = "a000000000000004";
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_orphan_ciphertext(&db, ORPHAN).await;

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::KeyMissing {
            key_id: ORPHAN.into()
        }
    );
    assert!(key_rows(&db).await.is_empty());

    let _guard = EntryGuard(ORPHAN.into());
    entry(ORPHAN).set_password(&"07".repeat(32)).unwrap();

    assert_eq!(
        crypto::state(&db).await.unwrap(),
        KeyState::KeyMismatch {
            key_id: ORPHAN.into()
        }
    );
    assert!(key_rows(&db).await.is_empty());
    db.close().await;
}

// Both refusals come before any keychain call, so this needs no keychain. A planted row
// proves `delete` refuses on the name, not because the name is unlisted.
#[tokio::test]
async fn secret_commands_refuse_data_key_names() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let name = format!("{KEY_ACCOUNT_PREFIX}{KEY_ID}");

    let set = secrets::set(&db, &name, "replacement", "gui").await;
    assert!(matches!(set, Err(Error::Refused(_))), "{set:?}");
    assert!(secrets::names(&db).await.unwrap().is_empty());

    let upper = name.to_uppercase();
    let set = secrets::set(&db, &upper, "replacement", "gui").await;
    assert!(matches!(set, Err(Error::Refused(_))), "{set:?}");

    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES (?, 't')")
        .bind(&name)
        .execute(&db.pool)
        .await
        .unwrap();
    let delete = secrets::delete(&db, &name, "gui").await;
    assert!(matches!(delete, Err(Error::Refused(_))), "{delete:?}");
    assert_eq!(secrets::names(&db).await.unwrap(), [name]);
    assert!(audit::recent(&db, 10).await.unwrap().is_empty());
    db.close().await;
}

// `key_state` before any use is the one value that says nothing about the key.
#[tokio::test]
async fn state_before_first_use_is_not_loaded_and_opens_plain_work() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    assert_eq!(db.key_state(), KeyState::NotLoaded);
    jobpilot_core::documents::create(&db, "Title", "body")
        .await
        .unwrap();
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    assert!(key_rows(&db).await.is_empty());
    db.close().await;
}

#[tokio::test]
async fn export_lists_key_ids_and_no_key_material() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', x'0102', 0)",
    )
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
    assert_eq!(
        serde_json::to_value(KeyState::NotLoaded).unwrap(),
        serde_json::json!({ "state": "key_not_loaded" })
    );
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
