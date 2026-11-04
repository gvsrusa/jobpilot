use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jobpilot_core::ai::{self, DataCategory, consent};
use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{
    AppPaths, Db, Error, MIGRATOR, app_schema_version, audit, backup, documents, pause, settings,
};
use serde_json::{Value, json};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use tempfile::TempDir;

const CLAUDE: &str = "claude_code/cli_login";

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

fn snapshots(paths: &AppPaths, prefix: &str) -> Vec<PathBuf> {
    fs::read_dir(paths.backups())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(prefix)
        })
        .collect()
}

fn pre_restore(paths: &AppPaths) -> Vec<PathBuf> {
    snapshots(paths, "pre-restore-")
}

fn migrator_plus(sql: &'static str) -> Migrator {
    let mut all: Vec<Migration> = MIGRATOR.iter().cloned().collect();
    all.push(Migration::new(
        9000,
        "test".into(),
        MigrationType::Simple,
        sql.into_sql_str(),
        false,
    ));
    Migrator::with_migrations(all)
}

fn root_is_free_of_temp_names(paths: &AppPaths) -> bool {
    fs::read_dir(paths.root()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("restore-")
    })
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
    let db = backup::restore(db, &info.path, "gui").await.unwrap().db;

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

    let db = backup::restore(db, &info.path, "gui").await.unwrap().db;
    let rows = audit::recent(&db, 1).await.unwrap();
    assert_eq!(rows[0].action, "restore");
    assert_eq!(rows[0].actor, "gui");
    assert_eq!(rows[0].target.as_deref(), Some(source.as_str()));
    assert_eq!(rows[0].detail, json!({}));
    db.close().await;
}

/// Consent is a decision made now, so a backup cannot bring one back: not one that was active
/// when it was taken, and not one that was revoked since.
#[tokio::test]
async fn restore_revokes_external_consent_and_says_so_in_the_audit_log() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let info = backup::create(&db, "test").await.unwrap();
    consent::revoke(&db, CLAUDE, "gui").await.unwrap();

    let db = backup::restore(db, &info.path, "test").await.unwrap().db;

    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
    let actions = audit::recent(&db, 10).await.unwrap();
    let revoked: Vec<_> = actions
        .iter()
        .filter(|entry| entry.action == "consent_revoked_by_restore")
        .collect();
    assert_eq!(revoked.len(), 1, "{actions:?}");
    assert_eq!(revoked[0].actor, "test");
    assert_eq!(revoked[0].target.as_deref(), Some(CLAUDE));
    // The record of the old decision stays: only its end is written.
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM consent")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    db.close().await;
}

/// A restore that closes Claude Code's consent switches it off too, as the AI screen's own
/// Revoke does, so the screen never shows it on and failing every task.
#[tokio::test]
async fn restore_turns_claude_off_with_its_consent() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    consent::give(&db, CLAUDE, &[DataCategory::JobText], "2026-11-01", "gui")
        .await
        .unwrap();
    ai::set_enabled(&db, "claude_code", true).await.unwrap();
    ai::set_enabled(&db, "ollama", true).await.unwrap();
    let info = backup::create(&db, "test").await.unwrap();

    let restored = backup::restore(db, &info.path, "test").await.unwrap().db;

    let enabled = settings::get(&restored, "ai.enabled")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(enabled["claude_code"], false);
    assert_eq!(
        enabled["ollama"], true,
        "only the revoked provider is turned off"
    );
    restored.close().await;
}

/// With no consent to revoke the switch is left as it was.
#[tokio::test]
async fn restore_with_no_consent_leaves_the_claude_switch_alone() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    ai::set_enabled(&db, "claude_code", true).await.unwrap();
    let info = backup::create(&db, "test").await.unwrap();

    let restored = backup::restore(db, &info.path, "test").await.unwrap().db;

    let enabled = settings::get(&restored, "ai.enabled")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(enabled["claude_code"], true);
    restored.close().await;
}

/// The recovery screen's restore goes through the same temporary copy.
#[tokio::test]
async fn offline_restore_revokes_external_consent_too() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let job_text = [DataCategory::JobText];
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let info = backup::create(&db, "test").await.unwrap();
    db.close().await;

    let db = backup::restore_offline(&paths, &info.path, "test")
        .await
        .unwrap()
        .db;

    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
    db.close().await;
}

#[tokio::test]
async fn restore_turns_every_consented_provider_off() {
    const OPENAI: &str = "openai/api_key";
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let job_text = [DataCategory::JobText];
    for provider in ["claude_code", "openai", "anthropic"] {
        ai::set_enabled(&db, provider, true).await.unwrap();
    }
    consent::give(&db, CLAUDE, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    consent::give(&db, OPENAI, &job_text, "2026-11-01", "gui")
        .await
        .unwrap();
    let info = backup::create(&db, "test").await.unwrap();

    let db = backup::restore(db, &info.path, "test").await.unwrap().db;

    let enabled = settings::get(&db, "ai.enabled").await.unwrap().unwrap();
    assert_eq!(enabled["claude_code"], false);
    assert_eq!(enabled["openai"], false);
    assert_eq!(enabled["anthropic"], true, "no consent, so left as it was");
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
    assert_eq!(consent::active(&db, OPENAI).await.unwrap(), None);
    db.close().await;
}

/// A restore that revokes nothing writes nothing about consent.
#[tokio::test]
async fn restore_with_no_consent_writes_no_consent_row() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let info = backup::create(&db, "test").await.unwrap();

    let db = backup::restore(db, &info.path, "test").await.unwrap().db;

    let actions = audit::recent(&db, 10).await.unwrap();
    assert!(
        actions
            .iter()
            .all(|entry| entry.action != "consent_revoked_by_restore"),
        "{actions:?}"
    );
    db.close().await;
}

