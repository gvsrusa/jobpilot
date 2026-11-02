//! The candidate profile. International multi-value fields are plain (`profile.basic`);
//! work authorization is encrypted per country.
//!
//! FR-4.10: nothing here or in any later task reads `countries`, `cities`, a resume, or
//! any other field to set `authorized` or `needs_sponsorship`. The only writer is
//! [`set_authorization`], with a value the user supplied.
//!
//! `basic` and `set_basic` never call `crypto::state` or `crypto::unlocked`, so plain
//! profile work never touches the keychain.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::crypto::{self, DataKey, KeyState};
use crate::db::Db;
use crate::error::{Error, Result};

const TABLE: &str = crypto::AUTHORIZATION_AAD.0;
const COLUMN: &str = crypto::AUTHORIZATION_AAD.1;
const MAX_CHIPS: usize = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileBasic {
    pub name: String,
    pub email: String,
    /// ISO 3166-1 alpha-2, upper-case.
    pub countries: Vec<String>,
    pub regions: Vec<String>,
    pub cities: Vec<String>,
    /// ISO 4217.
    pub currencies: Vec<String>,
    /// IANA names, accepted as typed: trimmed and non-empty.
    pub time_zones: Vec<String>,
    /// BCP 47 tags as typed.
    pub languages: Vec<String>,
    pub work_modes: Vec<WorkMode>,
    pub employment_types: Vec<EmploymentType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    Remote,
    Hybrid,
    Onsite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmploymentType {
    pub country: String,
    pub kind: String,
}

/// The only three values there are. A missing answer is `NotStated`, never a guess.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Stated {
    #[default]
    NotStated,
    Yes,
    No,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkAuthorization {
    pub country: String,
    pub authorized: Stated,
    pub needs_sponsorship: Stated,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub basic: ProfileBasic,
    pub authorization: Vec<WorkAuthorization>,
    pub locked: bool,
    pub key_state: KeyState,
}

/// Plain fields, plus authorization rows when the key is unlocked. When it is not,
/// `locked` is true and `authorization` is empty.
pub async fn get(db: &Db) -> Result<Profile> {
    let basic = basic(db).await?;
    let mut key_state = crypto::state(db).await?;
    let mut authorization = Vec::new();
    if matches!(key_state, KeyState::Unlocked { .. }) {
        match read_authorization(db).await {
            Ok(rows) => authorization = rows,
            Err(error) => key_state = degraded(error)?,
        }
    }
    Ok(Profile {
        basic,
        authorization,
        locked: !matches!(key_state, KeyState::Unlocked { .. }),
        key_state,
    })
}

/// [`get`] for a screen that only looks: with no authorization rows it asks the keychain
/// nothing, so it can never make a key. `locked` is false then, as there is nothing to lock.
pub async fn show(db: &Db) -> Result<Profile> {
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM work_authorization")
        .fetch_one(&db.pool)
        .await?;
    if held > 0 {
        return get(db).await;
    }
    Ok(Profile {
        basic: basic(db).await?,
        authorization: Vec::new(),
        locked: false,
        key_state: crypto::known_state(db).await?,
    })
}

/// A key error while reading rows (a row sealed under a key this machine lost) leaves the
/// profile locked instead of failing it. Any other error is real.
pub(crate) fn degraded(error: Error) -> Result<KeyState> {
    match error {
        Error::KeyMissing { key_id } => Ok(KeyState::KeyMissing { key_id }),
        Error::KeyMismatch { key_id } => Ok(KeyState::KeyMismatch { key_id }),
        Error::KeyUnavailable(reason) => Ok(KeyState::KeyUnavailable {
            key_id: String::new(),
            reason,
        }),
        Error::Locked => Ok(KeyState::NotLoaded),
        other => Err(other),
    }
}

/// Never touches `work_authorization` or the keychain.
pub async fn basic(db: &Db) -> Result<ProfileBasic> {
    let stored: Option<String> = sqlx::query_scalar("SELECT basic FROM profile WHERE id = 'me'")
        .fetch_optional(&db.pool)
        .await?;
    match stored {
        None => Ok(ProfileBasic::default()),
        Some(text) => serde_json::from_str(&text)
            .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error)))),
    }
}

