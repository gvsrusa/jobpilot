//! The data key and AES-256-GCM field encryption (D6). A bug here makes the owner's
//! sensitive fields unreadable, so these rules are fixed:
//!
//! - `Db::open` never touches the keychain. The key is read, or the first one made, on
//!   the first sensitive read or write, so plain work never waits on a system prompt.
//!   Until then the state is `NotLoaded`, which blocks nothing that is not sensitive.
//! - A key is created only while `data_keys` and every ciphertext column are empty. Any
//!   row there, active or not, may have data tied to it, and a second key would strand it.
//! - Only an explicit "no such entry" from the store is `KeyMissing`. Anything else the
//!   store says is `KeyUnavailable`, which is temporary: a locked keychain must never
//!   read as a lost one, because the user may discard data over a lost key.
//! - A value that fails the key-check is `KeyMismatch`, and the entry is left as it is.
//!
//! The key lives only in the keychain. This module holds it for one call at a time.

use std::future::Future;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, Generate, Key, KeyInit, Nonce, Payload};
use serde::Serialize;
use serde_json::json;
use zeroize::{Zeroize, Zeroizing};

use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::secrets;

/// Keychain account for a key: `data-key-<key_id>`.
pub const KEY_ACCOUNT_PREFIX: &str = "data-key-";

const MAGIC: &[u8; 4] = b"JPK1";
const KEY_ID_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const NONCE_START: usize = MAGIC.len() + KEY_ID_LEN;
const BODY_START: usize = NONCE_START + NONCE_LEN;

/// `key_id` is empty only when the first key could not be stored: no key exists yet.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeyState {
    /// Nothing has asked for the key on this handle. Says nothing about the key, so it
    /// offers neither import nor discard.
    #[serde(rename = "key_not_loaded")]
    NotLoaded,
    Unlocked {
        key_id: String,
    },
    KeyUnavailable {
        key_id: String,
        reason: String,
    },
    KeyMissing {
        key_id: String,
    },
    KeyMismatch {
        key_id: String,
    },
}

/// The key, or the state that says why there is none.
type Found = std::result::Result<DataKey, KeyState>;

/// An AES-256 key, zeroed on drop. It has no `Debug` and no `Serialize`, so it cannot
/// reach a log, an IPC reply, or an export by accident:
///
/// ```compile_fail,E0277
/// fn debug<T: std::fmt::Debug>() {}
/// debug::<jobpilot_core::crypto::DataKey>();
/// ```
///
/// ```compile_fail,E0277
/// fn serialize<T: serde::Serialize>() {}
/// serialize::<jobpilot_core::crypto::DataKey>();
/// ```
// ponytail: the zeroing covers this struct only. The cipher's key schedule is not
// zeroed, as aes-gcm's `zeroize` feature is off, and a same-user process reading this
// one's memory is out of scope in the threat model. Turn it on if that ever changes.
pub struct DataKey {
    key_id: String,
    key: [u8; 32],
}

impl Drop for DataKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl DataKey {
    /// For tests and recovery import. A key id the header cannot hold fails at `encrypt`.
    #[doc(hidden)]
    pub fn from_bytes(key_id: &str, key: [u8; 32]) -> DataKey {
        DataKey {
            key_id: key_id.to_owned(),
            key,
        }
    }

    fn generate() -> Result<DataKey> {
        let mut generated: Key<Aes256Gcm> = random()?;
        let mut key = [0; 32];
        key.copy_from_slice(&generated);
        generated.as_mut_slice().zeroize();
        let id: [u8; KEY_ID_LEN / 2] = random()?;
        Ok(DataKey {
            key_id: to_hex(&id),
            key,
        })
    }

    /// Recovery export and the keychain only. Callers zero what they get.
    pub(crate) fn to_hex(&self) -> String {
        to_hex(&self.key)
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Layout: `b"JPK1"` | key id (16 ascii hex) | nonce (12) | AES-GCM body and tag.
    /// The nonce is new for every call.
    pub fn encrypt(
        &self,
        table: &str,
        column: &str,
        row_id: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        if !is_key_id(self.key_id.as_bytes()) {
            return Err(Error::Validation(
                "this key id does not fit a ciphertext header".into(),
            ));
        }
        let nonce: Nonce<Aes256Gcm> = random()?;
        let aad = aad(table, column, row_id, &self.key_id);
        let payload = Payload {
            msg: plaintext,
            aad: aad.as_bytes(),
        };
        let body = self
            .cipher()
            .encrypt(&nonce, payload)
            .map_err(|_| Error::Validation("the value could not be encrypted".into()))?;

        let mut sealed = Vec::with_capacity(BODY_START + body.len());
        sealed.extend_from_slice(MAGIC);
        sealed.extend_from_slice(self.key_id.as_bytes());
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&body);
        Ok(sealed)
    }

