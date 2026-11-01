use serde::Serialize;
use sqlx::Row;

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
    sqlx::query(
        "INSERT INTO audit_log (at, actor, action, target, detail) \
         VALUES (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?, ?, ?, ?)",
    )
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(detail.to_string())
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Newest first. `id` breaks ties between rows in the same millisecond.
pub async fn recent(db: &Db, limit: i64) -> Result<Vec<AuditEntry>> {
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
