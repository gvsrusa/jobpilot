//! All of the runner's SQL. A state change and its log row share one transaction, and it
//! lands only from the state and generation the writer last saw. So a run reaped by another
//! handle, or resumed by one and running again, is not written over.

// ponytail: runs and logs are never pruned. A run writes a few rows and export reads them
// all into memory, so add retention past 100,000 rows.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::{Agent, Checkpoint, LogRow, RunInput, RunState, RunSummary};
use crate::ai::{DataCategory, ProviderId};
use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};

const RUN_STEP: &str = "run";
const INTERRUPTED: &str = "interrupted";

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

/// Not finished, and not waiting on a person to resume or cancel.
macro_rules! active {
    () => {
        "('queued','checking','running','checkpointed','retrying','waiting_for_human')"
    };
}

macro_rules! summary {
    () => {
        "id, agent, state, step, steps, reason, actor, created_at, finished_at"
    };
}

/// The snake_case name of a serde enum value.
fn name<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        _ => Err(Error::Db(sqlx::Error::Decode(
            "a run value has no name".into(),
        ))),
    }
}

fn parsed<T: DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_value(json!(text)).map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
}

fn from_json<T: DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| std::io::Error::other(error).into())
}

/// One state change of a run.
pub(super) struct Change<'a> {
    pub id: &'a str,
    pub agent: Agent,
    pub from: RunState,
    pub to: RunState,
    pub generation: i64,
    /// A resume: the next process owns the run from here.
    pub bump: bool,
    pub step: i64,
    pub step_name: &'a str,
    pub checkpoint: &'a Checkpoint,
    pub reason: Option<&'a str>,
    pub outcome: &'a str,
    pub provider: Option<&'a ProviderId>,
    pub categories: &'a [DataCategory],
    pub duration_ms: i64,
}

/// A run row as the runner needs it again.
pub(crate) struct Stored {
    pub agent: Agent,
    pub state: RunState,
    pub step: i64,
    pub steps: i64,
    pub reason: Option<String>,
    pub checkpoint: Checkpoint,
    pub generation: i64,
    pub created_at: String,
    pub input: RunInput,
}

#[derive(sqlx::FromRow)]
struct StoredRow {
    agent: String,
    state: String,
    step: i64,
    steps: i64,
    reason: Option<String>,
    checkpoint: String,
    generation: i64,
    created_at: String,
    input: String,
}

#[derive(sqlx::FromRow)]
struct LogColumns {
    at: String,
    step: String,
    state: String,
    duration_ms: i64,
    provider: Option<String>,
    data_categories: String,
    outcome: String,
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: String,
    agent: String,
    state: String,
    step: i64,
    steps: i64,
    reason: Option<String>,
    actor: String,
    created_at: String,
    finished_at: Option<String>,
}

impl TryFrom<SummaryRow> for RunSummary {
    type Error = Error;

    fn try_from(row: SummaryRow) -> Result<RunSummary> {
        Ok(RunSummary {
            id: row.id,
            agent: parsed(&row.agent)?,
            state: parsed(&row.state)?,
            step: row.step,
            steps: row.steps,
            reason: row.reason,
            actor: row.actor,
            created_at: row.created_at,
            finished_at: row.finished_at,
        })
    }
}

