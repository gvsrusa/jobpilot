use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{AppPaths, Db, Error, audit, backup, documents, secrets, transfer};
use serde_json::Value;
use tempfile::TempDir;

fn temp_root() -> (TempDir, AppPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    (tmp, paths)
}

fn read_export(dir: &Path) -> (String, Value) {
    let text = fs::read_to_string(dir.join("data.json")).unwrap();
    let data = serde_json::from_str(&text).unwrap();
    (text, data)
}

fn table_names(data: &Value) -> BTreeSet<String> {
    data["tables"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

fn entries(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

async fn add_secret_row(db: &Db, name: &str) {
    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES (?, 't')")
        .bind(name)
        .execute(&db.pool)
        .await
        .unwrap();
}

async fn document_count(paths: &AppPaths) -> usize {
    let db = Db::open(paths).await.unwrap();
    let count = documents::list(&db).await.unwrap().len();
    db.close().await;
    count
}

#[tokio::test]
async fn export_covers_every_user_table() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    let dir = transfer::export_all(&db, "cli").await.unwrap();

    let expected: BTreeSet<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT GLOB '_sqlx_*' AND name NOT GLOB 'documents_fts*'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    db.close().await;

    let (_, data) = read_export(&dir);
    assert!(expected.contains("audit_log"), "{expected:?}");
    assert_eq!(table_names(&data), expected);
    assert_eq!(data["format"], 1);
    assert_eq!(data["schema_version"], jobpilot_core::app_schema_version());
    assert!(data["exported_at"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn export_finds_a_future_table_with_awkward_names() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    sqlx::query(r#"CREATE TABLE "odd ""table'" ("a ""b'" TEXT, n INTEGER)"#)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "odd ""table'" VALUES ('{"not":"parsed"}', NULL)"#)
        .execute(&db.pool)
        .await
        .unwrap();

    let dir = transfer::export_all(&db, "cli").await.unwrap();
    db.close().await;

    let (_, data) = read_export(&dir);
    assert_eq!(
        data["tables"][r#"odd "table'"#],
        serde_json::json!([{r#"a "b'"#: r#"{"not":"parsed"}"#, "n": null}])
    );
}

#[tokio::test]
async fn export_has_documents_and_secret_names() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let doc = documents::create(&db, "resume", "body").await.unwrap();
    add_secret_row(&db, "openai").await;
    fs::write(paths.documents().join("resume.pdf"), "pdf").unwrap();

    let dir = transfer::export_all(&db, "cli").await.unwrap();

    let (_, data) = read_export(&dir);
    let rows = data["tables"]["documents"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], doc.id.as_str());
    assert_eq!(rows[0]["title"], "resume");
    assert_eq!(data["tables"]["secret_index"][0]["name"], "openai");
    assert_eq!(
        fs::read_to_string(dir.join("documents").join("resume.pdf")).unwrap(),
        "pdf"
    );

    let log = audit::recent(&db, 1).await.unwrap();
    assert_eq!(log[0].action, "export");
    assert_eq!(log[0].actor, "cli");
    assert_eq!(log[0].target.as_deref(), Some(&*dir.to_string_lossy()));
    db.close().await;
}

#[tokio::test]
async fn exports_never_overwrite_and_leave_no_temporary_folder() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    let first = transfer::export_all(&db, "cli").await.unwrap();
    let second = transfer::export_all(&db, "cli").await.unwrap();
    let third = transfer::export_all(&db, "cli").await.unwrap();
    db.close().await;

    assert_ne!(first, second);
    assert_ne!(second, third);
    assert_eq!(first.parent().unwrap(), paths.exports());
    let found = entries(&paths.exports());
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(
        found
            .iter()
            .all(|name| name.starts_with("jobpilot-export-"))
    );
}

#[tokio::test]
async fn delete_requires_exact_confirmation() {
    let (_tmp, paths) = temp_root();
    for wrong in ["delete", "", "DELETE ", "yes"] {
        let db = Db::open(&paths).await.unwrap();
        documents::create(&db, "keep", "body").await.unwrap();

        let result = transfer::delete_all(db, wrong, true).await;

        assert!(
            matches!(result, Err(Error::ConfirmationRequired)),
            "{result:?}"
        );
    }
    assert_eq!(document_count(&paths).await, 4);
}

#[tokio::test]
async fn delete_all_removes_data_and_can_keep_backups() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    documents::create(&db, "gone", "body").await.unwrap();
    fs::write(paths.documents().join("resume.pdf"), "pdf").unwrap();
    let snapshot = backup::create(&db, "cli").await.unwrap();
    transfer::export_all(&db, "cli").await.unwrap();

    transfer::delete_all(db, "DELETE", false).await.unwrap();

    assert_eq!(
        entries(paths.root()),
        [".jobpilot-root", "backups", "jobpilot.lock"]
            .map(String::from)
            .into()
    );
    assert!(snapshot.path.join("jobpilot.db").is_file());

    let db = Db::open(&paths).await.unwrap();
    assert!(documents::list(&db).await.unwrap().is_empty());
    assert!(
        audit::recent(&db, 10).await.unwrap().is_empty(),
        "delete-all removes the log and writes no row of its own"
    );
    assert_eq!(backup::list(&paths).await.unwrap().len(), 1);

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert_eq!(
        entries(paths.root()),
        [".jobpilot-root", "jobpilot.lock"].map(String::from).into(),
        "these two stay, so a retry can open the root"
    );
}

#[tokio::test]
async fn delete_all_removes_what_a_crashed_restore_left_and_nothing_foreign() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let root = paths.root();
    for leftover in [
        "restore-tmp.db",
        "restore-tmp.db-wal",
        "restore-old.db-wal",
        ".jobpilot-swap",
    ] {
        fs::write(root.join(leftover), "user data").unwrap();
    }
    for leftover in ["restore-tmp-documents", "restore-old-documents"] {
        fs::create_dir(root.join(leftover)).unwrap();
        fs::write(root.join(leftover).join("resume.pdf"), "user data").unwrap();
    }
    // The marker proves the root, and still only JobPilot's own names may go.
    fs::write(root.join("not-ours.txt"), "keep").unwrap();

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert_eq!(
        entries(root),
        [".jobpilot-root", "jobpilot.lock", "not-ours.txt"]
            .map(String::from)
            .into()
    );
}

#[tokio::test]
async fn delete_refused_while_another_handle_open() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    documents::create(&db, "keep", "body").await.unwrap();
    fs::write(paths.documents().join("resume.pdf"), "pdf").unwrap();
    let other = Db::open(&paths).await.unwrap();

    let result = transfer::delete_all(db, "DELETE", true).await;

    assert!(matches!(result, Err(Error::InUse)), "{result:?}");
    assert_eq!(documents::list(&other).await.unwrap().len(), 1);
    assert!(paths.documents().join("resume.pdf").is_file());
    other.close().await;
}

#[tokio::test]
async fn an_empty_name_row_cannot_block_deletion() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_secret_row(&db, "").await;
    secrets::delete(&db, "", "cli").await.unwrap();
    assert!(secrets::names(&db).await.unwrap().is_empty());

    add_secret_row(&db, "").await;
    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert!(!paths.db().exists());
}

#[tokio::test]
async fn empty_name_is_refused_before_any_write() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    let result = secrets::set(&db, "", "hunter2-value", "cli").await;

    let Err(error @ Error::Secret(_)) = result else {
        panic!("{result:?}");
    };
    assert!(!error.to_string().contains("hunter2-value"), "{error}");
    assert!(secrets::names(&db).await.unwrap().is_empty());
    assert!(audit::recent(&db, 10).await.unwrap().is_empty());
    db.close().await;
}

// Another data root may have stored this name, so the keychain must not be touched.
#[tokio::test]
async fn delete_of_an_unlisted_name_is_not_found() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    let result = secrets::delete(&db, "not-in-this-root", "cli").await;

    assert!(matches!(result, Err(Error::NotFound)), "{result:?}");
    assert!(audit::recent(&db, 10).await.unwrap().is_empty());
    db.close().await;
}

