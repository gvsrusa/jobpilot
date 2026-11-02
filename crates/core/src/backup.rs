//! Backup and restore. A bug here destroys the user's data, so the restore order
//! below is fixed:
//!
//! 1. Validate the source and ask the store about its key. Live data is not touched.
//! 2. Write a `pre-restore` snapshot and read the live `secret_index` and `data_keys`.
//! 3. Close every handle and take the exclusive lock.
//! 4. Build the new database as a temporary copy inside the root: copy, merge a WAL
//!    the manifest claims, migrate, check the schema, put back the saved
//!    `secret_index` and `data_keys` rows, revoke external consent, write the audit rows,
//!    check it again. Copy the documents.
//! 5. Under `.jobpilot-swap`, rename the documents, then the database, over the live
//!    paths. Clearing the live `-wal` and `-shm` comes right before the database rename,
//!    the commit point.
//! 6. Release the lock and reopen.
//!
//! All database work happens in step 4 because a backup that cannot migrate must not
//! replace anything. Any failure before the database rename leaves live data unchanged.
//! Once that rename succeeds the restore has happened, and a failed reopen is
//! `Error::RestoredButNotOpened`, never an error that reads as "nothing changed".
//!
//! A restore removes no keychain entry, so the `pre-restore` snapshot stays readable,
//! and the outgoing key rows stay in the new database, so `delete_all` can find them.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

use crate::ai::api;
use crate::ai::register;
use crate::crypto::{self, KeyState};
use crate::db::{
    Db, MIGRATOR, Manifest, claim_root, copy_dir, current_version, latest_version, lock_exclusive,
    new_snapshot_dir, release, snapshot, utc_stamps,
};
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::pause;
use crate::{audit, secrets};

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
    /// From the backup's own `data_keys`, the key it is under first. Empty for a backup
    /// with no such table, or a database that cannot be read.
    pub key_ids: Vec<String>,
}

/// A finished restore. `backup_key_state` is the state the backup's key had on this
/// machine before live data was touched, and `None` for a backup with no key row, as
/// from 0.1. With rows, it is the active key's, else the newest key's.
#[derive(Debug)]
pub struct RestoreReport {
    pub db: Db,
    pub backup_key_state: Option<KeyState>,
}

/// A `secret_index` row: name and `created_at`. Never a secret value.
type SecretRow = (String, String);

/// A `data_keys` row: id, `created_at`, key-check. Never a key. Its active flag stays
/// behind, because a live row that goes back is no longer the key in use.
type KeyRow = (String, String, Vec<u8>);

/// What the live database knows that a backup may not. `None` means it could not be read.
struct LiveRows {
    secrets: Option<Vec<SecretRow>>,
    keys: Option<Vec<KeyRow>>,
    /// Whether the emergency pause was on. A restore is never the way out of it.
    paused: Option<bool>,
    /// The endpoint address, to tell whether the backup swaps it for another.
    endpoint: Option<String>,
}

/// What step 1 found in the source.
struct Validated {
    wal: bool,
    backup_key_state: Option<KeyState>,
}

/// What step 4 needs to know to build the new database.
struct Plan<'a> {
    source: &'a Path,
    actor: &'a str,
    migrator: &'a Migrator,
    wal: bool,
    live: LiveRows,
}

