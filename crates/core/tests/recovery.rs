//! The `#[ignore]`d tests use the real OS keychain, under the service from
//! `.cargo/config.toml`. The rest never reach it: their refusals come first.

use std::path::{Path, PathBuf};

use jobpilot_core::crypto::{self, DataKey, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{AppPaths, Db, Error, audit, backup, recovery, settings, transfer};
use tempfile::TempDir;

const OTHER_HEX: &str = "ab";

struct Root {
    tmp: TempDir,
    paths: AppPaths,
}

impl Root {
    fn new() -> Root {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        std::fs::create_dir(tmp.path().join("outside")).unwrap();
        Root { tmp, paths }
    }

    fn outside(&self, name: &str) -> PathBuf {
        self.tmp.path().join("outside").join(name)
    }
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

async fn unlocked_root(paths: &AppPaths) -> (Db, String, EntryGuard) {
    let db = Db::open(paths).await.unwrap();
    let state = crypto::state(&db).await.unwrap();
    let KeyState::Unlocked { key_id } = state else {
        panic!("{state:?}");
    };
    let guard = EntryGuard(key_id.clone());
    (db, key_id, guard)
}

async fn key_rows(db: &Db) -> Vec<(String, Vec<u8>, i64)> {
    sqlx::query_as("SELECT key_id, key_check, active FROM data_keys ORDER BY key_id")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn add_ciphertext(db: &Db, key_id: &str) {
    add_ciphertext_in(db, key_id, "PT").await;
}

async fn add_ciphertext_in(db: &Db, key_id: &str, country: &str) {
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES (?, x'0102', ?, 't')",
    )
    .bind(country)
    .bind(key_id)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn ciphertext_rows(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM work_authorization")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

fn missing(key_id: &str) -> KeyState {
    KeyState::KeyMissing {
        key_id: key_id.into(),
    }
}

fn mismatch(key_id: &str) -> KeyState {
    KeyState::KeyMismatch {
        key_id: key_id.into(),
    }
}

fn unlocked(key_id: &str) -> KeyState {
    KeyState::Unlocked {
        key_id: key_id.into(),
    }
}

fn is_already_exists<T>(result: &jobpilot_core::Result<T>) -> bool {
    matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn export_then_import_restores_unlocked() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    let hex = entry(&key_id).get_password().unwrap();
    add_ciphertext(&db, &key_id).await;
    assert!(recovery::no_recovery_key_exported(&db).await.unwrap());

    let out = root.outside("key.json");
    let receipt = recovery::export(&db, &out, "cli").await.unwrap();

    assert_eq!(receipt.key_id, key_id);
    assert_eq!(receipt.path, out);
    assert!(!recovery::no_recovery_key_exported(&db).await.unwrap());

    entry(&key_id).delete_credential().unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), missing(&key_id));

    let state = recovery::import(&db, &out, "cli").await.unwrap();

    assert_eq!(state, unlocked(&key_id));
    assert_eq!(db.key_state(), unlocked(&key_id));
    assert_eq!(entry(&key_id).get_password().unwrap(), hex);

    let log = audit::recent(&db, 20).await.unwrap();
    for action in ["recovery_key_exported", "recovery_key_imported"] {
        let row = log.iter().find(|e| e.action == action).unwrap();
        assert_eq!(
            row.detail,
            serde_json::json!({ "key_id": key_id }),
            "{action}"
        );
        assert_eq!(row.actor, "cli");
    }
    assert!(log.iter().all(|e| !e.detail.to_string().contains(&hex)));
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn exported_file_holds_the_documented_fields_and_mode() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    let out = root.outside("key.json");

    recovery::export(&db, &out, "gui").await.unwrap();

    let text = std::fs::read_to_string(&out).unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["format"], recovery::RECOVERY_FORMAT);
    assert_eq!(file["key_id"], key_id.as_str());
    assert_eq!(file["key_hex"], entry(&key_id).get_password().unwrap());
    assert!(file["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(file.as_object().unwrap().len(), 4);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    db.close().await;
}

// The destination is checked before the keychain, so a failed read leaves an empty
// file to clear up, and it must go.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn export_with_no_key_leaves_no_file() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    entry(&key_id).delete_credential().unwrap();
    let out = root.outside("key.json");

    let result = recovery::export(&db, &out, "cli").await;

    assert!(
        matches!(result, Err(Error::KeyMissing { .. })),
        "{result:?}"
    );
    assert!(!out.exists());
    let log = audit::recent(&db, 10).await.unwrap();
    assert!(log.iter().all(|e| e.action != "recovery_key_exported"));
    assert!(recovery::no_recovery_key_exported(&db).await.unwrap());
    db.close().await;
}

#[tokio::test]
async fn export_refuses_existing_file_and_root_paths() {
    let root = Root::new();
    let db = Db::open(&root.paths).await.unwrap();

    let existing = root.outside("existing.json");
    std::fs::write(&existing, "keep me").unwrap();
    let result = recovery::export(&db, &existing, "cli").await;
    assert!(is_already_exists(&result), "{result:?}");
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "keep me");

    let exports = root.paths.exports().join("key.json");
    let climbed = root.outside("..").join("data/backups/key.json");
    let in_root = root.paths.root().join("key.json");
    let missing_folder = root.outside("no-such-folder").join("key.json");
    let inside_a_missing_folder = root.paths.exports().join("new/key.json");
    for out in [
        exports,
        climbed,
        in_root,
        missing_folder,
        inside_a_missing_folder,
    ] {
        let result = recovery::export(&db, &out, "cli").await;
        assert!(
            matches!(result, Err(Error::Refused(_))),
            "{out:?}: {result:?}"
        );
        assert!(!out.exists());
    }

    assert!(key_rows(&db).await.is_empty(), "no key was made to refuse");
    assert!(audit::recent(&db, 10).await.unwrap().is_empty());
    db.close().await;
}

// On a case-insensitive volume this is the same folder as `exports`.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn export_refuses_the_root_spelled_in_another_case() {
    let root = Root::new();
    let db = Db::open(&root.paths).await.unwrap();

    let out = root.paths.root().join("EXPORTS").join("key.json");
    let result = recovery::export(&db, &out, "cli").await;

    assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    db.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn export_refuses_devices_and_symlinks() {
    use std::os::unix::fs::symlink;

    let root = Root::new();
    let db = Db::open(&root.paths).await.unwrap();

    let result = recovery::export(&db, Path::new("/dev/stdout"), "cli").await;
    assert!(is_already_exists(&result), "{result:?}");

    let target = root.outside("target.json");
    let dangling = root.outside("dangling.json");
    symlink(&target, &dangling).unwrap();
    let result = recovery::export(&db, &dangling, "cli").await;
    assert!(is_already_exists(&result), "{result:?}");
    assert!(!target.exists(), "the link was not followed");

    let folder = root.outside("folder");
    symlink(root.paths.exports(), &folder).unwrap();
    let result = recovery::export(&db, &folder.join("key.json"), "cli").await;
    assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    assert!(
        std::fs::read_dir(root.paths.exports())
            .unwrap()
            .next()
            .is_none()
    );
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn import_wrong_key_changes_nothing() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    let out = root.outside("key.json");
    recovery::export(&db, &out, "cli").await.unwrap();

    entry(&key_id).delete_credential().unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), missing(&key_id));
    let before = key_rows(&db).await;

    let text = std::fs::read_to_string(&out).unwrap();
    let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();

    file["key_hex"] = OTHER_HEX.repeat(32).into();
    let wrong_key = root.outside("wrong-key.json");
    std::fs::write(&wrong_key, file.to_string()).unwrap();

    file["key_id"] = "99887766ffeeddcc".into();
    let wrong_id = root.outside("wrong-id.json");
    std::fs::write(&wrong_id, file.to_string()).unwrap();

    for wrong in [wrong_key, wrong_id] {
        let result = recovery::import(&db, &wrong, "cli").await;
        assert!(
            matches!(&result, Err(Error::KeyMismatch { key_id: id }) if *id == key_id),
            "{result:?}"
        );
    }

    assert!(matches!(
        entry(&key_id).get_password(),
        Err(keyring::Error::NoEntry)
    ));
    assert!(matches!(
        entry("99887766ffeeddcc").get_password(),
        Err(keyring::Error::NoEntry)
    ));
    assert_eq!(key_rows(&db).await, before);
    let log = audit::recent(&db, 10).await.unwrap();
    assert!(log.iter().all(|e| e.action != "recovery_key_imported"));
    db.close().await;
}