fn root_with_a_user_file() -> (TempDir, AppPaths, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("home"));
    // On a case-insensitive volume `paths.documents()` is this folder. Refusing the
    // whole root keeps the assertions true on a case-sensitive volume as well.
    let file = paths.root().join("Documents").join("taxes.pdf");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "user data").unwrap();
    (tmp, paths, file)
}

#[tokio::test]
async fn open_refuses_a_root_with_files_and_no_marker() {
    let (_tmp, paths, file) = root_with_a_user_file();

    let result = Db::open(&paths).await;

    let Err(error @ Error::RootNotOwned(_)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(error.code(), "root_not_owned");
    assert!(error.to_string().contains(&*paths.root().to_string_lossy()));
    assert_eq!(fs::read_to_string(&file).unwrap(), "user data");
    assert_eq!(
        entries(paths.root()),
        ["Documents"].map(String::from).into(),
        "a refused root gains nothing"
    );
}

#[tokio::test]
async fn open_accepts_a_root_with_the_marker() {
    let (_tmp, paths, file) = root_with_a_user_file();
    fs::write(paths.marker(), "io.github.gvsrusa.jobpilot").unwrap();

    let db = Db::open(&paths).await.unwrap();

    db.close().await;
    assert_eq!(fs::read_to_string(&file).unwrap(), "user data");
}

#[tokio::test]
async fn open_marks_a_new_or_empty_root() {
    let (_tmp, paths) = temp_root();
    Db::open(&paths).await.unwrap().close().await;
    assert_eq!(
        fs::read_to_string(paths.marker()).unwrap(),
        "io.github.gvsrusa.jobpilot"
    );

    let tmp = tempfile::tempdir().unwrap();
    let empty = AppPaths::new(tmp.path());
    Db::open(&empty).await.unwrap().close().await;
    assert!(empty.marker().is_file());
}

#[tokio::test]
async fn delete_all_refuses_a_root_that_lost_its_marker() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_secret_row(&db, "kept").await;
    fs::write(paths.documents().join("resume.pdf"), "pdf").unwrap();
    fs::remove_file(paths.marker()).unwrap();

    let result = transfer::delete_all(db, "DELETE", true).await;

    assert!(matches!(result, Err(Error::RootNotOwned(_))), "{result:?}");
    assert!(paths.db().is_file());
    assert!(paths.documents().join("resume.pdf").is_file());
}