    /// `KeyMismatch` names the key the ciphertext asks for, when that is another key.
    /// Damage, or a value moved from another row, is `Validation`.
    pub fn decrypt(
        &self,
        table: &str,
        column: &str,
        row_id: &str,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        if ciphertext.len() < BODY_START + TAG_LEN || !ciphertext.starts_with(MAGIC) {
            return Err(Error::Validation(
                "this is not a JobPilot ciphertext".into(),
            ));
        }
        let header_id = &ciphertext[MAGIC.len()..NONCE_START];
        if !is_key_id(header_id) {
            return Err(Error::Validation(
                "this is not a JobPilot ciphertext".into(),
            ));
        }
        if header_id != self.key_id.as_bytes() {
            return Err(Error::KeyMismatch {
                key_id: String::from_utf8_lossy(header_id).into_owned(),
            });
        }
        let nonce = Nonce::<Aes256Gcm>::try_from(&ciphertext[NONCE_START..BODY_START])
            .map_err(|_| Error::Validation("this is not a JobPilot ciphertext".into()))?;
        let aad = aad(table, column, row_id, &self.key_id);
        let payload = Payload {
            msg: &ciphertext[BODY_START..],
            aad: aad.as_bytes(),
        };
        self.cipher()
            .decrypt(&nonce, payload)
            .map_err(|_| Error::Validation("the ciphertext failed its check".into()))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new((&self.key).into())
    }

    fn opens(&self, key_check: &[u8]) -> bool {
        self.decrypt("data_keys", "key_check", &self.key_id, key_check)
            .is_ok_and(|text| text == key_check_text())
    }
}

/// Fixed text sealed under each key in `data_keys.key_check`.
pub fn key_check_text() -> &'static [u8] {
    b"jobpilot-key-check-v1"
}

/// Asks the store, making the first key if this root has none, and updates
/// [`Db::key_state`]. This is the first-use path and the Retry button: every call
/// asks again, so a first key that could not be stored is tried again here.
pub async fn state(db: &Db) -> Result<KeyState> {
    let found = resolve(db).await?;
    Ok(publish(db, &found))
}

/// The key, for one operation: the entry point of every seal and open. The key is read
/// from the keychain on every call and kept nowhere, so `Db` caches the state and no
/// secret, and a key removed from the store stops working.
pub async fn unlocked(db: &Db) -> Result<DataKey> {
    let found = resolve(db).await?;
    publish(db, &found);
    found.map_err(|state| match state {
        KeyState::KeyUnavailable { reason, .. } => Error::KeyUnavailable(reason),
        KeyState::KeyMissing { key_id } => Error::KeyMissing { key_id },
        KeyState::KeyMismatch { key_id } => Error::KeyMismatch { key_id },
        KeyState::Unlocked { .. } | KeyState::NotLoaded => Error::Locked,
    })
}

/// Makes this root's first key, audited as `data_key_created`. With a key already in
/// `data_keys` it makes nothing and behaves as [`unlocked`].
pub async fn create_first_key(db: &Db, actor: &str) -> Result<DataKey> {
    match first_key(db, actor).await? {
        Some(key) => Ok(key),
        None => unlocked(db).await,
    }
}

/// The state a key would have on this machine, with no `Db`. Blocks on the keychain,
/// so async callers run it in `spawn_blocking`.
pub fn key_state_of(key_id: &str, key_check: &[u8]) -> KeyState {
    state_of(&read_key(key_id, key_check))
}

async fn resolve(db: &Db) -> Result<Found> {
    if let Some(found) = existing_key(db).await? {
        return Ok(found);
    }
    let created = first_key(db, "system").await;
    after_first_key(db, created).await
}

async fn after_first_key(db: &Db, created: Result<Option<DataKey>>) -> Result<Found> {
    match created {
        Ok(Some(key)) => Ok(Ok(key)),
        Ok(None) => Ok(existing_key(db).await?.unwrap_or_else(|| {
            Err(no_key_yet(
                "the first key was removed while it was being made".into(),
            ))
        })),
        Err(Error::Secret(reason)) => Ok(Err(no_key_yet(reason))),
        Err(error) => Err(error),
    }
}

