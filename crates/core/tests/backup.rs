use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jobpilot_core::{AppPaths, Db, Error, app_schema_version, audit, backup, documents};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use tempfile::TempDir;

fn temp_root() -> (TempDir, AppPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    (tmp, paths)
}

fn sidecar(file: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(file);
    name.push(suffix);
    name.into()
}

async fn add_doc(db: &Db, title: &str) {
    documents::create(db, title, "body").await.unwrap();
}

async fn titles(db: &Db) -> Vec<String> {
    let mut found: Vec<_> = documents::list(db)
        .await
        .unwrap()
        .into_iter()
        .map(|doc| doc.title)
        .collect();
    found.sort();
    found
}

/// What a caller sees after a failed restore: the handle is gone, so it reopens.
async fn titles_after_reopen(paths: &AppPaths) -> Vec<String> {
    let db = Db::open(paths).await.unwrap();
    let found = titles(&db).await;
    db.close().await;
    found
}

async fn add_secret(db: &Db, name: &str) {
    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES (?, 't')")
        .bind(name)
        .execute(&db.pool)
        .await
        .unwrap();
}

async fn secrets(db: &Db) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM secret_index ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn plant_future_version(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'future', 1, x'00', 0)",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// A connection that sees only the main file, never a `-wal` beside it.
async fn titles_in_main_file(file: &Path) -> Vec<String> {
    let pool = SqlitePool::connect_with(SqliteConnectOptions::new().filename(file).immutable(true))
        .await
        .unwrap();
    let found = sqlx::query_scalar("SELECT title FROM documents ORDER BY title")
        .fetch_all(&pool)
        .await
        .unwrap();
    pool.close().await;
    found
}

async fn backup_with_doc(paths: &AppPaths, title: &str) -> PathBuf {
    let db = Db::open(paths).await.unwrap();
    add_doc(&db, title).await;
    let info = backup::create(&db, "cli").await.unwrap();
    db.close().await;
    info.path
}

fn pre_restore(paths: &AppPaths) -> Vec<PathBuf> {
    fs::read_dir(paths.backups())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pre-restore-")
        })
        .collect()
}

fn manifest(dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap()
}

fn edit_manifest(dir: &Path, key: &str, value: Value) {
    let mut edited = manifest(dir);
    edited[key] = value;
    fs::write(dir.join("manifest.json"), edited.to_string()).unwrap();
}

/// Leaves `paths` as a crash would: `title` is committed to the WAL and absent from
/// the main file, and no process is attached.
async fn leave_crashed_wal(paths: &AppPaths, title: &str) {
    let db = Db::open(paths).await.unwrap();
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&db.pool)
        .await
        .unwrap();
    add_doc(&db, title).await;
    let main = fs::read(paths.db()).unwrap();
    let wal = fs::read(sidecar(&paths.db(), "-wal")).unwrap();
    db.close().await;

    fs::write(paths.db(), main).unwrap();
    fs::write(sidecar(&paths.db(), "-wal"), wal).unwrap();
    assert!(
        !titles_in_main_file(&paths.db())
            .await
            .contains(&title.to_string()),
        "the row must live only in the WAL"
    );
}

/// A snapshot folder as `restore_offline` writes one: rows only in its WAL.
async fn wal_snapshot(paths: &AppPaths, title: &str) -> PathBuf {
    leave_crashed_wal(paths, title).await;
    let dir = paths.backups().join("offline-test");
    fs::create_dir_all(dir.join("documents")).unwrap();
    fs::copy(paths.db(), dir.join("jobpilot.db")).unwrap();
    fs::copy(
        sidecar(&paths.db(), "-wal"),
        sidecar(&dir.join("jobpilot.db"), "-wal"),
    )
    .unwrap();
    let body = json!({
        "format": 1,
        "schema_version": app_schema_version(),
        "created_at": "2026-01-01T00:00:00.000Z",
        "wal": true,
    });
    fs::write(dir.join("manifest.json"), body.to_string()).unwrap();
    dir
}

#[tokio::test]
async fn backup_then_restore_round_trip() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "alpha").await;
    fs::write(paths.documents().join("kept.txt"), "kept").unwrap();
    let info = backup::create(&db, "cli").await.unwrap();

    add_doc(&db, "bravo").await;
    fs::write(paths.documents().join("extra.txt"), "extra").unwrap();
    let db = backup::restore(db, &info.path, "gui").await.unwrap();

    assert_eq!(titles(&db).await, ["alpha"]);
    assert!(paths.documents().join("kept.txt").is_file());
    assert!(!paths.documents().join("extra.txt").exists());
    let hits = documents::search(&db, "alpha").await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(documents::search(&db, "bravo").await.unwrap().is_empty());
    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    db.close().await;

    assert!(
        fs::read_dir(paths.root()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("restore-")),
        "no temporary names survive"
    );
}

