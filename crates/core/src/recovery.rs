//! The recovery key (D5): the one deliberate copy of the data key outside the keychain,
//! and what to do when a data key is lost for good. See threat model section 9.
//!
//! Every action here is refused in the terminal panel, whose input and output pass
//! through the webview. A refusal comes before any keychain call.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

use crate::crypto::{self, DataKey, KeyState};
use crate::db::{self, Db};
use crate::error::{Error, Result};
use crate::{audit, secrets, settings};

pub const RECOVERY_FORMAT: u32 = 1;

/// A recovery file is about 150 bytes, so this is only a limit on what gets read.
const MAX_FILE_BYTES: u64 = 4096;

const EXPORTED_MARK: &str = "recovery_key_exported_";

#[derive(Debug, Serialize)]
pub struct RecoveryReceipt {
    pub key_id: String,
    pub path: PathBuf,
}

/// No `Debug`, as it holds the key. The strings borrow from a buffer that gets zeroed.
// ponytail: no passphrase wrapping (D5 option c not taken). A stolen file opens every
// sensitive field. A passphrase is one more thing to lose, and it has to be typed
// somewhere other than the webview. Add a format 2 if that trade ever changes.
#[derive(Serialize, Deserialize)]
struct RecoveryFile<'a> {
    format: u32,
    key_id: &'a str,
    created_at: &'a str,
    key_hex: &'a str,
}

/// The terminal panel's child processes carry this marker, and panel input has no shell
/// to clear it with.
pub fn panel_marker_set() -> bool {
    std::env::var_os("JOBPILOT_PANEL").is_some_and(|marker| !marker.is_empty())
}

fn refuse_panel() -> Result<()> {
    if panel_marker_set() {
        return Err(Error::PanelRefused);
    }
    Ok(())
}

/// Writes the data key to a new regular file outside the data root. A file is left
/// behind only on success, so the audit row and the file always go together.
pub async fn export(db: &Db, out: &Path, actor: &str) -> Result<RecoveryReceipt> {
    refuse_panel()?;
    if db.paths().is_inside_root(out) {
        return Err(Error::Refused(
            "the recovery file must go in an existing folder outside the data folder".into(),
        ));
    }

    let file = create_new_regular(out)?;
    match write_and_record(db, file, actor).await {
        Ok(key_id) => Ok(RecoveryReceipt {
            key_id,
            path: out.to_path_buf(),
        }),
        Err(error) => {
            // Only a file this call created reaches here. The first error is the one
            // that matters, so a failed removal does not replace it.
            let _ = fs::remove_file(out);
            Err(error)
        }
    }
}

/// `create_new` is `O_CREAT|O_EXCL`, or `CREATE_NEW` on Windows: it does not follow a final
/// symlink and does not reuse a path, so a device path fails here on `AlreadyExists`. The
/// handle check is for what a name can reach on Windows, such as `CONOUT$`.
fn create_new_regular(out: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(out)?;
    require_regular(&file)?;
    Ok(file)
}

fn require_regular(file: &File) -> Result<()> {
    if !file.metadata()?.file_type().is_file() {
        return Err(Error::Refused("this is not a regular file".into()));
    }
    Ok(())
}

async fn write_and_record(db: &Db, mut file: File, actor: &str) -> Result<String> {
    let key = crypto::unlocked(db).await?;
    let (_, created_at) = db::utc_stamps(&db.pool).await?;
    let hex = Zeroizing::new(key.to_hex());
    let text = Zeroizing::new(
        serde_json::to_string(&RecoveryFile {
            format: RECOVERY_FORMAT,
            key_id: key.key_id(),
            created_at: &created_at,
            key_hex: &hex,
        })
        .map_err(std::io::Error::other)?,
    );
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);

    let mut write = db.pool.begin().await?;
    mark_exported(&mut write, key.key_id()).await?;
    audit::record_to(
        &mut *write,
        actor,
        "recovery_key_exported",
        Some(key.key_id()),
        json!({ "key_id": key.key_id() }),
    )
    .await?;
    write.commit().await?;
    Ok(key.key_id().to_owned())
}

async fn mark_exported(write: &mut sqlx::SqliteConnection, key_id: &str) -> Result<()> {
    let mark = format!("{EXPORTED_MARK}{key_id}");
    settings::set_to(write, &mark, &json!(true)).await
}

/// True while the active key has no recovery file. False with no active key: there is
/// nothing to export yet. Reads no key, so it can never raise a keychain prompt.
pub async fn no_recovery_key_exported(db: &Db) -> Result<bool> {
    let Some(key_id) = crypto::active_key_id(&db.pool).await? else {
        return Ok(false);
    };
    let mark = format!("{EXPORTED_MARK}{key_id}");
    Ok(settings::get(db, &mark).await?.is_none())
}

