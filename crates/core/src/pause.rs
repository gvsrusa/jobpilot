//! The emergency pause (FR-11.17). One row says nothing automated runs until a person
//! clears it. The flag lives in the database, so another process sees it: a `pause set` from
//! the CLI stops a run the desktop started, and the reverse.
//!
//! Setting and clearing are a person's act. The gates are in the runner (`start`, `resume`,
//! and each step), the router (`run` and each attempt, `embed`), `Ollama::pull`, and
//! `applications::set_status`. Cancelling a run and editing fields stay open.

use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use sqlx::Row;
use tokio::time::sleep;

use crate::ai::CancellationToken;
use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::runner;

/// How often [`watch`] looks.
const POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize)]
pub struct PauseState {
    pub paused: bool,
    pub set_at: Option<String>,
    pub set_by: Option<String>,
}

pub async fn state(db: &Db) -> Result<PauseState> {
    let row = sqlx::query("SELECT set_at, set_by FROM emergency_pause WHERE id = 'me'")
        .fetch_optional(&db.pool)
        .await?;
    Ok(match row {
        Some(row) => PauseState {
            paused: true,
            set_at: Some(row.try_get("set_at")?),
            set_by: Some(row.try_get("set_by")?),
        },
        None => PauseState {
            paused: false,
            set_at: None,
            set_by: None,
        },
    })
}

pub async fn is_set(db: &Db) -> Result<bool> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM emergency_pause WHERE id = 'me'")
        .fetch_optional(&db.pool)
        .await?;
    Ok(found.is_some())
}

/// `Err(Paused)` while the pause is on. Every path that starts automation calls this.
pub async fn ensure_off(db: &Db) -> Result<()> {
    if is_set(db).await? {
        Err(Error::Paused)
    } else {
        Ok(())
    }
}

/// A person turns it on. A second call changes nothing and writes no audit row. Runs whose
/// process is already gone move to `halted` now; a live one sees the flag itself.
pub async fn set(db: &Db, actor: &str) -> Result<PauseState> {
    runner::person(actor)?;
    let mut tx = db.pool.begin().await?;
    let added = sqlx::query(
        "INSERT OR IGNORE INTO emergency_pause (id, set_at, set_by) \
         VALUES ('me', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?)",
    )
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    if added.rows_affected() == 1 {
        audit::record_to(&mut *tx, actor, "emergency_pause_set", None, json!({})).await?;
    }
    tx.commit().await?;
    runner::reap_stale(db).await?;
    state(db).await
}

/// Only a person turns it off. Halted runs stay halted: each is resumed or cancelled by hand.
pub async fn clear(db: &Db, actor: &str) -> Result<PauseState> {
    runner::person(actor)?;
    let mut tx = db.pool.begin().await?;
    let removed = sqlx::query("DELETE FROM emergency_pause WHERE id = 'me'")
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 1 {
        audit::record_to(&mut *tx, actor, "emergency_pause_cleared", None, json!({})).await?;
    }
    tx.commit().await?;
    state(db).await
}

/// Resolves once the pause is on, after firing `cancel`. Polls every 250 ms. A failed read
/// costs one poll: a busy database must not end the wait.
pub async fn watch(db: &Db, cancel: &CancellationToken) {
    loop {
        if is_set(db).await.unwrap_or(false) {
            cancel.cancel();
            return;
        }
        sleep(POLL).await;
    }
}