#[tokio::test]
async fn restore_works_under_unicode_path_with_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    // Windows forbids `?` in file names, so only Unix gets it.
    let name = if cfg!(unix) {
        "José Ñ 日本語 #1?"
    } else {
        "José Ñ 日本語 #1"
    };
    let paths = AppPaths::new(tmp.path().join(name));
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "alpha").await;
    let info = backup::create(&db, "cli").await.unwrap();
    add_doc(&db, "bravo").await;

    let db = backup::restore(db, &info.path, "gui").await.unwrap().db;
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
        .db
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
    let db = backup::restore(db, &source, "cli").await.unwrap().db;
    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;

    // Nothing is attached, so only the restore itself can clear these.
    fs::write(&wal, "junk").unwrap();
    fs::write(&shm, "junk").unwrap();
    let db = backup::restore_offline(&paths, &source, "cli")
        .await
        .unwrap()
        .db;
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
        .unwrap()
        .db;

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
        .unwrap()
        .db;
    let found = pre_restore(&paths);
    assert_eq!(found.len(), 1);
    assert_eq!(manifest(&found[0])["wal"], true);

    let db = backup::restore(db, &found[0], "cli").await.unwrap().db;
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
    let db = backup::restore(db, &source, "cli").await.unwrap().db;
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
    fs::write(sidecar(&temp, "-journal"), "left by a crash").unwrap();
    let leftovers = paths.root().join("restore-tmp-documents");
    fs::create_dir(&leftovers).unwrap();
    fs::write(leftovers.join("stale.txt"), "stale").unwrap();
    let moved = paths.root().join("restore-old-documents");
    fs::create_dir(&moved).unwrap();

    let db = Db::open(&paths).await.unwrap();
    let db = backup::restore(db, &source, "cli").await.unwrap().db;

    assert_eq!(titles(&db).await, ["from wal"]);
    assert!(!paths.documents().join("stale.txt").exists());
    assert!(!moved.exists());
    assert!(root_is_free_of_temp_names(&paths));
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

#[cfg(unix)]
#[tokio::test]
async fn list_skips_a_symlinked_snapshot() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let real = backup::create(&db, "cli").await.unwrap();
    db.close().await;
    let link = paths.backups().join("pre-restore-link");
    std::os::unix::fs::symlink(&real.path, &link).unwrap();

    let listed = backup::list(&paths).await.unwrap();

    let found: Vec<_> = listed.iter().map(|info| &info.path).collect();
    assert_eq!(found, [&real.path]);
}

/// Every `pre-migrate-<v>-` snapshot an upgrade writes restores through the schema check,
/// migrates to the current schema, and keeps its rows.
#[tokio::test]
async fn a_pre_migrate_snapshot_of_an_older_schema_restores() {
    for v in [1, 5, 11, 12, 15] {
        let (_tmp, paths) = temp_root();
        let older: Vec<Migration> = MIGRATOR
            .iter()
            .filter(|m| m.version <= v)
            .cloned()
            .collect();
        let db = Db::open_with(&paths, &Migrator::with_migrations(older))
            .await
            .unwrap();
        add_doc(&db, "old row").await;
        db.close().await;
        let db = Db::open(&paths).await.unwrap();
        assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
        add_doc(&db, "new row").await;
        let snapshot = snapshots(&paths, &format!("pre-migrate-{v}-"));
        assert_eq!(snapshot.len(), 1, "schema {v}: one pre-migrate snapshot");

        let restored = backup::restore(db, &snapshot[0], "gui")
            .await
            .unwrap_or_else(|e| panic!("schema {v}: restore failed: {e}"))
            .db;

        assert_eq!(
            restored.schema_version().await.unwrap(),
            app_schema_version(),
            "schema {v}: migrated to current"
        );
        assert_eq!(
            titles(&restored).await,
            ["old row"],
            "schema {v}: rows kept"
        );
        restored.close().await;
    }
}

#[tokio::test]
async fn list_of_a_root_with_no_backups_is_empty() {
    let (_tmp, paths) = temp_root();
    assert!(backup::list(&paths).await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_offline_refuses_a_root_with_files_and_no_marker() {
    let (_tmp, source_paths) = temp_root();
    let source = Db::open(&source_paths).await.unwrap();
    let info = backup::create(&source, "cli").await.unwrap();
    source.close().await;
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path());
    fs::write(paths.root().join("taxes.pdf"), "user data").unwrap();

    let result = backup::restore_offline(&paths, &info.path, "gui").await;

    assert!(matches!(result, Err(Error::RootNotOwned(_))), "{result:?}");
    let found: Vec<_> = fs::read_dir(paths.root()).unwrap().collect();
    assert_eq!(found.len(), 1, "a refused root gains nothing");
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
        .unwrap()
        .db;

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
        .unwrap()
        .db;

    assert_eq!(titles(&db).await, ["alpha"]);
    let row = &audit::recent(&db, 1).await.unwrap()[0];
    assert_eq!(row.action, "restore");
    assert_eq!(
        row.detail,
        json!({"secret_index_lost": true, "data_keys_lost": true})
    );

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
    assert!(kept.key_ids.is_empty(), "still listed");

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
    let db = backup::restore(db, &source, "cli").await.unwrap().db;

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
    let db = backup::restore(db, &source, "cli").await.unwrap().db;

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
    let db = backup::restore(db, &source, "cli").await.unwrap().db;

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

    let db = backup::restore(db, &info.path, "cli").await.unwrap().db;

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
        .unwrap()
        .db;

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
        .unwrap()
        .db;

    assert_eq!(secrets(&db).await, ["only in wal"]);
    db.close().await;
}

#[tokio::test]
async fn restore_migrates_an_older_backup_forward() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let newer = migrator_plus("CREATE TABLE added (x);");
    let db = Db::open_with(&paths, &newer).await.unwrap();
    add_doc(&db, "bravo").await;
    let before = snapshots(&paths, "pre-migrate-").len();

    let db = backup::restore_with(db, &source, "cli", &newer)
        .await
        .unwrap()
        .db;

    assert_eq!(titles(&db).await, ["alpha"]);
    assert_eq!(db.schema_version().await.unwrap(), 9000);
    sqlx::query("SELECT x FROM added")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(audit::recent(&db, 1).await.unwrap()[0].action, "restore");
    assert_eq!(
        snapshots(&paths, "pre-migrate-").len(),
        before,
        "the backup migrates as a temp copy, which needs no snapshot"
    );
    db.close().await;
}

#[tokio::test]
async fn failing_migration_leaves_live_data_and_no_pre_migrate_copies() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    add_secret(&db, "live").await;
    fs::write(paths.documents().join("kept.txt"), "kept").unwrap();
    let broken = migrator_plus("CREATE TABLE settings (x);");

    let err = backup::restore_with(db, &source, "cli", &broken)
        .await
        .err()
        .unwrap();

    assert!(matches!(err, Error::MigrationFailed(_)), "{err}");
    assert!(snapshots(&paths, "pre-migrate-").is_empty());
    assert!(root_is_free_of_temp_names(&paths));
    assert!(paths.documents().join("kept.txt").is_file());

    let db = Db::open(&paths).await.unwrap();
    assert_eq!(titles(&db).await, ["alpha", "bravo"]);
    assert_eq!(secrets(&db).await, ["live"]);
    assert!(
        audit::recent(&db, 10)
            .await
            .unwrap()
            .iter()
            .all(|row| row.action != "restore"),
        "no restore happened"
    );
    db.close().await;
    let newest = &backup::list(&paths).await.unwrap()[0];
    assert_eq!(newest.path, pre_restore(&paths)[0]);
}