pub async fn create(db: &Db, actor: &str) -> Result<BackupInfo> {
    let path = snapshot(db.paths(), &db.pool, "backup").await?;
    let manifest = read_manifest(&path)?;
    let target = path.to_string_lossy();
    audit::record(db, actor, "backup", Some(&target), json!({})).await?;
    Ok(BackupInfo {
        created_at: manifest.created_at,
        schema_version: manifest.schema_version,
        key_ids: key_ids_in(&path, manifest.wal).await,
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
        if !is_plain_file(&path.join(DB_FILE)) {
            continue;
        }
        if let Ok(manifest) = read_manifest(&path) {
            found.push(BackupInfo {
                key_ids: key_ids_in(&path, manifest.wal).await,
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
/// the reopen failed. That error's text carries what the report would have said about
/// the backup's key.
pub async fn restore(db: Db, source: &Path, actor: &str) -> Result<RestoreReport> {
    restore_with(db, source, actor, &MIGRATOR).await
}

/// `restore` with a chosen migrator. Tests use this to restore an older backup or
/// fail its migration; `restore` is the only production caller.
#[doc(hidden)]
pub async fn restore_with(
    db: Db,
    source: &Path,
    actor: &str,
    migrator: &Migrator,
) -> Result<RestoreReport> {
    let paths = db.paths().clone();
    // ponytail: a refused attempt (`InUse`, from the lock below) has already written a
    // `pre-restore` snapshot. An early `lock_exclusive` probe cannot work: this
    // process holds a shared lock through `db`, so it would always report `InUse`,
    // and dropping that lock first opens the gap `Db::open_with` describes.
    let prepared = prepare(&db, source, migrator).await;
    db.close().await;
    let (validated, live) = match prepared {
        Ok(prepared) => prepared,
        // `validate` raises no `Db` errors, so this is the live database failing. One
        // can open and still be corrupt, and then no recovery screen ever shows, so
        // this is the way out: the offline path copies files instead of reading rows.
        Err(Error::Db(_)) => return offline_with(&paths, source, actor, migrator).await,
        Err(error) => return Err(error),
    };

    let lock = lock_exclusive(&paths)?;
    // A `pause set` from another process can land between the read in `prepare` and the lock.
    // No handle is open now and nothing can write, so one more read closes that gap.
    let mut live = live;
    live.paused = merge_pause(live.paused, read_live(&paths).await.1.paused);
    let endpoint = live.endpoint.clone();
    let plan = Plan {
        source,
        actor,
        migrator,
        wal: validated.wal,
        live,
    };
    let installed = install(&paths, &plan).await;
    release(lock);
    installed?;
    reopen(&paths, migrator, validated.backup_key_state, endpoint).await
}

/// `restore` for the recovery screen, where no `Db` could open. The lock comes before
/// step 2, so no writer can tear the database and its WAL apart while they are copied.
/// The same error contract as `restore`.
pub async fn restore_offline(
    paths: &AppPaths,
    source: &Path,
    actor: &str,
) -> Result<RestoreReport> {
    offline_with(paths, source, actor, &MIGRATOR).await
}

async fn offline_with(
    paths: &AppPaths,
    source: &Path,
    actor: &str,
    migrator: &Migrator,
) -> Result<RestoreReport> {
    let validated = validate(source, migrator).await?;

    claim_root(paths)?;
    let lock = lock_exclusive(paths)?;
    let mut endpoint = None;
    let installed = match snapshot_offline(paths).await {
        Ok(live) => {
            endpoint = live.endpoint.clone();
            let plan = Plan {
                source,
                actor,
                migrator,
                wal: validated.wal,
                live,
            };
            install(paths, &plan).await
        }
        Err(error) => Err(error),
    };
    release(lock);
    installed?;
    reopen(paths, migrator, validated.backup_key_state, endpoint).await
}

/// Steps 1 and 2, while the pool is still open.
// ponytail: live rows are read before the lock is won, because no pool is open after it.
// A key or secret another process adds and closes inside that gap loses its row, and
// with it the way `delete_all` finds its entry.
async fn prepare(db: &Db, source: &Path, migrator: &Migrator) -> Result<(Validated, LiveRows)> {
    let validated = validate(source, migrator).await?;
    snapshot(db.paths(), &db.pool, "pre-restore").await?;
    let live = LiveRows {
        secrets: Some(read_secrets(&db.pool).await?),
        keys: Some(read_keys(&db.pool).await?),
        paused: Some(pause::is_set(db).await?),
        endpoint: read_endpoint(&db.pool).await,
    };
    Ok((validated, live))
}

/// A backup is untrusted, so this looks before anything opens the path: opening a FIFO
/// waits for ever, on the lock the desktop holds for a restore, and a link reads whatever
/// it points at.
// ponytail: looks by name and then opens, so a file swapped in between gets through. Only
// another process of this user can do that, and it can already write the live data.
fn is_plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|found| found.is_file())
}

/// What SQLite opens itself beside a database that is not immutable: each is absent or a
/// plain file. A read-only connection can leave an empty `-shm`, so one may be there.
fn sidecars_are_plain(database: &Path) -> bool {
    ["-wal", "-shm", "-journal"].iter().all(|suffix| {
        let beside = sidecar(database, suffix);
        fs::symlink_metadata(&beside).is_err() || is_plain_file(&beside)
    })
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let invalid = |error: String| Error::InvalidBackup(format!("manifest: {error}"));
    let file = dir.join("manifest.json");
    if !is_plain_file(&file) {
        return Err(invalid("no regular file".into()));
    }
    let bytes = fs::read(file).map_err(|e| invalid(e.to_string()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    if manifest.format != 1 {
        return Err(invalid(format!("unknown format {}", manifest.format)));
    }
    Ok(manifest)
}

async fn validate(source: &Path, migrator: &Migrator) -> Result<Validated> {
    let manifest = read_manifest(source)?;
    let app = latest_version(migrator);
    if let Some(db) = manifest.schema_version.filter(|version| *version > app) {
        return Err(Error::SchemaTooNew { db, app });
    }
    // A restore replaces `documents/`, so a missing copy would silently empty it. A link
    // is no copy, and `copy_dir` would pull in whatever it points at.
    let documents = fs::symlink_metadata(source.join(DOCUMENTS));
    if !documents.is_ok_and(|found| found.is_dir()) {
        return Err(Error::InvalidBackup("no documents folder".into()));
    }
    let database = source.join(DB_FILE);
    if !is_plain_file(&database) {
        return Err(Error::InvalidBackup("no regular database file".into()));
    }
    if manifest.wal && !is_plain_file(&sidecar(&database, "-wal")) {
        return Err(Error::InvalidBackup("its WAL is missing".into()));
    }
    if manifest.wal && !sidecars_are_plain(&database) {
        return Err(Error::InvalidBackup(
            "a file beside its database is not a regular file".into(),
        ));
    }
    // The manifest is only a claim, so the database's own version decides.
    let (db, keys) = check_database(&database, manifest.wal).await?;
    if db > app {
        return Err(Error::SchemaTooNew { db, app });
    }
    // Last, so a backup that is refused anyway raises no keychain prompt.
    let backup_key_state = match keys.into_iter().next() {
        Some((key_id, _, key_check)) => Some(key_state_here(key_id, key_check).await),
        None => None,
    };
    Ok(Validated {
        wal: manifest.wal,
        backup_key_state,
    })
}

/// Only a report, so a keychain call that dies is an answer, never a failed restore.
async fn key_state_here(key_id: String, key_check: Vec<u8>) -> KeyState {
    let lookup = key_id.clone();
    let asked = secrets::keychain(move || Ok(crypto::key_state_of(&lookup, &key_check))).await;
    asked.unwrap_or_else(|error| KeyState::KeyUnavailable {
        key_id,
        reason: error.to_string(),
    })
}

/// `immutable` reads the main file alone. Plain read-only would replay any `-wal`
/// beside it, so a stray one would decide what passes validation.
fn read_only(file: &Path, with_wal: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(file)
        .read_only(true)
        .immutable(!with_wal)
}

/// Returns the schema version and key rows of a healthy database, else `InvalidBackup`.
async fn check_database(file: &Path, with_wal: bool) -> Result<(i64, Vec<KeyRow>)> {
    let pool = SqlitePool::connect_with(read_only(file, with_wal))
        .await
        .map_err(|error| Error::InvalidBackup(format!("database: {error}")))?;
    let checked = match integrity_and_version(&pool).await {
        Ok(version) => read_keys(&pool).await.map(|keys| (version, keys)),
        Err(error) => Err(error),
    };
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

/// `None` when absent or unreadable: unknown compares as changed against any restored address.
async fn read_endpoint(pool: &SqlitePool) -> Option<String> {
    let raw: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'ai.endpoint.url'")
            .fetch_one(pool)
            .await
            .ok()?;
    serde_json::from_str(&raw).ok()
}

async fn read_secrets(pool: &SqlitePool) -> Result<Vec<SecretRow>> {
    Ok(sqlx::query_as("SELECT name, created_at FROM secret_index")
        .fetch_all(pool)
        .await?)
}

/// The key in use comes first: the active row, else the newest. A database from before
/// `data_keys` holds none, which is valid.
async fn read_keys(pool: &SqlitePool) -> Result<Vec<KeyRow>> {
    let table: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'data_keys'",
    )
    .fetch_optional(pool)
    .await?;
    if table.is_none() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(
        "SELECT key_id, created_at, key_check FROM data_keys \
         ORDER BY active = 1 DESC, created_at DESC, key_id",
    )
    .fetch_all(pool)
    .await?)
}

/// A folder stays listed whatever its database holds: a snapshot of a broken database is
/// what the recovery screen must still offer. So one that cannot be read has no ids.
async fn key_ids_in(dir: &Path, with_wal: bool) -> Vec<String> {
    let database = dir.join(DB_FILE);
    if with_wal && !sidecars_are_plain(&database) {
        return Vec::new();
    }
    let Ok(pool) = SqlitePool::connect_with(read_only(&database, with_wal)).await else {
        return Vec::new();
    };
    let keys = read_keys(&pool).await.unwrap_or_default();
    pool.close().await;
    keys.into_iter().map(|(key_id, ..)| key_id).collect()
}

/// Step 2 with no pool: copies the live files as they are, WAL included, because
/// the database may be unopenable.
async fn snapshot_offline(paths: &AppPaths) -> Result<LiveRows> {
    // Only for the clock: no live database can be trusted to answer.
    let memory = SqlitePool::connect_with(SqliteConnectOptions::new().in_memory(true)).await?;
    let stamps = utc_stamps(&memory).await;
    memory.close().await;
    let (stamp, created_at) = stamps?;

    let dir = new_snapshot_dir(&paths.backups(), &format!("pre-restore-{stamp}"))?;
    let (live, target) = (paths.clone(), dir.clone());
    let wal = blocking(move || copy_live_files(&live, &target)).await?;

    // After the copy: a read-only connection can leave an empty `-wal` behind.
    let (schema_version, live) = read_live(paths).await;
    Manifest::new(schema_version, created_at, wal).write(&dir)?;
    Ok(live)
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

/// The schema version and rows of a database that may be unreadable.
/// Not immutable: rows committed to the live WAL count.
async fn read_live(paths: &AppPaths) -> (Option<i64>, LiveRows) {
    let options = SqliteConnectOptions::new()
        .filename(paths.db())
        .read_only(true);
    let Ok(pool) = SqlitePool::connect_with(options).await else {
        // No file is no live data, and so no pause. A file that will not open is unknown.
        let paused = (!paths.db().exists()).then_some(false);
        let live = LiveRows {
            secrets: None,
            keys: None,
            paused,
            endpoint: None,
        };
        return (None, live);
    };
    let version = current_version(&pool).await.ok();
    let live = LiveRows {
        secrets: read_secrets(&pool).await.ok(),
        keys: read_keys(&pool).await.ok(),
        paused: read_paused(&pool).await.ok(),
        endpoint: read_endpoint(&pool).await,
    };
    pool.close().await;
    (version, live)
}

/// Steps 4 and 5. The exclusive lock is held and no handle is open. An error means
/// the database rename did not happen, so live data is unchanged.
async fn install(paths: &AppPaths, plan: &Plan<'_>) -> Result<()> {
    let root = paths.clone();
    blocking(move || remove_restore_leftovers(&root)).await?;

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
        swap_marked(&root)
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
    for (name, created_at) in plan.live.secrets.iter().flatten() {
        sqlx::query("INSERT OR IGNORE INTO secret_index (name, created_at) VALUES (?, ?)")
            .bind(name)
            .bind(created_at)
            .execute(pool)
            .await?;
    }

    // The same goes for data keys, and a row the backup has keeps its own active flag.
    for (key_id, created_at, key_check) in plan.live.keys.iter().flatten() {
        sqlx::query(
            "INSERT OR IGNORE INTO data_keys (key_id, created_at, key_check, active) \
             VALUES (?, ?, ?, 0)",
        )
        .bind(key_id)
        .bind(created_at)
        .bind(key_check)
        .execute(pool)
        .await?;
    }

    revoke_external_consent(pool, plan.actor).await?;
    keep_pause(pool, plan).await?;

    let target = plan.source.to_string_lossy();
    let mut detail = json!({});
    if plan.live.secrets.is_none() {
        detail["secret_index_lost"] = json!(true);
    }
    if plan.live.keys.is_none() {
        detail["data_keys_lost"] = json!(true);
    }
    audit::record_to(pool, plan.actor, "restore", Some(&target), detail).await
}

/// A pause that was on stays on, whatever the backup says. The backup's own row, if it has
/// one, is kept as it is. Only a person clears it, so a restore cannot be the way out.
async fn keep_pause(pool: &SqlitePool, plan: &Plan<'_>) -> Result<()> {
    let unknown = plan.live.paused.is_none();
    if plan.live.paused == Some(false) {
        return Ok(());
    }
    // The table takes a person only. An actor outside it still keeps the pause, as the CLI's.
    let by = if matches!(plan.actor, "gui" | "cli") {
        plan.actor
    } else {
        "cli"
    };
    let added = sqlx::query(
        "INSERT OR IGNORE INTO emergency_pause (id, set_at, set_by) \
         VALUES ('me', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?)",
    )
    .bind(by)
    .execute(pool)
    .await?;
    if added.rows_affected() == 1 {
        // Fail closed: when the live pause could not be read, the data comes up paused.
        let (action, detail) = if unknown {
            (
                "emergency_pause_set_by_restore",
                json!({ "why": "live_state_unreadable" }),
            )
        } else {
            ("emergency_pause_kept_by_restore", json!({}))
        };
        audit::record_to(pool, plan.actor, action, None, detail).await?;
    }
    Ok(())
}

/// A schema older than the pause has no such table, which is a pause that was never on. Any
/// other failure is an unknown, and the caller treats it as on.
async fn read_paused(pool: &SqlitePool) -> Result<bool> {
    let found: std::result::Result<Option<i64>, sqlx::Error> =
        sqlx::query_scalar("SELECT 1 FROM emergency_pause WHERE id = 'me'")
            .fetch_optional(pool)
            .await;
    match found {
        Ok(found) => Ok(found.is_some()),
        Err(sqlx::Error::Database(error)) if error.message().contains("no such table") => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// What a pause read before the lock and one read under it say together. Either one on
/// means on, and an unreadable one with no "on" is unknown.
fn merge_pause(before: Option<bool>, under_lock: Option<bool>) -> Option<bool> {
    if before == Some(true) || under_lock == Some(true) {
        Some(true)
    } else if before.is_none() || under_lock.is_none() {
        None
    } else {
        Some(false)
    }
}

/// Consent is a decision made now, and a backup is an old one, or someone else's copy, so a
/// restore never brings one back (FR-3.30). Fails closed as the router does: anything but
/// Ollama is external. The row stays as the record, closed by the restore. When one was
/// closed, its provider is also switched off, as the AI screen's own Revoke does, so the
/// screen shows it off with no consent rather than on and failing every task. With no
/// consent to close, the switch is left as it was.
async fn revoke_external_consent(pool: &SqlitePool, actor: &str) -> Result<()> {
    let revoked: Vec<String> = sqlx::query_scalar(
        "UPDATE consent SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE revoked_at IS NULL AND provider <> 'ollama' RETURNING provider",
    )
    .fetch_all(pool)
    .await?;
    if revoked.is_empty() {
        return Ok(());
    }
    for provider in revoked {
        // A missing or non-object setting has nothing to switch off: the default is off.
        if let Some(info) = register::info_for(&provider) {
            sqlx::query(
                "UPDATE settings SET value = json_set(value, '$.' || ?, json('false')), \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE key = 'ai.enabled' AND json_type(value) = 'object'",
            )
            .bind(info.name)
            .execute(pool)
            .await?;
        }
        let action = "consent_revoked_by_restore";
        audit::record_to(pool, actor, action, Some(&provider), json!({})).await?;
    }
    Ok(())
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

/// Only what JobPilot's migrations define. sqlx's bookkeeping, SQLite's internals,
/// and FTS5's shadow tables are written by libraries whose DDL may change with a
/// version bump, which would make every older backup unrestorable.
async fn schema_of(pool: &SqlitePool) -> Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT name, sql FROM sqlite_master \
         WHERE name NOT LIKE '\\_sqlx\\_%' ESCAPE '\\' \
           AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
           AND name NOT IN (SELECT name FROM pragma_table_list WHERE type = 'shadow')",
    )
    .fetch_all(pool)
    .await?)
}

/// Step 5 inside its marker, so a crash anywhere in it leaves `Db::swap_interrupted` true.
/// The marker is written first and goes at once, ahead of the reopen, unless a failed swap
/// must keep it: the undo itself failed, or it is an earlier crash's warning, which only a
/// finished swap answers. Once the swap is done its removal can fail only as a stale
/// warning, so that must not read as a failed restore.
fn swap_marked(paths: &AppPaths) -> io::Result<()> {
    let marker = paths.swap_marker();
    let earlier = marker.exists();
    File::create(&marker)?.sync_all()?;

    let swapped = swap_in(paths);
    let settled = match swapped {
        Ok(()) => true,
        Err(_) => !earlier && !set_aside_files_remain(paths),
    };
    if settled {
        let _ = remove_if_present(&marker);
    }
    swapped
}

/// `swap_in` puts these back or removes them, so after a failure they mean an undo failed.
fn set_aside_files_remain(paths: &AppPaths) -> bool {
    let root = paths.root();
    root.join(OLD_DOCUMENTS).exists() || root.join(OLD_WAL).exists()
}

/// Step 5. A folder cannot be renamed over a folder, so the live one moves aside.
/// That comes first because it is the rename most likely to fail: Windows refuses
/// while any file inside is open. The database goes last because its rename is the
/// commit point: every failure before it is undone here, so an error means live data
/// is as it was.
// ponytail: not crash-safe across the renames. A crash here can leave new documents
// beside the old database, or none in place. `.jobpilot-swap` says so at the next open,
// and the `pre-restore` snapshot holds them.
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
async fn reopen(
    paths: &AppPaths,
    migrator: &Migrator,
    backup_key_state: Option<KeyState>,
    endpoint: Option<String>,
) -> Result<RestoreReport> {
    match Db::open_with(paths, migrator).await {
        Ok(db) => {
            api::forget_key_of_replaced_endpoint(&db, endpoint.as_deref()).await;
            secrets::forget_missing_api_keys(&db).await;
            Ok(RestoreReport {
                db,
                backup_key_state,
            })
        }
        Err(error) => Err(Error::RestoredButNotOpened(match &backup_key_state {
            Some(state) => format!("{error}. {}", key_note(state)),
            None => error.to_string(),
        })),
    }
}

fn key_note(state: &KeyState) -> String {
    match state {
        KeyState::NotLoaded => "The backup's key was not checked".into(),
        KeyState::Unlocked { key_id } => format!("The backup's key {key_id} is on this machine"),
        KeyState::KeyUnavailable { key_id, reason } => {
            format!("The backup's key {key_id} could not be read: {reason}")
        }
        KeyState::KeyMissing { key_id } => {
            format!("The backup's key {key_id} has no entry on this machine")
        }
        KeyState::KeyMismatch { key_id } => {
            format!("The backup's key {key_id} does not match the entry on this machine")
        }
    }
}

/// Everything a restore that crashed can leave in the root. `swap_in` leaves the old
/// files, and that restore's `pre-restore` snapshot holds them.
pub(crate) fn remove_restore_leftovers(paths: &AppPaths) -> io::Result<()> {
    clear_temp(paths)?;
    remove_if_present(&paths.root().join(OLD_WAL))?;
    remove_dir_if_present(&paths.root().join(OLD_DOCUMENTS))
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

pub(crate) fn remove_if_present(file: &Path) -> io::Result<()> {
    match fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

pub(crate) fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// `jobpilot.db` plus `-wal` or `-shm`.
pub(crate) fn sidecar(file: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(file);
    name.push(suffix);
    name.into()
}

pub(crate) async fn blocking<T, F>(work: F) -> Result<T>
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

    /// A pause set between the first read and the lock must not be lost.
    #[test]
    fn a_pause_set_in_the_gap_before_the_lock_is_kept() {
        assert_eq!(merge_pause(Some(false), Some(true)), Some(true));
        assert_eq!(merge_pause(Some(true), Some(false)), Some(true));
        assert_eq!(merge_pause(Some(false), Some(false)), Some(false));
        assert_eq!(merge_pause(Some(false), None), None);
        assert_eq!(merge_pause(None, Some(false)), None);
        assert_eq!(merge_pause(None, Some(true)), Some(true));
    }

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
    fn the_marker_is_gone_after_a_swap_and_after_an_undone_one() {
        let (_tmp, paths) = staged_root();
        swap_marked(&paths).unwrap();
        assert!(!paths.swap_marker().exists());

        let (_tmp, paths) = staged_root();
        fs::remove_dir_all(paths.root().join(TEMP_DOCUMENTS)).unwrap();
        swap_marked(&paths).unwrap_err();
        assert_live_unchanged(&paths);
        assert!(!paths.swap_marker().exists());
    }

    // A folder cannot be written as a file on any OS, so this stops the swap at its start.
    #[test]
    fn nothing_is_renamed_until_the_marker_is_written() {
        let (_tmp, paths) = staged_root();
        fs::create_dir(paths.swap_marker()).unwrap();

        swap_marked(&paths).unwrap_err();

        assert_live_unchanged(&paths);
        assert_eq!(fs::read_to_string(paths.db()).unwrap(), "live");
    }

    #[test]
    fn a_failed_swap_keeps_an_earlier_warning_and_a_finished_one_clears_it() {
        let (_tmp, paths) = staged_root();
        fs::write(paths.swap_marker(), "").unwrap();
        let temp_documents = paths.root().join(TEMP_DOCUMENTS);
        let held = paths.root().join("held");
        fs::rename(&temp_documents, &held).unwrap();

        swap_marked(&paths).unwrap_err();
        assert!(paths.swap_marker().exists());

        fs::rename(&held, &temp_documents).unwrap();
        swap_marked(&paths).unwrap();
        assert!(!paths.swap_marker().exists());
    }

    // An undo cannot be made to fail on demand, so this plants what one leaves behind.
    #[test]
    fn a_failed_undo_keeps_the_marker() {
        let (_tmp, paths) = staged_root();
        fs::remove_dir_all(paths.root().join(TEMP_DOCUMENTS)).unwrap();
        fs::write(paths.root().join(OLD_WAL), "left by a failed undo").unwrap();

        swap_marked(&paths).unwrap_err();

        assert!(paths.swap_marker().exists());
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