#[tokio::test]
async fn backup_and_restore_write_audit_rows() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let info = backup::create(&db, "cli").await.unwrap();
    let source = info.path.to_string_lossy().into_owned();

    let rows = audit::recent(&db, 1).await.unwrap();
    assert_eq!(rows[0].action, "backup");
    assert_eq!(rows[0].actor, "cli");
    assert_eq!(rows[0].target.as_deref(), Some(source.as_str()));

    let db = backup::restore(db, &info.path, "gui").await.unwrap();
    let rows = audit::recent(&db, 1).await.unwrap();
    assert_eq!(rows[0].action, "restore");
    assert_eq!(rows[0].actor, "gui");
    assert_eq!(rows[0].target.as_deref(), Some(source.as_str()));
    assert_eq!(rows[0].detail, json!({}));
    db.close().await;
}

#[tokio::test]
async fn restore_works_under_unicode_path_with_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("José Ñ 日本語 #1?"));
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "alpha").await;
    let info = backup::create(&db, "cli").await.unwrap();
    add_doc(&db, "bravo").await;

    let db = backup::restore(db, &info.path, "gui").await.unwrap();
    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;
}

#[tokio::test]
async fn restore_keeps_safety_snapshot() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    fs::write(paths.documents().join("extra.txt"), "extra").unwrap();

    backup::restore(db, &source, "cli")
        .await
        .unwrap()
        .close()
        .await;

    let found = pre_restore(&paths);
    assert_eq!(found.len(), 1);
    assert_eq!(
        titles_in_main_file(&found[0].join("jobpilot.db")).await,
        ["alpha", "bravo"]
    );
    assert!(found[0].join("documents").join("extra.txt").is_file());
}

#[tokio::test]
async fn restore_rejects_truncated_database() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db_file = source.join("jobpilot.db");
    let bytes = fs::read(&db_file).unwrap();
    fs::write(&db_file, &bytes[..bytes.len() / 2]).unwrap();

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
    assert!(pre_restore(&paths).is_empty(), "refused before step 2");
}

// Every query still works. This damage shows up only as a row in the report from
// `integrity_check`, not as an SQLite error.
#[tokio::test]
async fn restore_rejects_database_failing_integrity_check() {
    async fn edit_schema(file: &Path, from: &str, to: &str) {
        let options = SqliteConnectOptions::new()
            .filename(file)
            .pragma("writable_schema", "ON");
        let pool = SqlitePool::connect_with(options).await.unwrap();
        sqlx::query("UPDATE sqlite_master SET sql = replace(sql, ?, ?) WHERE name = 'settings'")
            .bind(from)
            .bind(to)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let file = source.join("jobpilot.db");
    edit_schema(&file, "value TEXT NOT NULL", "value TEXT").await;
    let pool = SqlitePool::connect_with(SqliteConnectOptions::new().filename(&file))
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('k', NULL, 't')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    edit_schema(&file, "value TEXT", "value TEXT NOT NULL").await;

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
}

#[tokio::test]
async fn restore_rejects_empty_database() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    // SQLite reads a zero-length file as a healthy database with no tables.
    fs::write(source.join("jobpilot.db"), "").unwrap();

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn restore_rejects_missing_manifest() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    fs::remove_file(source.join("manifest.json")).unwrap();

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
}

#[tokio::test]
async fn restore_rejects_unparsable_manifest() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let text = fs::read(source.join("manifest.json")).unwrap();
    fs::write(source.join("manifest.json"), &text[..text.len() / 2]).unwrap();

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn restore_rejects_unknown_format() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    edit_manifest(&source, "format", json!(2));

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
}

#[tokio::test]
async fn restore_rejects_missing_documents_folder() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    fs::write(paths.documents().join("kept.txt"), "kept").unwrap();
    fs::remove_dir_all(source.join("documents")).unwrap();

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert!(paths.documents().join("kept.txt").is_file());
}

#[tokio::test]
async fn restore_rejects_newer_schema() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    edit_manifest(&source, "schema_version", json!(9999));

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::SchemaTooNew { db: 9999, .. }), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
}

#[tokio::test]
async fn restore_rejects_database_newer_than_its_manifest() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let pool =
        SqlitePool::connect_with(SqliteConnectOptions::new().filename(source.join("jobpilot.db")))
            .await
            .unwrap();
    plant_future_version(&pool).await;
    pool.close().await;

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::SchemaTooNew { db: 9999, .. }), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
}