#[tokio::test]
async fn backup_missing_a_table_is_refused_with_live_data_intact() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let pool =
        SqlitePool::connect_with(SqliteConnectOptions::new().filename(source.join("jobpilot.db")))
            .await
            .unwrap();
    sqlx::query("DROP TABLE secret_index")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    add_secret(&db, "live").await;
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert!(root_is_free_of_temp_names(&paths));
    let db = Db::open(&paths).await.unwrap();
    assert_eq!(titles(&db).await, ["alpha", "bravo"]);
    assert_eq!(secrets(&db).await, ["live"]);
    db.close().await;
}

// DDL owned by sqlx or SQLite is not ours to compare: a library bump must not
// strand every backup made before it.
#[tokio::test]
async fn backup_with_different_library_ddl_still_restores() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let pool =
        SqlitePool::connect_with(SqliteConnectOptions::new().filename(source.join("jobpilot.db")))
            .await
            .unwrap();
    sqlx::query("ALTER TABLE _sqlx_migrations ADD COLUMN planted TEXT")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let db = backup::restore(db, &source, "cli").await.unwrap().db;
    assert_eq!(titles(&db).await, ["alpha"]);
    db.close().await;
}

// A table the backup lacks fails loudly even with no saved rows to insert into it.
#[tokio::test]
async fn backup_missing_a_table_is_refused_with_no_live_secrets() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let pool =
        SqlitePool::connect_with(SqliteConnectOptions::new().filename(source.join("jobpilot.db")))
            .await
            .unwrap();
    sqlx::query("DROP TABLE secret_index")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
}

#[tokio::test]
async fn failed_reopen_after_the_swap_says_the_restore_happened() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "alpha").await;
    let info = backup::create(&db, "cli").await.unwrap();
    add_doc(&db, "bravo").await;
    add_secret(&db, "live").await;
    // A file where `Db::open` makes a folder fails the reopen, and nothing before it.
    fs::remove_dir(paths.exports()).unwrap();
    fs::write(paths.exports(), "in the way").unwrap();

    let err = backup::restore(db, &info.path, "gui").await.err().unwrap();

    assert!(matches!(err, Error::RestoredButNotOpened(_)), "{err}");
    assert_eq!(err.code(), "restored_not_opened");

    fs::remove_file(paths.exports()).unwrap();
    let db = Db::open(&paths).await.unwrap();
    assert_eq!(titles(&db).await, ["alpha"]);
    assert_eq!(secrets(&db).await, ["live"]);
    assert_eq!(audit::recent(&db, 1).await.unwrap()[0].action, "restore");
    db.close().await;
}

// `Db::open` succeeds here, so the recovery screen never shows: `restore` is the way out.
#[tokio::test]
async fn restore_recovers_a_corrupt_database_that_still_opens() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "alpha").await;
    add_secret(&db, "live").await;
    let info = backup::create(&db, "cli").await.unwrap();
    add_doc(&db, "bravo").await;
    add_secret(&db, "later").await;
    let root: i64 =
        sqlx::query_scalar("SELECT rootpage FROM sqlite_master WHERE name = 'documents'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    db.close().await;
    let mut bytes = fs::read(paths.db()).unwrap();
    let start = (root as usize - 1) * 4096;
    bytes[start..start + 4096].fill(0x5a);
    fs::write(paths.db(), bytes).unwrap();

    let db = Db::open(&paths).await.unwrap();
    assert!(documents::list(&db).await.is_err(), "really corrupt");

    let db = backup::restore(db, &info.path, "gui").await.unwrap().db;

    assert_eq!(titles(&db).await, ["alpha"]);
    assert_eq!(secrets(&db).await, ["later", "live"]);
    let row = &audit::recent(&db, 1).await.unwrap()[0];
    assert_eq!(row.action, "restore");
    assert_eq!(row.detail, json!({}));
    db.close().await;

    let kept = backup::list(&paths)
        .await
        .unwrap()
        .into_iter()
        .find(|found| found.path != info.path)
        .unwrap();
    let copy = fs::read(kept.path.join("jobpilot.db")).unwrap();
    assert_eq!(copy[(root as usize - 1) * 4096], 0x5a, "kept as it was");
}

#[tokio::test]
async fn corrupt_live_database_with_unreadable_secret_index_reports_it() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "alpha").await;
    let info = backup::create(&db, "cli").await.unwrap();
    let roots: Vec<i64> = sqlx::query_scalar(
        "SELECT rootpage FROM sqlite_master WHERE name IN ('documents', 'secret_index')",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    db.close().await;
    let mut bytes = fs::read(paths.db()).unwrap();
    for root in roots {
        let start = (root as usize - 1) * 4096;
        bytes[start..start + 4096].fill(0x5a);
    }
    fs::write(paths.db(), bytes).unwrap();

    let db = Db::open(&paths).await.unwrap();
    let db = backup::restore(db, &info.path, "gui").await.unwrap().db;

    assert_eq!(titles(&db).await, ["alpha"]);
    let row = &audit::recent(&db, 1).await.unwrap()[0];
    assert_eq!(row.detail, json!({"secret_index_lost": true}));
    db.close().await;
}

const KEY_ID: &str = "1a2b3c4d5e6f7a8b";
const OTHER_KEY_ID: &str = "8b7a6f5e4d3c2b1a";

