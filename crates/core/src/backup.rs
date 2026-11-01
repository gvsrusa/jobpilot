//! Backup and restore. A bug here destroys the user's data, so the restore order
//! below is fixed:
//!
//! 1. Validate the source. Live data is not touched.
//! 2. Write a `pre-restore` snapshot and read the live `secret_index`.
//! 3. Close every handle and take the exclusive lock.
//! 4. Copy the source to temporary names inside the root, so a full disk fails here.
//! 5. Delete the live `-wal` and `-shm`, then rename the copies over the live paths.
//! 6. Release the lock and reopen, which migrates an older backup forward.
//! 7. Put back the saved `secret_index` rows and write the audit row.
//!
//! A failure in steps 1 to 4 leaves live data unchanged. From step 5 on, the
//! `pre-restore` snapshot is what holds the old data.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::audit;
use crate::db::{
    Db, Manifest, app_schema_version, copy_dir, current_version, lock_exclusive, new_snapshot_dir,
    release, snapshot, utc_stamps,
};
use crate::error::{Error, Result};
use crate::paths::AppPaths;

const DB_FILE: &str = "jobpilot.db";
const DOCUMENTS: &str = "documents";
const TEMP_DB: &str = "restore-tmp.db";
const TEMP_DOCUMENTS: &str = "restore-tmp-documents";
const OLD_DOCUMENTS: &str = "restore-old-documents";

#[derive(Debug, Clone, Serialize)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub created_at: String,
    pub schema_version: Option<i64>,
}

/// A `secret_index` row: name and `created_at`. Never a secret value.
type SecretRow = (String, String);

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
/// run `Db::open` again; live data is unchanged unless the failure came at or after
/// step 5, when the `pre-restore` snapshot holds it.
pub async fn restore(db: Db, source: &Path, actor: &str) -> Result<Db> {
    let paths = db.paths().clone();
    let prepared = prepare(&db, source).await;
    db.close().await;
    let (manifest, secrets) = prepared?;

    let lock = lock_exclusive(&paths)?;
    let installed = install(&paths, source, manifest.wal).await;
    release(lock);
    installed?;

    reopen(&paths, source, actor, &secrets, false).await
}

/// `restore` for the recovery screen, where no `Db` could open. The lock comes before
/// step 2, so no writer can tear the database and its WAL apart while they are copied.
pub async fn restore_offline(paths: &AppPaths, source: &Path, actor: &str) -> Result<Db> {
    let manifest = validate(source).await?;

    fs::create_dir_all(paths.root())?;
    let lock = lock_exclusive(paths)?;
    let secrets = match snapshot_offline(paths).await {
        Ok(secrets) => secrets,
        Err(error) => {
            release(lock);
            return Err(error);
        }
    };
    let installed = install(paths, source, manifest.wal).await;
    release(lock);
    installed?;

    let lost = secrets.is_none();
    reopen(paths, source, actor, &secrets.unwrap_or_default(), lost).await
}

