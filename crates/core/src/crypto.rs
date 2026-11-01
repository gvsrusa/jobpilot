//! The data key and AES-256-GCM field encryption (D6). A bug here makes the owner's
//! sensitive fields unreadable, so these rules are fixed:
//!
//! - `Db::open` never touches the keychain. The key is read, or the first one made, on
//!   the first sensitive read or write, so plain work never waits on a system prompt.
//!   Until then the state is `NotLoaded`, which blocks nothing that is not sensitive.
//! - A key is created only while `data_keys` and every ciphertext column are empty. Any
//!   row there, active or not, may have data tied to it, and a second key would strand it.
//!   Discard of locked fields (`recovery`) is the one exception.
//! - Only an explicit "no such entry" from the store is `KeyMissing`. Anything else the
//!   store says is `KeyUnavailable`, which is temporary: a locked keychain must never
//!   read as a lost one, because the user may discard data over a lost key.
//! - A value that fails the key-check is `KeyMismatch`, and the entry is left as it is.
//! - Discard is offered only while some ciphertext has a key id no proven key opens. So
//!   every key id that holds ciphertext is proven before a locked state is reported.
//!
//! The key lives only in the keychain. This module holds it for one call at a time.

use std::future::Future;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, Generate, Key, KeyInit, Nonce, Payload};
use serde::Serialize;
use serde_json::json;
use sqlx::sqlite::SqliteExecutor;
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

/// The table and column in the associated data of a `work_authorization` value.
pub(crate) const AUTHORIZATION_AAD: (&str, &str) = ("work_authorization", "row");
/// The table and column in the associated data of a marked knowledge body.
pub(crate) const KNOWLEDGE_AAD: (&str, &str) = ("knowledge_items", "body_enc");

/// What shows that a stored value is a key id's key.
pub(crate) enum Proof {
    /// `data_keys.key_check`.
    KeyCheck(Vec<u8>),
    /// Ciphertext under an id with no key row: a trial decrypt stands in for the key-check.
    Sealed {
        table: &'static str,
        column: &'static str,
        row_id: String,
        ciphertext: Vec<u8>,
    },
}

