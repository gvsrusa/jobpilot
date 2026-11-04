use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};

use crate::backup::{blocking, remove_if_present};
use crate::crypto::KeyState;
use crate::error::{Error, Result};
use crate::paths::{APP_ID, AppPaths};

pub static MIGRATOR: Migrator = sqlx::migrate!();

/// The schema version this build migrates to.
pub fn app_schema_version() -> i64 {
    latest_version(&MIGRATOR)
}

/// An open, migrated database. Holds a shared lock on `jobpilot.lock` for its
/// whole life: any number of handles may coexist, and backup restore or delete
/// takes the exclusive lock, so it fails with `InUse` while one is open.
///
/// Anything that needs the exclusive lock after this handle must call [`Db::close`],
/// not drop it: dropping unlocks at once while the SQLite workers exit asynchronously,
/// so the lock could be won with database files still open.
#[derive(Debug)]
pub struct Db {
    pub pool: SqlitePool,
    lock: File,
    paths: AppPaths,
    key_state: Mutex<KeyState>,
}

impl Db {
    pub async fn open(paths: &AppPaths) -> Result<Db> {
        let db = Self::open_with(paths, &MIGRATOR).await?;
        // A job the 0.5 migration carried over stays `none` until the rules have read it.
        crate::jobs::apply_rules_to_unread(&db).await;
        Ok(db)
    }