/// Steps 1 and 2, while the pool is still open.
async fn prepare(db: &Db, source: &Path) -> Result<(Manifest, Vec<SecretRow>)> {
    let manifest = validate(source).await?;
    snapshot(db.paths(), &db.pool, "pre-restore").await?;
    Ok((manifest, read_secrets(&db.pool).await?))
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

async fn validate(source: &Path) -> Result<Manifest> {
    let manifest = read_manifest(source)?;
    let app = app_schema_version();
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
    // The manifest is only a claim: a wrong version here would pass validation and
    // then lock the user out at step 6, after the swap.
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

/// Steps 4 and 5. The exclusive lock is held and no handle is open.
async fn install(paths: &AppPaths, source: &Path, wal: bool) -> Result<()> {
    let root = paths.clone();
    blocking(move || {
        clear_temp(&root)?;
        // Left by a crash in `swap_in`; that restore's snapshot holds these files.
        remove_dir_if_present(&root.root().join(OLD_DOCUMENTS))
    })
    .await?;

    let installed = stage_and_swap(paths, source, wal).await;
    if installed.is_err() {
        let _ = clear_temp(paths);
    }
    installed
}

async fn stage_and_swap(paths: &AppPaths, source: &Path, wal: bool) -> Result<()> {
    let temp = paths.root().join(TEMP_DB);
    let database = source.join(DB_FILE);

    // Only the named files move: a `-wal` or `-shm` beside the source is a stranger
    // unless the manifest claims it, and a `-shm` is rebuilt from the WAL either way.
    let (from, to) = (database.clone(), temp.clone());
    blocking(move || fs::copy(from, to)).await?;
    if wal {
        let (from, to) = (sidecar(&database, "-wal"), sidecar(&temp, "-wal"));
        blocking(move || fs::copy(from, to)).await?;
        merge_wal(&temp).await?;
    }
    // What goes live is this copy, so check this copy, the merge included.
    check_database(&temp, false).await?;

    let (root, from) = (paths.clone(), source.join(DOCUMENTS));
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

/// Step 5. A folder cannot be renamed over a folder, so the live one moves aside.
/// That comes first because it is the rename most likely to fail: Windows refuses
/// while any file inside is open, and failing there leaves everything as it was.
///
/// Live documents are only deleted once the new ones are in place.
// ponytail: not crash-safe across the renames. A crash here leaves the new database
// with old or missing documents; the `pre-restore` snapshot holds the old set.
fn swap_in(paths: &AppPaths) -> io::Result<()> {
    let root = paths.root();
    let live = paths.documents();
    let old = root.join(OLD_DOCUMENTS);
    let moved = match fs::rename(&live, &old) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    let db = paths.db();
    let swapped = remove_if_present(&sidecar(&db, "-wal"))
        .and_then(|()| remove_if_present(&sidecar(&db, "-shm")))
        .and_then(|()| fs::rename(root.join(TEMP_DB), &db))
        .and_then(|()| fs::rename(root.join(TEMP_DOCUMENTS), &live));

    match swapped {
        Ok(()) => {
            // Leftovers are harmless, and the next restore clears them.
            let _ = fs::remove_dir_all(&old);
            Ok(())
        }
        Err(error) => {
            if moved {
                let _ = fs::rename(&old, &live);
            }
            Err(error)
        }
    }
}

/// Steps 6 and 7. A failure closes the handle, as `restore` promises.
async fn reopen(
    paths: &AppPaths,
    source: &Path,
    actor: &str,
    secrets: &[SecretRow],
    secret_index_lost: bool,
) -> Result<Db> {
    let db = Db::open(paths).await?;
    let detail = if secret_index_lost {
        json!({"secret_index_lost": true})
    } else {
        json!({})
    };
    match finish(&db, source, actor, secrets, detail).await {
        Ok(()) => Ok(db),
        Err(error) => {
            db.close().await;
            Err(error)
        }
    }
}

/// Secret values are never in a backup and their keychain entries are untouched,
/// so the live names go back or `delete_all` could not find those entries.
async fn finish(
    db: &Db,
    source: &Path,
    actor: &str,
    secrets: &[SecretRow],
    detail: serde_json::Value,
) -> Result<()> {
    for (name, created_at) in secrets {
        sqlx::query("INSERT OR IGNORE INTO secret_index (name, created_at) VALUES (?, ?)")
            .bind(name)
            .bind(created_at)
            .execute(&db.pool)
            .await?;
    }
    let target = source.to_string_lossy();
    audit::record(db, actor, "restore", Some(&target), detail).await
}

fn clear_temp(paths: &AppPaths) -> io::Result<()> {
    let temp = paths.root().join(TEMP_DB);
    remove_if_present(&sidecar(&temp, "-wal"))?;
    remove_if_present(&sidecar(&temp, "-shm"))?;
    remove_if_present(&temp)?;
    remove_dir_if_present(&paths.root().join(TEMP_DOCUMENTS))
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

    #[test]
    fn failed_database_rename_puts_documents_back() {
        let (_tmp, paths) = staged_root();
        // A non-empty folder cannot be renamed over, on any OS.
        fs::remove_file(paths.db()).unwrap();
        fs::create_dir(paths.db()).unwrap();
        fs::write(paths.db().join("child"), "x").unwrap();

        swap_in(&paths).unwrap_err();

        assert!(paths.documents().join("live.txt").is_file());
        assert!(!paths.root().join(OLD_DOCUMENTS).exists());
    }

    #[test]
    fn failed_documents_rename_keeps_the_live_documents() {
        let (_tmp, paths) = staged_root();
        fs::remove_dir_all(paths.root().join(TEMP_DOCUMENTS)).unwrap();

        swap_in(&paths).unwrap_err();

        assert!(paths.documents().join("live.txt").is_file());
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
