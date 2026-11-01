//! Backup and restore. A bug here destroys the user's data, so the restore order
//! below is fixed:
//!
//! 1. Validate the source. Live data is not touched.
//! 2. Write a `pre-restore` snapshot and read the live `secret_index`.
//! 3. Close every handle and take the exclusive lock.
//! 4. Build the new database as a temporary copy inside the root: copy, merge a WAL
//!    the manifest claims, migrate, check the schema, put back the saved
//!    `secret_index` rows, write the audit row, check it again. Copy the documents.
//! 5. Rename the documents, then the database, over the live paths. Clearing the live
//!    `-wal` and `-shm` comes right before the database rename, the commit point.
//! 6. Release the lock and reopen.
//!
//! All database work happens in step 4 because a backup that cannot migrate must not
//! replace anything. Any failure before the database rename leaves live data unchanged.
//! Once that rename succeeds the restore has happened, and a failed reopen is
//! `Error::RestoredButNotOpened`, never an error that reads as "nothing changed".

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

use crate::audit;
use crate::db::{
    Db, MIGRATOR, Manifest, copy_dir, current_version, latest_version, lock_exclusive,
    new_snapshot_dir, release, snapshot, utc_stamps,
};
use crate::error::{Error, Result};
use crate::paths::AppPaths;

const DB_FILE: &str = "jobpilot.db";
const DOCUMENTS: &str = "documents";
const TEMP_DB: &str = "restore-tmp.db";
const TEMP_DOCUMENTS: &str = "restore-tmp-documents";
const OLD_DOCUMENTS: &str = "restore-old-documents";
const OLD_WAL: &str = "restore-old.db-wal";

#[derive(Debug, Clone, Serialize)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub created_at: String,
    pub schema_version: Option<i64>,
}

/// A `secret_index` row: name and `created_at`. Never a secret value.
type SecretRow = (String, String);

/// What step 4 needs to know to build the new database.
struct Plan<'a> {
    source: &'a Path,
    actor: &'a str,
    migrator: &'a Migrator,
    wal: bool,
    secrets: Vec<SecretRow>,
    secret_index_lost: bool,
}

pub async fn create(db: &Db, actor: &str) -> Result<BackupInfo> {
    let path = snapshot(db.paths(), &db.pool, "backup").await?;
    let manifest = read_manifest(&path)?;
    let target = path.to_string_lossy();
    audit::record(db, actor, "backup", Some(&target), json!({})).await?;
    Ok(BackupInfo {
        created_at: manifest.created_at,
        schema_version: manifest.schema_version,
        path,
    })
}

/// Newest first. A folder with no database or no readable manifest is not a snapshot:
/// `manifest.json` is not power-loss durable, so a torn one is skipped, not an error.
pub async fn list(paths: &AppPaths) -> Result<Vec<BackupInfo>> {
    let entries = match fs::read_dir(paths.backups()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.join(DB_FILE).is_file() {
            continue;
        }
        if let Ok(manifest) = read_manifest(&path) {
            found.push(BackupInfo {
                path,
                created_at: manifest.created_at,
                schema_version: manifest.schema_version,
            });
        }
    }
    found.sort_by(|a, b| (&b.created_at, &b.path).cmp(&(&a.created_at, &a.path)));
    Ok(found)
}

/// Replaces live data with `source`. On any error the handle is closed, so callers
/// run `Db::open` again. Live data is unchanged after every error except
/// `Error::RestoredButNotOpened`, which means live data is now the backup and only
/// the reopen failed.
pub async fn restore(db: Db, source: &Path, actor: &str) -> Result<Db> {
    restore_with(db, source, actor, &MIGRATOR).await
}

/// `restore` with a chosen migrator. Tests use this to restore an older backup or
/// fail its migration; `restore` is the only production caller.
#[doc(hidden)]
pub async fn restore_with(db: Db, source: &Path, actor: &str, migrator: &Migrator) -> Result<Db> {
    let paths = db.paths().clone();
    let prepared = prepare(&db, source, migrator).await;
    db.close().await;
    let (wal, secrets) = match prepared {
        Ok(prepared) => prepared,
        // `validate` raises no `Db` errors, so this is the live database failing. One
        // can open and still be corrupt, and then no recovery screen ever shows, so
        // this is the way out: the offline path copies files instead of reading rows.
        Err(Error::Db(_)) => return offline_with(&paths, source, actor, migrator).await,
        Err(error) => return Err(error),
    };

    let lock = lock_exclusive(&paths)?;
    let plan = Plan {
        source,
        actor,
        migrator,
        wal,
        secrets,
        secret_index_lost: false,
    };
    let installed = install(&paths, &plan).await;
    release(lock);
    installed?;
    reopen(&paths, migrator).await
}

