//! API keys live in the OS keychain. `secret_index` holds only their names, so
//! exports, backups, and `delete_all` can work with them without reading a value.
//!
//! Entries are keyed by name alone, so two data roots using one name share an entry.
// ponytail: 0.1 does not namespace entries per root. Namespace them if a user ever
// runs two roots with different keys.
// ponytail: writers to one name are not serialized, and two races can leave a stored
// entry with no `secret_index` row, which `delete_all` then misses. `set`: when two
// handles add one new name and the first keychain write fails, its rollback removes
// the row the second relies on. `delete`: a `set` landing between the keychain removal
// and the row removal stores a value whose row then goes. Both need simultaneous
// writers from two processes; hold the row in a transaction if that ever happens.

use serde_json::json;

use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};

const SERVICE: &str = "io.github.gvsrusa.jobpilot";

/// Stores `value` under `name`, replacing any earlier value.
///
/// The index row goes first: a crash between the two steps must leave a name that
/// `delete_all` can find, never a keychain entry it cannot.
pub async fn set(db: &Db, name: &str, value: &str, actor: &str) -> Result<()> {
    check_name(name)?;
    let added = sqlx::query(
        "INSERT INTO secret_index (name, created_at) \
         VALUES (?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(name) DO NOTHING",
    )
    .bind(name)
    .execute(&db.pool)
    .await?
    .rows_affected()
        == 1;

    let (entry_name, entry_value) = (name.to_owned(), value.to_owned());
    if let Err(error) = keychain(move || {
        entry(&entry_name)?
            .set_password(&entry_value)
            .map_err(secret_error)
    })
    .await
    {
        if added {
            // A leftover row only lists a key that has no value, so its own failure
            // must not hide the keychain error.
            let _ = remove_row(db, name).await;
        }
        return Err(error);
    }

    audit::record(db, actor, "secret_set", Some(name), json!({})).await
}

/// For Rust callers only: no IPC command or CLI path may reach this. It blocks on the
/// keychain, so async callers run it in `spawn_blocking`.
pub fn get(name: &str) -> Result<Option<String>> {
    match entry(name)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(secret_error(error)),
    }
}

/// `NotFound` when this root does not list `name`: the entry could belong to another
/// root. The row outlives the keychain entry, so a failure here can be retried.
pub async fn delete(db: &Db, name: &str, actor: &str) -> Result<()> {
    let listed: Option<i64> = sqlx::query_scalar("SELECT 1 FROM secret_index WHERE name = ?")
        .bind(name)
        .fetch_optional(&db.pool)
        .await?;
    if listed.is_none() {
        return Err(Error::NotFound);
    }

    remove_all(vec![name.to_owned()]).await?;
    remove_row(db, name).await?;
    audit::record(db, actor, "secret_delete", Some(name), json!({})).await
}

pub async fn names(db: &Db) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT name FROM secret_index ORDER BY name")
            .fetch_all(&db.pool)
            .await?,
    )
}

/// Removes keychain entries. One already gone counts as removed, so a call that failed
/// partway can run again. An empty name cannot have an entry, so a stray row is passed
/// over: it must never block deletion for good.
pub(crate) async fn remove_all(names: Vec<String>) -> Result<()> {
    keychain(move || {
        for name in names.iter().filter(|name| !name.is_empty()) {
            match entry(name)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(secret_error(error)),
            }
        }
        Ok(())
    })
    .await
}

async fn remove_row(db: &Db, name: &str) -> Result<()> {
    sqlx::query("DELETE FROM secret_index WHERE name = ?")
        .bind(name)
        .execute(&db.pool)
        .await?;
    Ok(())
}

fn entry(name: &str) -> Result<keyring::Entry> {
    check_name(name)?;
    keyring::Entry::new(&service(), name).map_err(secret_error)
}

/// Windows would accept an empty name and macOS refuses one, so it stops here on both.
fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Secret("the secret name is empty".into()));
    }
    Ok(())
}

/// `JOBPILOT_SECRET_SERVICE` lets the real keychain tests stay clear of the app's own
/// entries. Empty means unset, as with `JOBPILOT_HOME`.
fn service() -> String {
    std::env::var("JOBPILOT_SECRET_SERVICE")
        .ok()
        .filter(|service| !service.is_empty())
        .unwrap_or_else(|| SERVICE.to_owned())
}

/// Keychain calls can block on a system prompt for as long as the user takes.
async fn keychain<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Error::Secret("the keychain call stopped unexpectedly".into()))?
}

/// Only the variants known to describe the store keep their text. The rest may hold
/// the stored bytes (`BadEncoding`, `BadDataFormat`), and `keyring::Error` can grow.
fn secret_error(error: keyring::Error) -> Error {
    use keyring::Error as Keyring;
    Error::Secret(match error {
        Keyring::PlatformFailure(_)
        | Keyring::NoStorageAccess(_)
        | Keyring::NoEntry
        | Keyring::TooLong(..)
        | Keyring::NoDefaultStore => error.to_string(),
        _ => "the keychain refused the request".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_bytes_never_reach_the_message() {
        let source = Box::new(std::io::Error::other("hunter2"));
        for error in [
            keyring::Error::BadEncoding(b"hunter2".to_vec()),
            keyring::Error::BadDataFormat(b"hunter2".to_vec(), source),
            keyring::Error::Invalid("hunter2".into(), "hunter2".into()),
            keyring::Error::BadStoreFormat("hunter2".into()),
        ] {
            let message = secret_error(error).to_string();
            assert!(!message.contains("hunter2"), "{message}");
        }
    }

    #[test]
    fn store_failures_keep_their_reason() {
        let locked = keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("locked")));
        assert!(secret_error(locked).to_string().contains("locked"));
    }
}