async fn log_row(tx: &mut sqlx::SqliteConnection, change: &Change<'_>) -> Result<()> {
    sqlx::query(concat!(
        "INSERT INTO agent_run_log (run_id, at, agent, step, state, duration_ms, provider, \
         data_categories, outcome) VALUES (?, ",
        now!(),
        ", ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(change.id)
    .bind(name(&change.agent)?)
    .bind(change.step_name)
    .bind(name(&change.to)?)
    .bind(change.duration_ms)
    .bind(change.provider.map(ToString::to_string))
    .bind(to_json(&change.categories)?)
    .bind(change.outcome)
    .execute(tx)
    .await?;
    Ok(())
}

/// A new `queued` run. Its id is the run's own.
pub(super) async fn insert(
    db: &Db,
    agent: Agent,
    actor: &str,
    input: &RunInput,
    steps: i64,
) -> Result<String> {
    let mut tx = db.pool.begin().await?;
    let (id,): (String,) = sqlx::query_as(concat!(
        "INSERT INTO agent_runs (id, agent, state, actor, input, steps, heartbeat_at, created_at) \
         VALUES (lower(hex(randomblob(16))), ?, 'queued', ?, ?, ?, ",
        now!(),
        ", ",
        now!(),
        ") RETURNING id"
    ))
    .bind(name(&agent)?)
    .bind(actor)
    .bind(to_json(input)?)
    .bind(steps)
    .fetch_one(&mut *tx)
    .await?;
    log_row(
        &mut tx,
        &Change {
            id: &id,
            agent,
            from: RunState::Queued,
            to: RunState::Queued,
            generation: 0,
            bump: false,
            step: 0,
            step_name: RUN_STEP,
            checkpoint: &Checkpoint::default(),
            reason: None,
            outcome: "ok",
            provider: None,
            categories: &[],
            duration_ms: 0,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// False when the run is no longer this process's: nothing was written. A run ending
/// `cancelled` because a person asked is audited here, under who asked. A token alone is
/// the caller's own stop, not a person's act on a run.
pub(super) async fn transition(db: &Db, change: &Change<'_>) -> Result<bool> {
    let mut tx = db.pool.begin().await?;
    let moved = transition_in(&mut tx, change).await?;
    if !moved {
        return Ok(false);
    }
    if change.to == RunState::Cancelled {
        let asked: Option<String> =
            sqlx::query_scalar("SELECT cancel_actor FROM agent_runs WHERE id = ?")
                .bind(change.id)
                .fetch_one(&mut *tx)
                .await?;
        if let Some(actor) = asked {
            let detail = json!({ "agent": change.agent });
            audit::record_to(&mut *tx, &actor, "run_cancelled", Some(change.id), detail).await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

async fn transition_in(tx: &mut sqlx::SqliteConnection, change: &Change<'_>) -> Result<bool> {
    let moved = sqlx::query(concat!(
        "UPDATE agent_runs SET state = ?, step = ?, checkpoint = ?, reason = ?, \
         generation = generation + ?, heartbeat_at = ",
        now!(),
        ", finished_at = CASE WHEN ? THEN ",
        now!(),
        " END WHERE id = ? AND state = ? AND generation = ?"
    ))
    .bind(name(&change.to)?)
    .bind(change.step)
    .bind(to_json(change.checkpoint)?)
    .bind(change.reason)
    .bind(i64::from(change.bump))
    .bind(change.to.is_terminal())
    .bind(change.id)
    .bind(name(&change.from)?)
    .bind(change.generation)
    .execute(&mut *tx)
    .await?;
    if moved.rows_affected() == 0 {
        return Ok(false);
    }
    log_row(tx, change).await?;
    Ok(true)
}

/// False when the run has stopped being this process's.
pub(super) async fn heartbeat(db: &Db, id: &str, generation: i64) -> Result<bool> {
    let beat = sqlx::query(concat!(
        "UPDATE agent_runs SET heartbeat_at = ",
        now!(),
        " WHERE id = ? AND generation = ? AND state IN ",
        active!()
    ))
    .bind(id)
    .bind(generation)
    .execute(&db.pool)
    .await?;
    Ok(beat.rows_affected() == 1)
}

/// Whether the row is still as this process last wrote it.
pub(super) async fn owns(db: &Db, id: &str, state: RunState, generation: i64) -> Result<bool> {
    let held: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM agent_runs WHERE id = ? AND state = ? AND generation = ?",
    )
    .bind(id)
    .bind(name(&state)?)
    .bind(generation)
    .fetch_optional(&db.pool)
    .await?;
    Ok(held.is_some())
}

pub(super) async fn cancel_requested(db: &Db, id: &str) -> Result<bool> {
    let wanted: Option<bool> =
        sqlx::query_scalar("SELECT cancel_requested FROM agent_runs WHERE id = ?")
            .bind(id)
            .fetch_optional(&db.pool)
            .await?;
    Ok(wanted.unwrap_or(false))
}

pub(crate) async fn load(db: &Db, id: &str) -> Result<Stored> {
    let row: Option<StoredRow> = sqlx::query_as(
        "SELECT agent, state, step, steps, reason, checkpoint, generation, created_at, input \
         FROM agent_runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&db.pool)
    .await?;
    let row = row.ok_or(Error::NotFound)?;
    Ok(Stored {
        agent: parsed(&row.agent)?,
        state: parsed(&row.state)?,
        step: row.step,
        steps: row.steps,
        reason: row.reason,
        checkpoint: from_json(&row.checkpoint)?,
        generation: row.generation,
        created_at: row.created_at,
        input: from_json(&row.input)?,
    })
}

/// A person moves a `halted` run on, and the audit row lands with it. False when another
/// handle got there first, which is what keeps a run to one resume.
async fn leave_halted(
    db: &Db,
    actor: &str,
    id: &str,
    stored: &Stored,
    to: RunState,
    action: &str,
) -> Result<bool> {
    let change = Change {
        id,
        agent: stored.agent,
        from: RunState::Halted,
        to,
        generation: stored.generation,
        bump: to == RunState::Checking,
        step: stored.step,
        step_name: RUN_STEP,
        checkpoint: &stored.checkpoint,
        reason: None,
        outcome: action,
        provider: None,
        categories: &[],
        duration_ms: 0,
    };
    let detail = json!({ "agent": stored.agent });
    let mut tx = db.pool.begin().await?;
    if !transition_in(&mut tx, &change).await? {
        return Ok(false);
    }
    audit::record_to(&mut *tx, actor, action, Some(id), detail).await?;
    tx.commit().await?;
    Ok(true)
}

pub(super) async fn claim(db: &Db, actor: &str, id: &str, stored: &Stored) -> Result<bool> {
    leave_halted(db, actor, id, stored, RunState::Checking, "run_resumed").await
}

/// A `halted` run has no process to poll the flag, so the cancel lands here.
pub(crate) async fn cancel_halted(db: &Db, actor: &str, id: &str, stored: &Stored) -> Result<bool> {
    leave_halted(db, actor, id, stored, RunState::Cancelled, "run_cancelled").await
}

/// An active run gets the flag its process polls for, and the first person to ask stays on
/// it. Nothing is audited yet: the run may finish first. False when it is no longer active.
pub(crate) async fn request_cancel(db: &Db, actor: &str, id: &str) -> Result<bool> {
    let flagged = sqlx::query(concat!(
        "UPDATE agent_runs SET cancel_requested = 1, cancel_actor = coalesce(cancel_actor, ?) \
         WHERE id = ? AND state IN ",
        active!()
    ))
    .bind(actor)
    .bind(id)
    .execute(&db.pool)
    .await?;
    Ok(flagged.rows_affected() == 1)
}

/// Active rows whose process has gone quiet move to `halted`. Reads first, so a list on an
/// idle app takes no write lock.
pub(super) async fn reap(db: &Db, stale: Duration) -> Result<usize> {
    let age = format!("-{:.3} seconds", stale.as_secs_f64());
    let found: Option<i64> = sqlx::query_scalar(concat!(
        "SELECT 1 FROM agent_runs WHERE state IN ",
        active!(),
        " AND heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?) LIMIT 1"
    ))
    .bind(&age)
    .fetch_optional(&db.pool)
    .await?;
    if found.is_none() {
        return Ok(0);
    }
    let mut tx = db.pool.begin().await?;
    let reaped: Vec<(String, String)> = sqlx::query_as(concat!(
        "UPDATE agent_runs SET state = 'halted', reason = ?1, heartbeat_at = ",
        now!(),
        " WHERE state IN ",
        active!(),
        " AND heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) RETURNING id, agent"
    ))
    .bind(INTERRUPTED)
    .bind(&age)
    .fetch_all(&mut *tx)
    .await?;
    for (id, agent) in &reaped {
        log_row(
            &mut tx,
            &Change {
                id,
                agent: parsed(agent)?,
                from: RunState::Running,
                to: RunState::Halted,
                generation: 0,
                bump: false,
                step: 0,
                step_name: RUN_STEP,
                checkpoint: &Checkpoint::default(),
                reason: Some(INTERRUPTED),
                outcome: INTERRUPTED,
                provider: None,
                categories: &[],
                duration_ms: 0,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(reaped.len())
}

pub(super) async fn list(db: &Db, limit: i64) -> Result<Vec<RunSummary>> {
    let rows: Vec<SummaryRow> = sqlx::query_as(concat!(
        "SELECT ",
        summary!(),
        " FROM agent_runs ORDER BY seq DESC LIMIT ?"
    ))
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;
    rows.into_iter().map(RunSummary::try_from).collect()
}

pub(super) async fn summary(db: &Db, id: &str) -> Result<RunSummary> {
    let row: Option<SummaryRow> = sqlx::query_as(concat!(
        "SELECT ",
        summary!(),
        " FROM agent_runs WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?;
    row.ok_or(Error::NotFound)?.try_into()
}

pub(super) async fn log(db: &Db, id: &str) -> Result<Vec<LogRow>> {
    let rows: Vec<LogColumns> = sqlx::query_as(
        "SELECT at, step, state, duration_ms, provider, data_categories, outcome \
         FROM agent_run_log WHERE run_id = ? ORDER BY seq",
    )
    .bind(id)
    .fetch_all(&db.pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(LogRow {
                at: row.at,
                step: row.step,
                state: parsed(&row.state)?,
                duration_ms: row.duration_ms,
                provider: row.provider,
                data_categories: from_json(&row.data_categories)?,
                outcome: row.outcome,
            })
        })
        .collect()
}