/// `restore` for the recovery screen, where no `Db` could open. The lock comes before
/// step 2, so no writer can tear the database and its WAL apart while they are copied.
/// The same error contract as `restore`.
pub async fn restore_offline(paths: &AppPaths, source: &Path, actor: &str) -> Result<Db> {
    offline_with(paths, source, actor, &MIGRATOR).await
}

async fn offline_with(
    paths: &AppPaths,
    source: &Path,
    actor: &str,
    migrator: &Migrator,
) -> Result<Db> {
    let manifest = validate(source, migrator).await?;

    fs::create_dir_all(paths.root())?;
    let lock = lock_exclusive(paths)?;
    let installed = match snapshot_offline(paths).await {
        Ok(secrets) => {
            let plan = Plan {
                source,
                actor,
                migrator,
                wal: manifest.wal,
                secret_index_lost: secrets.is_none(),
                secrets: secrets.unwrap_or_default(),
            };
            install(paths, &plan).await
        }
        Err(error) => Err(error),
    };
    release(lock);
    installed?;
    reopen(paths, migrator).await
}

/// Steps 1 and 2, while the pool is still open. Returns the manifest's `wal` flag
/// and the live `secret_index`.
async fn prepare(db: &Db, source: &Path, migrator: &Migrator) -> Result<(bool, Vec<SecretRow>)> {
    let manifest = validate(source, migrator).await?;
    snapshot(db.paths(), &db.pool, "pre-restore").await?;
    Ok((manifest.wal, read_secrets(&db.pool).await?))
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let invalid = |error: String| Error::InvalidBackup(format!("manifest: {error}"));
    let bytes = fs::read(dir.join("manifest.json")).map_err(|e| invalid(e.to_string()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    if manifest.format != 1 {
        return Err(invalid(format!("unknown format {}", manifest.format)));
    }
    Ok(manifest)
}

async fn validate(source: &Path, migrator: &Migrator) -> Result<Manifest> {
    let manifest = read_manifest(source)?;
    let app = latest_version(migrator);
    if let Some(db) = manifest.schema_version.filter(|version| *version > app) {
        return Err(Error::SchemaTooNew { db, app });
    }
    // A restore replaces `documents/`, so a missing copy would silently empty it.
    if !source.join(DOCUMENTS).is_dir() {
        return Err(Error::InvalidBackup("no documents folder".into()));
    }
    let database = source.join(DB_FILE);
    if manifest.wal && !sidecar(&database, "-wal").is_file() {
        return Err(Error::InvalidBackup("its WAL is missing".into()));
    }
    // The manifest is only a claim, so the database's own version decides.
    let db = check_database(&database, manifest.wal).await?;
    if db > app {
        return Err(Error::SchemaTooNew { db, app });
    }
    Ok(manifest)
}

/// Returns the schema version of a healthy database, else `InvalidBackup`.
///
/// `immutable` reads the main file alone. Plain read-only would replay any `-wal`
/// beside it, so a stray one would decide what passes validation.
async fn check_database(file: &Path, with_wal: bool) -> Result<i64> {
    let options = SqliteConnectOptions::new()
        .filename(file)
        .read_only(true)
        .immutable(!with_wal);
    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(|error| Error::InvalidBackup(format!("database: {error}")))?;
    let checked = integrity_and_version(&pool).await;
    pool.close().await;
    checked.map_err(|error| match error {
        Error::Db(error) => Error::InvalidBackup(format!("database: {error}")),
        other => other,
    })
}

async fn integrity_and_version(pool: &SqlitePool) -> Result<i64> {
    let report: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(pool)
        .await?;
    if report != ["ok"] {
        return Err(Error::InvalidBackup(report.join("; ")));
    }
    // SQLite reads a zero-length file as a healthy database with no tables.
    match current_version(pool).await? {
        0 => Err(Error::InvalidBackup("no JobPilot schema".into())),
        version => Ok(version),
    }
}

async fn read_secrets(pool: &SqlitePool) -> Result<Vec<SecretRow>> {
    Ok(sqlx::query_as("SELECT name, created_at FROM secret_index")
        .fetch_all(pool)
        .await?)
}

/// Step 2 with no pool: copies the live files as they are, WAL included, because
/// the database may be unopenable. Returns the live `secret_index`, `None` if unread.
async fn snapshot_offline(paths: &AppPaths) -> Result<Option<Vec<SecretRow>>> {
    // Only for the clock: no live database can be trusted to answer.
    let memory = SqlitePool::connect_with(SqliteConnectOptions::new().in_memory(true)).await?;
    let stamps = utc_stamps(&memory).await;
    memory.close().await;
    let (stamp, created_at) = stamps?;

    let dir = new_snapshot_dir(&paths.backups(), &format!("pre-restore-{stamp}"))?;
    let (live, target) = (paths.clone(), dir.clone());
    let wal = blocking(move || copy_live_files(&live, &target)).await?;

    // After the copy: a read-only connection can leave an empty `-wal` behind.
    let (schema_version, secrets) = read_live(paths).await;
    Manifest::new(schema_version, created_at, wal).write(&dir)?;
    Ok(secrets)
}

/// Returns whether a WAL was copied.
fn copy_live_files(paths: &AppPaths, dir: &Path) -> io::Result<bool> {
    let live = paths.db();
    let target = dir.join(DB_FILE);
    copy_if_present(&live, &target)?;
    let wal = copy_if_present(&sidecar(&live, "-wal"), &sidecar(&target, "-wal"))?;
    copy_if_present(&sidecar(&live, "-shm"), &sidecar(&target, "-shm"))?;
    copy_dir(&paths.documents(), &dir.join(DOCUMENTS))?;
    Ok(wal)
}

fn copy_if_present(from: &Path, to: &Path) -> io::Result<bool> {
    match fs::copy(from, to) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// The schema version and `secret_index` of a database that may be unreadable.
/// Not immutable: rows committed to the live WAL count.
async fn read_live(paths: &AppPaths) -> (Option<i64>, Option<Vec<SecretRow>>) {
    let options = SqliteConnectOptions::new()
        .filename(paths.db())
        .read_only(true);
    let Ok(pool) = SqlitePool::connect_with(options).await else {
        return (None, None);
    };
    let version = current_version(&pool).await.ok();
    let secrets = read_secrets(&pool).await.ok();
    pool.close().await;
    (version, secrets)
}

/// Steps 4 and 5. The exclusive lock is held and no handle is open. An error means
/// the database rename did not happen, so live data is unchanged.
async fn install(paths: &AppPaths, plan: &Plan<'_>) -> Result<()> {
    let root = paths.clone();
    blocking(move || {
        clear_temp(&root)?;
        // Left by a crash in `swap_in`; that restore's snapshot holds these files.
        remove_if_present(&root.root().join(OLD_WAL))?;
        remove_dir_if_present(&root.root().join(OLD_DOCUMENTS))
    })
    .await?;

    let installed = stage_and_swap(paths, plan).await;
    if installed.is_err() {
        let _ = clear_temp(paths);
    }
    installed
}

async fn stage_and_swap(paths: &AppPaths, plan: &Plan<'_>) -> Result<()> {
    let temp = paths.root().join(TEMP_DB);
    let database = plan.source.join(DB_FILE);

    // Only the named files move: a `-wal` or `-shm` beside the source is a stranger
    // unless the manifest claims it, and a `-shm` is rebuilt from the WAL either way.
    let (from, to) = (database.clone(), temp.clone());
    blocking(move || fs::copy(from, to)).await?;
    if plan.wal {
        let (from, to) = (sidecar(&database, "-wal"), sidecar(&temp, "-wal"));
        blocking(move || fs::copy(from, to)).await?;
        merge_wal(&temp).await?;
    }
    build_new_database(&temp, plan).await?;

    let (root, from) = (paths.clone(), plan.source.join(DOCUMENTS));
    blocking(move || {
        OpenOptions::new()
            .write(true)
            .open(root.root().join(TEMP_DB))?
            .sync_all()?;
        copy_dir(&from, &root.root().join(TEMP_DOCUMENTS))?;
        swap_in(&root)
    })
    .await
}

/// SQLite does not bind a WAL to its database, so one must never reach the live
/// root. It is folded into the temporary copy here and goes no further.
async fn merge_wal(temp: &Path) -> Result<()> {
    let pool = SqlitePool::connect_with(SqliteConnectOptions::new().filename(temp)).await?;
    let merged: std::result::Result<(i64, i64, i64), sqlx::Error> =
        sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&pool)
            .await;
    pool.close().await;
    let (busy, _, _) = merged?;
    if busy != 0 {
        return Err(Error::InvalidBackup("its WAL did not merge".into()));
    }
    remove_if_present(&sidecar(temp, "-wal"))?;
    remove_if_present(&sidecar(temp, "-shm"))?;
    Ok(())
}

/// Everything that could fail on the backup's content, done while it is still a
/// temporary copy. Rollback journal, so the copy stays one file; `Db::open` turns
/// WAL back on. What goes live is this copy, so it is checked last.
async fn build_new_database(temp: &Path, plan: &Plan<'_>) -> Result<()> {
    let options = SqliteConnectOptions::new()
        .filename(temp)
        .journal_mode(SqliteJournalMode::Delete);
    let pool = SqlitePool::connect_with(options).await?;
    let built = migrate_and_fill(&pool, plan).await;
    pool.close().await;
    built?;
    check_database(temp, false).await?;
    Ok(())
}

/// No `pre-migrate` snapshot: this is a copy, and `pre-restore` holds the live data.
async fn migrate_and_fill(pool: &SqlitePool, plan: &Plan<'_>) -> Result<()> {
    run_migrations(pool, plan.migrator).await?;
    check_schema(pool, plan.migrator).await?;

    // Secret values are never in a backup and their keychain entries are untouched,
    // so the live names go back or `delete_all` could not find those entries.
    for (name, created_at) in &plan.secrets {
        sqlx::query("INSERT OR IGNORE INTO secret_index (name, created_at) VALUES (?, ?)")
            .bind(name)
            .bind(created_at)
            .execute(pool)
            .await?;
    }

    let target = plan.source.to_string_lossy();
    let detail = if plan.secret_index_lost {
        json!({"secret_index_lost": true})
    } else {
        json!({})
    };
    audit::record_to(pool, plan.actor, "restore", Some(&target), detail).await
}

async fn run_migrations(pool: &SqlitePool, migrator: &Migrator) -> Result<()> {
    migrator
        .run(pool)
        .await
        .map_err(|error| Error::MigrationFailed(error.to_string()))
}

/// A backup can claim a version without holding its tables, so it is compared with
/// what the migrations build from nothing.
async fn check_schema(pool: &SqlitePool, migrator: &Migrator) -> Result<()> {
    let empty = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().in_memory(true))
        .await?;
    let built = run_migrations(&empty, migrator).await;
    let expected = schema_of(&empty).await;
    empty.close().await;
    built?;

    let found = schema_of(pool).await?;
    match expected?.into_iter().find(|item| !found.contains(item)) {
        Some((name, _)) => Err(Error::InvalidBackup(format!("schema differs at {name}"))),
        None => Ok(()),
    }
}

async fn schema_of(pool: &SqlitePool) -> Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query_as("SELECT name, sql FROM sqlite_master")
        .fetch_all(pool)
        .await?)
}