impl Proof {
    fn holds(&self, key: &DataKey) -> bool {
        match self {
            Proof::KeyCheck(key_check) => key.opens(key_check),
            Proof::Sealed {
                table,
                column,
                row_id,
                ciphertext,
            } => key.decrypt(table, column, row_id, ciphertext).is_ok(),
        }
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
    found.map_err(state_error)
}

fn state_error(state: KeyState) -> Error {
    match state {
        KeyState::KeyUnavailable { reason, .. } => Error::KeyUnavailable(reason),
        KeyState::KeyMissing { key_id } => Error::KeyMissing { key_id },
        KeyState::KeyMismatch { key_id } => Error::KeyMismatch { key_id },
        KeyState::Unlocked { .. } | KeyState::NotLoaded => Error::Locked,
    }
}

/// The key a ciphertext header names, for one open. Two proven ids can exist, and
/// [`unlocked`] returns only one of them.
pub(crate) async fn key_by_id(db: &Db, key_id: &str) -> Result<DataKey> {
    let Some(proof) = proof_of(db, key_id).await? else {
        return Err(Error::KeyMissing {
            key_id: key_id.to_owned(),
        });
    };
    let lookup = key_id.to_owned();
    let found = secrets::keychain(move || Ok(read_key(&lookup, &proof))).await?;
    found.map_err(state_error)
}

/// The state a key would have on this machine, with no `Db`. Blocks on the keychain,
/// so async callers run it in `spawn_blocking`.
pub fn key_state_of(key_id: &str, key_check: &[u8]) -> KeyState {
    state_of(&read_key(key_id, &Proof::KeyCheck(key_check.to_vec())))
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

/// `None` only when this root holds no key row and no ciphertext.
async fn existing_key(db: &Db) -> Result<Option<Found>> {
    let keys: Vec<(String, Vec<u8>, bool)> = sqlx::query_as(
        "SELECT key_id, key_check, active = 1 FROM data_keys ORDER BY created_at DESC, key_id",
    )
    .fetch_all(&db.pool)
    .await?;
    let held = held_key_ids(db).await?;
    if keys.is_empty() && held.is_empty() {
        return Ok(None);
    }

    let active = keys.iter().find(|key| key.2);
    let active = active.map(|key| (key.0.clone(), key.1.clone()));
    match active {
        Some((key_id, key_check)) if held.iter().all(|id| *id == key_id) => {
            let found = read_active(key_id.clone(), Proof::KeyCheck(key_check)).await;
            // With no ciphertext, a lost active key strands nothing, so a readable key
            // listed beside it takes over. Unavailable says nothing about the key, so it
            // stays. When none can, the state still names the active key.
            let lost = matches!(
                found,
                Err(KeyState::KeyMissing { .. } | KeyState::KeyMismatch { .. })
            );
            if lost && held.is_empty() && keys.len() > 1 {
                let stand_in = prove_every_key(db, keys, held, Some(key_id)).await?;
                if stand_in.is_ok() {
                    return Ok(Some(stand_in));
                }
            }
            Ok(Some(found))
        }
        active => {
            let active = active.map(|(key_id, _)| key_id);
            Ok(Some(prove_every_key(db, keys, held, active).await?))
        }
    }
}

/// The key ids that hold ciphertext.
// ponytail: work_authorization and marked knowledge bodies hold ciphertext. Every later
// ciphertext column joins this query, `sample_of`, the check in `add_first_key` and
// `reactivate`, and the delete and check in `recovery::discard_locked`.
pub(crate) async fn held_key_ids(db: &Db) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT key_id FROM work_authorization \
         UNION SELECT key_id FROM knowledge_items WHERE body_enc IS NOT NULL \
         ORDER BY key_id",
    )
    .fetch_all(&db.pool)
    .await?)
}

/// What can prove `key_id`: its key-check, or with no key row, a value sealed under it.
pub(crate) async fn proof_of(db: &Db, key_id: &str) -> Result<Option<Proof>> {
    let key_check: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT key_check FROM data_keys WHERE key_id = ?")
            .bind(key_id)
            .fetch_optional(&db.pool)
            .await?;
    if let Some(key_check) = key_check {
        return Ok(Some(Proof::KeyCheck(key_check)));
    }
    sample_of(db, key_id).await
}

/// One ciphertext is enough: an AES-GCM tag that passes proves the key.
async fn sample_of(db: &Db, key_id: &str) -> Result<Option<Proof>> {
    let row: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT country, ciphertext FROM work_authorization WHERE key_id = ? LIMIT 1",
    )
    .bind(key_id)
    .fetch_optional(&db.pool)
    .await?;
    if let Some((row_id, ciphertext)) = row {
        let (table, column) = AUTHORIZATION_AAD;
        return Ok(Some(Proof::Sealed {
            table,
            column,
            row_id,
            ciphertext,
        }));
    }
    let row: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT id, body_enc FROM knowledge_items \
         WHERE key_id = ? AND body_enc IS NOT NULL LIMIT 1",
    )
    .bind(key_id)
    .fetch_optional(&db.pool)
    .await?;
    let (table, column) = KNOWLEDGE_AAD;
    Ok(row.map(|(row_id, ciphertext)| Proof::Sealed {
        table,
        column,
        row_id,
        ciphertext,
    }))
}

/// The newest active key id. Reads no key.
pub(crate) async fn active_key_id(executor: impl SqliteExecutor<'_>) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT key_id FROM data_keys WHERE active = 1 ORDER BY created_at DESC, key_id LIMIT 1",
    )
    .fetch_optional(executor)
    .await?)
}

/// The usual case: one key, holding all the ciphertext there is.
async fn read_active(key_id: String, proof: Proof) -> Found {
    let lookup = key_id.clone();
    match secrets::keychain(move || Ok(read_key(&lookup, &proof))).await {
        Ok(found) => found,
        Err(error) => Err(KeyState::KeyUnavailable {
            key_id,
            reason: error.to_string(),
        }),
    }
}

