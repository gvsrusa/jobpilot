use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};

use crate::error::{Error, Result};
use crate::paths::AppPaths;

pub static MIGRATOR: Migrator = sqlx::migrate!();

/// The schema version this build migrates to.
pub fn app_schema_version() -> i64 {
    latest_version(&MIGRATOR)
}

/// An open, migrated database. Holds a shared lock on `jobpilot.lock` for its
/// whole life: any number of handles may coexist, and backup restore or delete
/// takes the exclusive lock, so it fails with `InUse` while one is open.
#[derive(Debug)]
pub struct Db {
    pub pool: SqlitePool,
    lock: File,
    paths: AppPaths,
}

impl Db {
    pub async fn open(paths: &AppPaths) -> Result<Db> {
        Self::open_with(paths, &MIGRATOR).await
    }

    /// `open` with a chosen migrator. Tests use this to supply a future or broken
    /// migration; `Db::open` is the only production caller.
    #[doc(hidden)]
    pub async fn open_with(paths: &AppPaths, migrator: &Migrator) -> Result<Db> {
        for dir in [paths.documents(), paths.backups(), paths.exports()] {
            fs::create_dir_all(dir)?;
        }

        // A pool is never open without a lock held. File locks cannot be upgraded
        // portably, so moving between shared and exclusive leaves a gap; the pool
        // closes first, so a restore landing in that gap has no live handle to
        // tear, and this open fails with `InUse` instead. Ends after one migration
        // because the next pass finds nothing pending.
        loop {
            let lock = lock_shared(paths)?;
            let pool = connect(paths).await?;
            match pending_migration(&pool, migrator).await {
                Ok(None) => {
                    return Ok(Db {
                        pool,
                        lock,
                        paths: paths.clone(),
                    });
                }
                Ok(Some(_)) => {
                    pool.close().await;
                    release(lock);
                }
                Err(error) => {
                    pool.close().await;
                    release(lock);
                    return Err(error);
                }
            }

            let exclusive = lock_exclusive(paths)?;
            let pool = connect(paths).await?;
            let migrated = migrate(paths, &pool, migrator).await;
            pool.close().await;
            release(exclusive);
            migrated?;
        }
    }

    /// Closes the pool, then releases the file lock. Returns once SQLite has let go
    /// of its files, so a caller may rename or delete them, including on Windows.
    pub async fn close(self) {
        self.pool.close().await;
        release(self.lock);
    }

    pub async fn schema_version(&self) -> Result<i64> {
        current_version(&self.pool).await
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }
}

/// Takes the lock only if no other handle exists. `InUse` means one does.
pub(crate) fn lock_exclusive(paths: &AppPaths) -> Result<File> {
    let file = lock_file(paths)?;
    lock_result(file.try_lock())?;
    Ok(file)
}

fn lock_shared(paths: &AppPaths) -> Result<File> {
    let file = lock_file(paths)?;
    lock_result(file.try_lock_shared())?;
    Ok(file)
}

fn lock_file(paths: &AppPaths) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(paths.lock())?)
}

fn lock_result(result: std::result::Result<(), TryLockError>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(Error::InUse),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Unlocks before closing: Windows only promises that closing a locked handle
/// frees the lock eventually. A failed unlock still gets the OS release on drop.
fn release(lock: File) {
    let _ = lock.unlock();
}

// `.filename` takes the path as is. Building a `sqlite://` URL would need percent
// encoding for spaces and non-ASCII folders such as `C:\Users\José Ñ\`.
async fn connect(paths: &AppPaths) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(paths.db())
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);
    Ok(SqlitePool::connect_with(options).await?)
}

fn latest_version(migrator: &Migrator) -> i64 {
    migrator.iter().map(|m| m.version).max().unwrap_or(0)
}

async fn applied_versions(pool: &SqlitePool) -> Result<Vec<i64>> {
    let tracked: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if tracked.is_none() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await?)
}

async fn current_version(pool: &SqlitePool) -> Result<i64> {
    let applied = applied_versions(pool).await?;
    Ok(applied.into_iter().max().unwrap_or(0))
}

/// The version to migrate from when migrations are pending, else `None`.
/// Refuses a database newer than `migrator`, which `Db::open` makes the app's own.
async fn pending_migration(pool: &SqlitePool, migrator: &Migrator) -> Result<Option<i64>> {
    let applied = applied_versions(pool).await?;
    let from = applied.iter().copied().max().unwrap_or(0);
    let app = latest_version(migrator);
    if from > app {
        return Err(Error::SchemaTooNew { db: from, app });
    }
    let pending = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .any(|m| !applied.contains(&m.version));
    Ok(pending.then_some(from))
}

async fn migrate(paths: &AppPaths, pool: &SqlitePool, migrator: &Migrator) -> Result<()> {
    // Checked again: another process may have migrated while no lock was held.
    let Some(from) = pending_migration(pool, migrator).await? else {
        return Ok(());
    };
    if from > 0 {
        snapshot(paths, pool, &format!("pre-migrate-{from}")).await?;
    }
    // Each migration runs in its own transaction, so a failure leaves earlier ones
    // applied and the failing one rolled back.
    migrator
        .run(pool)
        .await
        .map_err(|error| Error::MigrationFailed(error.to_string()))
}