// No write replaces an entry. The right file still cannot go in over a bad one, and
// the state stays what it was.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn import_leaves_a_different_entry_as_it_is() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    let out = root.outside("key.json");
    recovery::export(&db, &out, "cli").await.unwrap();

    let wrong = OTHER_HEX.repeat(32);
    entry(&key_id).set_password(&wrong).unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), mismatch(&key_id));

    let result = recovery::import(&db, &out, "cli").await;

    assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    assert_eq!(entry(&key_id).get_password().unwrap(), wrong);
    assert_eq!(db.key_state(), mismatch(&key_id));
    let log = audit::recent(&db, 10).await.unwrap();
    assert!(log.iter().all(|e| e.action != "recovery_key_imported"));
    db.close().await;
}

/// A second key with a working key-check and ciphertext of its own, and no keychain
/// entry, plus the recovery file that restores it.
async fn second_key_and_file(db: &Db, key_id: &str, file: &Path) -> DataKey {
    let key = DataKey::from_bytes(key_id, [7; 32]);
    let check = key
        .encrypt("data_keys", "key_check", key_id, crypto::key_check_text())
        .unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', ?, 0)",
    )
    .bind(key_id)
    .bind(check)
    .execute(&db.pool)
    .await
    .unwrap();
    add_ciphertext_in(db, key_id, "ES").await;
    let text = serde_json::json!({
        "format": 1, "key_id": key_id, "created_at": "t", "key_hex": "07".repeat(32),
    });
    std::fs::write(file, text.to_string()).unwrap();
    key
}