#[tokio::test]
async fn wrong_confirmation_wins_over_a_missing_marker() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    fs::remove_file(paths.marker()).unwrap();

    let result = transfer::delete_all(db, "delete", true).await;

    assert!(
        matches!(result, Err(Error::ConfirmationRequired)),
        "{result:?}"
    );
}

#[tokio::test]
async fn reopen_works_after_delete_all() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    transfer::delete_all(db, "DELETE", true).await.unwrap();

    let db = Db::open(&paths).await.unwrap();

    assert!(documents::list(&db).await.unwrap().is_empty());
    db.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn delete_all_removes_folder_symlinks_and_not_their_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    let paths = AppPaths::new(tmp.path().join("data"));
    fs::create_dir(paths.root()).unwrap();
    for (folder, link) in [("docs", paths.documents()), ("out", paths.exports())] {
        fs::create_dir_all(outside.join(folder)).unwrap();
        fs::write(outside.join(folder).join("taxes.pdf"), "user data").unwrap();
        std::os::unix::fs::symlink(outside.join(folder), link).unwrap();
    }
    fs::write(paths.marker(), "io.github.gvsrusa.jobpilot").unwrap();
    let db = Db::open(&paths).await.unwrap();

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert!(fs::symlink_metadata(paths.documents()).is_err());
    assert!(fs::symlink_metadata(paths.exports()).is_err());
    for folder in ["docs", "out"] {
        let file = outside.join(folder).join("taxes.pdf");
        assert_eq!(fs::read_to_string(file).unwrap(), "user data");
    }
}

#[tokio::test]
async fn names_are_sorted() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_secret_row(&db, "b").await;
    add_secret_row(&db, "a").await;

    assert_eq!(secrets::names(&db).await.unwrap(), ["a", "b"]);
    db.close().await;
}

/// The real tests refuse to run against the app's own keychain service. Entries go
/// even when an assertion fails first.
struct KeychainEntries {
    service: String,
    names: Vec<String>,
}

impl KeychainEntries {
    fn new(test: &str, count: usize) -> Self {
        let service = std::env::var("JOBPILOT_SECRET_SERVICE")
            .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
        assert_ne!(service, "io.github.gvsrusa.jobpilot");
        let pid = std::process::id();
        let names = (0..count).map(|i| format!("{test}-{pid}-{i}")).collect();
        Self { service, names }
    }

    fn present(&self, name: &str) -> bool {
        match keyring::Entry::new(&self.service, name)
            .unwrap()
            .get_password()
        {
            Ok(_) => true,
            Err(keyring::Error::NoEntry) => false,
            Err(error) => panic!("{error}"),
        }
    }
}