/// A key row whose key-check opens under nothing. Only a keychain read looks at it.
async fn add_key_row(db: &Db, key_id: &str, active: bool) {
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) \
         VALUES (?, 'then', x'0102', ?)",
    )
    .bind(key_id)
    .bind(active)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn key_rows(db: &Db) -> Vec<(String, i64)> {
    sqlx::query_as("SELECT key_id, active FROM data_keys ORDER BY key_id")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn audit_count(db: &Db, action: &str) -> usize {
    let rows = audit::recent(db, 100).await.unwrap();
    rows.iter().filter(|row| row.action == action).count()
}

fn missing(key_id: &str) -> KeyState {
    KeyState::KeyMissing {
        key_id: key_id.into(),
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

fn first_migration_only() -> Migrator {
    let first: Vec<Migration> = MIGRATOR
        .iter()
        .filter(|migration| migration.version == 1)
        .cloned()
        .collect();
    Migrator::with_migrations(first)
}

/// A snapshot from before `data_keys` existed. The next `Db::open` migrates the root.
async fn backup_from_before_data_keys(paths: &AppPaths) -> backup::BackupInfo {
    let old = first_migration_only();
    let db = Db::open_with(paths, &old).await.unwrap();
    add_doc(&db, "alpha").await;
    let info = backup::create(&db, "cli").await.unwrap();
    db.close().await;
    info
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn restore_reports_backup_key_state_first() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    let info = backup::create(&db, "cli").await.unwrap();
    entry(&key_id).delete_credential().unwrap();

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, Some(missing(&key_id)));
    assert_eq!(key_rows(&report.db).await, [(key_id.clone(), 1)]);
    assert_eq!(
        crypto::state(&report.db).await.unwrap(),
        missing(&key_id),
        "no second key"
    );
    assert_eq!(key_rows(&report.db).await, [(key_id.clone(), 1)]);
    assert_eq!(audit_count(&report.db, "data_key_created").await, 1);
    report.db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn restore_reports_a_key_this_machine_holds_as_unlocked() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;
    let info = backup::create(&db, "cli").await.unwrap();

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    let expected = KeyState::Unlocked { key_id };
    assert_eq!(report.backup_key_state, Some(expected));
    report.db.close().await;
}

// Key rows with none active are a key this root once used, so `None` would read as 0.1.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn a_backup_with_no_active_row_reports_its_newest_key() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, false).await;
    let info = backup::create(&db, "cli").await.unwrap();

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, Some(missing(KEY_ID)));
    report.db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn restore_keeps_the_backups_key_active_beside_the_outgoing_one() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, true).await;
    let info = backup::create(&db, "cli").await.unwrap();
    sqlx::query("DELETE FROM data_keys")
        .execute(&db.pool)
        .await
        .unwrap();
    add_key_row(&db, OTHER_KEY_ID, true).await;

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, Some(missing(KEY_ID)));
    assert_eq!(
        key_rows(&report.db).await,
        [(KEY_ID.to_owned(), 1), (OTHER_KEY_ID.to_owned(), 0)]
    );
    report.db.close().await;
}

// The rename is done, so the caller gets no report: the state travels in the text.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn failed_reopen_still_names_the_backups_key_state() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, true).await;
    let info = backup::create(&db, "cli").await.unwrap();
    fs::remove_dir(paths.exports()).unwrap();
    fs::write(paths.exports(), "in the way").unwrap();

    let err = backup::restore(db, &info.path, "gui").await.err().unwrap();

    let Error::RestoredButNotOpened(text) = &err else {
        panic!("{err}");
    };
    assert!(text.contains(KEY_ID), "{text}");
    assert!(text.contains("no entry"), "{text}");
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn a_readable_outgoing_key_reactivates_after_an_older_backup() {
    let (_tmp, paths) = temp_root();
    let info = backup_from_before_data_keys(&paths).await;
    let (db, key_id, _guard) = unlocked_root(&paths).await;

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, None);
    assert_eq!(key_rows(&report.db).await, [(key_id.clone(), 0)]);
    let expected = KeyState::Unlocked {
        key_id: key_id.clone(),
    };
    assert_eq!(crypto::state(&report.db).await.unwrap(), expected);
    assert_eq!(key_rows(&report.db).await, [(key_id, 1)]);
    report.db.close().await;
}

#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn backup_lists_key_ids_from_its_database() {
    let (_tmp, paths) = temp_root();
    let (db, key_id, _guard) = unlocked_root(&paths).await;

    let info = backup::create(&db, "cli").await.unwrap();

    assert_eq!(info.key_ids, [key_id.as_str()]);
    assert_eq!(backup::list(&paths).await.unwrap()[0].key_ids, [key_id]);
    db.close().await;
}

#[tokio::test]
async fn restore_of_a_backup_without_data_keys_reports_none() {
    let (_tmp, paths) = temp_root();
    let info = backup_from_before_data_keys(&paths).await;
    assert!(info.key_ids.is_empty());
    let db = Db::open(&paths).await.unwrap();
    add_doc(&db, "bravo").await;
    let listed = backup::list(&paths).await.unwrap();
    assert!(listed.iter().all(|found| found.key_ids.is_empty()));

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, None);
    assert_eq!(titles(&report.db).await, ["alpha"]);
    assert!(key_rows(&report.db).await.is_empty());
    report.db.close().await;
}

#[tokio::test]
async fn key_ids_come_from_each_backups_own_database_active_first() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, false).await;
    add_key_row(&db, OTHER_KEY_ID, true).await;
    let created = backup::create(&db, "cli").await.unwrap();
    sqlx::query("DELETE FROM data_keys")
        .execute(&db.pool)
        .await
        .unwrap();
    db.close().await;

    let expected = [OTHER_KEY_ID, KEY_ID];
    assert_eq!(created.key_ids, expected);
    assert_eq!(backup::list(&paths).await.unwrap()[0].key_ids, expected);
}

/// The live row must survive a restore for `delete_all` to find its entry.
#[tokio::test]
async fn restore_puts_live_key_rows_back_inactive() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, true).await;

    let report = backup::restore(db, &source, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, None);
    assert_eq!(key_rows(&report.db).await, [(KEY_ID.to_owned(), 0)]);
    let (created_at, key_check): (String, Vec<u8>) =
        sqlx::query_as("SELECT created_at, key_check FROM data_keys")
            .fetch_one(&report.db.pool)
            .await
            .unwrap();
    assert_eq!(created_at, "then");
    assert_eq!(key_check, [1, 2]);
    report.db.close().await;
}

#[tokio::test]
async fn offline_restore_puts_key_rows_of_a_too_new_database_back_inactive() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, true).await;
    plant_future_version(&db.pool).await;
    db.close().await;

    let report = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap();

    assert_eq!(key_rows(&report.db).await, [(KEY_ID.to_owned(), 0)]);
    let row = &audit::recent(&report.db, 1).await.unwrap()[0];
    assert_eq!(row.detail, json!({}));
    report.db.close().await;
}

// A live database from before `data_keys` has no rows to lose, so this is not "lost".
#[tokio::test]
async fn offline_restore_over_a_database_with_no_data_keys_table_loses_nothing() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    for statement in ["DROP TABLE work_authorization", "DROP TABLE data_keys"] {
        sqlx::query(statement).execute(&db.pool).await.unwrap();
    }
    plant_future_version(&db.pool).await;
    db.close().await;

    let report = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap();

    let row = &audit::recent(&report.db, 1).await.unwrap()[0];
    assert_eq!(row.detail, json!({}));
    report.db.close().await;
}

#[cfg(unix)]
fn mkfifo(path: &Path) {
    let made = std::process::Command::new("mkfifo").arg(path).status();
    assert!(made.unwrap().success());
}