// The active key holds nothing here, so importing the key that does must leave it active.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn import_leaves_the_key_that_holds_the_fields_active() {
    const SECOND: &str = "b000000000000001";
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    let file = root.outside("second.json");
    second_key_and_file(&db, SECOND, &file).await;
    let _second = EntryGuard(SECOND.into());
    assert_eq!(crypto::state(&db).await.unwrap(), mismatch(SECOND));

    let state = recovery::import(&db, &file, "cli").await.unwrap();

    assert_eq!(state, unlocked(SECOND));
    let rows = key_rows(&db).await;
    assert_eq!(rows.iter().find(|row| row.0 == key_id).unwrap().2, 0);
    assert_eq!(rows.iter().find(|row| row.0 == SECOND).unwrap().2, 1);
    assert_eq!(crypto::unlocked(&db).await.unwrap().key_id(), SECOND);
    db.close().await;
}

// Two keys hold fields and one is lost. Import brings it back, and the active key stays.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn import_of_the_lost_key_in_a_mix_unlocks() {
    const SECOND: &str = "b000000000000002";
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    add_ciphertext(&db, &key_id).await;
    let file = root.outside("second.json");
    second_key_and_file(&db, SECOND, &file).await;
    let _second = EntryGuard(SECOND.into());
    assert_eq!(crypto::state(&db).await.unwrap(), mismatch(SECOND));

    let state = recovery::import(&db, &file, "cli").await.unwrap();

    assert_eq!(state, unlocked(&key_id));
    assert_eq!(ciphertext_rows(&db).await, 2);
    db.close().await;
}

// Discard names the unproven id, and the readable fields under the other stay. They
// stand in the way of a new key, so none is made.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn discard_in_a_mix_clears_only_the_unproven_id() {
    const SECOND: &str = "b000000000000003";
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    add_ciphertext(&db, &key_id).await;
    second_key_and_file(&db, SECOND, &root.outside("second.json")).await;
    assert_eq!(crypto::state(&db).await.unwrap(), mismatch(SECOND));

    let wrong = recovery::discard_locked(&db, &key_id, "cli").await;
    assert!(
        matches!(wrong, Err(Error::ConfirmationRequired)),
        "{wrong:?}"
    );

    let state = recovery::discard_locked(&db, SECOND, "cli").await.unwrap();

    assert_eq!(state, unlocked(&key_id));
    let left: Vec<String> = sqlx::query_scalar("SELECT key_id FROM work_authorization")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, std::slice::from_ref(&key_id));
    let rows = key_rows(&db).await;
    assert_eq!(rows.len(), 2, "no new key");
    assert_eq!(rows.iter().find(|row| row.0 == key_id).unwrap().2, 1);
    assert_eq!(rows.iter().find(|row| row.0 == SECOND).unwrap().2, 0);

    let log = audit::recent(&db, 20).await.unwrap();
    let row = log
        .iter()
        .find(|e| e.action == "locked_fields_discarded")
        .unwrap();
    assert_eq!(
        row.detail,
        serde_json::json!({ "old_key_id": SECOND, "new_key_id": null })
    );
    db.close().await;
}

