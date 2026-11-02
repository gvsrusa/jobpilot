//! Data export and full deletion. Exports hold `secret_index` names and never a value:
//! values exist only in the keychain, which nothing here reads.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use sqlx::sqlite::SqlitePool;

use crate::backup::{
    blocking, remove_dir_if_present, remove_if_present, remove_restore_leftovers, sidecar,
};
use crate::db::{
    Db, copy_dir, current_version, lock_exclusive, new_snapshot_dir, release, utc_stamps,
};
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::profile;
use crate::secrets;
use crate::{audit, crypto, memory};

/// Writes `exports/jobpilot-export-<stamp>/`, holding `data.json`, `work_authorization.json`
/// and `memory_sealed_answers.json` (each plain when the key is unlocked, else a locked
/// marker; callers warn first) and a copy of `documents/`. A folder under that name is
/// always complete: it is built under a temporary name and renamed last.
pub async fn export_all(db: &Db, actor: &str) -> Result<PathBuf> {
    Ok(export_report(db, actor).await?.path)
}

/// What an export did, for an adapter to tell the user.
#[derive(Debug)]
pub struct ExportReport {
    pub path: PathBuf,
    pub sensitive_plain: bool,
    /// Sealed answers left out, because no key could open them.
    pub withheld: usize,
}

/// [`export_all`], with what it wrote and what it left out.
pub async fn export_report(db: &Db, actor: &str) -> Result<ExportReport> {
    let (stamp, exported_at) = utc_stamps(&db.pool).await?;
    let data = json!({
        "format": 1,
        "schema_version": current_version(&db.pool).await?,
        "exported_at": exported_at,
        "tables": read_tables(&db.pool).await?,
    });
    let text = serde_json::to_string_pretty(&data).map_err(io::Error::other)?;
    let (authorization, authorization_plain) = profile::export_authorization(db).await?;
    let (answers, answers_plain, withheld) = memory::export_sealed(db).await?;
    let sensitive_plain = authorization_plain || answers_plain;

    let (exports, documents) = (db.paths().exports(), db.paths().documents());
    let dir = tokio::task::spawn_blocking(move || {
        write_export(
            &exports,
            &documents,
            &stamp,
            &text,
            &authorization,
            &answers,
        )
    })
    .await
    .map_err(io::Error::other)??;

    let target = dir.to_string_lossy();
    let detail = json!({ "sensitive_plain": sensitive_plain, "withheld": withheld });
    audit::record(db, actor, "export", Some(&target), detail).await?;
    Ok(ExportReport {
        path: dir,
        sensitive_plain,
        withheld,
    })
}

/// Every row of every user table, in one read transaction so the tables agree.
///
/// `pragma_table_list` is `sqlite_master` plus each table's kind, and that kind is what
/// leaves out FTS virtual and shadow tables, whatever a later migration names them.
/// Their content is derived from the tables that are exported.
/// BLOB columns are left out: `json_object` refuses them, and a declared BLOB is
/// ciphertext, a key-check, or a vector, none of which means anything in an export. An
/// answer body under a sealed class is blanked too, as it may wait in plain to be sealed.
// ponytail: all rows sit in memory at once, and `json_object` refuses tables wider than
// SQLite's function argument limit, so such a table fails the export loudly. No table is
// that wide today; stream rows when one is.
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
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info(?) WHERE upper(type) <> 'BLOB' ORDER BY cid",
        )
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
                "'{}', {}",
                column.replace('\'', "''"),
                value_sql(table, column)
            )
        })
        .collect();
    format!(
        "SELECT json_object({}) FROM \"{}\"",
        pairs.join(", "),
        table.replace('"', "\"\"")
    )
}

/// `memory_sealed_answers.json` carries a body blanked here. A row whose question cannot be
/// found counts as sealed.
fn value_sql(table: &str, column: &str) -> String {
    if (table, column) == ("memory_answers", "body") {
        return "CASE WHEN EXISTS (SELECT 1 FROM memory_questions q \
                WHERE q.id = question_id AND q.class = 'normal') THEN body ELSE '' END"
            .to_owned();
    }
    format!("\"{}\"", column.replace('"', "\"\""))
}

fn write_export(
    exports: &Path,
    documents: &Path,
    stamp: &str,
    text: &str,
    authorization: &str,
    answers: &str,
) -> Result<PathBuf> {
    let name = format!("jobpilot-export-{stamp}");
    let temp = new_snapshot_dir(exports, &format!("tmp-{name}"))?;
    let finished = fill_and_publish(
        &temp,
        exports,
        documents,
        &name,
        text,
        authorization,
        answers,
    );
    if finished.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    Ok(finished?)
}

/// Owner-only on Unix: this file may hold plaintext work authorization or sealed answers.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    use io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