fn no_key_yet(reason: String) -> KeyState {
    KeyState::KeyUnavailable {
        key_id: String::new(),
        reason,
    }
}

/// `None` only when this root holds no key row and no ciphertext. With no active row
/// the newest key is named and reported missing: rows exist, so a new key must not be
/// made, and nothing says which entry would be right. Ciphertext with no key row takes
/// a damaged or hand-edited file, as the foreign key refuses it, and names its own key.
async fn existing_key(db: &Db) -> Result<Option<Found>> {
    let row: Option<(String, Vec<u8>, bool)> = sqlx::query_as(
        "SELECT key_id, key_check, active FROM data_keys \
         ORDER BY active DESC, created_at DESC, key_id LIMIT 1",
    )
    .fetch_optional(&db.pool)
    .await?;
    let Some((key_id, key_check, active)) = row else {
        let orphan: Option<String> =
            sqlx::query_scalar("SELECT key_id FROM work_authorization LIMIT 1")
                .fetch_optional(&db.pool)
                .await?;
        return Ok(orphan.map(|key_id| Err(KeyState::KeyMissing { key_id })));
    };
    if !active {
        return Ok(Some(Err(KeyState::KeyMissing { key_id })));
    }

    let lookup = key_id.clone();
    let read = secrets::keychain(move || Ok(read_key(&lookup, &key_check))).await;
    let key = match read {
        Ok(Ok(key)) => key,
        Ok(Err(state)) => return Ok(Some(Err(state))),
        Err(error) => {
            return Ok(Some(Err(KeyState::KeyUnavailable {
                key_id,
                reason: error.to_string(),
            })));
        }
    };

    // ponytail: only work_authorization holds ciphertext so far. Every later
    // ciphertext column joins this check.
    let foreign: Option<String> =
        sqlx::query_scalar("SELECT key_id FROM work_authorization WHERE key_id <> ? LIMIT 1")
            .bind(&key_id)
            .fetch_optional(&db.pool)
            .await?;
    Ok(Some(match foreign {
        Some(key_id) => Err(KeyState::KeyMismatch { key_id }),
        None => Ok(key),
    }))
}

fn read_key(key_id: &str, key_check: &[u8]) -> Found {
    let stored = match secrets::get(&account(key_id)) {
        Ok(Some(stored)) => Zeroizing::new(stored),
        Ok(None) => {
            return Err(KeyState::KeyMissing {
                key_id: key_id.to_owned(),
            });
        }
        Err(error) => {
            return Err(KeyState::KeyUnavailable {
                key_id: key_id.to_owned(),
                reason: error.to_string(),
            });
        }
    };
    match parse_key(&stored).map(|bytes| DataKey::from_bytes(key_id, bytes)) {
        Some(key) if key.opens(key_check) => Ok(key),
        _ => Err(KeyState::KeyMismatch {
            key_id: key_id.to_owned(),
        }),
    }
}

async fn first_key(db: &Db, actor: &str) -> Result<Option<DataKey>> {
    let key = DataKey::generate()?;
    let name = account(key.key_id());
    let hex = Zeroizing::new(key.to_hex());
    add_first_key(db, actor, key, secrets::store_key(&name, &hex)).await
}

/// The keychain goes first, so a row never names a key that was not stored. `None`
/// means another handle got there first, and its key is the one to use.
///
/// Two handles can both find this root empty, so the empty check is part of the
/// insert. The loser removes its own entry, whose id no row has ever named. After any
/// other failure the entry stays: a commit that reports failure may still have landed,
/// and an unused entry costs nothing while a removed one would lose the key.
///
/// Takes the store step as a future because a real keychain cannot fail on demand.
async fn add_first_key(
    db: &Db,
    actor: &str,
    key: DataKey,
    store: impl Future<Output = Result<()>>,
) -> Result<Option<DataKey>> {
    let key_check = key.encrypt("data_keys", "key_check", key.key_id(), key_check_text())?;
    store.await?;

    let mut write = db.pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) \
         SELECT ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?, 1 \
         WHERE NOT EXISTS (SELECT 1 FROM data_keys) \
         AND NOT EXISTS (SELECT 1 FROM work_authorization)",
    )
    .bind(key.key_id())
    .bind(key_check)
    .execute(&mut *write)
    .await?
    .rows_affected()
        == 1;
    if !inserted {
        write.rollback().await?;
        // An entry left behind is unused, so its removal must not hide the winner's key.
        let _ = secrets::remove_all(vec![account(key.key_id())]).await;
        return Ok(None);
    }
    let detail = json!({ "key_id": key.key_id() });
    audit::record_to(
        &mut *write,
        actor,
        "data_key_created",
        Some(key.key_id()),
        detail,
    )
    .await?;
    write.commit().await?;
    Ok(Some(key))
}