#[tokio::test]
async fn restore_rejects_snapshot_whose_wal_is_missing() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    edit_manifest(&source, "wal", json!(true));

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn restore_removes_stale_wal() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let wal = sidecar(&paths.db(), "-wal");
    let shm = sidecar(&paths.db(), "-shm");

    fs::write(&wal, "junk").unwrap();
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let db = backup::restore(db, &source, "cli").await.unwrap();
    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;

    // Nothing is attached, so only the restore itself can clear these.
    fs::write(&wal, "junk").unwrap();
    fs::write(&shm, "junk").unwrap();
    let db = backup::restore_offline(&paths, &source, "cli")
        .await
        .unwrap();
    assert_eq!(titles(&db).await, ["alpha"]);
    assert!(
        fs::read(&wal).unwrap_or_default() != b"junk",
        "the junk WAL was replaced"
    );
    db.close().await;
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn restore_offline_does_not_replay_old_wal_over_new_database() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    leave_crashed_wal(&paths, "from wal").await;
    let stale = fs::read(sidecar(&paths.db(), "-wal")).unwrap();

    let db = backup::restore_offline(&paths, &source, "cli")
        .await
        .unwrap();

    // VACUUM moved the pages, so a replay does not show as a row: it corrupts others.
    let report: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(report, ["ok"]);
    assert_eq!(titles(&db).await, ["alpha"]);
    let wal = fs::read(sidecar(&paths.db(), "-wal")).unwrap_or_default();
    assert!(
        wal.len() < 32 || wal[..32] != stale[..32],
        "every WAL has fresh salts, so the same header means the old one survived"
    );
    db.close().await;
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn restore_offline_snapshot_holds_live_wal_and_can_be_restored() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    leave_crashed_wal(&paths, "from wal").await;

    let db = backup::restore_offline(&paths, &source, "cli")
        .await
        .unwrap();
    let found = pre_restore(&paths);
    assert_eq!(found.len(), 1);
    assert_eq!(manifest(&found[0])["wal"], true);

    let db = backup::restore(db, &found[0], "cli").await.unwrap();
    assert_eq!(titles(&db).await, ["alpha", "from wal"]);
    db.close().await;
}

#[tokio::test]
async fn restore_refused_while_another_handle_open() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    let other = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;

    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InUse), "{err}");
    assert_eq!(titles(&other).await, ["alpha", "bravo"]);
    add_doc(&other, "charlie").await;
    other.close().await;
    assert_eq!(
        titles_after_reopen(&paths).await,
        ["alpha", "bravo", "charlie"]
    );
}

#[tokio::test]
async fn restore_offline_refused_while_a_handle_is_open() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let other = Db::open(&paths).await.unwrap();
    add_doc(&other, "bravo").await;

    let err = backup::restore_offline(&paths, &source, "cli")
        .await
        .err()
        .unwrap();

    assert!(matches!(err, Error::InUse), "{err}");
    assert_eq!(titles(&other).await, ["alpha", "bravo"]);
    assert!(pre_restore(&paths).is_empty(), "locks before step 2");
    other.close().await;
}