/// Offered from `KeyMissing` and `KeyMismatch` only, and the file must hold a key that
/// proves itself against this database and opens its ciphertext. A wrong file is
/// `KeyMismatch` for the key the database needs, with nothing changed.
///
/// An entry already there is never replaced, so a right file over a bad entry is
/// `Refused`. Asking the store afterwards makes a key active if it needs to be.
pub async fn import(db: &Db, file: &Path, actor: &str) -> Result<KeyState> {
    refuse_panel()?;
    offered(&db.key_state())?;
    let named = offered(&crypto::state(db).await?)?;

    let text = read_recovery_file(file)?;
    let recovery = parse(&text)?;
    let key = usable_key(db, &recovery, &named).await?;
    let hex = Zeroizing::new(key.to_hex());
    secrets::add_key(&crypto::account(key.key_id()), &hex).await?;

    let mut write = db.pool.begin().await?;
    // The file exists, so this key needs no reminder either.
    mark_exported(&mut write, key.key_id()).await?;
    audit::record_to(
        &mut *write,
        actor,
        "recovery_key_imported",
        Some(key.key_id()),
        json!({ "key_id": key.key_id() }),
    )
    .await?;
    write.commit().await?;
    crypto::state(db).await
}

/// The key id a locked state names, which is the only reason to import or discard. The
/// cached state is checked first, so a handle that never asked starts neither. Callers
/// then check what the store says now, as the cache can be out of date.
fn offered(state: &KeyState) -> Result<String> {
    match state {
        KeyState::KeyMissing { key_id } | KeyState::KeyMismatch { key_id } => Ok(key_id.clone()),
        KeyState::NotLoaded => Err(Error::Refused("the key has not been checked yet".into())),
        KeyState::Unlocked { .. } => Err(Error::Refused("the key is already unlocked".into())),
        KeyState::KeyUnavailable { .. } => Err(Error::Refused(
            "the key store could not answer, so try again".into(),
        )),
    }
}

/// Never echoes `file` or what is in it: a `serde_json` message can quote its input.
fn parse(text: &[u8]) -> Result<RecoveryFile<'_>> {
    let parsed = std::str::from_utf8(text)
        .ok()
        .and_then(|text| serde_json::from_str::<RecoveryFile>(text).ok());
    match parsed {
        Some(file) if file.format == RECOVERY_FORMAT => Ok(file),
        _ => Err(Error::Validation(
            "this is not a JobPilot recovery file".into(),
        )),
    }
}

/// The file's key, if it would leave this root with a usable active key: with ciphertext
/// about it must be a key that holds some, and with none, the key the state names.
async fn usable_key(db: &Db, recovery: &RecoveryFile<'_>, named: &str) -> Result<DataKey> {
    let key_id = recovery.key_id;
    let held = crypto::held_key_ids(db).await?;
    let usable = match held.is_empty() {
        true => key_id == named,
        false => held.iter().any(|id| id == key_id),
    };
    let proof = match usable {
        true => crypto::proof_of(db, key_id).await?,
        false => None,
    };
    proof
        .and_then(|proof| crypto::proven_key(key_id, recovery.key_hex, &proof))
        .ok_or_else(|| Error::KeyMismatch {
            key_id: named.to_owned(),
        })
}

/// An existing regular file, within the limit. `O_NONBLOCK` is for the open itself: on a
/// FIFO it would otherwise wait for a writer, and the handle check would never run.
fn read_recovery_file(file: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(file)?;
    require_regular(&file)?;

    let too_large = Error::TooLarge {
        limit: MAX_FILE_BYTES,
    };
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(too_large);
    }
    // Sized up front, so the buffer never moves and leaves a copy behind to be zeroed.
    let mut text = Zeroizing::new(Vec::with_capacity(MAX_FILE_BYTES as usize + 2));
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut text)?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large);
    }
    Ok(text)
}