    /// `open` with a chosen migrator. Tests use this to supply a future or broken
    /// migration; `Db::open` is the only production caller.
    #[doc(hidden)]
    pub async fn open_with(paths: &AppPaths, migrator: &Migrator) -> Result<Db> {
        claim_root(paths)?;
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
                        key_state: Mutex::new(KeyState::NotLoaded),
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

    /// The state from the last time the key was asked for, or `NotLoaded`. Never touches
    /// the store, so it is safe anywhere. `Db::open` does not read the key:
    /// `crypto::state` and `crypto::unlocked` do.
    pub fn key_state(&self) -> KeyState {
        self.key_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// True when a restore stopped during its renames, so documents and the database may
    /// not match; its `pre-restore` snapshot holds what was there. A restore cannot be
    /// running while this handle holds its lock, so only a crash leaves the marker.
    pub fn swap_interrupted(&self) -> bool {
        self.paths.swap_marker().exists()
    }

    /// The user has seen the warning. Anything but a clear stays until a restore finishes.
    pub async fn clear_swap_warning(&self) -> Result<()> {
        let marker = self.paths.swap_marker();
        blocking(move || remove_if_present(&marker)).await
    }

    pub(crate) fn set_key_state(&self, state: KeyState) {
        *self
            .key_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = state;
    }
}

/// Proves `root` is JobPilot's before anything is created in it. `JOBPILOT_HOME` can
/// name any folder, and on a case-insensitive volume `<home>/documents` is the user's
/// `~/Documents`, so an unproven root would have its folders exported and deleted.
///
/// A new or empty root gets the marker first, ahead of every other file. The emptiness
/// check comes before the marker check, so a second process racing a first open sees
/// the marker with any file it saw, and is never refused.
pub(crate) fn claim_root(paths: &AppPaths) -> Result<()> {
    create_user_only(paths.root())?;
    let empty = fs::read_dir(paths.root())?.next().is_none();
    if paths.marker().is_file() {
        return Ok(());
    }
    if !empty {
        return Err(Error::RootNotOwned(paths.root().to_path_buf()));
    }
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths.marker());
    match created {
        Ok(mut marker) => Ok(marker.write_all(APP_ID.as_bytes())?),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The data root is user-only wherever it is (S-7, M-06): on Unix a root this creates is
/// 0700, and so is any missing parent it creates. An existing folder keeps its mode. On
/// Windows the user profile's ACL is what makes it user-only.
fn create_user_only(root: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root)
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
pub(crate) fn release(lock: File) {
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

pub(crate) fn latest_version(migrator: &Migrator) -> i64 {
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

pub(crate) async fn current_version(pool: &SqlitePool) -> Result<i64> {
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

/// `manifest.json`, the one file that makes a folder under `backups/` a snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: i64,
    /// `None` only when the database of an offline snapshot could not be read.
    pub schema_version: Option<i64>,
    pub created_at: String,
    /// True when a `-wal` beside the database is part of the snapshot. A WAL is not
    /// bound to its database, so any other `-wal` there is a stranger and is ignored.
    #[serde(default)]
    pub wal: bool,
}

impl Manifest {
    pub fn new(schema_version: Option<i64>, created_at: String, wal: bool) -> Self {
        Self {
            format: 1,
            schema_version,
            created_at,
            wal,
        }
    }

    pub fn write(&self, dir: &Path) -> Result<()> {
        let text = serde_json::to_string(self).map_err(io::Error::other)?;
        Ok(fs::write(dir.join("manifest.json"), text)?)
    }
}

/// The folder-name stamp and the `created_at` value. `now` is fixed for one
/// statement, so both name the same instant.
pub(crate) async fn utc_stamps(pool: &SqlitePool) -> Result<(String, String)> {
    Ok(sqlx::query_as(
        "SELECT strftime('%Y%m%dT%H%M%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .fetch_one(pool)
    .await?)
}

/// Writes `backups/<label>-<UTC stamp>/` holding the database, `documents/`, and
/// `manifest.json`. The manifest goes last, so a folder left by a crash or a full
/// disk has none and `backup::list` skips it.
// ponytail: no retention. Snapshots stay until the user removes them or delete-all
// runs with include_backups, so disk use grows with every migration and restore.
pub(crate) async fn snapshot(paths: &AppPaths, pool: &SqlitePool, label: &str) -> Result<PathBuf> {
    let (stamp, created_at) = utc_stamps(pool).await?;
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

    Manifest::new(Some(schema_version), created_at, false).write(&dir)?;
    Ok(dir)
}

/// `create_dir` fails on an existing folder, so a snapshot never lands in another.
pub(crate) fn new_snapshot_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(parent)?;
    let mut dir = parent.join(name);
    let mut attempt = 0;
    loop {
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt += 1;
                dir = parent.join(format!("{name}-{attempt}"));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Copies regular files and real folders, and skips the rest: a link can reach outside the
/// root, and opening a FIFO waits for ever. `from` itself is the caller's to choose.
// ponytail: each entry is checked by name and then opened, so a file swapped for a link
// between the two is followed. Only another process of this user can do that, and it can
// read these files anyway. Open with `O_NOFOLLOW` if documents ever come from elsewhere.
pub fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let target = to.join(entry.file_name());
        // Reads the entry and not what it points at, and a junction counts as a link.
        let kind = fs::symlink_metadata(entry.path())?.file_type();
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
            drop_execute_bits(&target)?;
        }
    }
    Ok(())
}

/// Documents are data. A copied file never keeps an execute bit, so a backup cannot bring a
/// program into the data root that is ready to run (S-1).
#[cfg(unix)]
fn drop_execute_bits(file: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(file)?.permissions().mode();
    if mode & 0o111 != 0 {
        fs::set_permissions(file, fs::Permissions::from_mode(mode & !0o111))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn drop_execute_bits(_file: &Path) -> io::Result<()> {
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

    // Every backup is a `VACUUM INTO`, and FTS5 external content finds rows by key
    // alone, so a key that moved would silently return the wrong document.
    #[tokio::test]
    async fn snapshot_search_index_matches_documents_after_gaps() {
        async fn insert(db: &Db, id: &str, title: &str) {
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
        async fn delete(db: &Db, id: &str) {
            sqlx::query("DELETE FROM documents WHERE id = ?")
                .bind(id)
                .execute(&db.pool)
                .await
                .unwrap();
        }
        async fn hit(pool: &SqlitePool, term: &str) -> Vec<String> {
            sqlx::query_scalar(
                "SELECT d.id FROM documents_fts f JOIN documents d ON d.seq = f.rowid \
                 WHERE documents_fts MATCH ?",
            )
            .bind(term)
            .fetch_all(pool)
            .await
            .unwrap()
        }

        let (_tmp, paths) = temp_root();
        let db = Db::open(&paths).await.unwrap();
        for (id, title) in [("a", "alpha"), ("b", "bravo"), ("c", "charlie")] {
            insert(&db, id, title).await;
        }
        // Leaves a gap at the low keys, then reuses the highest key, which is freed.
        delete(&db, "a").await;
        delete(&db, "c").await;
        insert(&db, "a", "delta").await;
        insert(&db, "e", "echo").await;
        delete(&db, "b").await;

        let dir = snapshot(&paths, &db.pool, "pre-test").await.unwrap();
        db.close().await;

        let pool =
            SqlitePool::connect_with(SqliteConnectOptions::new().filename(dir.join("jobpilot.db")))
                .await
                .unwrap();
        assert_eq!(hit(&pool, "delta").await, ["a"]);
        assert_eq!(hit(&pool, "echo").await, ["e"]);
        for gone in ["alpha", "bravo", "charlie"] {
            assert!(hit(&pool, gone).await.is_empty(), "{gone}");
        }
        // With rank 1, FTS5 also compares the index against the content table.
        sqlx::query(
            "INSERT INTO documents_fts (documents_fts, rank) VALUES ('integrity-check', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }
}
