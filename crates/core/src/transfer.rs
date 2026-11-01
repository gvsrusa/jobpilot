//! Data export and full deletion. Exports hold `secret_index` names and never a value:
//! values exist only in the keychain, which nothing here reads.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use sqlx::sqlite::SqlitePool;

use crate::audit;
use crate::backup::{
    blocking, remove_dir_if_present, remove_if_present, remove_restore_leftovers, sidecar,
};
use crate::db::{
    Db, copy_dir, current_version, lock_exclusive, new_snapshot_dir, release, utc_stamps,
};
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::secrets;

/// Writes `exports/jobpilot-export-<stamp>/`, holding `data.json` and a copy of
/// `documents/`. A folder under that name is always complete: it is built under a
/// temporary name and renamed last.
pub async fn export_all(db: &Db, actor: &str) -> Result<PathBuf> {
    let (stamp, exported_at) = utc_stamps(&db.pool).await?;
    let data = json!({
        "format": 1,
        "schema_version": current_version(&db.pool).await?,
        "exported_at": exported_at,
        "tables": read_tables(&db.pool).await?,
    });
    let text = serde_json::to_string_pretty(&data).map_err(io::Error::other)?;

    let (exports, documents) = (db.paths().exports(), db.paths().documents());
    let dir =
        tokio::task::spawn_blocking(move || write_export(&exports, &documents, &stamp, &text))
            .await
            .map_err(io::Error::other)??;

    let target = dir.to_string_lossy();
    audit::record(db, actor, "export", Some(&target), json!({})).await?;
    Ok(dir)
}

/// Every row of every user table, in one read transaction so the tables agree.
///
/// `pragma_table_list` is `sqlite_master` plus each table's kind, and that kind is what
/// leaves out FTS virtual and shadow tables, whatever a later migration names them.
/// Their content is derived from the tables that are exported.
// ponytail: all rows sit in memory at once, and `json_object` refuses BLOB columns and
// tables wider than SQLite's function argument limit, so such a table fails the export
// loudly. No table is either today; stream rows and encode BLOBs when one is.
async fn read_tables(pool: &SqlitePool) -> Result<Map<String, Value>> {
    let mut read = pool.begin().await?;
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'table' \
         AND name NOT GLOB '_sqlx_*' AND name NOT GLOB 'sqlite_*'",
    )
    .fetch_all(&mut *read)
    .await?;

    let mut tables = Map::new();
    for name in names {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(&name)
                .fetch_all(&mut *read)
                .await?;
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(rows_sql(&name, &columns)))
            .fetch_all(&mut *read)
            .await?;
        let rows = rows
            .iter()
            .map(|row| serde_json::from_str(row))
            .collect::<std::result::Result<Vec<Value>, _>>()
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        tables.insert(name, Value::Array(rows));
    }
    read.commit().await?;
    Ok(tables)
}

/// Names come from SQLite's own catalog, and quoting keeps an odd one harmless.
fn rows_sql(table: &str, columns: &[String]) -> String {
    let pairs: Vec<String> = columns
        .iter()
        .map(|column| {
            format!(
                "'{}', \"{}\"",
                column.replace('\'', "''"),
                column.replace('"', "\"\"")
            )
        })
        .collect();
    format!(
        "SELECT json_object({}) FROM \"{}\"",
        pairs.join(", "),
        table.replace('"', "\"\"")
    )
}

fn write_export(exports: &Path, documents: &Path, stamp: &str, text: &str) -> Result<PathBuf> {
    let name = format!("jobpilot-export-{stamp}");
    let temp = new_snapshot_dir(exports, &format!("tmp-{name}"))?;
    let finished = fill_and_publish(&temp, exports, documents, &name, text);
    if finished.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    Ok(finished?)
}

fn fill_and_publish(
    temp: &Path,
    exports: &Path,
    documents: &Path,
    name: &str,
    text: &str,
) -> io::Result<PathBuf> {
    fs::write(temp.join("data.json"), text)?;
    copy_dir(documents, &temp.join("documents"))?;

    let mut dir = exports.join(name);
    let mut attempt = 0;
    while dir.exists() {
        attempt += 1;
        dir = exports.join(format!("{name}-{attempt}"));
    }
    fs::rename(temp, &dir)?;
    Ok(dir)
}

/// Removes everything JobPilot owns under the root and its keychain entries. This
/// cannot be undone, and it writes no audit row because it removes the log.
///
/// Consumes `db`. On any error the handle is closed, so callers run `Db::open` again.
/// `confirm` must be exactly `"DELETE"`. An error before the files go leaves them
/// whole, and a call that failed can run again from a fresh handle: keychain entries
/// already gone count as removed. `exports/` always goes; `include_backups` decides
/// `backups/`.
// ponytail: names are read before the lock is won, because no pool is open after it.
// A secret another process adds and closes inside that gap keeps its keychain entry.
pub async fn delete_all(db: Db, confirm: &str, include_backups: bool) -> Result<()> {
    let names = match confirm {
        "DELETE" => secrets::names(&db).await,
        _ => Err(Error::ConfirmationRequired),
    };
    let paths = db.paths().clone();
    db.close().await;
    let names = names?;

    let lock = lock_exclusive(&paths)?;
    let deleted = erase(&paths, names, include_backups).await;
    release(lock);
    deleted
}

/// The keychain goes first: a failure there must leave `secret_index` to retry from.
async fn erase(paths: &AppPaths, names: Vec<String>, include_backups: bool) -> Result<()> {
    secrets::remove_all(names).await?;
    let paths = paths.clone();
    blocking(move || remove_files(&paths, include_backups)).await
}

/// Only JobPilot's own names go, never the whole root: `JOBPILOT_HOME` can point at a
/// folder that holds other things. The lock file stays, as it is held.
///
/// The database goes last, so a failure partway still reads as "not deleted yet". Its
/// WAL goes before it because SQLite does not bind a WAL to its database, so a stray
/// one could replay into the fresh database the next open creates.
fn remove_files(paths: &AppPaths, include_backups: bool) -> io::Result<()> {
    remove_dir_if_present(&paths.documents())?;
    remove_dir_if_present(&paths.exports())?;
    if include_backups {
        remove_dir_if_present(&paths.backups())?;
    }
    remove_restore_leftovers(paths)?;

    let db = paths.db();
    for suffix in ["-wal", "-shm", "-journal"] {
        remove_if_present(&sidecar(&db, suffix))?;
    }
    remove_if_present(&db)
}