fn fill_and_publish(
    temp: &Path,
    exports: &Path,
    documents: &Path,
    name: &str,
    text: &str,
    authorization: &str,
    answers: &str,
) -> io::Result<PathBuf> {
    fs::write(temp.join("data.json"), text)?;
    write_private(&temp.join("work_authorization.json"), authorization)?;
    write_private(&temp.join("memory_sealed_answers.json"), answers)?;
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

/// Removes JobPilot's data under the root and its keychain entries: this root's data keys,
/// active or not, then the API keys. This cannot be undone, and it writes no audit row
/// because it removes the log.
///
/// Consumes `db`. On any error the handle is closed, so callers run `Db::open` again.
/// `confirm` must be exactly `"DELETE"`, and the root must still hold its marker. An
/// error before the files go leaves them whole, and a call that failed can run again
/// from a fresh handle: keychain entries already gone count as removed. `exports/`
/// always goes; `include_backups` decides `backups/`.
///
/// What stays: `.jobpilot-root`, so a retry or the next `Db::open` still finds an owned
/// root instead of a non-empty one with no proof; `jobpilot.lock`, as it is held; and
/// any file JobPilot did not name.
// ponytail: names are read before the lock is won, because no pool is open after it.
// A secret another process adds and closes inside that gap keeps its keychain entry.
pub async fn delete_all(db: Db, confirm: &str, include_backups: bool) -> Result<()> {
    let names = names_to_erase(&db, confirm).await;
    let paths = db.paths().clone();
    db.close().await;
    let names = names?;

    let lock = lock_exclusive(&paths)?;
    let deleted = erase(&paths, secrets::remove_all(names), include_backups).await;
    release(lock);
    deleted
}

async fn names_to_erase(db: &Db, confirm: &str) -> Result<Vec<String>> {
    if confirm != "DELETE" {
        return Err(Error::ConfirmationRequired);
    }
    if !db.paths().marker().is_file() {
        return Err(Error::RootNotOwned(db.paths().root().to_path_buf()));
    }
    // A key can have ciphertext and no row, and no other root's id is in either list. A
    // restored row can hold any text, and on Windows a name that is too long fails before
    // the store is asked, so it would fail every retry. No such id has an entry.
    let mut key_ids: Vec<String> = sqlx::query_scalar("SELECT key_id FROM data_keys")
        .fetch_all(&db.pool)
        .await?;
    key_ids.extend(crypto::held_key_ids(db).await?);
    let mut names: Vec<String> = key_ids
        .iter()
        .filter(|id| crypto::is_key_id(id.as_bytes()))
        .map(|id| crypto::account(id))
        .collect();
    names.extend(secrets::names(db).await?);
    Ok(names)
}

/// The keychain goes first: a failure there must leave `secret_index` to retry from.
/// Takes that step as a future because a real keychain cannot be made to fail on demand.
async fn erase(
    paths: &AppPaths,
    clear_keychain: impl Future<Output = Result<()>>,
    include_backups: bool,
) -> Result<()> {
    clear_keychain.await?;
    let paths = paths.clone();
    blocking(move || remove_files(&paths, include_backups)).await
}

/// Only JobPilot's own names go, never the whole root. `remove_dir_all` removes a
/// symlink at one of those names and leaves its target.
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
    remove_if_present(&paths.swap_marker())?;

    let db = paths.db();
    for suffix in ["-wal", "-shm", "-journal"] {
        remove_if_present(&sidecar(&db, suffix))?;
    }
    remove_if_present(&db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keychain_failure_leaves_files_for_a_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        let db = Db::open(&paths).await.unwrap();
        fs::write(paths.documents().join("resume.pdf"), "pdf").unwrap();
        db.close().await;

        let failing = async { Err(Error::Secret("locked".into())) };
        let result = erase(&paths, failing, true).await;

        assert!(matches!(result, Err(Error::Secret(_))), "{result:?}");
        assert!(paths.documents().join("resume.pdf").is_file());
        assert!(paths.db().is_file());

        let db = Db::open(&paths).await.unwrap();
        delete_all(db, "DELETE", true).await.unwrap();
        assert!(!paths.db().exists());
    }

    #[tokio::test]
    async fn only_real_key_ids_become_keychain_names() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        let db = Db::open(&paths).await.unwrap();
        let long = "a".repeat(100_000);
        for key_id in [long.as_str(), "", "ABCDEF0123456789", "00112233aabbccdd"] {
            sqlx::query(
                "INSERT INTO data_keys (key_id, created_at, key_check, active) \
                 VALUES (?, 't', x'00', 0)",
            )
            .bind(key_id)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO secret_index (name, created_at) VALUES ('openai', 't')")
            .execute(&db.pool)
            .await
            .unwrap();

        let names = names_to_erase(&db, "DELETE").await.unwrap();
        db.close().await;

        assert_eq!(names, ["data-key-00112233aabbccdd", "openai"]);
    }
}