/// Runs on its own thread, so a copy stuck opening a FIFO fails the test instead of
/// holding the runtime open for ever.
#[cfg(unix)]
fn within_five_seconds<T: Send + 'static>(work: impl Future<Output = T> + Send + 'static) -> T {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = done.send(runtime.block_on(work));
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("the copy hung")
}

#[cfg(unix)]
#[test]
fn copy_dir_skips_symlinks_and_fifos_without_hanging() {
    let (tmp, paths) = temp_root();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("taxes.pdf"), "user data").unwrap();

    let dir = within_five_seconds(async move {
        let db = Db::open(&paths).await.unwrap();
        let documents = paths.documents();
        fs::write(documents.join("kept.txt"), "kept").unwrap();
        fs::create_dir(documents.join("nested")).unwrap();
        fs::write(documents.join("nested").join("inner.txt"), "inner").unwrap();
        mkfifo(&documents.join("nested").join("pipe"));
        mkfifo(&documents.join("pipe"));
        std::os::unix::fs::symlink("/etc/hosts", documents.join("hosts")).unwrap();
        std::os::unix::fs::symlink("/etc/nowhere", documents.join("dangling")).unwrap();
        std::os::unix::fs::symlink(&outside, documents.join("folder")).unwrap();
        let info = backup::create(&db, "cli").await.unwrap();
        db.close().await;
        info.path.join("documents")
    });

    let mut found: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    found.sort();
    assert_eq!(found, ["kept.txt", "nested"]);
    assert_eq!(fs::read_to_string(dir.join("kept.txt")).unwrap(), "kept");
    let nested: Vec<_> = fs::read_dir(dir.join("nested")).unwrap().collect();
    assert_eq!(nested.len(), 1, "the FIFO one level down is skipped too");
}

#[cfg(unix)]
#[test]
fn copy_dir_leaves_a_unix_socket_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let (from, to) = (tmp.path().join("from"), tmp.path().join("to"));
    fs::create_dir(&from).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(from.join("socket")).unwrap();

    jobpilot_core::db::copy_dir(&from, &to).unwrap();

    assert_eq!(fs::read_dir(&to).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn restore_from_a_backup_holding_a_fifo_does_not_hang() {
    let (_tmp, paths) = temp_root();
    within_five_seconds(async move {
        let source = backup_with_doc(&paths, "alpha").await;
        mkfifo(&source.join("documents").join("pipe"));
        let db = Db::open(&paths).await.unwrap();

        let report = backup::restore(db, &source, "cli").await.unwrap();

        assert!(!paths.documents().join("pipe").exists());
        report.db.close().await;
    });
}

// A backup is untrusted input, and this link would pull the target folder into the root.
#[cfg(unix)]
#[tokio::test]
async fn restore_refuses_a_documents_folder_that_is_a_link() {
    let (tmp, paths) = temp_root();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("taxes.pdf"), "user data").unwrap();
    let source = backup_with_doc(&paths, "alpha").await;
    fs::remove_dir_all(source.join("documents")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join("documents")).unwrap();

    let db = Db::open(&paths).await.unwrap();
    let err = backup::restore(db, &source, "cli").await.err().unwrap();

    assert!(matches!(err, Error::InvalidBackup(_)), "{err}");
    assert!(!paths.documents().join("taxes.pdf").exists());
}

#[tokio::test]
async fn swap_marker_left_by_a_crash_is_reported_and_clears() {
    let (_tmp, paths) = temp_root();
    Db::open(&paths).await.unwrap().close().await;
    fs::write(paths.swap_marker(), "").unwrap();

    let db = Db::open(&paths).await.unwrap();
    assert!(db.swap_interrupted());
    db.close().await;
    assert!(paths.swap_marker().is_file(), "open leaves it for the user");

    let db = Db::open(&paths).await.unwrap();
    db.clear_swap_warning().await.unwrap();
    assert!(!db.swap_interrupted());
    assert!(!paths.swap_marker().exists());
    db.clear_swap_warning().await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn good_restore_leaves_no_swap_marker() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();

    let report = backup::restore(db, &source, "cli").await.unwrap();

    assert!(!paths.swap_marker().exists());
    assert!(!report.db.swap_interrupted());
    report.db.close().await;

    let report = backup::restore_offline(&paths, &source, "cli")
        .await
        .unwrap();
    assert!(!report.db.swap_interrupted());
    report.db.close().await;
}

// The swap replaced everything, so what the crash damaged is gone.
#[tokio::test]
async fn good_restore_clears_the_warning_of_an_earlier_crash() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    fs::write(paths.swap_marker(), "").unwrap();
    let db = Db::open(&paths).await.unwrap();

    let report = backup::restore(db, &source, "cli").await.unwrap();

    assert!(!report.db.swap_interrupted());
    report.db.close().await;
}

// It never reached the swap, so the damage it warns of is still there.
#[tokio::test]
async fn failed_restore_keeps_the_warning_of_an_earlier_crash() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    fs::write(source.join("jobpilot.db"), "").unwrap();
    fs::write(paths.swap_marker(), "").unwrap();
    let db = Db::open(&paths).await.unwrap();

    backup::restore(db, &source, "cli").await.err().unwrap();

    let db = Db::open(&paths).await.unwrap();
    assert!(db.swap_interrupted());
    db.close().await;
}

// Past the commit point the data is the backup, so nothing is left to warn about.
#[tokio::test]
async fn failed_reopen_after_the_swap_leaves_no_swap_marker() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let info = backup::create(&db, "cli").await.unwrap();
    fs::remove_dir(paths.exports()).unwrap();
    fs::write(paths.exports(), "in the way").unwrap();

    let err = backup::restore(db, &info.path, "gui").await.err().unwrap();

    assert!(matches!(err, Error::RestoredButNotOpened(_)), "{err}");
    assert!(!paths.swap_marker().exists());
}

/// What restore and `list` make of a backup with one file swapped for something else.
/// Live data must come through whole.
#[cfg(unix)]
fn assert_refused_in_time(name: &'static str, wal: bool, plant: fn(&Path, &Path)) {
    let (tmp, paths) = temp_root();
    let spare = tmp.path().join("spare");
    within_five_seconds(async move {
        let source = backup_with_doc(&paths, "alpha").await;
        if wal {
            edit_manifest(&source, "wal", json!(true));
        }
        let target = source.join(name);
        if target.exists() {
            fs::rename(&target, &spare).unwrap();
        } else {
            fs::write(&spare, "not a WAL").unwrap();
        }
        plant(&target, &spare);

        let listed = backup::list(&paths).await.unwrap();
        let shown = listed.iter().find(|info| info.path == source);
        assert!(
            shown.is_none_or(|info| info.key_ids.is_empty()),
            "{name}: nothing was read through it"
        );

        let db = Db::open(&paths).await.unwrap();
        let err = backup::restore(db, &source, "cli").await.err().unwrap();
        assert!(matches!(err, Error::InvalidBackup(_)), "{name}: {err}");
        assert_eq!(titles_after_reopen(&paths).await, ["alpha"], "{name}");
    });
    drop(tmp);
}