#[tokio::test]
async fn failed_temp_copy_leaves_live_data_and_releases_the_lock() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    // A folder where the temporary database goes makes step 4 fail on every OS.
    let blocker = paths.root().join("restore-tmp.db");
    fs::create_dir(&blocker).unwrap();

    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::Io(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
    assert_eq!(pre_restore(&paths).len(), 1);

    fs::remove_dir(blocker).unwrap();
    let db = Db::open(&paths).await.unwrap();
    let db = backup::restore(db, &source, "cli").await.unwrap();
    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_source_document_leaves_live_data_and_no_temp_files() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let locked = source.join("documents").join("locked.txt");
    fs::write(&locked, "x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    fs::write(paths.documents().join("kept.txt"), "kept").unwrap();

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let result = backup::restore(db, &source, "cli").await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(matches!(result.err().unwrap(), Error::Io(_)));
    assert_eq!(titles_after_reopen(&paths).await, ["alpha", "bravo"]);
    assert!(paths.documents().join("kept.txt").is_file());
    assert!(!paths.root().join("restore-tmp.db").exists());
    assert!(!paths.root().join("restore-tmp-documents").exists());
}

#[tokio::test]
async fn restore_clears_temp_files_left_by_a_crash() {
    let (_tmp, paths) = temp_root();
    let source = wal_snapshot(&paths, "from wal").await;
    let temp = paths.root().join("restore-tmp.db");
    fs::write(&temp, "left by a crash").unwrap();
    fs::write(sidecar(&temp, "-shm"), "left by a crash").unwrap();
    let leftovers = paths.root().join("restore-tmp-documents");
    fs::create_dir(&leftovers).unwrap();
    fs::write(leftovers.join("stale.txt"), "stale").unwrap();
    let moved = paths.root().join("restore-old-documents");
    fs::create_dir(&moved).unwrap();

    let db = Db::open(&paths).await.unwrap();
    let db = backup::restore(db, &source, "cli").await.unwrap();

    assert_eq!(titles(&db).await, ["from wal"]);
    assert!(!paths.documents().join("stale.txt").exists());
    assert!(!moved.exists());
    db.close().await;
}

#[tokio::test]
async fn list_skips_invalid_folders() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let older = backup::create(&db, "cli").await.unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let newer = backup::create(&db, "cli").await.unwrap();
    db.close().await;

    let backups = paths.backups();
    fs::create_dir(backups.join("junk")).unwrap();
    fs::write(backups.join("stray.txt"), "x").unwrap();
    // A crash or full disk leaves a folder with no manifest.
    let no_manifest = backups.join("backup-partial");
    fs::create_dir(&no_manifest).unwrap();
    fs::write(no_manifest.join("jobpilot.db"), "x").unwrap();
    // A power cut can leave a torn manifest.
    let torn = backups.join("backup-torn");
    fs::create_dir(&torn).unwrap();
    fs::write(torn.join("jobpilot.db"), "x").unwrap();
    fs::write(torn.join("manifest.json"), "{\"format\": 1, \"sch").unwrap();
    let no_database = backups.join("backup-nodb");
    fs::create_dir(&no_database).unwrap();
    fs::copy(
        older.path.join("manifest.json"),
        no_database.join("manifest.json"),
    )
    .unwrap();

    let listed = backup::list(&paths).await.unwrap();

    let found: Vec<_> = listed.iter().map(|info| &info.path).collect();
    assert_eq!(found, [&newer.path, &older.path]);
    assert_eq!(listed[0].created_at, newer.created_at);
    assert_eq!(listed[0].schema_version, Some(app_schema_version()));
}

#[tokio::test]
async fn list_of_a_root_with_no_backups_is_empty() {
    let (_tmp, paths) = temp_root();
    assert!(backup::list(&paths).await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_offline_recovers_when_db_cannot_open() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    fs::write(paths.documents().join("extra.txt"), "extra").unwrap();
    plant_future_version(&db.pool).await;
    db.close().await;
    assert!(matches!(
        Db::open(&paths).await,
        Err(Error::SchemaTooNew { .. })
    ));

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap();

    assert_eq!(titles(&db).await, ["alpha"]);
    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    assert!(!paths.documents().join("extra.txt").exists());
    let row = &audit::recent(&db, 1).await.unwrap()[0];
    assert_eq!(row.action, "restore");
    assert_eq!(row.actor, "gui");
    assert_eq!(row.detail, json!({}));
    db.close().await;

    let found = pre_restore(&paths);
    assert_eq!(found.len(), 1);
    assert_eq!(
        titles_in_main_file(&found[0].join("jobpilot.db")).await,
        ["alpha", "bravo"]
    );
    assert!(found[0].join("documents").join("extra.txt").is_file());

    let listed = backup::list(&paths).await.unwrap();
    let kept = listed.iter().find(|info| info.path == found[0]).unwrap();
    assert_eq!(kept.schema_version, Some(9999));
}

// The live database is junk, so `schema_version` truly comes back null.
#[tokio::test]
async fn unreadable_live_database_gives_null_snapshot_and_reports_lost_secrets() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    fs::write(paths.db(), vec![0x5a; 8192]).unwrap();

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap();

    assert_eq!(titles(&db).await, ["alpha"]);
    let row = &audit::recent(&db, 1).await.unwrap()[0];
    assert_eq!(row.action, "restore");
    assert_eq!(row.detail, json!({"secret_index_lost": true}));

    let found = pre_restore(&paths);
    assert_eq!(found.len(), 1);
    let written = manifest(&found[0]);
    assert!(
        written.get("schema_version").is_some() && written["schema_version"].is_null(),
        "{written}"
    );
    assert_eq!(written["wal"], false);

    let listed = backup::list(&paths).await.unwrap();
    let kept = listed.iter().find(|info| info.path == found[0]).unwrap();
    assert_eq!(kept.schema_version, None);

    let err = backup::restore(db, &found[0], "gui").await.err().unwrap();
    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn null_snapshot_that_opens_is_restored() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    edit_manifest(&source, "schema_version", Value::Null);

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let db = backup::restore(db, &source, "cli").await.unwrap();

    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;
}