// Another key's file may pass its own key-check and still open nothing here.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn import_refuses_a_listed_key_that_does_not_hold_the_ciphertext() {
    let root = Root::new();
    let (db, old_id, _guard) = unlocked_root(&root.paths).await;
    let old_file = root.outside("old.json");
    recovery::export(&db, &old_file, "cli").await.unwrap();

    entry(&old_id).delete_credential().unwrap();
    crypto::state(&db).await.unwrap();
    let state = recovery::discard_locked(&db, &old_id, "cli").await.unwrap();
    let KeyState::Unlocked { key_id: new_id } = state else {
        panic!("{state:?}");
    };
    let _new_guard = EntryGuard(new_id.clone());
    add_ciphertext(&db, &new_id).await;

    entry(&new_id).delete_credential().unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), missing(&new_id));

    let result = recovery::import(&db, &old_file, "cli").await;

    assert!(
        matches!(&result, Err(Error::KeyMismatch { key_id }) if *key_id == new_id),
        "{result:?}"
    );
    assert!(matches!(
        entry(&old_id).get_password(),
        Err(keyring::Error::NoEntry)
    ));
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn import_refuses_while_unlocked() {
    let root = Root::new();
    let (db, _key_id, _guard) = unlocked_root(&root.paths).await;
    let out = root.outside("key.json");
    recovery::export(&db, &out, "cli").await.unwrap();

    let result = recovery::import(&db, &out, "cli").await;

    assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    db.close().await;
}

// `key_not_loaded` says nothing about the key, so neither action may start from it.
#[tokio::test]
async fn import_and_discard_refuse_a_handle_that_has_not_asked() {
    let root = Root::new();
    let db = Db::open(&root.paths).await.unwrap();
    let file = root.outside("key.json");
    std::fs::write(&file, "{}").unwrap();

    let import = recovery::import(&db, &file, "cli").await;
    assert!(matches!(import, Err(Error::Refused(_))), "{import:?}");
    let discard = recovery::discard_locked(&db, "anything", "cli").await;
    assert!(matches!(discard, Err(Error::Refused(_))), "{discard:?}");

    assert!(key_rows(&db).await.is_empty(), "no first key was made");
    assert!(
        std::fs::read_dir(root.paths.backups())
            .unwrap()
            .next()
            .is_none()
    );
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn discard_requires_key_missing_and_matching_confirm() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    add_ciphertext(&db, &key_id).await;

    let result = recovery::discard_locked(&db, &key_id, "cli").await;
    assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    assert_eq!(ciphertext_rows(&db).await, 1);

    entry(&key_id).delete_credential().unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), missing(&key_id));

    let result = recovery::discard_locked(&db, "not-the-key-id", "cli").await;
    assert!(
        matches!(result, Err(Error::ConfirmationRequired)),
        "{result:?}"
    );
    assert_eq!(ciphertext_rows(&db).await, 1);
    assert!(
        std::fs::read_dir(root.paths.backups())
            .unwrap()
            .next()
            .is_none()
    );

    let state = recovery::discard_locked(&db, &key_id, "gui").await.unwrap();

    let KeyState::Unlocked { key_id: new_id } = state else {
        panic!("{state:?}");
    };
    let _new_guard = EntryGuard(new_id.clone());
    assert_ne!(new_id, key_id);
    assert_eq!(db.key_state(), unlocked(&new_id));
    assert_eq!(ciphertext_rows(&db).await, 0);
    assert_eq!(entry(&new_id).get_password().unwrap().len(), 64);

    let rows = key_rows(&db).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().find(|row| row.0 == key_id).unwrap().2, 0);
    assert_eq!(rows.iter().find(|row| row.0 == new_id).unwrap().2, 1);

    let snapshot = std::fs::read_dir(root.paths.backups())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            name.starts_with("pre-discard-")
        })
        .expect("a pre-discard snapshot");
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(snapshot.join("jobpilot.db"));
    let old = sqlx::SqlitePool::connect_with(options).await.unwrap();
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM work_authorization")
        .fetch_one(&old)
        .await
        .unwrap();
    assert_eq!(held, 1, "the snapshot keeps the old ciphertext");
    old.close().await;

    let log = audit::recent(&db, 20).await.unwrap();
    let row = log
        .iter()
        .find(|e| e.action == "locked_fields_discarded")
        .unwrap();
    assert_eq!(
        row.detail,
        serde_json::json!({ "old_key_id": key_id, "new_key_id": new_id })
    );
    assert_eq!(row.actor, "gui");
    let created = log.iter().filter(|e| e.action == "data_key_created");
    assert_eq!(created.count(), 2, "the first key and the new one");
    assert!(
        recovery::no_recovery_key_exported(&db).await.unwrap(),
        "the new key has no recovery file yet"
    );
    db.close().await;
}