#[cfg(unix)]
fn plant_fifo(target: &Path, _spare: &Path) {
    mkfifo(target);
}

#[cfg(unix)]
fn plant_link(target: &Path, spare: &Path) {
    std::os::unix::fs::symlink(spare, target).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fifo_for_a_backup_file_is_refused_and_does_not_hang() {
    assert_refused_in_time("manifest.json", false, plant_fifo);
    assert_refused_in_time("jobpilot.db", false, plant_fifo);
    assert_refused_in_time("jobpilot.db-wal", true, plant_fifo);
}

#[cfg(unix)]
#[test]
fn a_link_for_a_backup_file_is_refused_and_never_followed() {
    assert_refused_in_time("manifest.json", false, plant_link);
    assert_refused_in_time("jobpilot.db", false, plant_link);
    assert_refused_in_time("jobpilot.db-wal", true, plant_link);
}

// SQLite opens these itself once the manifest claims a WAL, and no copy of ours does.
#[cfg(unix)]
#[test]
fn a_fifo_beside_a_wal_snapshot_does_not_hang() {
    for suffix in ["-shm", "-journal"] {
        let (_tmp, paths) = temp_root();
        within_five_seconds(async move {
            let source = wal_snapshot(&paths, "from wal").await;
            mkfifo(&sidecar(&source.join("jobpilot.db"), suffix));

            backup::list(&paths).await.unwrap();
            let db = Db::open(&paths).await.unwrap();
            let err = backup::restore(db, &source, "cli").await.err().unwrap();

            assert!(matches!(err, Error::InvalidBackup(_)), "{suffix}: {err}");
        });
    }
}

// The usual route to this state: a restore leaves the live key beside a backup's lost one.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn a_readable_outgoing_key_stands_in_for_a_lost_backup_key_with_no_ciphertext() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_key_row(&db, KEY_ID, true).await;
    let info = backup::create(&db, "cli").await.unwrap();
    sqlx::query("DELETE FROM data_keys")
        .execute(&db.pool)
        .await
        .unwrap();
    let state = crypto::state(&db).await.unwrap();
    let KeyState::Unlocked { key_id } = state else {
        panic!("{state:?}");
    };
    let _guard = EntryGuard(key_id.clone());

    let report = backup::restore(db, &info.path, "cli").await.unwrap();

    assert_eq!(report.backup_key_state, Some(missing(KEY_ID)));
    let expected = KeyState::Unlocked {
        key_id: key_id.clone(),
    };
    assert_eq!(crypto::state(&report.db).await.unwrap(), expected);
    let mut rows = key_rows(&report.db).await;
    rows.sort_by_key(|(_, active)| *active);
    assert_eq!(rows, [(KEY_ID.to_owned(), 0), (key_id, 1)]);
    report.db.close().await;
}

/// A restore must never be the way out of an emergency pause: only a person clears it.
#[tokio::test]
async fn restore_keeps_a_set_pause() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let earlier = backup::create(&db, "cli").await.unwrap().path;
    pause::set(&db, "cli").await.unwrap();

    let restored = backup::restore(db, &earlier, "cli").await.unwrap().db;

    assert!(pause::is_set(&restored).await.unwrap());
    let kept: Vec<_> = audit::recent(&restored, 20)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.action == "emergency_pause_kept_by_restore")
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].actor, "cli");
    restored.close().await;
}

#[tokio::test]
async fn restoring_a_paused_backup_stays_paused() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    pause::set(&db, "cli").await.unwrap();
    let during = backup::create(&db, "cli").await.unwrap().path;
    pause::clear(&db, "cli").await.unwrap();

    let restored = backup::restore(db, &during, "cli").await.unwrap().db;

    assert!(pause::is_set(&restored).await.unwrap());
    restored.close().await;
}

/// A restore with no pause on either side leaves none, and a restore under an actor the
/// pause table does not take still completes.
#[tokio::test]
async fn restore_adds_no_pause_when_none_was_set() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let earlier = backup::create(&db, "cli").await.unwrap().path;
    let restored = backup::restore(db, &earlier, "test").await.unwrap().db;
    assert!(!pause::is_set(&restored).await.unwrap());
    pause::set(&restored, "gui").await.unwrap();
    let again = backup::restore(restored, &earlier, "test")
        .await
        .unwrap()
        .db;
    assert!(pause::is_set(&again).await.unwrap());
    // The table takes a person only, so an actor outside it is recorded as the CLI.
    assert_eq!(
        pause::state(&again).await.unwrap().set_by.as_deref(),
        Some("cli")
    );
    again.close().await;
}

fn audit_actions(entries: &[audit::AuditEntry], action: &str) -> usize {
    entries.iter().filter(|e| e.action == action).count()
}

/// A live database that cannot be read cannot say whether the pause was on, so the restored
/// data comes up paused and the audit log says why.
#[tokio::test]
async fn offline_restore_over_an_unreadable_database_comes_up_paused() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    db.close().await;
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(sidecar(&paths.db(), suffix));
    }
    fs::write(paths.db(), b"this is not a database at all, just bytes").unwrap();

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap()
        .db;

    assert!(pause::is_set(&db).await.unwrap());
    let entries = audit::recent(&db, 20).await.unwrap();
    assert_eq!(audit_actions(&entries, "emergency_pause_set_by_restore"), 1);
    let row = entries
        .iter()
        .find(|e| e.action == "emergency_pause_set_by_restore")
        .unwrap();
    assert_eq!(row.detail, json!({ "why": "live_state_unreadable" }));
    db.close().await;
}

/// An older schema has no pause table, which is a pause that was never on.
#[tokio::test]
async fn offline_restore_over_a_database_with_no_pause_table_stays_unpaused() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    sqlx::query("DROP TABLE emergency_pause")
        .execute(&db.pool)
        .await
        .unwrap();
    plant_future_version(&db.pool).await;
    db.close().await;

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap()
        .db;

    assert!(!pause::is_set(&db).await.unwrap());
    let entries = audit::recent(&db, 20).await.unwrap();
    assert_eq!(audit_actions(&entries, "emergency_pause_set_by_restore"), 0);
    db.close().await;
}