#[tokio::test]
async fn null_snapshot_too_new_for_this_app_is_refused() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    edit_manifest(&source, "schema_version", Value::Null);
    let pool =
        SqlitePool::connect_with(SqliteConnectOptions::new().filename(source.join("jobpilot.db")))
            .await
            .unwrap();
    plant_future_version(&pool).await;
    pool.close().await;

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::SchemaTooNew { db: 9999, .. }), "{err}");
}

#[tokio::test]
async fn restore_of_offline_snapshot_keeps_wal_writes() {
    let (_tmp, paths) = temp_root();
    let source = wal_snapshot(&paths, "from wal").await;
    assert!(
        titles_in_main_file(&source.join("jobpilot.db"))
            .await
            .is_empty()
    );
    let wal_before = fs::read(sidecar(&source.join("jobpilot.db"), "-wal")).unwrap();

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let db = backup::restore(db, &source, "cli").await.unwrap();

    assert_eq!(titles(&db).await, ["from wal"]);
    db.close().await;
    assert_eq!(titles_after_reopen(&paths).await, ["from wal"]);
    assert_eq!(
        fs::read(sidecar(&source.join("jobpilot.db"), "-wal")).unwrap(),
        wal_before,
        "a restore never alters its source"
    );
    assert!(!sidecar(&paths.root().join("restore-tmp.db"), "-wal").exists());
}

#[tokio::test]
async fn restore_ignores_stray_wal_beside_backup() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let backup_db = source.join("jobpilot.db");

    // A real WAL written against a copy of this very database, so SQLite would
    // happily replay it if it were ever opened beside the backup.
    let other = paths.root().join("other.db");
    fs::copy(&backup_db, &other).unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&other)
            .journal_mode(SqliteJournalMode::Wal),
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO documents (id, title, body, created_at, updated_at) \
         VALUES ('foreign', 'foreign', 'x', 't', 't')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Validation must not see this either, or a sound backup would be refused.
    plant_future_version(&pool).await;
    fs::copy(sidecar(&other, "-wal"), sidecar(&backup_db, "-wal")).unwrap();
    pool.close().await;
    fs::remove_file(&other).unwrap();

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let db = backup::restore(db, &source, "cli").await.unwrap();

    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;
}

#[tokio::test]
async fn restore_rejects_corrupt_backup_despite_stray_wal() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let backup_db = source.join("jobpilot.db");
    let bytes = fs::read(&backup_db).unwrap();
    fs::write(&backup_db, &bytes[..bytes.len() / 2]).unwrap();
    fs::write(sidecar(&backup_db, "-wal"), "junk").unwrap();

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert_eq!(titles_after_reopen(&paths).await, ["alpha"]);
}

#[tokio::test]
async fn restore_keeps_secret_index_of_live_secrets() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_secret(&db, "a").await;
    let info = backup::create(&db, "cli").await.unwrap();
    add_secret(&db, "b").await;

    let db = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(secrets(&db).await, ["a", "b"]);
    db.close().await;
}

#[tokio::test]
async fn offline_restore_unions_secret_index_of_too_new_database() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_secret(&db, "a").await;
    let info = backup::create(&db, "cli").await.unwrap();
    add_secret(&db, "b").await;
    plant_future_version(&db.pool).await;
    db.close().await;
    assert!(matches!(
        Db::open(&paths).await,
        Err(Error::SchemaTooNew { .. })
    ));

    let db = backup::restore_offline(&paths, &info.path, "gui")
        .await
        .unwrap();

    assert_eq!(secrets(&db).await, ["a", "b"]);
    assert_eq!(audit::recent(&db, 1).await.unwrap()[0].detail, json!({}));
    db.close().await;
}

#[tokio::test]
async fn offline_restore_reads_secret_index_rows_held_in_the_wal() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&db.pool)
        .await
        .unwrap();
    add_secret(&db, "only in wal").await;
    let main = fs::read(paths.db()).unwrap();
    let wal = fs::read(sidecar(&paths.db(), "-wal")).unwrap();
    db.close().await;
    fs::write(paths.db(), main).unwrap();
    fs::write(sidecar(&paths.db(), "-wal"), wal).unwrap();

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap();

    assert_eq!(secrets(&db).await, ["only in wal"]);
    db.close().await;
}