/// Trims chips, upper-cases country and currency codes, validates, and saves. Never
/// touches `work_authorization` or the keychain. Returns what was stored.
pub async fn set_basic(db: &Db, mut basic: ProfileBasic) -> Result<ProfileBasic> {
    normalize(&mut basic);
    validate(&basic)?;
    let text = serde_json::to_string(&basic).map_err(|e| Error::Validation(e.to_string()))?;
    sqlx::query(
        "INSERT INTO profile (id, basic, updated_at) \
         VALUES ('me', ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(id) DO UPDATE SET basic = excluded.basic, updated_at = excluded.updated_at",
    )
    .bind(text)
    .execute(&db.pool)
    .await?;
    Ok(basic)
}

/// `Err(Locked)` or the key's own error unless the key is unlocked.
pub async fn set_authorization(db: &Db, mut row: WorkAuthorization) -> Result<()> {
    row.country = row.country.trim().to_ascii_uppercase();
    if !is_letters(&row.country, 2) {
        return Err(Error::Validation(
            "a country is two letters, like PT".into(),
        ));
    }
    if row.note.chars().count() > 2000 {
        return Err(Error::Validation(
            "a note is at most 2000 characters".into(),
        ));
    }
    let key = crypto::unlocked(db).await?;
    let plain = serde_json::to_vec(&row).map_err(|e| Error::Validation(e.to_string()))?;
    let sealed = key.encrypt(TABLE, COLUMN, &row.country, &plain)?;
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(country) DO UPDATE SET ciphertext = excluded.ciphertext, \
         key_id = excluded.key_id, updated_at = excluded.updated_at",
    )
    .bind(&row.country)
    .bind(sealed)
    .bind(key.key_id())
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Needs no key: it only deletes ciphertext.
pub async fn remove_authorization(db: &Db, country: &str) -> Result<()> {
    sqlx::query("DELETE FROM work_authorization WHERE country = ?")
        .bind(country.trim().to_ascii_uppercase())
        .execute(&db.pool)
        .await?;
    Ok(())
}

pub fn validate(basic: &ProfileBasic) -> Result<()> {
    if basic.name.chars().count() > 200 {
        return Err(bad("name is at most 200 characters"));
    }
    if !basic.email.is_empty() && !basic.email.contains('@') {
        return Err(bad("email needs an @"));
    }
    chips("countries", &basic.countries, |c| is_letters(c, 2))?;
    chips("currencies", &basic.currencies, |c| is_letters(c, 3))?;
    chips("regions", &basic.regions, |_| true)?;
    chips("cities", &basic.cities, |_| true)?;
    chips("time_zones", &basic.time_zones, |_| true)?;
    chips("languages", &basic.languages, |_| true)?;
    if basic.work_modes.len() > MAX_CHIPS || basic.employment_types.len() > MAX_CHIPS {
        return Err(bad("at most 50 entries per field"));
    }
    for kind in &basic.employment_types {
        if !is_letters(&kind.country, 2) || kind.kind.trim().is_empty() {
            return Err(bad(
                "an employment type needs a 2-letter country and a kind",
            ));
        }
    }
    Ok(())
}

/// Rows for `transfer::export_all`, and whether plaintext was written. Rows exist, so
/// `crypto::state` makes no key here. With none, nothing is read or asked of the keychain.
pub async fn export_authorization(db: &Db) -> Result<(String, bool)> {
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM work_authorization")
        .fetch_one(&db.pool)
        .await?;
    if held == 0 {
        return Ok(("[]".into(), false));
    }
    let key_state = crypto::state(db).await?;
    if !matches!(key_state, KeyState::Unlocked { .. }) {
        let locked = json!({ "locked": key_state });
        return Ok((locked.to_string(), false));
    }
    let rows = match read_authorization(db).await {
        Ok(rows) => rows,
        Err(error) => {
            let locked = json!({ "locked": degraded(error)? });
            return Ok((locked.to_string(), false));
        }
    };
    let text = serde_json::to_string(&rows).map_err(|e| Error::Validation(e.to_string()))?;
    Ok((text, true))
}