fn publish(db: &Db, found: &Found) -> KeyState {
    let state = state_of(found);
    db.set_key_state(state.clone());
    state
}

fn state_of(found: &Found) -> KeyState {
    match found {
        Ok(key) => KeyState::Unlocked {
            key_id: key.key_id.clone(),
        },
        Err(state) => state.clone(),
    }
}

fn account(key_id: &str) -> String {
    format!("{KEY_ACCOUNT_PREFIX}{key_id}")
}

fn aad(table: &str, column: &str, row_id: &str, key_id: &str) -> String {
    format!("{table}\x1f{column}\x1f{row_id}\x1f{key_id}")
}

fn is_key_id(id: &[u8]) -> bool {
    id.len() == KEY_ID_LEN && id.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A failed OS random source is reported as a store failure, so a first key that cannot
/// be made leaves the app open and locked instead of panicking at startup.
fn random<T: Generate>() -> Result<T> {
    T::try_generate().map_err(|_| Error::Secret("the system random source failed".into()))
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0xf)] as char);
    }
    text
}

fn parse_key(hex: &str) -> Option<[u8; 32]> {
    let digits = hex.as_bytes();
    if digits.len() != 64 {
        return None;
    }
    let mut key = [0; 32];
    let (pairs, _) = digits.as_chunks::<2>();
    for (byte, [high, low]) in key.iter_mut().zip(pairs) {
        let high = char::from(*high).to_digit(16)?;
        let low = char::from(*low).to_digit(16)?;
        *byte = (high * 16 + low) as u8;
    }
    Some(key)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::paths::AppPaths;
    use crate::{backup, documents, settings, transfer};

    #[test]
    fn hex_round_trips_and_refuses_what_is_not_hex() {
        let key = DataKey::from_bytes("00112233aabbccdd", [0xa5; 32]);
        assert_eq!(parse_key(&key.to_hex()), Some([0xa5; 32]));
        assert_eq!(parse_key(&"a".repeat(63)), None);
        assert_eq!(parse_key(&format!("+f{}", "a".repeat(62))), None);
        assert_eq!(parse_key(&"g".repeat(64)), None);
    }

    #[tokio::test]
    async fn failed_first_write_inserts_no_row() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        let db = Db::open(&paths).await.unwrap();

        let key = DataKey::generate().unwrap();
        let failing = async { Err(Error::Secret("locked".into())) };
        let created = add_first_key(&db, "system", key, failing).await;
        let found = after_first_key(&db, created).await.unwrap();

        assert_eq!(
            state_of(&found),
            KeyState::KeyUnavailable {
                key_id: String::new(),
                reason: "locked".into(),
            }
        );
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM data_keys")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert!(audit::recent(&db, 10).await.unwrap().is_empty());
        db.close().await;
    }

    /// Counts every route to the keychain, so it needs no keychain to run. No other test
    /// in this binary may reach `secrets::entry`: this one reads a process-wide count.
    #[tokio::test]
    async fn plain_work_makes_no_keychain_call() {
        let calls = || secrets::KEYCHAIN_CALLS.load(Ordering::SeqCst);
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        let before = calls();

        let db = Db::open(&paths).await.unwrap();
        let doc = documents::create(&db, "Title", "body").await.unwrap();
        documents::get(&db, &doc.id).await.unwrap();
        documents::search(&db, "body").await.unwrap();
        settings::set(&db, "k", &serde_json::json!("v"))
            .await
            .unwrap();
        backup::create(&db, "cli").await.unwrap();
        transfer::export_all(&db, "cli").await.unwrap();
        db.schema_version().await.unwrap();
        db.close().await;
        let db = Db::open(&paths).await.unwrap();

        assert_eq!(calls(), before);
        assert_eq!(db.key_state(), KeyState::NotLoaded);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM data_keys")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        db.close().await;

        // The seam itself: an empty name stops before the OS, but it passed through.
        assert!(secrets::get("").is_err());
        assert_eq!(calls(), before + 1);
    }
}