/// Step 5. A folder cannot be renamed over a folder, so the live one moves aside.
/// That comes first because it is the rename most likely to fail: Windows refuses
/// while any file inside is open. The database goes last because its rename is the
/// commit point: every failure before it is undone here, so an error means live data
/// is as it was.
// ponytail: not crash-safe across the renames. A crash here can leave new documents
// beside the old database, or none in place; the `pre-restore` snapshot holds them.
fn swap_in(paths: &AppPaths) -> io::Result<()> {
    let root = paths.root();
    let (live, old, new) = (
        paths.documents(),
        root.join(OLD_DOCUMENTS),
        root.join(TEMP_DOCUMENTS),
    );

    let moved = rename_if_present(&live, &old)?;
    if let Err(error) = fs::rename(&new, &live) {
        if moved {
            let _ = fs::rename(&old, &live);
        }
        return Err(error);
    }
    if let Err(error) = swap_database(paths) {
        let _ = fs::rename(&live, &new);
        if moved {
            let _ = fs::rename(&old, &live);
        }
        return Err(error);
    }

    // The restore has happened. Leftovers are harmless, and the next one clears them.
    let _ = fs::remove_dir_all(&old);
    let _ = fs::remove_file(root.join(OLD_WAL));
    Ok(())
}

/// The live WAL moves aside instead of going at once: on the offline path it can hold
/// the only copy of committed rows, and a failed rename must give them back.
fn swap_database(paths: &AppPaths) -> io::Result<()> {
    let db = paths.db();
    let (wal, old_wal) = (sidecar(&db, "-wal"), paths.root().join(OLD_WAL));

    let moved = rename_if_present(&wal, &old_wal)?;
    let swapped = remove_if_present(&sidecar(&db, "-shm"))
        .and_then(|()| fs::rename(paths.root().join(TEMP_DB), &db));
    if swapped.is_err() && moved {
        let _ = fs::rename(&old_wal, &wal);
    }
    swapped
}