impl Drop for KeychainEntries {
    fn drop(&mut self) {
        for name in &self.names {
            if let Ok(entry) = keyring::Entry::new(&self.service, name) {
                let _ = entry.delete_credential();
            }
        }
    }
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn keychain_round_trip() {
    const VALUE: &str = "round-trip-secret-value";
    let entries = KeychainEntries::new("round-trip", 1);
    let name = entries.names[0].as_str();
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    assert_eq!(secrets::get(name).unwrap(), None);
    secrets::set(&db, name, "first-value", "cli").await.unwrap();
    secrets::set(&db, name, VALUE, "cli").await.unwrap();

    assert_eq!(secrets::get(name).unwrap().as_deref(), Some(VALUE));
    assert_eq!(secrets::names(&db).await.unwrap(), [name]);

    let dir = transfer::export_all(&db, "cli").await.unwrap();
    let (text, data) = read_export(&dir);
    assert!(!text.contains(VALUE));
    assert!(!text.contains("first-value"));
    assert_eq!(data["tables"]["secret_index"][0]["name"], name);

    secrets::delete(&db, name, "cli").await.unwrap();

    assert_eq!(secrets::get(name).unwrap(), None);
    assert!(secrets::names(&db).await.unwrap().is_empty());

    let log = audit::recent(&db, 10).await.unwrap();
    let actions: Vec<_> = log.iter().map(|entry| entry.action.as_str()).collect();
    assert_eq!(
        actions,
        ["secret_delete", "export", "secret_set", "secret_set"]
    );
    assert_eq!(log[0].target.as_deref(), Some(name));
    for entry in &log {
        let expected = match entry.action.as_str() {
            "export" => serde_json::json!({ "sensitive_plain": false }),
            _ => serde_json::json!({}),
        };
        assert_eq!(entry.detail, expected);
    }
    db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn delete_all_clears_keychain() {
    let entries = KeychainEntries::new("delete-all", 2);
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    for name in &entries.names {
        secrets::set(&db, name, "delete-all-value", "cli")
            .await
            .unwrap();
    }
    // A retry after a partial failure meets entries that are already gone.
    keyring::Entry::new(&entries.service, &entries.names[0])
        .unwrap()
        .delete_credential()
        .unwrap();

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    for name in &entries.names {
        assert_eq!(secrets::get(name).unwrap(), None);
    }
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn refused_delete_all_leaves_the_keychain_alone() {
    let entries = KeychainEntries::new("refused", 1);
    let name = entries.names[0].as_str();
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    secrets::set(&db, name, "kept-value", "cli").await.unwrap();
    let other = Db::open(&paths).await.unwrap();

    let result = transfer::delete_all(db, "DELETE", true).await;

    assert!(matches!(result, Err(Error::InUse)), "{result:?}");
    assert_eq!(secrets::get(name).unwrap().as_deref(), Some("kept-value"));
    other.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn another_roots_entry_survives_delete() {
    let entries = KeychainEntries::new("other-root", 1);
    let name = entries.names[0].as_str();
    keyring::Entry::new(&entries.service, name)
        .unwrap()
        .set_password("other-root-value")
        .unwrap();
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    let result = secrets::delete(&db, name, "cli").await;

    assert!(matches!(result, Err(Error::NotFound)), "{result:?}");
    assert!(entries.present(name));
    db.close().await;
}

/// A data key's entry, removed even when an assertion fails first.
struct KeyEntry(keyring::Entry);

impl KeyEntry {
    fn new(key_id: &str) -> Self {
        let service = std::env::var("JOBPILOT_SECRET_SERVICE")
            .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
        assert_ne!(service, "io.github.gvsrusa.jobpilot");
        let name = format!("{KEY_ACCOUNT_PREFIX}{key_id}");
        Self(keyring::Entry::new(&service, &name).unwrap())
    }

    fn stored(key_id: &str) -> Self {
        let entry = Self::new(key_id);
        entry.0.set_password("not a real key").unwrap();
        entry
    }

    fn present(&self) -> bool {
        match self.0.get_password() {
            Ok(_) => true,
            Err(keyring::Error::NoEntry) => false,
            Err(error) => panic!("{error}"),
        }
    }
}

impl Drop for KeyEntry {
    fn drop(&mut self) {
        let _ = self.0.delete_credential();
    }
}

async fn unlocked(paths: &AppPaths) -> (Db, KeyEntry) {
    let db = Db::open(paths).await.unwrap();
    let state = crypto::state(&db).await.unwrap();
    let KeyState::Unlocked { key_id } = state else {
        panic!("{state:?}");
    };
    (db, KeyEntry::new(&key_id))
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn delete_all_removes_this_roots_data_keys_only() {
    let (_tmp_a, a) = temp_root();
    let (_tmp_b, b) = temp_root();
    let (db_a, entry_a) = unlocked(&a).await;
    let (db_b, entry_b) = unlocked(&b).await;
    let before = audit::recent(&db_b, 10).await.unwrap().len();
    assert_eq!(before, 1);

    transfer::delete_all(db_a, "DELETE", true).await.unwrap();

    assert!(!entry_a.present());
    assert!(entry_b.present());
    assert_eq!(audit::recent(&db_b, 10).await.unwrap().len(), before);
    assert!(matches!(
        crypto::state(&db_b).await.unwrap(),
        KeyState::Unlocked { .. }
    ));
    db_b.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn delete_all_removes_inactive_data_keys_too() {
    const INACTIVE: &str = "a0a1a2a3a4a5a6a7";
    let (_tmp, paths) = temp_root();
    let (db, active) = unlocked(&paths).await;
    let inactive = KeyEntry::stored(INACTIVE);
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', x'00', 0)",
    )
    .bind(INACTIVE)
    .execute(&db.pool)
    .await
    .unwrap();

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert!(!active.present());
    assert!(!inactive.present());
}

// With no key row, the ciphertext is all that says this root ever used the key.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn delete_all_removes_a_key_only_its_ciphertext_names() {
    const ROWLESS: &str = "b0b1b2b3b4b5b6b7";
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let entry = KeyEntry::stored(ROWLESS);
    let mut connection = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES ('PT', x'00', ?, 't')",
    )
    .bind(ROWLESS)
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert!(!entry.present());
}

// M-08: the restored database no longer uses this key, and its row is all that finds it.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn delete_all_after_a_restore_removes_the_outgoing_key() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let info = backup::create(&db, "cli").await.unwrap();
    db.close().await;
    let (db, outgoing) = unlocked(&paths).await;

    let report = backup::restore(db, &info.path, "cli").await.unwrap();
    transfer::delete_all(report.db, "DELETE", true)
        .await
        .unwrap();

    assert!(!outgoing.present());
}

// Windows refuses a name this long before asking the store, so every retry would fail.
#[tokio::test]
async fn an_id_no_key_can_have_cannot_block_deletion() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    for key_id in [
        "x".repeat(100_000),
        String::new(),
        "NOT-A-KEY-ID-16C".into(),
    ] {
        sqlx::query(
            "INSERT INTO data_keys (key_id, created_at, key_check, active) \
             VALUES (?, 't', x'00', 0)",
        )
        .bind(key_id)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    transfer::delete_all(db, "DELETE", true).await.unwrap();

    assert!(!paths.db().exists());
}

#[tokio::test]
async fn an_application_survives_backup_restore_and_export_and_dies_with_delete_all() {
    use jobpilot_core::applications::{self, ApplicationInput};
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let input = ApplicationInput {
        company: "Acme".into(),
        role: "Engineer".into(),
        ..ApplicationInput::default()
    };
    let app = applications::add(&db, "cli", input).await.unwrap();
    applications::set_status(&db, "cli", &app.id, "applied", Some("2026-10-02"), "sent")
        .await
        .unwrap();
    let snapshot = backup::create(&db, "cli").await.unwrap();
    applications::remove(&db, "cli", &app.id).await.unwrap();

    let db = backup::restore(db, &snapshot.path, "cli").await.unwrap().db;
    let back = applications::get(&db, &app.id).await.unwrap();
    assert_eq!(back.applied_on.as_deref(), Some("2026-10-02"));
    assert_eq!(applications::history(&db, &app.id).await.unwrap().len(), 2);

    let dir = transfer::export_all(&db, "cli").await.unwrap();
    let (_, data) = read_export(&dir);
    let apps = data["tables"]["applications"].as_array().unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0]["company"], "Acme");
    assert_eq!(
        data["tables"]["application_events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    transfer::delete_all(db, "DELETE", true).await.unwrap();
    let db = Db::open(&paths).await.unwrap();
    assert!(applications::list(&db, None).await.unwrap().is_empty());
    db.close().await;
}
