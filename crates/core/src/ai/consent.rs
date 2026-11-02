//! No data leaves the machine without an active record (FR-3.30). The record keys on
//! `ProviderId`'s display text, so it names the sign-in method too.

use serde_json::json;

use super::{DataCategory, ProviderId, api, register};
use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum ConsentState {
    None,
    Active(Vec<DataCategory>),
    /// A record given under other register text than this build's, or for an endpoint address
    /// that is no longer the stored one. It allows nothing until the user confirms again.
    /// Computed on read: a read must not write.
    Suspended(Vec<DataCategory>),
}

impl ConsentState {
    /// The categories this state allows sending, so anything but `Active` fails closed.
    pub fn allowed(self) -> Option<Vec<DataCategory>> {
        match self {
            ConsentState::Active(categories) => Some(categories),
            ConsentState::None | ConsentState::Suspended(_) => None,
        }
    }
}

/// Ends any active record for `provider`, so the categories given here are the whole
/// consent: giving fewer narrows it. A record given in the same millisecond is replaced.
/// `register_row` is `''` for a name the provider table does not hold, which is never active.
pub async fn give(
    db: &Db,
    provider: &str,
    categories: &[DataCategory],
    terms_date: &str,
    actor: &str,
) -> Result<()> {
    if categories.is_empty() {
        return Err(Error::Validation("no data categories".into()));
    }
    let names = json!(categories);
    let register_row = current_row(db, provider).await?.unwrap_or_default();
    let mut tx = db.pool.begin().await?;
    close_active(&mut tx, provider).await?;
    sqlx::query(
        "INSERT INTO consent (provider, categories, given_at, terms_date, register_row) \
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?, ?) \
         ON CONFLICT (provider, given_at) DO UPDATE SET categories = excluded.categories, \
         revoked_at = NULL, terms_date = excluded.terms_date, \
         register_row = excluded.register_row",
    )
    .bind(provider)
    .bind(names.to_string())
    .bind(terms_date)
    .bind(register_row)
    .execute(&mut *tx)
    .await?;
    let detail = json!({"categories": names, "terms_date": terms_date});
    audit::record_to(&mut *tx, actor, "consent_given", Some(provider), detail).await?;
    tx.commit().await?;
    Ok(())
}

/// By settings name. Takes the date from the register, and refuses `attestation_needed` when
/// the provider has one and `attested` is false: no attestation, no record, so no call.
pub async fn give_for(
    db: &Db,
    name: &str,
    categories: &[DataCategory],
    attested: bool,
    actor: &str,
) -> Result<()> {
    let info = register::info(name)
        .ok_or_else(|| Error::Validation("consent does not apply to this provider".into()))?;
    if info.attestation.is_some() && !attested {
        return Err(Error::Refused("attestation_needed".into()));
    }
    let key = info.id.to_string();
    let terms_date = register::date_read(&key).unwrap_or_else(register::register_date);
    give(db, &key, categories, &terms_date, actor).await
}

/// Takes effect before the router's next call. Revoking nothing writes nothing.
pub async fn revoke(db: &Db, provider: &str, actor: &str) -> Result<()> {
    let mut tx = db.pool.begin().await?;
    if close_active(&mut tx, provider).await? > 0 {
        audit::record_to(
            &mut *tx,
            actor,
            "consent_revoked",
            Some(provider),
            json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn close_active(tx: &mut sqlx::SqliteConnection, provider: &str) -> Result<u64> {
    let closed = sqlx::query(
        "UPDATE consent SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE provider = ? AND revoked_at IS NULL",
    )
    .bind(provider)
    .execute(tx)
    .await?;
    Ok(closed.rows_affected())
}

/// What a record is bound to: the row text this build holds, and for the endpoint the address
/// too, since that is where the data goes. A loopback address says nothing about who listens.
async fn current_row(db: &Db, provider: &str) -> Result<Option<String>> {
    let row = register::row_text(provider);
    if provider != ProviderId::Endpoint.to_string() {
        return Ok(row);
    }
    let address = api::endpoint_address(db).await?;
    Ok(row.map(|row| match address {
        Some(address) => format!("{row}\n{address}"),
        None => row,
    }))
}

/// The newest open record, against what it is bound to now.
pub async fn state(db: &Db, provider: &str) -> Result<ConsentState> {
    let record: Option<(String, String)> = sqlx::query_as(
        "SELECT categories, register_row FROM consent \
         WHERE provider = ? AND revoked_at IS NULL ORDER BY given_at DESC LIMIT 1",
    )
    .bind(provider)
    .fetch_optional(&db.pool)
    .await?;
    let Some((categories, row)) = record else {
        return Ok(ConsentState::None);
    };
    let categories =
        serde_json::from_str(&categories).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    Ok(match current_row(db, provider).await? {
        Some(current) if current == row => ConsentState::Active(categories),
        _ => ConsentState::Suspended(categories),
    })
}

/// `None` while suspended, so every caller fails closed.
pub async fn active(db: &Db, provider: &str) -> Result<Option<Vec<DataCategory>>> {
    Ok(state(db, provider).await?.allowed())
}