/// No key is active, as Task 5's restore can leave, or ciphertext sits under another id,
/// or a row was damaged. Rows exist, so no key is made here: the store classifies what is
/// left, in [`settle`], and every held id gets its chance to be proven first.
async fn prove_every_key(
    db: &Db,
    keys: Vec<(String, Vec<u8>, bool)>,
    held: Vec<String>,
    active: Option<String>,
) -> Result<Found> {
    let mut probes: Vec<(String, Proof)> = keys
        .into_iter()
        .map(|(key_id, key_check, _)| (key_id, Proof::KeyCheck(key_check)))
        .collect();
    for key_id in &held {
        if !probes.iter().any(|(id, _)| id == key_id) {
            // Nothing to try, if the row went since: this empty key-check opens nothing.
            let proof = sample_of(db, key_id).await?;
            let proof = proof.unwrap_or(Proof::KeyCheck(Vec::new()));
            probes.push((key_id.clone(), proof));
        }
    }

    let stop_at_a_key = held.is_empty();
    let read = secrets::keychain(move || {
        let mut found = Vec::new();
        for (key_id, proof) in &probes {
            let read = read_key(key_id, proof);
            let done = stop_at_a_key && read.is_ok();
            found.push(read);
            if done {
                break;
            }
        }
        Ok(found)
    })
    .await;
    let found = match read {
        Ok(found) => found,
        Err(error) => return Ok(Err(no_key_yet(error.to_string()))),
    };

    match settle(found, &held, active.as_deref()) {
        Ok(key) if active.as_deref() == Some(key.key_id()) => Ok(Ok(key)),
        Ok(key) => reactivate(db, key, active.as_deref(), &held).await,
        Err(state) => Ok(Err(state)),
    }
}

/// `Ok` is the key to use, and it means every held id is proven, so nothing is locked: it
/// is the active key if that holds ciphertext, else the newest that does. With no
/// ciphertext, any readable key will do.
///
/// `Err` means a held id has no proven key, and that is the only state that offers
/// discard. It names the held id, because discard clears what is under the id it names.
/// A store error outranks the rest, because it says nothing about the key: `KeyMissing`
/// needs a clean "no such entry". A key that is proven beside a held id that is not makes
/// that id a mismatch, and an entry that fails its proof is a mismatch too.
fn settle(found: Vec<Found>, held: &[String], active: Option<&str>) -> Found {
    let (mut readable, mut failed, mut unavailable) = (Vec::new(), Vec::new(), None);
    for read in found {
        match read {
            Ok(key) => readable.push(key),
            Err(state @ KeyState::KeyUnavailable { .. }) => {
                unavailable = unavailable.or(Some(state))
            }
            Err(state) => failed.push(state),
        }
    }

    let wanted = |key_id: &str| held.is_empty() || held.iter().any(|id| id == key_id);
    let unproven = held
        .iter()
        .find(|id| !readable.iter().any(|key| key.key_id() == id.as_str()));
    if unproven.is_none() {
        let usable = |key: &DataKey| wanted(key.key_id());
        let chosen = readable
            .iter()
            .position(|key| usable(key) && Some(key.key_id()) == active)
            .or_else(|| readable.iter().position(usable));
        if let Some(chosen) = chosen {
            return Ok(readable.swap_remove(chosen));
        }
    }

    if let Some(state) = unavailable {
        return Err(state);
    }
    if let (Some(key_id), false) = (unproven, readable.is_empty()) {
        return Err(KeyState::KeyMismatch {
            key_id: key_id.clone(),
        });
    }
    failed.retain(|state| wanted(named(state)));
    let first = failed
        .iter()
        .position(|state| matches!(state, KeyState::KeyMismatch { .. }));
    match (first, failed.is_empty()) {
        (_, true) => Err(no_key_yet("no key was read".into())),
        (first, false) => Err(failed.swap_remove(first.unwrap_or(0))),
    }
}

