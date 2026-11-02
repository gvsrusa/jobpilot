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

use std::fs::OpenOptions;
use std::sync::{Mutex, PoisonError};

use serde_json::json;
pub use zeroize::Zeroizing;

use crate::ai::register::{self, ProviderInfo, ProviderKind};
use crate::audit;
use crate::crypto::KEY_ACCOUNT_PREFIX;
use crate::db::Db;
use crate::error::{Error, Result};

const SERVICE: &str = "io.github.gvsrusa.jobpilot";

pub const API_KEY_PREFIX: &str = "ai-key-";

/// The longest key taken. It keeps an entry under Credential Manager's 2,560-byte blob limit.
const MAX_API_KEY: usize = 512;

/// The keychain entry and `secret_index` name of a provider's key.
pub fn api_key_name(provider: &str) -> String {
    format!("{API_KEY_PREFIX}{provider}")
}

/// Trims, then 8 to 512 visible ASCII characters with no space. Pasted text may be anything
/// at all, so the one error never holds it.
pub fn check_api_key(raw: &str) -> Result<Zeroizing<String>> {
    let key = raw.trim();
    let shaped =
        (8..=MAX_API_KEY).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic());
    if !shaped {
        return Err(Error::Validation("that is not an API key".into()));
    }
    Ok(Zeroizing::new(key.to_owned()))
}

/// The provider, if it is one that takes a key. Adapters ask this first, so nobody types or
/// confirms a key that cannot be stored.
pub fn key_provider(provider: &str) -> Result<&'static ProviderInfo> {
    register::info(provider)
        .filter(|info| matches!(info.kind, ProviderKind::Api | ProviderKind::Endpoint))
        .ok_or_else(|| Error::Refused("this provider takes no key".into()))
}

/// Stores a provider's key under its reserved name, replacing any earlier one.
pub async fn set_api_key(db: &Db, provider: &str, key: &str, actor: &str) -> Result<()> {
    key_provider(provider)?;
    let key = check_api_key(key)?;
    store(db, &api_key_name(provider), &key, actor).await
}

/// `NotFound` when no key is stored.
pub async fn delete_api_key(db: &Db, provider: &str, actor: &str) -> Result<()> {
    key_provider(provider)?;
    erase(db, &api_key_name(provider), actor).await
}

/// A restore can bring back the row of a key removed since the backup, and the screen reads
/// rows. Run after one, so a row names a key the keychain holds. It reads the entry, as
/// keyring has no check for one, and drops the value at once. Best effort: the restore is
/// done, and a stale row costs only a `no_key` at the next send.
pub(crate) async fn forget_missing_api_keys(db: &Db) {
    forget_unheld(db, |name| Ok(get(name)?.map(Zeroizing::new).is_some())).await
}

/// Only a definite no drops a row: a locked keychain says nothing about the key. Provider
/// names come from the table, as a backup is untrusted and each ask can prompt the user.
async fn forget_unheld(db: &Db, holds: impl Fn(&str) -> Result<bool> + Send + Copy + 'static) {
    let Ok(listed) = names(db).await else {
        return;
    };
    let takes_key = register::PROVIDERS
        .iter()
        .filter(|info| key_provider(info.name).is_ok());
    for name in takes_key.map(|info| api_key_name(info.name)) {
        if !listed.contains(&name) {
            continue;
        }
        let asked = name.clone();
        if let Ok(false) = keychain(move || holds(&asked)).await {
            let _ = remove_row(db, &name).await;
        }
    }
}

/// Run after a restore, which is done and must not fail here. The row goes even when the
/// keychain will not answer: with no row, no send reads the entry.
pub(crate) async fn forget_api_key(db: &Db, provider: &str) {
    let name = api_key_name(provider);
    if names(db).await.is_ok_and(|listed| listed.contains(&name)) {
        let _ = remove_all(vec![name.clone()]).await;
        let _ = remove_row(db, &name).await;
    }
}

/// Off the runtime. `None` when no entry.
pub(crate) async fn read_api_key(provider: &str) -> Result<Option<Zeroizing<String>>> {
    let name = api_key_name(provider);
    keychain(move || Ok(get(&name)?.map(Zeroizing::new))).await
}

/// Stores `value` under `name`, replacing any earlier value.
pub async fn set(db: &Db, name: &str, value: &str, actor: &str) -> Result<()> {
    check_name(name)?;
    refuse_reserved(name)?;
    store(db, name, value, actor).await
}