/// No database file is no live data, and no pause.
#[tokio::test]
async fn offline_restore_with_no_live_database_stays_unpaused() {
    let (_tmp, paths) = temp_root();
    let source = backup_with_doc(&paths, "alpha").await;
    let db = Db::open(&paths).await.unwrap();
    db.close().await;
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(sidecar(&paths.db(), suffix));
    }

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap()
        .db;

    assert!(!pause::is_set(&db).await.unwrap());
    db.close().await;
}

/// A restore whose live pause was readable keeps the backup's own row and audits once.
#[tokio::test]
async fn a_kept_pause_is_audited_once_and_not_when_the_backup_already_had_it() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let plain = backup::create(&db, "cli").await.unwrap().path;
    pause::set(&db, "gui").await.unwrap();
    let paused_backup = backup::create(&db, "cli").await.unwrap().path;

    let db = backup::restore(db, &plain, "cli").await.unwrap().db;
    let entries = audit::recent(&db, 50).await.unwrap();
    assert_eq!(
        audit_actions(&entries, "emergency_pause_kept_by_restore"),
        1
    );
    assert_eq!(
        pause::state(&db).await.unwrap().set_by.as_deref(),
        Some("cli")
    );

    let db = backup::restore(db, &paused_backup, "cli").await.unwrap().db;
    let entries = audit::recent(&db, 50).await.unwrap();
    // The backup carried the row, so this restore adds none and audits none (the log is the
    // backup's own, which never saw the first restore).
    assert_eq!(
        audit_actions(&entries, "emergency_pause_kept_by_restore"),
        0
    );
    assert_eq!(
        pause::state(&db).await.unwrap().set_by.as_deref(),
        Some("gui")
    );
    db.close().await;
}

// S-1 (review-1.0): a restored backup must not choose which program runs.

#[cfg(unix)]
const CLAUDE_NAME: &str = "claude";
#[cfg(windows)]
const CLAUDE_NAME: &str = "claude.cmd";
#[cfg(unix)]
const GEMINI_NAME: &str = "gemini";
#[cfg(windows)]
const GEMINI_NAME: &str = "gemini.cmd";

/// Every setting that names a program, plus the launch-time network check.
const OWNED: [&str; 5] = [
    "ai.claude_path",
    "ai.gemini_path",
    "ai.codex_path",
    "ai.ollama_url",
    settings::UPDATES_CHECK_ON_LAUNCH,
];

/// An executable named like Claude Code in `dir`.
fn plant_program(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// The review's scenario: a backup made in root A names a program planted in its own
/// documents folder, and turns the launch check on. Restored into root B, B keeps its own
/// values, and an absent value stays absent.
async fn crafted_backup(tmp: &TempDir) -> PathBuf {
    let paths = AppPaths::new(tmp.path().join("crafted"));
    let db = Db::open(&paths).await.unwrap();
    let planted = plant_program(&paths.documents(), CLAUDE_NAME);
    let gemini = plant_program(&paths.documents(), "gemini");
    let path = json!(planted.display().to_string());
    settings::set(&db, "ai.claude_path", &path).await.unwrap();
    let gemini = json!(gemini.display().to_string());
    settings::set(&db, "ai.gemini_path", &gemini).await.unwrap();
    settings::set(&db, "ai.codex_path", &json!("/tmp/codex"))
        .await
        .unwrap();
    settings::set(&db, settings::UPDATES_CHECK_ON_LAUNCH, &json!(true))
        .await
        .unwrap();
    settings::set(&db, "ai.ollama_url", &json!("http://127.0.0.1:1"))
        .await
        .unwrap();
    let info = backup::create(&db, "cli").await.unwrap();
    db.close().await;
    info.path
}

async fn owned_values(db: &Db) -> Vec<Option<Value>> {
    let mut found = Vec::new();
    for key in OWNED {
        found.push(settings::get(db, key).await.unwrap());
    }
    found
}

#[tokio::test]
async fn restore_keeps_the_live_program_paths_and_launch_check() {
    let tmp = tempfile::tempdir().unwrap();
    let source = crafted_backup(&tmp).await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = Db::open(&paths).await.unwrap();
    let mine = json!("/opt/mine/claude");
    settings::set(&db, "ai.claude_path", &mine).await.unwrap();
    settings::set(&db, settings::UPDATES_CHECK_ON_LAUNCH, &json!(false))
        .await
        .unwrap();
    settings::set(&db, "ai.ollama_url", &json!("http://127.0.0.1:11434"))
        .await
        .unwrap();
    let before = owned_values(&db).await;

    let db = backup::restore(db, &source, "gui").await.unwrap().db;

    assert_eq!(owned_values(&db).await, before);
    assert_eq!(before[1], None, "an absent path stays absent");
    let entries = audit::recent(&db, 20).await.unwrap();
    let kept: Vec<_> = entries
        .iter()
        .filter(|entry| entry.action == "settings_kept_by_restore")
        .collect();
    assert_eq!(kept.len(), 1, "{entries:?}");
    assert_eq!(kept[0].detail["keys"], json!(OWNED));
    db.close().await;
}

#[tokio::test]
async fn offline_restore_keeps_the_live_program_paths_and_launch_check() {
    let tmp = tempfile::tempdir().unwrap();
    let source = crafted_backup(&tmp).await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = Db::open(&paths).await.unwrap();
    settings::set(&db, "ai.codex_path", &json!("/opt/mine/codex"))
        .await
        .unwrap();
    let before = owned_values(&db).await;
    db.close().await;

    let db = backup::restore_offline(&paths, &source, "gui")
        .await
        .unwrap()
        .db;

    assert_eq!(owned_values(&db).await, before);
    db.close().await;
}

/// A backup whose values match the live ones changes nothing, so it audits nothing.
#[tokio::test]
async fn restore_with_the_same_program_paths_audits_no_kept_setting() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    settings::set(&db, "ai.claude_path", &json!("/opt/mine/claude"))
        .await
        .unwrap();
    let info = backup::create(&db, "cli").await.unwrap();

    let db = backup::restore(db, &info.path, "gui").await.unwrap().db;

    let entries = audit::recent(&db, 20).await.unwrap();
    assert_eq!(audit_actions(&entries, "settings_kept_by_restore"), 0);
    assert_eq!(
        settings::get(&db, "ai.claude_path").await.unwrap(),
        Some(json!("/opt/mine/claude"))
    );
    db.close().await;
}

