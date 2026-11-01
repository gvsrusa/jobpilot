use sqlx::sqlite::SqliteExecutor;

use crate::db::Db;
use crate::error::Result;

pub async fn get(db: &Db, key: &str) -> Result<Option<serde_json::Value>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(&db.pool)
        .await?;
    raw.map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
}

pub async fn set(db: &Db, key: &str, value: &serde_json::Value) -> Result<()> {
    set_to(&db.pool, key, value).await
}

/// `set` for an open transaction, so the value lands with the change it records.
pub(crate) async fn set_to(
    executor: impl SqliteExecutor<'_>,
    key: &str,
    value: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) \
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value.to_string())
    .execute(executor)
    .await?;
    Ok(())
}