fn named(state: &KeyState) -> &str {
    match state {
        KeyState::NotLoaded => "",
        KeyState::Unlocked { key_id }
        | KeyState::KeyUnavailable { key_id, .. }
        | KeyState::KeyMissing { key_id }
        | KeyState::KeyMismatch { key_id } => key_id,
    }
}

/// Makes a proven key the active one. Every condition is part of the update, so a handle
/// that lost a race changes nothing. A key proven by a trial decrypt has no row, so it
/// gains one here, and with it a key-check.
async fn reactivate(db: &Db, key: DataKey, active: Option<&str>, held: &[String]) -> Result<Found> {
    let key_check = key.encrypt("data_keys", "key_check", key.key_id(), key_check_text())?;
    let mut write = db.pool.begin().await?;
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) \
         VALUES (?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?, 0) \
         ON CONFLICT(key_id) DO NOTHING",
    )
    .bind(key.key_id())
    .bind(key_check)
    .execute(&mut *write)
    .await?;
    let updated = sqlx::query(
        "UPDATE data_keys SET active = (key_id = ?) \
         WHERE (SELECT key_id FROM data_keys WHERE active = 1 \
                ORDER BY created_at DESC, key_id LIMIT 1) IS ? \
         AND NOT EXISTS (SELECT 1 FROM work_authorization \
                         WHERE key_id NOT IN (SELECT value FROM json_each(?3))) \
         AND NOT EXISTS (SELECT 1 FROM knowledge_items WHERE body_enc IS NOT NULL \
                         AND key_id NOT IN (SELECT value FROM json_each(?3)))",
    )
    .bind(key.key_id())
    .bind(active)
    .bind(json!(held).to_string())
    .execute(&mut *write)
    .await?
    .rows_affected()
        > 0;
    if !updated {
        write.rollback().await?;
        return Ok(match active_key_id(&db.pool).await? {
            Some(key_id) if key_id == key.key_id() => Ok(key),
            _ => Err(no_key_yet("the key rows changed during the read".into())),
        });
    }
    let detail = json!({ "key_id": key.key_id() });
    audit::record_to(
        &mut *write,
        "system",
        "data_key_reactivated",
        Some(key.key_id()),
        detail,
    )
    .await?;
    write.commit().await?;
    Ok(Ok(key))
}

fn read_key(key_id: &str, proof: &Proof) -> Found {
    classify(key_id, secrets::get(&account(key_id)), proof)
}