/// The notes, for `drafts` to keep out of generated text; it never sets a value. With no
/// rows, nothing is asked of the keychain.
pub(crate) async fn authorization_notes(db: &Db) -> Result<Vec<String>> {
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM work_authorization")
        .fetch_one(&db.pool)
        .await?;
    if held == 0 {
        return Ok(Vec::new());
    }
    let rows = read_authorization(db).await?;
    Ok(rows.into_iter().map(|row| row.note).collect())
}

/// One country's row, by the exact code. With no row it returns `None` before any key call,
/// so "not stated" asks the keychain nothing. With one, only the key that sealed it is read.
pub(crate) async fn authorization_row(db: &Db, country: &str) -> Result<Option<WorkAuthorization>> {
    let stored: Option<(Vec<u8>, String)> =
        sqlx::query_as("SELECT ciphertext, key_id FROM work_authorization WHERE country = ?")
            .bind(country)
            .fetch_optional(&db.pool)
            .await?;
    let Some((ciphertext, key_id)) = stored else {
        return Ok(None);
    };
    let key = crypto::key_by_id(db, &key_id).await?;
    let plain = key.decrypt(TABLE, COLUMN, country, &ciphertext)?;
    serde_json::from_slice(&plain)
        .map(Some)
        .map_err(|e| Error::Validation(e.to_string()))
}

async fn read_authorization(db: &Db) -> Result<Vec<WorkAuthorization>> {
    let stored: Vec<(String, Vec<u8>, String)> = sqlx::query_as(
        "SELECT country, ciphertext, key_id FROM work_authorization ORDER BY country",
    )
    .fetch_all(&db.pool)
    .await?;
    let current = crypto::unlocked(db).await?;
    let mut others: Vec<DataKey> = Vec::new();
    let mut rows = Vec::new();
    for (country, ciphertext, key_id) in stored {
        let plain = if key_id == current.key_id() {
            current.decrypt(TABLE, COLUMN, &country, &ciphertext)?
        } else {
            if !others.iter().any(|key| key.key_id() == key_id) {
                others.push(crypto::key_by_id(db, &key_id).await?);
            }
            let key = others.iter().find(|key| key.key_id() == key_id);
            key.ok_or(Error::Locked)?
                .decrypt(TABLE, COLUMN, &country, &ciphertext)?
        };
        rows.push(serde_json::from_slice(&plain).map_err(|e| Error::Validation(e.to_string()))?);
    }
    Ok(rows)
}

fn normalize(basic: &mut ProfileBasic) {
    basic.name = basic.name.trim().to_owned();
    basic.email = basic.email.trim().to_owned();
    for chip in basic.countries.iter_mut().chain(&mut basic.currencies) {
        *chip = chip.trim().to_ascii_uppercase();
    }
    for chip in basic
        .regions
        .iter_mut()
        .chain(&mut basic.cities)
        .chain(&mut basic.time_zones)
        .chain(&mut basic.languages)
    {
        *chip = chip.trim().to_owned();
    }
    for kind in &mut basic.employment_types {
        kind.country = kind.country.trim().to_ascii_uppercase();
        kind.kind = kind.kind.trim().to_owned();
    }
}

fn chips(field: &str, values: &[String], valid: impl Fn(&str) -> bool) -> Result<()> {
    if values.len() > MAX_CHIPS {
        return Err(bad(&format!("{field} has more than {MAX_CHIPS} entries")));
    }
    for value in values {
        if value.trim().is_empty() {
            return Err(bad(&format!("{field} has an empty entry")));
        }
        if !valid(value) {
            return Err(bad(&format!("{field}: \"{value}\" is not a valid code")));
        }
    }
    Ok(())
}

fn is_letters(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_uppercase())
}

fn bad(message: &str) -> Error {
    Error::Validation(message.to_owned())
}