/// The index row goes first: a crash between the two steps must leave a name that
/// `delete_all` can find, never a keychain entry it cannot.
async fn store(db: &Db, name: &str, value: &str, actor: &str) -> Result<()> {
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

    let (entry_name, entry_value) = (name.to_owned(), Zeroizing::new(value.to_owned()));
    if let Err(error) = keychain(move || {
        one_at_a_time(|| {
            entry(&entry_name)?
                .set_password(&entry_value)
                .map_err(secret_error)
        })
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
    one_at_a_time(|| read(name))
}

fn read(name: &str) -> Result<Option<String>> {
    match entry(name)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(secret_error(error)),
    }
}

/// `NotFound` when this root does not list `name`: the entry could belong to another
/// root. The row outlives the keychain entry, so a failure here can be retried.
pub async fn delete(db: &Db, name: &str, actor: &str) -> Result<()> {
    refuse_reserved(name)?;
    erase(db, name, actor).await
}

async fn erase(db: &Db, name: &str, actor: &str) -> Result<()> {
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

/// Keychain only: a data key has no `secret_index` row and no `secret_set` audit, so it
/// stays out of `names` and `delete`. Replaces any value at `name`, so callers pass a
/// new name: a stored key must never be overwritten (threat model section 8).
pub(crate) async fn store_key(name: &str, value: &str) -> Result<()> {
    let (name, value) = (name.to_owned(), Zeroizing::new(value.to_owned()));
    keychain(move || one_at_a_time(|| entry(&name)?.set_password(&value).map_err(secret_error)))
        .await
}

/// Adds a key the caller has proven, and never replaces an entry. An entry that already
/// holds this value is fine as it is; one holding anything else is `Refused`, because
/// nothing here can say which of the two is worth keeping.
pub(crate) async fn add_key(name: &str, value: &str) -> Result<()> {
    let (name, value) = (name.to_owned(), Zeroizing::new(value.to_owned()));
    keychain(move || {
        one_at_a_time(|| match read(&name)?.map(Zeroizing::new) {
            Some(stored) if stored == value => Ok(()),
            Some(_) => Err(Error::Refused(
                "the keychain holds another value for this key, and it is left as it is. \
                 Remove that entry to import this file"
                    .into(),
            )),
            None => entry(&name)?.set_password(&value).map_err(secret_error),
        })
    })
    .await
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
        one_at_a_time(|| {
            for name in names.iter().filter(|name| !name.is_empty()) {
                match entry(name)?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(error) => return Err(secret_error(error)),
                }
            }
            Ok(())
        })
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

/// Runs one keychain call while no other runs, in this process or another of ours. Windows
/// Credential Manager mishandles overlapping calls, even on different entries: a read can
/// answer "no such entry" for a key it holds, and a delete can report success and leave the
/// entry. That answer is what offers discard (see `crypto`), so a held key must never get it.
/// The app and the CLI in its terminal are two processes, hence the file as well as the
/// mutex. It is per user, not per root, because entries are. If it cannot be taken, the call
/// goes ahead: a key must stay reachable when the temp folder is not.
///
/// Every keychain call is made inside this, once, so nothing here may call `get`.
// ponytail: other programs write Credential Manager too, and nothing can hold them back.
// An overlap with one reads as a missing key until the next ask, as Retry does.
fn one_at_a_time<T>(call: impl FnOnce() -> T) -> T {
    static THIS_PROCESS: Mutex<()> = Mutex::new(());
    let _this_process = THIS_PROCESS.lock().unwrap_or_else(PoisonError::into_inner);
    let _other_processes = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(std::env::temp_dir().join("jobpilot-keychain.lock"))
        .ok()
        .filter(|file| file.lock().is_ok());
    call()
}

/// Every keychain call passes through `entry`, so counting here covers them all.
#[cfg(test)]
pub(crate) static KEYCHAIN_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn entry(name: &str) -> Result<keyring::Entry> {
    #[cfg(test)]
    KEYCHAIN_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

/// `set` and `delete` are open to the webview, and a key id is not secret: the audit log
/// shows it. Replacing or removing that entry would destroy the only copy of the data
/// key. Swapping a provider key for one of its own would route the user's data to another
/// account. Windows matches credential names without regard to case, so this does too.
fn refuse_reserved(name: &str) -> Result<()> {
    let starts_with = |prefix: &str| {
        name.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    };
    if starts_with(KEY_ACCOUNT_PREFIX) {
        return Err(Error::Refused(
            "this name is reserved for the data key".into(),
        ));
    }
    if starts_with(API_KEY_PREFIX) {
        return Err(Error::Refused(
            "this name is reserved for a provider key".into(),
        ));
    }
    Ok(())
}

/// `JOBPILOT_SECRET_SERVICE` lets the keychain tests stay clear of the app's own
/// entries. Only debug and test builds read it: a release build must not let the
/// environment point the app at another service, so it always uses `SERVICE`.
// ponytail: one service per build, so no multi-profile secret namespaces.
fn service() -> String {
    let honour = cfg!(any(test, debug_assertions));
    resolve_service(std::env::var("JOBPILOT_SECRET_SERVICE").ok(), honour)
}

/// Empty means unset, as with `JOBPILOT_HOME`.
fn resolve_service(requested: Option<String>, honour: bool) -> String {
    requested
        .filter(|service| honour && !service.is_empty())
        .unwrap_or_else(|| SERVICE.to_owned())
}

/// Keychain calls can block on a system prompt for as long as the user takes.
pub(crate) async fn keychain<T, F>(work: F) -> Result<T>
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
    fn only_data_key_names_are_reserved() {
        for name in ["data-key-00112233aabbccdd", "DATA-KEY-x", "Data-Key-"] {
            assert!(
                matches!(refuse_reserved(name), Err(Error::Refused(_))),
                "{name}"
            );
        }
        for name in ["data-key", "ai-key", "openai", "my-data-key-1", "é"] {
            assert!(refuse_reserved(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn api_key_names_are_reserved_from_the_generic_commands() {
        for name in ["ai-key-openai", "AI-KEY-x"] {
            assert!(
                matches!(refuse_reserved(name), Err(Error::Refused(_))),
                "{name}"
            );
        }
        assert_eq!(api_key_name("openai"), "ai-key-openai");
    }

    #[test]
    fn check_api_key_trims_and_never_quotes() {
        assert_eq!(
            check_api_key("  sk-abcdefgh\n").unwrap().as_str(),
            "sk-abcdefgh"
        );
        let long = "x".repeat(513);
        for bad in [
            "",
            "short",
            "two words here",
            "has\ttab1",
            long.as_str(),
            "café-key-1234",
        ] {
            let error = check_api_key(bad).unwrap_err();
            assert!(matches!(error, Error::Validation(_)), "{error:?}");
            let message = error.to_string();
            assert!(bad.is_empty() || !message.contains(bad), "{message}");
        }
    }

    #[test]
    fn check_api_key_takes_the_limits_exactly() {
        assert!(check_api_key(&"x".repeat(8)).is_ok());
        assert!(check_api_key(&"x".repeat(7)).is_err());
        assert!(check_api_key(&"x".repeat(512)).is_ok());
        assert!(check_api_key("\u{7f}xxxxxxxx").is_err());
    }

    /// Refused before the index or the keychain, so it needs neither.
    #[tokio::test]
    async fn only_api_and_endpoint_providers_take_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&crate::AppPaths::new(tmp.path().join("data")))
            .await
            .unwrap();
        for provider in ["ollama", "claude_code", "gemini_cli", "codex_cli", "x"] {
            let set = set_api_key(&db, provider, "sk-abcdefgh", "test").await;
            assert!(matches!(set, Err(Error::Refused(_))), "{provider}: {set:?}");
            let gone = delete_api_key(&db, provider, "test").await;
            assert!(
                matches!(gone, Err(Error::Refused(_))),
                "{provider}: {gone:?}"
            );
        }
        for provider in ["openai", "anthropic", "google", "openrouter", "endpoint"] {
            assert!(key_provider(provider).is_ok(), "{provider}");
        }
        assert!(names(&db).await.unwrap().is_empty());
        db.close().await;
    }

    #[tokio::test]
    async fn only_a_key_known_to_be_gone_loses_its_row() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&crate::AppPaths::new(tmp.path().join("data")))
            .await
            .unwrap();
        for name in [
            "ai-key-anthropic",
            "ai-key-google",
            "ai-key-no-such-provider",
            "ai-key-openai",
            "my-token",
        ] {
            sqlx::query("INSERT INTO secret_index (name, created_at) VALUES (?, 't')")
                .bind(name)
                .execute(&db.pool)
                .await
                .unwrap();
        }

        forget_unheld(&db, |name| match name {
            "ai-key-openai" => Ok(true),
            "ai-key-anthropic" => Err(Error::Secret("locked".into())),
            "ai-key-google" => Ok(false),
            other => panic!("asked about {other}"),
        })
        .await;

        assert_eq!(
            names(&db).await.unwrap(),
            [
                "ai-key-anthropic",
                "ai-key-no-such-provider",
                "ai-key-openai",
                "my-token"
            ]
        );
        db.close().await;
    }

    /// The row check runs first, so a name this root does not list asks no keychain.
    #[tokio::test]
    async fn a_bad_key_and_a_missing_key_touch_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&crate::AppPaths::new(tmp.path().join("data")))
            .await
            .unwrap();
        let set = set_api_key(&db, "openai", "two words here", "test").await;
        assert!(matches!(set, Err(Error::Validation(_))), "{set:?}");
        let gone = delete_api_key(&db, "openai", "test").await;
        assert!(matches!(gone, Err(Error::NotFound)), "{gone:?}");
        assert!(names(&db).await.unwrap().is_empty());
        db.close().await;
    }

    #[test]
    fn resolve_service_honours_a_request_only_when_told() {
        let asked = |value: &str| Some(value.to_owned());
        assert_eq!(resolve_service(asked("x"), true), "x");
        assert_eq!(resolve_service(asked("x"), false), SERVICE);
        assert_eq!(resolve_service(asked(""), true), SERVICE);
        assert_eq!(resolve_service(None, true), SERVICE);
    }

    #[test]
    fn store_failures_keep_their_reason() {
        let locked = keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("locked")));
        assert!(secret_error(locked).to_string().contains("locked"));
    }
}