/// What the store's answer means. Only `Ok(None)`, an explicit "no such entry", is
/// `KeyMissing`. Telling that from an error is what keeps a locked keychain from
/// offering to discard the user's data.
fn classify(key_id: &str, stored: Result<Option<String>>, proof: &Proof) -> Found {
    let stored = match stored {
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
    proven_key(key_id, &stored, proof).ok_or_else(|| KeyState::KeyMismatch {
        key_id: key_id.to_owned(),
    })
}

/// The key `hex` encodes, if `proof` says it is `key_id`'s.
pub(crate) fn proven_key(key_id: &str, hex: &str, proof: &Proof) -> Option<DataKey> {
    let key = DataKey::from_bytes(key_id, parse_key(hex)?);
    proof.holds(&key).then_some(key)
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
         AND NOT EXISTS (SELECT 1 FROM work_authorization) \
         AND NOT EXISTS (SELECT 1 FROM knowledge_items WHERE body_enc IS NOT NULL)",
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

/// A key for a root that gives up its old one, stored under a new id, so the write
/// replaces nothing. With its key-check, for the row the caller adds. Discard only.
pub(crate) async fn stored_new_key() -> Result<(DataKey, Vec<u8>)> {
    let key = DataKey::generate()?;
    let key_check = key.encrypt("data_keys", "key_check", key.key_id(), key_check_text())?;
    let hex = Zeroizing::new(key.to_hex());
    secrets::store_key(&account(key.key_id()), &hex).await?;
    Ok((key, key_check))
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

pub(crate) fn account(key_id: &str) -> String {
    format!("{KEY_ACCOUNT_PREFIX}{key_id}")
}

fn aad(table: &str, column: &str, row_id: &str, key_id: &str) -> String {
    format!("{table}\x1f{column}\x1f{row_id}\x1f{key_id}")
}

pub(crate) fn is_key_id(id: &[u8]) -> bool {
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

    const KEY_ID: &str = "00112233aabbccdd";
    const OTHER_KEY_ID: &str = "99887766ffeeddcc";

    fn key_check(key: &DataKey) -> Vec<u8> {
        key.encrypt("data_keys", "key_check", key.key_id(), key_check_text())
            .unwrap()
    }

    fn unavailable(key_id: &str) -> KeyState {
        KeyState::KeyUnavailable {
            key_id: key_id.into(),
            reason: "locked".into(),
        }
    }

    fn missing(key_id: &str) -> KeyState {
        KeyState::KeyMissing {
            key_id: key_id.into(),
        }
    }

    fn mismatch(key_id: &str) -> KeyState {
        KeyState::KeyMismatch {
            key_id: key_id.into(),
        }
    }

    fn state_after(found: Vec<Found>, held: &[String]) -> KeyState {
        state_of(&settle(found, held, None))
    }

    fn check_of(key: &DataKey) -> Proof {
        Proof::KeyCheck(key_check(key))
    }

    #[test]
    fn a_store_error_is_unavailable_and_only_no_entry_is_missing() {
        let key = DataKey::from_bytes(KEY_ID, [7; 32]);
        let check = check_of(&key);

        let error = classify(KEY_ID, Err(Error::Secret("locked".into())), &check);
        let KeyState::KeyUnavailable { key_id, reason } = state_of(&error) else {
            panic!("{:?}", state_of(&error));
        };
        assert_eq!(key_id, KEY_ID);
        assert!(reason.contains("locked"), "{reason}");

        assert_eq!(
            state_of(&classify(KEY_ID, Ok(None), &check)),
            missing(KEY_ID)
        );

        let wrong = Ok(Some("ab".repeat(32)));
        assert_eq!(state_of(&classify(KEY_ID, wrong, &check)), mismatch(KEY_ID));
        let garbage = Ok(Some("hunter2".to_owned()));
        assert_eq!(
            state_of(&classify(KEY_ID, garbage, &check)),
            mismatch(KEY_ID)
        );

        let right = classify(KEY_ID, Ok(Some(key.to_hex())), &check)
            .ok()
            .unwrap();
        assert_eq!(right.key_id(), KEY_ID);
    }

    // An empty key-check stands in when nothing is left to try, so a real entry is not
    // a key we can trust, and must not read as missing.
    #[test]
    fn a_stored_value_with_no_key_check_is_a_mismatch() {
        let key = DataKey::from_bytes(KEY_ID, [7; 32]);
        let read = classify(KEY_ID, Ok(Some(key.to_hex())), &Proof::KeyCheck(Vec::new()));
        assert_eq!(state_of(&read), mismatch(KEY_ID));
    }

    #[test]
    fn a_trial_decrypt_proves_the_key_of_the_row_it_was_sealed_for() {
        let key = DataKey::from_bytes(KEY_ID, [7; 32]);
        let sealed = key
            .encrypt("work_authorization", "row", "PT", b"{}")
            .unwrap();
        let proof = |row_id: &str| Proof::Sealed {
            table: AUTHORIZATION_AAD.0,
            column: AUTHORIZATION_AAD.1,
            row_id: row_id.into(),
            ciphertext: sealed.clone(),
        };

        let read = classify(KEY_ID, Ok(Some(key.to_hex())), &proof("PT"));
        assert_eq!(read.ok().unwrap().key_id(), KEY_ID);

        let moved = classify(KEY_ID, Ok(Some(key.to_hex())), &proof("ES"));
        assert_eq!(state_of(&moved), mismatch(KEY_ID));
        let wrong = classify(KEY_ID, Ok(Some("ab".repeat(32))), &proof("PT"));
        assert_eq!(state_of(&wrong), mismatch(KEY_ID));
    }

    #[test]
    fn a_readable_key_with_no_ciphertext_is_to_be_reactivated() {
        let found = vec![
            Err(unavailable(OTHER_KEY_ID)),
            Ok(DataKey::from_bytes(KEY_ID, [7; 32])),
        ];
        let key = settle(found, &[], None).ok().unwrap();
        assert_eq!(key.key_id(), KEY_ID);
    }

    #[test]
    fn every_held_id_proven_is_a_key_whatever_else_the_store_says() {
        let held = [KEY_ID.to_owned()];
        let found = vec![
            Err(unavailable(OTHER_KEY_ID)),
            Ok(DataKey::from_bytes(KEY_ID, [7; 32])),
        ];
        let key = settle(found, &held, Some(OTHER_KEY_ID)).ok().unwrap();
        assert_eq!(key.key_id(), KEY_ID);
    }

    #[test]
    fn the_active_key_stays_when_it_holds_ciphertext_and_only_then() {
        let found = || {
            vec![
                Ok(DataKey::from_bytes(OTHER_KEY_ID, [7; 32])),
                Ok(DataKey::from_bytes(KEY_ID, [7; 32])),
            ]
        };
        let both = [KEY_ID.to_owned(), OTHER_KEY_ID.to_owned()];
        let one = [OTHER_KEY_ID.to_owned()];

        let key = settle(found(), &both, Some(KEY_ID)).ok().unwrap();
        assert_eq!(key.key_id(), KEY_ID);
        let key = settle(found(), &one, Some(KEY_ID)).ok().unwrap();
        assert_eq!(key.key_id(), OTHER_KEY_ID);
    }

    #[test]
    fn a_store_error_outranks_every_other_answer_without_an_active_key() {
        let held = [KEY_ID.to_owned()];
        for (found, held) in [
            (vec![Err(unavailable(KEY_ID))], &[][..]),
            (
                vec![Err(missing(OTHER_KEY_ID)), Err(unavailable(KEY_ID))],
                &[][..],
            ),
            (
                vec![Err(unavailable(KEY_ID)), Err(missing(OTHER_KEY_ID))],
                &held[..],
            ),
            (
                vec![Err(mismatch(OTHER_KEY_ID)), Err(unavailable(KEY_ID))],
                &held[..],
            ),
            (
                vec![
                    Ok(DataKey::from_bytes(OTHER_KEY_ID, [7; 32])),
                    Err(unavailable(KEY_ID)),
                ],
                &held[..],
            ),
        ] {
            assert_eq!(state_after(found, held), unavailable(KEY_ID));
        }
    }

    #[test]
    fn missing_needs_a_clean_no_entry_for_every_id() {
        let held = [KEY_ID.to_owned()];
        assert_eq!(
            state_after(
                vec![Err(missing(OTHER_KEY_ID)), Err(missing(KEY_ID))],
                &held
            ),
            missing(KEY_ID)
        );
        assert_eq!(
            state_after(
                vec![Err(missing(OTHER_KEY_ID)), Err(mismatch(KEY_ID))],
                &held
            ),
            mismatch(KEY_ID)
        );
    }

    // Discard clears what is under the id it names, so the name is always a held id.
    #[test]
    fn a_locked_state_names_a_held_id_over_a_newer_row() {
        let held = [KEY_ID.to_owned()];
        assert_eq!(
            state_after(
                vec![Err(mismatch(OTHER_KEY_ID)), Err(missing(KEY_ID))],
                &held
            ),
            missing(KEY_ID)
        );
    }

    #[test]
    fn a_readable_key_beside_ciphertext_is_never_reactivated() {
        let held = [OTHER_KEY_ID.to_owned()];
        let found = vec![Ok(DataKey::from_bytes(KEY_ID, [7; 32]))];
        assert_eq!(state_after(found, &held), mismatch(OTHER_KEY_ID));
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
