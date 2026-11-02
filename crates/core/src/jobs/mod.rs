//! The job store. A job is a posting's fields over a `job_texts` row, which keeps the untrusted
//! text. A job imported before schema 13 and one pasted now share their text's id, so a draft's
//! `job_text_id` and a job id name the same thing.

pub mod link;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::audit;
use crate::db::Db;
use crate::documents::fts_query;
use crate::error::{Error, Result};
use crate::knowledge;
use crate::pause;
use crate::runner;

use link::{LinkRoute, attribution_of, connector_of, route};

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

const COMPANY_LIMIT: usize = 300;
const URL_LIMIT: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayPeriod {
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl PayPeriod {
    fn from_column(value: &str) -> Option<Self> {
        match value {
            "hour" => Some(Self::Hour),
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayRange {
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub currency: Option<String>,
    pub period: PayPeriod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayWish {
    pub amount: i64,
    pub currency: String,
    pub period: PayPeriod,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub job_text_id: String,
    pub connector: String,
    pub title: String,
    pub company: Option<String>,
    pub company_id: Option<String>,
    pub location: String,
    pub country: Option<String>,
    pub work_mode: Option<String>,
    pub employment_type: Option<String>,
    pub url: Option<String>,
    pub attribution: Option<String>,
    pub posted_on: Option<String>,
    pub pay: Option<PayRange>,
    pub seniority: Option<String>,
    pub skills: Vec<String>,
    pub fields_by: String,
    pub duplicate_of: Option<String>,
    pub duplicates: i64,
    pub findings: i64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub score: Option<i64>,
    pub stale: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PastedJob {
    pub title: String,
    pub company: String,
    pub url: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JobFilter {
    pub country: Option<String>,
    pub work_mode: Option<String>,
    // ponytail: not applied until Task 8 reads pay.
    pub min_pay: Option<PayWish>,
    pub query: Option<String>,
    pub include_duplicates: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SourceRow {
    pub id: String,
    pub connector: String,
    pub target: String,
    pub query: String,
    pub cursor: i64,
    pub created_at: String,
}

macro_rules! select {
    () => {
        "SELECT j.id, j.job_text_id, j.connector, j.title, c.name AS company, j.company_id, \
                j.location, j.country, j.work_mode, j.employment_type, j.url, j.attribution, \
                j.posted_on, j.pay_min, j.pay_max, j.pay_currency, j.pay_period, j.seniority, \
                j.skills, j.fields_by, j.duplicate_of, \
                (SELECT count(*) FROM jobs d WHERE d.duplicate_of = j.id) AS duplicates, \
                json_array_length(t.findings) AS findings, j.first_seen_at, j.last_seen_at, \
                m.score AS score \
         FROM jobs j JOIN job_texts t ON t.id = j.job_text_id \
         LEFT JOIN companies c ON c.id = j.company_id \
         LEFT JOIN match_results m ON m.job_id = j.id "
    };
}

#[derive(FromRow)]
struct JobRow {
    id: String,
    job_text_id: String,
    connector: String,
    title: String,
    company: Option<String>,
    company_id: Option<String>,
    location: String,
    country: Option<String>,
    work_mode: Option<String>,
    employment_type: Option<String>,
    url: Option<String>,
    attribution: Option<String>,
    posted_on: Option<String>,
    pay_min: Option<i64>,
    pay_max: Option<i64>,
    pay_currency: Option<String>,
    pay_period: Option<String>,
    seniority: Option<String>,
    skills: String,
    fields_by: String,
    duplicate_of: Option<String>,
    duplicates: i64,
    findings: i64,
    first_seen_at: String,
    last_seen_at: String,
    score: Option<i64>,
}

impl JobRow {
    fn into_job(self) -> Result<Job> {
        let skills = serde_json::from_str(&self.skills)
            .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))?;
        // A range with no stored period is not shown: a period is never guessed.
        let pay = match self.pay_period.as_deref().and_then(PayPeriod::from_column) {
            Some(period) if self.pay_min.is_some() || self.pay_max.is_some() => Some(PayRange {
                min: self.pay_min,
                max: self.pay_max,
                currency: self.pay_currency,
                period,
            }),
            _ => None,
        };
        Ok(Job {
            id: self.id,
            job_text_id: self.job_text_id,
            connector: self.connector,
            title: self.title,
            company: self.company,
            company_id: self.company_id,
            location: self.location,
            country: self.country,
            work_mode: self.work_mode,
            employment_type: self.employment_type,
            url: self.url,
            attribution: self.attribution,
            posted_on: self.posted_on,
            pay,
            seniority: self.seniority,
            skills,
            fields_by: self.fields_by,
            duplicate_of: self.duplicate_of,
            duplicates: self.duplicates,
            findings: self.findings,
            first_seen_at: self.first_seen_at,
            last_seen_at: self.last_seen_at,
            score: self.score,
            // ponytail: nothing marks a score stale until scoring exists (Task 8).
            stale: false,
        })
    }
}

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}

/// The company name as a lookup key: case and spacing do not make a second company.
fn company_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A person pastes a posting, with its link if there is one. The link is routed and kept,
/// never requested. A link with no text is refused: nothing here may fetch it.
pub async fn import_pasted(db: &Db, input: PastedJob) -> Result<Job> {
    pause::ensure_off(db).await?;
    let url = input
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let routed = match url {
        Some(url) if url.chars().count() > URL_LIMIT => return Err(bad("a link is too long")),
        Some(url) => Some(route(url)?),
        None => None,
    };
    if input.text.trim().is_empty() {
        return Err(Error::Refused(needs_text(routed.as_ref())));
    }
    let company = input.company.trim();
    if company.chars().count() > COMPANY_LIMIT {
        return Err(bad("a company name is up to 300 characters"));
    }
    let connector = routed.as_ref().map_or("import", connector_of);
    let attribution = routed.as_ref().and_then(attribution_of);

    let text = knowledge::import_job_text(db, &input.title, &input.text).await?;
    let made = store(db, &text, company, connector, url, attribution).await;
    match made {
        Ok(()) => get(db, &text.id).await,
        Err(error) => {
            // The text exists only for this job, so a failed job leaves nothing behind.
            let _ = sqlx::query("DELETE FROM job_texts WHERE id = ?")
                .bind(&text.id)
                .execute(&db.pool)
                .await;
            Err(error)
        }
    }
}

fn needs_text(routed: Option<&LinkRoute>) -> String {
    let mut message = String::from("needs_text: paste the posting's text with its link");
    if let Some(
        route @ (LinkRoute::Lever { .. } | LinkRoute::Greenhouse { .. } | LinkRoute::Ashby { .. }),
    ) = routed
    {
        message.push_str(&format!(
            "; or read the whole board with `job source add {} <board>`",
            connector_of(route)
        ));
    }
    message
}

async fn store(
    db: &Db,
    text: &knowledge::JobText,
    company: &str,
    connector: &str,
    url: Option<&str>,
    attribution: Option<&str>,
) -> Result<()> {
    let mut tx = db.pool.begin().await?;
    let company_id: Option<String> = if company.is_empty() {
        None
    } else {
        Some(
            sqlx::query_scalar(concat!(
                "INSERT INTO companies (id, name, key, created_at) \
                 VALUES (lower(hex(randomblob(16))), ?, ?, ",
                now!(),
                ") ON CONFLICT (key) DO UPDATE SET key = key RETURNING id"
            ))
            .bind(company)
            .bind(company_key(company))
            .fetch_one(&mut *tx)
            .await?,
        )
    };
    let seq: i64 = sqlx::query_scalar(concat!(
        "INSERT INTO jobs (id, job_text_id, connector, title, company_id, url, attribution, \
                           first_seen_at, last_seen_at) \
         VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ",
        now!(),
        ", ",
        now!(),
        ") RETURNING seq"
    ))
    .bind(&text.id)
    .bind(connector)
    .bind(&text.title)
    .bind(company_id)
    .bind(url)
    .bind(attribution)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO jobs_fts (rowid, title, company, body) VALUES (?, ?, ?, ?)")
        .bind(seq)
        .bind(&text.title)
        .bind(company)
        .bind(&text.normalized)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Best score first, unscored last, then oldest first seen.
pub async fn list(db: &Db, filter: &JobFilter, limit: i64) -> Result<Vec<Job>> {
    let query = filter.query.as_deref().and_then(fts_query);
    let rows: Vec<JobRow> = sqlx::query_as(concat!(
        select!(),
        "WHERE (?1 IS NULL OR j.country = ?1 COLLATE NOCASE) \
           AND (?2 IS NULL OR j.work_mode = ?2) \
           AND (?3 IS NULL OR j.seq IN (SELECT rowid FROM jobs_fts WHERE jobs_fts MATCH ?3)) \
           AND (?4 OR j.duplicate_of IS NULL) \
         ORDER BY m.score IS NULL, m.score DESC, j.first_seen_at, j.seq LIMIT ?5"
    ))
    .bind(filter.country.as_deref())
    .bind(filter.work_mode.as_deref())
    .bind(query)
    .bind(filter.include_duplicates)
    .bind(limit.max(0))
    .fetch_all(&db.pool)
    .await?;
    rows.into_iter().map(JobRow::into_job).collect()
}

pub async fn get(db: &Db, id: &str) -> Result<Job> {
    let row: JobRow = sqlx::query_as(concat!(select!(), "WHERE j.id = ?"))
        .bind(id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or(Error::NotFound)?;
    row.into_job()
}

/// Removes the job, its score, and its search row. Its text goes too unless a draft cites it.
pub async fn remove(db: &Db, actor: &str, id: &str) -> Result<()> {
    runner::person(actor)?;
    let mut tx = db.pool.begin().await?;
    let (seq, text_id): (i64, String) =
        sqlx::query_as("SELECT seq, job_text_id FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Error::NotFound)?;
    sqlx::query("DELETE FROM jobs_fts WHERE rowid = ?")
        .bind(seq)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM jobs WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM job_texts WHERE id = ?1 \
           AND NOT EXISTS (SELECT 1 FROM drafts WHERE job_text_id = ?1) \
           AND NOT EXISTS (SELECT 1 FROM jobs WHERE job_text_id = ?1)",
    )
    .bind(&text_id)
    .execute(&mut *tx)
    .await?;
    audit::record_to(
        &mut *tx,
        actor,
        "job_removed",
        Some(id),
        serde_json::json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The current `job_texts` id for a job id, or the id unchanged if it names a text.
pub async fn text_id(db: &Db, id: &str) -> Result<String> {
    sqlx::query_scalar(
        "SELECT job_text_id FROM jobs WHERE id = ?1 \
         UNION ALL SELECT id FROM job_texts WHERE id = ?1 LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)
}