/// Gives up the fields under the key the state names, from `KeyMissing` or `KeyMismatch`
/// only. `confirm_key_id` must be that key id. It names a held id whenever ciphertext
/// exists, and an id with a proven key is never named, so readable fields stay.
///
/// A `pre-discard` snapshot goes first, so a recovery file found later can still open
/// what was cleared, and the old keychain entry stays for the same reason. A new key is
/// made only when no ciphertext is left: otherwise the next state read settles what
/// remains, and `new_key_id` is null in the audit row.
pub async fn discard_locked(db: &Db, confirm_key_id: &str, actor: &str) -> Result<KeyState> {
    refuse_panel()?;
    offered(&db.key_state())?;
    let active = crypto::active_key_id(&db.pool).await?;
    let old = offered(&crypto::state(db).await?)?;
    if confirm_key_id != old {
        return Err(Error::ConfirmationRequired);
    }

    let others = other_ciphertext(&db.pool, &old).await?;
    db::snapshot(db.paths(), &db.pool, "pre-discard").await?;
    let new = match others {
        true => None,
        false => Some(crypto::stored_new_key().await?),
    };

    let mut write = db.pool.begin().await?;
    let same_active = crypto::active_key_id(&mut *write).await? == active;
    if !same_active || other_ciphertext(&mut *write, &old).await? != others {
        write.rollback().await?;
        if let Some((key, _)) = &new {
            // No row has ever named it, so its removal must not hide this refusal.
            let name = crypto::account(key.key_id());
            let _ = secrets::remove_all(vec![name]).await;
        }
        return Err(Error::Refused(
            "the key changed during the discard, and nothing was cleared".into(),
        ));
    }

    // ponytail: only work_authorization holds ciphertext so far. Task 7 adds knowledge
    // marked bodies here, in `crypto::held_key_ids`, and in `other_ciphertext`.
    sqlx::query("DELETE FROM work_authorization WHERE key_id = ?")
        .bind(&old)
        .execute(&mut *write)
        .await?;

    let new_key_id = new.as_ref().map(|(key, _)| key.key_id().to_owned());
    match &new {
        Some((key, key_check)) => {
            sqlx::query("UPDATE data_keys SET active = 0")
                .execute(&mut *write)
                .await?;
            sqlx::query(
                "INSERT INTO data_keys (key_id, created_at, key_check, active) \
                 VALUES (?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?, 1)",
            )
            .bind(key.key_id())
            .bind(key_check)
            .execute(&mut *write)
            .await?;
            audit::record_to(
                &mut *write,
                actor,
                "data_key_created",
                Some(key.key_id()),
                json!({ "key_id": key.key_id() }),
            )
            .await?;
        }
        None => {
            sqlx::query("UPDATE data_keys SET active = 0 WHERE key_id = ?")
                .bind(&old)
                .execute(&mut *write)
                .await?;
        }
    }
    audit::record_to(
        &mut *write,
        actor,
        "locked_fields_discarded",
        Some(&old),
        json!({ "old_key_id": old, "new_key_id": new_key_id }),
    )
    .await?;
    write.commit().await?;
    crypto::state(db).await
}

async fn other_ciphertext(executor: impl sqlx::SqliteExecutor<'_>, key_id: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM work_authorization WHERE key_id <> ?)")
            .bind(key_id)
            .fetch_one(executor)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(state: KeyState) -> bool {
        matches!(offered(&state), Err(Error::Refused(_)))
    }

    #[test]
    fn only_a_lost_or_wrong_key_offers_import_and_discard() {
        let key_id = || "k".to_owned();
        assert!(refused(KeyState::NotLoaded));
        assert!(refused(KeyState::Unlocked { key_id: key_id() }));
        assert!(refused(KeyState::KeyUnavailable {
            key_id: key_id(),
            reason: "locked".into(),
        }));
        let missing = KeyState::KeyMissing { key_id: key_id() };
        assert_eq!(offered(&missing).unwrap(), "k");
        let mismatch = KeyState::KeyMismatch { key_id: key_id() };
        assert_eq!(offered(&mismatch).unwrap(), "k");
    }

    fn hunter_files() -> Vec<String> {
        vec![
            "hunter2".to_owned(),
            r#"{"format":"hunter2"}"#.to_owned(),
            r#"{"format":2,"key_id":"hunter2","created_at":"t","key_hex":"hunter2"}"#.to_owned(),
            r#"{"format":1,"key_id":"hunter2"}"#.to_owned(),
            r#"{"format":1,"key_id":7,"created_at":"t","key_hex":"hunter2"}"#.to_owned(),
        ]
    }

    #[test]
    fn a_file_that_cannot_be_read_never_reaches_a_message() {
        for text in hunter_files() {
            let error = parse(text.as_bytes()).err().unwrap();
            assert!(matches!(error, Error::Validation(_)), "{text}");
            assert!(!format!("{error} {error:?}").contains("hunter2"), "{text}");
        }
        assert!(parse(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn the_size_limit_holds_on_both_sides() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("key.json");

        fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize]).unwrap();
        assert_eq!(read_recovery_file(&path).unwrap().len(), 4096);

        fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        let result = read_recovery_file(&path);
        assert!(matches!(result, Err(Error::TooLarge { limit: 4096 })));
    }

    #[test]
    fn a_folder_or_a_missing_file_is_not_a_recovery_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_recovery_file(&tmp.path().join("missing.json")).is_err());
        assert!(read_recovery_file(tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn import_opens_no_symlink_device_or_fifo() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.json");
        fs::write(&real, "{}").unwrap();
        let link = tmp.path().join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(read_recovery_file(&link).is_err(), "not followed");

        let result = read_recovery_file(Path::new("/dev/null"));
        assert!(matches!(result, Err(Error::Refused(_))));

        let fifo = tmp.path().join("fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.unwrap().success());
        let result = read_recovery_file(&fifo);
        assert!(
            matches!(result, Err(Error::Refused(_))),
            "and it did not hang"
        );
    }
}
