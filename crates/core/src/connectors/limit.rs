//! Rate limits kept in the database, so the GUI, the CLI, and a restart all share one clock.
//! A slot is taken by one conditional `UPDATE`, which SQLite runs as a single write: two
//! handles that race for the same slot cannot both pass. The table holds counters and times
//! only, never text.

use std::time::Duration;

use sqlx::Row;

use super::ConnectorDecl;
use crate::db::Db;
use crate::error::{Error, Result};

/// The longest the fetcher sleeps for a slot. A longer wait is refused as `RateLimited`.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The slot is taken and recorded: send now.
    Go,
    /// Nothing is recorded: try again after this long.
    Wait(Duration),
}

pub async fn take(db: &Db, decl: &ConnectorDecl, host: &str) -> Result<Slot> {
    take_at(db, decl, host, now_ms()).await
}

/// [`take`] on the caller's clock, so a test can set the time.
#[doc(hidden)]
pub async fn take_at(db: &Db, decl: &ConnectorDecl, host: &str, now_ms: i64) -> Result<Slot> {
    let limit = decl
        .rate_limit
        .ok_or_else(|| Error::Validation("a read connector declares a rate limit".into()))?;
    let interval = i64::try_from(limit.min_interval.as_millis()).unwrap_or(i64::MAX);
    let day = utc_day(db, now_ms).await?;
    sqlx::query("INSERT OR IGNORE INTO connector_limits (connector, host) VALUES (?, ?)")
        .bind(decl.name)
        .bind(host)
        .execute(&db.pool)
        .await?;
    let taken = sqlx::query(
        "UPDATE connector_limits SET last_at_ms = ?1, day = ?2, \
           day_count = CASE WHEN day = ?2 THEN day_count + 1 ELSE 1 END \
         WHERE connector = ?3 AND host = ?4 \
           AND ?1 >= last_at_ms + ?5 AND ?1 >= not_before_ms \
           AND (day <> ?2 OR day_count < ?6)",
    )
    .bind(now_ms)
    .bind(&day)
    .bind(decl.name)
    .bind(host)
    .bind(interval)
    .bind(i64::from(limit.per_day))
    .execute(&db.pool)
    .await?;
    if taken.rows_affected() == 1 {
        return Ok(Slot::Go);
    }
    let row = sqlx::query(
        "SELECT last_at_ms, not_before_ms, day, day_count FROM connector_limits \
         WHERE connector = ? AND host = ?",
    )
    .bind(decl.name)
    .bind(host)
    .fetch_one(&db.pool)
    .await?;
    let last: i64 = row.try_get("last_at_ms")?;
    let not_before: i64 = row.try_get("not_before_ms")?;
    let stored_day: String = row.try_get("day")?;
    let count: i64 = row.try_get("day_count")?;
    let mut until = (last + interval).max(not_before);
    if stored_day == day && count >= i64::from(limit.per_day) {
        until = until.max((now_ms.div_euclid(DAY_MS) + 1) * DAY_MS);
    }
    // A rival may have freed the slot between the two statements: ask for a moment, not zero.
    let wait = (until - now_ms).max(1);
    Ok(Slot::Wait(Duration::from_millis(wait as u64)))
}

/// Keeps the host quiet for `wait` from now. It never shortens an earlier hold.
pub async fn hold_off(db: &Db, decl: &ConnectorDecl, host: &str, wait: Duration) -> Result<()> {
    let until = now_ms().saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
    sqlx::query(
        "INSERT INTO connector_limits (connector, host, not_before_ms) VALUES (?1, ?2, ?3) \
         ON CONFLICT (connector, host) DO UPDATE SET \
           not_before_ms = MAX(not_before_ms, excluded.not_before_ms)",
    )
    .bind(decl.name)
    .bind(host)
    .bind(until)
    .execute(&db.pool)
    .await?;
    Ok(())
}

fn now_ms() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The UTC date of `now_ms`, from SQLite, so no date crate is needed.
async fn utc_day(db: &Db, now_ms: i64) -> Result<String> {
    Ok(sqlx::query_scalar("SELECT date(?, 'unixepoch')")
        .bind(now_ms.div_euclid(1000))
        .fetch_one(&db.pool)
        .await?)
}
