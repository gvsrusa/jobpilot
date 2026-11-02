//! Report figures from the application store: counts and rates, never a company, role, or
//! note (rule 7). Aggregate SQL does the counting; only the median sorts in Rust.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{Error, Result};

/// Moves that answer an application, as the JSON array the queries bind.
const RESPONSES: &str = r#"["screening","interviewing","offer","accepted","rejected"]"#;

/// Ever applied: the date is set by the first move to `applied`, and the event outlives a
/// later move to `rejected` or `withdrawn`.
macro_rules! applied {
    () => {
        "(a.applied_on IS NOT NULL OR EXISTS (SELECT 1 FROM application_events e \
         WHERE e.application_id = a.id AND e.to_status = 'applied'))"
    };
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Volume {
    pub total: i64,
    pub by_status: BTreeMap<String, i64>,
    pub applied_last_30_days: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Funnel {
    pub applied: i64,
    pub responded: i64,
    pub interviews: i64,
    pub offers: i64,
    pub response_rate: Option<f64>,
    pub offer_rate: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    pub median_days_to_response: Option<f64>,
    pub responses_counted: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub id: String,
    pub created_at: String,
    pub volume: Volume,
    pub funnel: Funnel,
    pub timing: Timing,
}

/// What `reports.figures` holds. A section is absent until its step has run.
#[derive(Default, Deserialize)]
struct Figures {
    #[serde(default)]
    volume: Volume,
    #[serde(default)]
    funnel: Funnel,
    #[serde(default)]
    timing: Timing,
}

pub(crate) async fn volume(db: &Db) -> Result<Volume> {
    volume_at(db, "now").await
}

/// `volume` as of `now`, a SQLite time value in UTC. The window follows the local calendar,
/// so an `applied_on` of today east of UTC still counts. A test passes a fixed moment.
#[doc(hidden)]
pub async fn volume_at(db: &Db, now: &str) -> Result<Volume> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM applications GROUP BY status")
            .fetch_all(&db.pool)
            .await?;
    let (recent,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM applications \
         WHERE applied_on IS NOT NULL AND applied_on >= date(?1, 'localtime', '-30 days') \
         AND applied_on <= date(?1, 'localtime')",
    )
    .bind(now)
    .fetch_one(&db.pool)
    .await?;
    Ok(Volume {
        total: rows.iter().map(|(_, n)| n).sum(),
        by_status: rows.into_iter().collect(),
        applied_last_30_days: recent,
    })
}

/// Of the applied applications, those with a status event to one of `targets`.
async fn applied_reaching(db: &Db, targets: &str) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(concat!(
        "SELECT count(*) FROM applications a WHERE ",
        applied!(),
        " AND EXISTS (SELECT 1 FROM application_events e WHERE e.application_id = a.id \
         AND e.kind = 'status' AND e.to_status IN (SELECT value FROM json_each(?)))"
    ))
    .bind(targets)
    .fetch_one(&db.pool)
    .await?;
    Ok(n)
}

fn rate(part: i64, whole: i64) -> Option<f64> {
    // Counts are far below 2^53, so the conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    (whole > 0).then(|| part as f64 / whole as f64)
}

pub(crate) async fn funnel(db: &Db) -> Result<Funnel> {
    let (applied,): (i64,) = sqlx::query_as(concat!(
        "SELECT count(*) FROM applications a WHERE ",
        applied!()
    ))
    .fetch_one(&db.pool)
    .await?;
    let responded = applied_reaching(db, RESPONSES).await?;
    let interviews = applied_reaching(db, r#"["interviewing"]"#).await?;
    let offers = applied_reaching(db, r#"["offer"]"#).await?;
    Ok(Funnel {
        applied,
        responded,
        interviews,
        offers,
        response_rate: rate(responded, applied),
        offer_rate: rate(offers, applied),
    })
}

/// Days from `applied_on` to the day of the first response. A response dated before the
/// applied date is a data slip, so it is left out rather than pulling the median down.
pub(crate) async fn timing(db: &Db) -> Result<Timing> {
    let rows: Vec<(Option<f64>,)> = sqlx::query_as(
        "SELECT julianday(substr(min(e.at), 1, 10)) - julianday(a.applied_on) \
         FROM applications a JOIN application_events e ON e.application_id = a.id \
         WHERE a.applied_on IS NOT NULL AND e.kind = 'status' \
         AND e.to_status IN (SELECT value FROM json_each(?)) GROUP BY a.id",
    )
    .bind(RESPONSES)
    .fetch_all(&db.pool)
    .await?;
    let mut days: Vec<f64> = rows
        .into_iter()
        .filter_map(|(gap,)| gap)
        .filter(|gap| *gap >= 0.0)
        .collect();
    days.sort_by(f64::total_cmp);
    let middle = days.len() / 2;
    let median = match days.len() {
        0 => None,
        n if n % 2 == 1 => Some(days[middle]),
        _ => Some(f64::midpoint(days[middle - 1], days[middle])),
    };
    Ok(Timing {
        median_days_to_response: median,
        responses_counted: i64::try_from(days.len()).unwrap_or(i64::MAX),
    })
}

fn report_from(id: String, created_at: String, figures: &str) -> Result<Report> {
    let figures: Figures = serde_json::from_str(figures)
        .map_err(|_| Error::Validation("a stored report is unreadable".into()))?;
    Ok(Report {
        id,
        created_at,
        volume: figures.volume,
        funnel: figures.funnel,
        timing: figures.timing,
    })
}

/// The newest finished report. One still being built is not shown.
pub async fn latest(db: &Db) -> Result<Option<Report>> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, created_at, figures FROM reports WHERE complete = 1 \
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .fetch_optional(&db.pool)
    .await?;
    row.map(|(id, at, figures)| report_from(id, at, &figures))
        .transpose()
}

pub(crate) async fn finished(db: &Db, id: &str) -> Result<Report> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT id, created_at, figures FROM reports WHERE id = ? AND complete = 1")
            .bind(id)
            .fetch_optional(&db.pool)
            .await?;
    let (id, at, figures) = row.ok_or(Error::NotFound)?;
    report_from(id, at, &figures)
}

/// Removes unfinished rows no run can still finish: a failed or cancelled run leaves one that
/// nobody deletes. A row named by a run still active or halted is kept, so a resume has it,
/// and so is a row under five minutes old, which an active run may not have checkpointed yet.
pub(crate) async fn drop_unfinished(db: &Db) -> Result<()> {
    sqlx::query(
        "DELETE FROM reports WHERE complete = 0 \
         AND created_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 minutes') \
         AND id NOT IN (SELECT json_extract(checkpoint, '$.report') FROM agent_runs \
           WHERE agent = 'analytics' AND json_extract(checkpoint, '$.report') IS NOT NULL \
           AND state NOT IN ('completed','manual_required','refused','failed','cancelled'))",
    )
    .execute(&db.pool)
    .await?;
    Ok(())
}