/// Writes `backups/<label>-<UTC stamp>/` holding the database, `documents/`, and
/// `manifest.json`. The manifest goes last, so a folder left by a crash or a full
/// disk has none and `backup::list` skips it.
// ponytail: no retention. Snapshots stay until the user removes them or delete-all
// runs with include_backups, so disk use grows with every migration and restore.
pub(crate) async fn snapshot(paths: &AppPaths, pool: &SqlitePool, label: &str) -> Result<PathBuf> {
    // `now` is fixed for one statement, so both values name the same instant.
    let (stamp, created_at): (String, String) = sqlx::query_as(
        "SELECT strftime('%Y%m%dT%H%M%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .fetch_one(pool)
    .await?;
    let schema_version = current_version(pool).await?;

    let dir = new_snapshot_dir(&paths.backups(), &format!("{label}-{stamp}"))?;

    let database = dir.join("jobpilot.db");
    let database = database
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    sqlx::query("VACUUM INTO ?")
        .bind(database)
        .execute(pool)
        .await?;

    let documents = (paths.documents(), dir.join("documents"));
    tokio::task::spawn_blocking(move || copy_dir(&documents.0, &documents.1))
        .await
        .map_err(io::Error::other)??;

    let manifest = serde_json::json!({
        "format": 1,
        "schema_version": schema_version,
        "created_at": created_at,
    });
    fs::write(dir.join("manifest.json"), manifest.to_string())?;
    Ok(dir)
}

/// `create_dir` fails on an existing folder, so a snapshot never lands in another.
fn new_snapshot_dir(backups: &Path, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(backups)?;
    let mut dir = backups.join(name);
    let mut attempt = 0;
    loop {
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt += 1;
                dir = backups.join(format!("{name}-{attempt}"));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        (tmp, paths)
    }

    #[tokio::test]
    async fn two_handles_share_a_root_but_block_exclusive() {
        let (_tmp, paths) = temp_root();
        let first = Db::open(&paths).await.unwrap();
        let second = Db::open(&paths).await.unwrap();

        assert!(matches!(lock_exclusive(&paths), Err(Error::InUse)));
        first.close().await;
        assert!(matches!(lock_exclusive(&paths), Err(Error::InUse)));
        second.close().await;
    }

    #[tokio::test]
    async fn exclusive_lock_refuses_open() {
        let (_tmp, paths) = temp_root();
        Db::open(&paths).await.unwrap().close().await;

        let held = lock_exclusive(&paths).unwrap();
        assert!(matches!(Db::open(&paths).await, Err(Error::InUse)));
        drop(held);
    }

    #[tokio::test]
    async fn close_releases_the_lock_and_files() {
        let (_tmp, paths) = temp_root();
        let db = Db::open(&paths).await.unwrap();
        db.close().await;

        let held = lock_exclusive(&paths).unwrap();
        drop(held);
        // Windows refuses this while any handle is open, so it proves the pool closed.
        std::fs::remove_file(paths.db()).unwrap();
        std::fs::remove_file(paths.lock()).unwrap();
    }

    #[tokio::test]
    async fn pending_migration_with_another_handle_is_in_use() {
        let (_tmp, paths) = temp_root();
        let other = Db::open(&paths).await.unwrap();
        let mut all: Vec<_> = MIGRATOR.iter().cloned().collect();
        all.push(sqlx::migrate::Migration::new(
            9000,
            "test".into(),
            sqlx::migrate::MigrationType::Simple,
            sqlx::SqlSafeStr::into_sql_str("CREATE TABLE t (x);"),
            false,
        ));
        let migrator = Migrator::with_migrations(all);

        assert!(matches!(
            Db::open_with(&paths, &migrator).await,
            Err(Error::InUse)
        ));
        assert_eq!(
            other.schema_version().await.unwrap(),
            app_schema_version(),
            "the live handle keeps working, nothing migrated"
        );
        other.close().await;
    }

    #[tokio::test]
    async fn snapshots_never_overwrite_and_stamp_with_suffix() {
        let (_tmp, paths) = temp_root();
        let db = Db::open(&paths).await.unwrap();

        let first = snapshot(&paths, &db.pool, "pre-test").await.unwrap();
        let second = snapshot(&paths, &db.pool, "pre-test").await.unwrap();
        let third = snapshot(&paths, &db.pool, "pre-test").await.unwrap();
        db.close().await;

        assert_ne!(first, second);
        assert_ne!(second, third);
        for dir in [first, second, third] {
            assert!(dir.join("jobpilot.db").is_file());
            assert!(dir.join("documents").is_dir());
            assert!(dir.join("manifest.json").is_file());
        }
    }

    // FTS5 external content points at documents.rowid, and `documents` has no
    // INTEGER PRIMARY KEY, so SQLite does not promise VACUUM keeps those rowids.
    // This pins the behaviour backups depend on across SQLite upgrades.
    #[tokio::test]
    async fn snapshot_search_index_still_matches_documents() {
        let (_tmp, paths) = temp_root();
        let db = Db::open(&paths).await.unwrap();
        for (id, title) in [("a", "alpha"), ("b", "bravo"), ("c", "charlie")] {
            sqlx::query(
                "INSERT INTO documents (id, title, body, created_at, updated_at) \
                 VALUES (?, ?, 'body', 't', 't')",
            )
            .bind(id)
            .bind(title)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        sqlx::query("DELETE FROM documents WHERE id = 'a'")
            .execute(&db.pool)
            .await
            .unwrap();

        let dir = snapshot(&paths, &db.pool, "pre-test").await.unwrap();
        db.close().await;

        let pool =
            SqlitePool::connect_with(SqliteConnectOptions::new().filename(dir.join("jobpilot.db")))
                .await
                .unwrap();
        let id: String = sqlx::query_scalar(
            "SELECT d.id FROM documents_fts f JOIN documents d ON d.rowid = f.rowid \
             WHERE documents_fts MATCH 'charlie'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(id, "c");
        pool.close().await;
    }
}
