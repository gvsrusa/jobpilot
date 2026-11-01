use serde::Serialize;
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::db::Db;
use crate::error::Result;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub detail: serde_json::Value,
}

/// `actor` is `"gui"`, `"cli"`, or `"system"`. `detail` must never hold secret values.
pub async fn record(
    db: &Db,
    actor: &str,
    action: &str,
    target: Option<&str>,
    detail: serde_json::Value,
) -> Result<()> {
    record_to(&db.pool, actor, action, target, detail).await
}

/// `record` for a database that is not the open one: a restore writes its row into
/// the temporary copy, so the row exists only if that copy goes live.
pub(crate) async fn record_to(
    pool: &SqlitePool,
    actor: &str,
    action: &str,
    target: Option<&str>,
    detail: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (at, actor, action, target, detail) \
         VALUES (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?, ?, ?, ?)",
    )
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(detail.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// Newest first. `id` breaks ties between rows in the same millisecond. `limit` is
/// clamped to 1..=1000: SQLite reads a negative `LIMIT` as no limit.
pub async fn recent(db: &Db, limit: i64) -> Result<Vec<AuditEntry>> {
    let limit = limit.clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT id, at, actor, action, target, detail FROM audit_log ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let detail: String = row.try_get("detail")?;
            Ok(AuditEntry {
                id: row.try_get("id")?,
                at: row.try_get("at")?,
                actor: row.try_get("actor")?,
                action: row.try_get("action")?,
                target: row.try_get("target")?,
                detail: serde_json::from_str(&detail)
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            })
        })
        .collect()
}