/// Step 6. The rename is done, so a failure here must not read as "nothing changed".
async fn reopen(paths: &AppPaths, migrator: &Migrator) -> Result<Db> {
    Db::open_with(paths, migrator)
        .await
        .map_err(|error| Error::RestoredButNotOpened(error.to_string()))
}

fn clear_temp(paths: &AppPaths) -> io::Result<()> {
    let temp = paths.root().join(TEMP_DB);
    remove_if_present(&sidecar(&temp, "-wal"))?;
    remove_if_present(&sidecar(&temp, "-shm"))?;
    remove_if_present(&sidecar(&temp, "-journal"))?;
    remove_if_present(&temp)?;
    remove_dir_if_present(&paths.root().join(TEMP_DOCUMENTS))
}

fn rename_if_present(from: &Path, to: &Path) -> io::Result<bool> {
    match fs::rename(from, to) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_if_present(file: &Path) -> io::Result<()> {
    match fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// `jobpilot.db` plus `-wal` or `-shm`.
fn sidecar(file: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(file);
    name.push(suffix);
    name.into()
}

async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    Ok(tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)??)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_root() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path());
        fs::create_dir(paths.documents()).unwrap();
        fs::write(paths.documents().join("live.txt"), "live").unwrap();
        fs::write(paths.db(), "live").unwrap();
        fs::write(paths.root().join(TEMP_DB), "new").unwrap();
        let temp_documents = paths.root().join(TEMP_DOCUMENTS);
        fs::create_dir(&temp_documents).unwrap();
        fs::write(temp_documents.join("new.txt"), "new").unwrap();
        (tmp, paths)
    }

    #[test]
    fn swap_replaces_database_and_documents_and_removes_wal() {
        let (_tmp, paths) = staged_root();
        fs::write(sidecar(&paths.db(), "-wal"), "stale").unwrap();
        fs::write(sidecar(&paths.db(), "-shm"), "stale").unwrap();

        swap_in(&paths).unwrap();

        assert_eq!(fs::read_to_string(paths.db()).unwrap(), "new");
        assert!(paths.documents().join("new.txt").is_file());
        assert!(!paths.documents().join("live.txt").exists());
        assert!(!sidecar(&paths.db(), "-wal").exists());
        assert!(!sidecar(&paths.db(), "-shm").exists());
        assert!(!paths.root().join(OLD_DOCUMENTS).exists());
    }

    #[test]
    fn swap_with_no_live_documents_folder() {
        let (_tmp, paths) = staged_root();
        fs::remove_dir_all(paths.documents()).unwrap();

        swap_in(&paths).unwrap();

        assert!(paths.documents().join("new.txt").is_file());
    }

    fn assert_live_unchanged(paths: &AppPaths) {
        assert!(paths.documents().join("live.txt").is_file());
        assert!(!paths.documents().join("new.txt").exists());
        assert!(!paths.root().join(OLD_DOCUMENTS).exists());
    }

    #[test]
    fn failed_database_rename_undoes_documents_and_wal() {
        let (_tmp, paths) = staged_root();
        // A non-empty folder cannot be renamed over, on any OS.
        fs::remove_file(paths.db()).unwrap();
        fs::create_dir(paths.db()).unwrap();
        fs::write(paths.db().join("child"), "x").unwrap();
        fs::write(sidecar(&paths.db(), "-wal"), "committed rows").unwrap();

        swap_in(&paths).unwrap_err();

        assert_live_unchanged(&paths);
        assert_eq!(
            fs::read_to_string(sidecar(&paths.db(), "-wal")).unwrap(),
            "committed rows"
        );
    }

    #[test]
    fn failed_documents_rename_keeps_the_live_database_and_documents() {
        let (_tmp, paths) = staged_root();
        fs::write(sidecar(&paths.db(), "-wal"), "committed rows").unwrap();
        fs::remove_dir_all(paths.root().join(TEMP_DOCUMENTS)).unwrap();

        swap_in(&paths).unwrap_err();

        assert_live_unchanged(&paths);
        assert_eq!(fs::read_to_string(paths.db()).unwrap(), "live");
        assert_eq!(
            fs::read_to_string(paths.root().join(TEMP_DB)).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(sidecar(&paths.db(), "-wal")).unwrap(),
            "committed rows"
        );
    }

    #[test]
    fn manifest_without_wal_field_reads_as_false() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("manifest.json"),
            r#"{"format": 1, "schema_version": 1, "created_at": "t"}"#,
        )
        .unwrap();

        assert!(!read_manifest(tmp.path()).unwrap().wal);
    }
}
