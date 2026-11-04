use sqlx::sqlite::SqliteExecutor;

use crate::db::Db;
use crate::error::Result;

/// Whether the desktop app looks for an update when it starts. Off unless a person turns it on:
/// a missing value means `false`, as the check is a network call.
pub const UPDATES_CHECK_ON_LAUNCH: &str = "updates.check_on_launch";

/// Settings only a person sets here, which a restore never takes from a backup: every path
/// that names a program to run, the Ollama address (set behind a native dialog, and where
/// every Ollama prompt goes), and the launch-time network check. A backup is an old
/// decision or someone else's file, so it must not choose what runs, where prompts go, or
/// turn on a network call (S-1). The live value stays, and an absent one stays absent.
/// `ai.endpoint.url` is not here: its consent is bound to the address and a restore
/// revokes it.
pub const KEPT_ON_RESTORE: [&str; 5] = [
    crate::ai::claude_code::PATH_SETTING,
    crate::ai::child::GEMINI.setting,
    crate::ai::child::CODEX.setting,
    "ai.ollama_url",
    UPDATES_CHECK_ON_LAUNCH,
];

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