// The stored entry may still open the snapshot, so a discard leaves it too.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn discard_from_mismatch_leaves_the_old_entry() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    add_ciphertext(&db, &key_id).await;
    let wrong = OTHER_HEX.repeat(32);
    entry(&key_id).set_password(&wrong).unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), mismatch(&key_id));

    let state = recovery::discard_locked(&db, &key_id, "cli").await.unwrap();

    let KeyState::Unlocked { key_id: new_id } = state else {
        panic!("{state:?}");
    };
    let _new_guard = EntryGuard(new_id);
    assert_eq!(entry(&key_id).get_password().unwrap(), wrong);
    assert_eq!(ciphertext_rows(&db).await, 0);
    db.close().await;
}

// The cached state can be out of date, so discard asks the store again.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn discard_rereads_the_store_before_it_clears_anything() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    add_ciphertext(&db, &key_id).await;
    let hex = entry(&key_id).get_password().unwrap();

    entry(&key_id).delete_credential().unwrap();
    assert_eq!(crypto::state(&db).await.unwrap(), missing(&key_id));
    entry(&key_id).set_password(&hex).unwrap();

    let result = recovery::discard_locked(&db, &key_id, "cli").await;

    assert!(matches!(result, Err(Error::Refused(_))), "{result:?}");
    assert_eq!(ciphertext_rows(&db).await, 1);
    assert_eq!(key_rows(&db).await.len(), 1);
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn recovery_file_never_lands_in_export_or_backup() {
    let root = Root::new();
    let (db, key_id, _guard) = unlocked_root(&root.paths).await;
    let hex = entry(&key_id).get_password().unwrap();
    recovery::export(&db, &root.outside("key.json"), "cli")
        .await
        .unwrap();

    backup::create(&db, "cli").await.unwrap();
    transfer::export_all(&db, "cli").await.unwrap();

    let files = files_under(root.paths.root());
    assert!(!files.is_empty());
    for file in files {
        let bytes = std::fs::read(&file).unwrap();
        assert!(!contains(&bytes, b"key_hex"), "{file:?}");
        assert!(!contains(&bytes, hex.as_bytes()), "{file:?}");
    }
    db.close().await;
}

#[tokio::test]
async fn no_key_means_nothing_to_export() {
    let root = Root::new();
    let db = Db::open(&root.paths).await.unwrap();
    assert!(!recovery::no_recovery_key_exported(&db).await.unwrap());
    db.close().await;
}

// Read with no keychain, so the data view can show it without a prompt.
#[tokio::test]
async fn the_export_mark_follows_the_active_key() {
    const KEY_ID: &str = "00112233aabbccdd";
    let root = Root::new();
    let db = Db::open(&root.paths).await.unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', x'00', 1)",
    )
    .bind(KEY_ID)
    .execute(&db.pool)
    .await
    .unwrap();

    assert!(recovery::no_recovery_key_exported(&db).await.unwrap());

    let mark = format!("recovery_key_exported_{KEY_ID}");
    settings::set(&db, &mark, &serde_json::json!(true))
        .await
        .unwrap();

    assert!(!recovery::no_recovery_key_exported(&db).await.unwrap());
    db.close().await;
}
