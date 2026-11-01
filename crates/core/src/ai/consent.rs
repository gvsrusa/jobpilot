//! No data leaves the machine without an active record (FR-3.30). The record keys on
//! `ProviderId`'s display text, so it names the sign-in method too.

use serde_json::json;

use super::DataCategory;
use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};

/// Ends any active record for `provider`, so the categories given here are the whole
/// consent: giving fewer narrows it. A record given in the same millisecond is replaced.
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
    let mut tx = db.pool.begin().await?;
    close_active(&mut tx, provider).await?;
    sqlx::query(
        "INSERT INTO consent (provider, categories, given_at, terms_date) \
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?) \
         ON CONFLICT (provider, given_at) DO UPDATE SET categories = excluded.categories, \
         revoked_at = NULL, terms_date = excluded.terms_date",
    )
    .bind(provider)
    .bind(names.to_string())
    .bind(terms_date)
    .execute(&mut *tx)
    .await?;
    let detail = json!({"categories": names, "terms_date": terms_date});
    audit::record_to(&mut *tx, actor, "consent_given", Some(provider), detail).await?;
    tx.commit().await?;
    Ok(())
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

pub async fn active(db: &Db, provider: &str) -> Result<Option<Vec<DataCategory>>> {
    let categories: Option<String> = sqlx::query_scalar(
        "SELECT categories FROM consent WHERE provider = ? AND revoked_at IS NULL \
         ORDER BY given_at DESC LIMIT 1",
    )
    .bind(provider)
    .fetch_optional(&db.pool)
    .await?;
    categories
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
}