/// Restored documents are data: nothing in them comes back executable.
#[cfg(unix)]
#[tokio::test]
async fn restored_documents_lose_their_execute_bits() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let source = crafted_backup(&tmp).await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = Db::open(&paths).await.unwrap();

    let db = backup::restore(db, &source, "gui").await.unwrap().db;

    let restored = paths.documents().join(CLAUDE_NAME);
    let mode = fs::metadata(&restored).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0, "mode {mode:o}");
    db.close().await;
}

/// The second wall: whatever a setting says, a program inside the data root never runs.
#[tokio::test]
async fn a_program_path_inside_the_data_root_is_refused() {
    use jobpilot_core::ai::{child, claude_code};
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let planted = plant_program(&paths.documents(), CLAUDE_NAME);
    let gemini = plant_program(&paths.documents().join("sub"), GEMINI_NAME);
    let path = json!(planted.display().to_string());
    settings::set(&db, claude_code::PATH_SETTING, &path)
        .await
        .unwrap();
    let path = json!(gemini.display().to_string());
    settings::set(&db, child::GEMINI.setting, &path)
        .await
        .unwrap();

    let claude = claude_code::find_binary_quietly(Some(&db)).await;
    let message = claude.unwrap_err();
    assert!(message.contains("data folder"), "{message}");
    let message = child::find_cli_quick(&child::GEMINI, Some(&db))
        .await
        .unwrap_err();
    assert!(message.contains("data folder"), "{message}");

    // The same file named through another path to the root: a macOS firmlink alias where
    // there is one, else a link to the root from outside it.
    let alias = aliased(&planted, &paths);
    settings::set(
        &db,
        claude_code::PATH_SETTING,
        &json!(alias.display().to_string()),
    )
    .await
    .unwrap();
    let message = claude_code::find_binary_quietly(Some(&db))
        .await
        .unwrap_err();
    assert!(message.contains("data folder"), "{alias:?}: {message}");

    // A link outside the root whose last component points at the planted program.
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        let link = outside.path().join(CLAUDE_NAME);
        std::os::unix::fs::symlink(&planted, &link).unwrap();
        settings::set(
            &db,
            claude_code::PATH_SETTING,
            &json!(link.display().to_string()),
        )
        .await
        .unwrap();
        let message = claude_code::find_binary_quietly(Some(&db))
            .await
            .unwrap_err();
        assert!(message.contains("data folder"), "{link:?}: {message}");
    }
    db.close().await;
}

/// `file` (inside the root) through a path that shares no prefix with the root's own.
fn aliased(file: &Path, paths: &AppPaths) -> PathBuf {
    let real = fs::canonicalize(file).unwrap();
    #[cfg(target_os = "macos")]
    {
        let firmlink = Path::new("/System/Volumes/Data").join(real.strip_prefix("/").unwrap());
        if firmlink.exists() {
            return firmlink;
        }
    }
    let root = fs::canonicalize(paths.root()).unwrap();
    let link = root.parent().unwrap().join("alias-of-root");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&root, &link).unwrap();
    link.join(real.strip_prefix(&root).unwrap())
}

// Review 6a I-1: a backup must hold exactly the schema JobPilot's migrations build, so no
// trigger or view of its own can run inside the restore or live on after it.

/// Runs `sql` against the backup's database file, as a crafted backup would hold it.
async fn tamper(backup: &Path, sql: &str) {
    let options = SqliteConnectOptions::new()
        .filename(backup.join("jobpilot.db"))
        .journal_mode(SqliteJournalMode::Delete);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

/// The live root: a program path of its own and the pause on. What the backup's objects try
/// to undo.
async fn guarded_root(paths: &AppPaths) -> Db {
    let db = Db::open(paths).await.unwrap();
    settings::set(&db, "ai.claude_path", &json!("/opt/mine/claude"))
        .await
        .unwrap();
    pause::set(&db, "gui").await.unwrap();
    db
}

async fn assert_refused_and_untouched(
    paths: &AppPaths,
    refused: Result<backup::RestoreReport, Error>,
) {
    let error = refused.expect_err("the restore is refused");
    assert!(
        matches!(&error, Error::InvalidBackup(why) if why.contains("schema differs")),
        "{error}"
    );
    let db = Db::open(paths).await.unwrap();
    assert!(pause::is_set(&db).await.unwrap());
    assert_eq!(
        settings::get(&db, "ai.claude_path").await.unwrap(),
        Some(json!("/opt/mine/claude"))
    );
    let foreign: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'evil%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(foreign, 0);
    db.close().await;
}

async fn tampered_backup(tmp: &TempDir, sql: &str) -> PathBuf {
    let source = crafted_backup(tmp).await;
    tamper(&source, sql).await;
    source
}

#[tokio::test]
async fn a_backup_with_an_extra_trigger_on_settings_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tampered_backup(
        &tmp,
        "CREATE TRIGGER evil_path AFTER INSERT ON settings WHEN NEW.key = 'ai.claude_path' \
         BEGIN UPDATE settings SET value = '\"/tmp/claude\"' WHERE key = NEW.key; END;",
    )
    .await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = guarded_root(&paths).await;

    let refused = backup::restore(db, &source, "gui").await;
    assert_refused_and_untouched(&paths, refused).await;
}

#[tokio::test]
async fn a_backup_with_an_extra_trigger_on_the_pause_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tampered_backup(
        &tmp,
        "CREATE TRIGGER evil_pause AFTER INSERT ON emergency_pause \
         BEGIN DELETE FROM emergency_pause; END;",
    )
    .await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = guarded_root(&paths).await;
    db.close().await;

    // The recovery screen's path is held to the same rule.
    let refused = backup::restore_offline(&paths, &source, "gui").await;
    assert_refused_and_untouched(&paths, refused).await;
}

#[tokio::test]
async fn a_backup_with_an_extra_view_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tampered_backup(&tmp, "CREATE VIEW evil_view AS SELECT * FROM settings;").await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = guarded_root(&paths).await;

    let refused = backup::restore(db, &source, "gui").await;
    assert_refused_and_untouched(&paths, refused).await;
}

/// A name in sqlx's own namespace is no pass: only its bookkeeping table is left out.
#[tokio::test]
async fn a_backup_with_a_trigger_named_like_sqlx_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tampered_backup(
        &tmp,
        "CREATE TRIGGER _sqlx_evil AFTER INSERT ON settings BEGIN SELECT 1; END;",
    )
    .await;
    let paths = AppPaths::new(tmp.path().join("live"));
    let db = guarded_root(&paths).await;

    let refused = backup::restore(db, &source, "gui").await;
    let error = refused.expect_err("the restore is refused");
    assert!(
        matches!(&error, Error::InvalidBackup(why) if why.contains("schema differs")),
        "{error}"
    );
}
