//! The job store. A job is a posting's fields over a `job_texts` row, which keeps the untrusted
//! text. A job imported before schema 13 and one pasted now share their text's id, so a draft's
//! `job_text_id` and a job id name the same thing.

pub mod company;
pub mod dedupe;
pub mod geo;
pub mod link;
pub mod pay;
pub mod prefs;
pub mod rules;
pub mod score;
pub mod semantic;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::audit;
use crate::connectors::{self, ConnectorDecl, Operation, Posting, Target, TargetKind, gate};
use crate::db::Db;
use crate::documents::fts_query;
use crate::error::{Error, Result};
use crate::knowledge;
use crate::pause;
use crate::profile::{self, Stated};
use crate::runner;

use link::{LinkRoute, attribution_of, connector_of, route};

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

/// Parsers read no more than this much of a text.
pub(crate) const PARSE_LIMIT: usize = 200_000;

/// The text up to [`PARSE_LIMIT`] bytes, cut on a character boundary.
pub(crate) fn capped(text: &str) -> &str {
    let mut end = text.len().min(PARSE_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
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
    fn column(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

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
    /// `None` when the source names no period. It is never guessed, and nothing compares a
    /// range whose period is unknown.
    pub period: Option<PayPeriod>,
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
    /// Fields a filter was asked about that this job does not state: `country`, `work_mode`.
    pub not_stated: Vec<&'static str>,
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
                m.score AS score, m.inputs AS inputs \
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
    inputs: Option<String>,
}

impl JobRow {
    /// The job, and the inputs its stored score was made from.
    fn into_job(self) -> Result<(Job, Option<String>)> {
        let skills = serde_json::from_str(&self.skills)
            .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))?;
        // A range with no stored period is shown with the period unknown: it is never guessed.
        let pay = (self.pay_min.is_some() || self.pay_max.is_some()).then(|| PayRange {
            min: self.pay_min,
            max: self.pay_max,
            currency: self.pay_currency,
            period: self.pay_period.as_deref().and_then(PayPeriod::from_column),
        });
        let inputs = self.inputs;
        let job = Job {
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
            stale: false,
            not_stated: Vec::new(),
        };
        Ok((job, inputs))
    }
}

/// Rows to jobs, each score marked stale when its stored inputs differ from the current ones.
async fn finish(db: &Db, rows: Vec<JobRow>) -> Result<Vec<Job>> {
    let me = if rows.iter().any(|row| row.inputs.is_some()) {
        Some(score::candidate(db).await?)
    } else {
        None
    };
    rows.into_iter()
        .map(|row| {
            let (mut job, inputs) = row.into_job()?;
            if let (Some(me), Some(stored)) = (&me, inputs) {
                job.stale = score::inputs_key(&job, me) != stored;
            }
            Ok(job)
        })
        .collect()
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
        // The same rule a fetched link passes: https, no user name, no port.
        Some(url) if !crate::connectors::link_ok(url, &[]) => {
            return Err(bad(
                "a link is https with no user name or port, such as https://example.com/job",
            ));
        }
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
        Ok(()) => {
            apply_rules(db, &text.id).await?;
            get(db, &text.id).await
        }
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

/// Best score first, unscored last, then oldest first seen. A filter on country, work mode, or
/// pay drops only a job that states something else: a job that does not state the field stays,
/// marked in `not_stated`.
// ponytail: country and pay are tested in Rust after the read, so the whole match set is read
// before the limit applies; fine to a few thousand jobs.
pub async fn list(db: &Db, filter: &JobFilter, limit: i64) -> Result<Vec<Job>> {
    let query = filter.query.as_deref().and_then(fts_query);
    let rows: Vec<JobRow> = sqlx::query_as(concat!(
        select!(),
        "WHERE (?1 IS NULL OR j.work_mode = ?1 OR j.work_mode IS NULL) \
           AND (?2 IS NULL OR j.seq IN (SELECT rowid FROM jobs_fts WHERE jobs_fts MATCH ?2)) \
           AND (?3 OR j.duplicate_of IS NULL) \
         ORDER BY m.score IS NULL, m.score DESC, j.first_seen_at, j.seq"
    ))
    .bind(filter.work_mode.as_deref())
    .bind(query)
    .bind(filter.include_duplicates)
    .fetch_all(&db.pool)
    .await?;
    let mut kept = Vec::new();
    for mut job in finish(db, rows).await? {
        if filter.work_mode.is_some() && job.work_mode.is_none() {
            job.not_stated.push("work_mode");
        }
        if let Some(country) = filter.country.as_deref() {
            match country_fit(&job, country) {
                Some(false) => continue,
                None => job.not_stated.push("country"),
                Some(true) => {}
            }
        }
        if let Some(wish) = &filter.min_pay {
            match job.pay.as_ref().map(|pay| pay::compare(pay, wish)) {
                Some(pay::PayCompare::Below { .. }) => continue,
                Some(pay::PayCompare::Meets) => {}
                _ => job.not_stated.push("pay"),
            }
        }
        kept.push(job);
    }
    kept.truncate(usize::try_from(limit.max(0)).unwrap_or(0));
    Ok(kept)
}

/// Whether the job is in the country: `Some(true)` if its country is equal, it is a remote
/// job open to anywhere, or its region holds the country; `Some(false)` if it states another
/// place; `None` if it states none.
fn country_fit(job: &Job, country: &str) -> Option<bool> {
    let scope = geo::scope_of(&job.location);
    if job.work_mode.as_deref() == Some("remote") && scope == geo::Scope::Anywhere {
        return Some(true);
    }
    if let geo::Scope::Region(region) = scope
        && geo::in_region(region, country)
    {
        return Some(true);
    }
    match &job.country {
        Some(own) => Some(own.eq_ignore_ascii_case(country.trim())),
        None if matches!(scope, geo::Scope::Region(_)) => Some(false),
        None => None,
    }
}

pub async fn get(db: &Db, id: &str) -> Result<Job> {
    let row: JobRow = sqlx::query_as(concat!(select!(), "WHERE j.id = ?"))
        .bind(id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or(Error::NotFound)?;
    Ok(finish(db, vec![row]).await?.remove(0))
}

/// Removes the job, its score, and its search row. Its text goes too, every version of it,
/// unless a draft cites that one.
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
    // A summary fact cites its job; with the job gone it cites nothing.
    sqlx::query("DELETE FROM company_notes WHERE origin = 'model' AND source_ref = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM job_texts WHERE (id = ?1 OR version_of = ?2) \
           AND NOT EXISTS (SELECT 1 FROM drafts WHERE job_text_id = job_texts.id) \
           AND NOT EXISTS (SELECT 1 FROM jobs WHERE job_text_id = job_texts.id)",
    )
    .bind(&text_id)
    .bind(id)
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

/// What an upsert did, with the job's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upserted {
    Added(String),
    Updated(String),
    Unchanged(String),
}

const SOURCE_LIMIT: i64 = 50;
const QUERY_LIMIT: usize = 200;
macro_rules! source_columns {
    () => {
        "id, connector, target, query, cursor, created_at"
    };
}

/// Stores a read posting under its `(connector, board, external_id)`. The same text is
/// `Unchanged` and only moves `last_seen_at`. A changed text is a new `job_texts` row that the
/// job moves to: the old one stays, since a draft may cite it, and the job's score goes,
/// since it described the old text. A text over the import limit, or with nothing readable,
/// stores nothing and says so with the error of [`knowledge::import_job_text`].
pub async fn upsert_posting(
    db: &Db,
    decl: &ConnectorDecl,
    board: &str,
    source_id: Option<&str>,
    posting: &Posting,
) -> Result<Upserted> {
    // Another handle can store the same new posting between this read and the insert. The
    // loser then finds it on the second pass and takes the path for a stored one.
    for _ in 0..2 {
        let existing: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT j.id, t.raw, j.title, t.normalized FROM jobs j JOIN job_texts t ON t.id = j.job_text_id \
             WHERE j.connector = ? AND j.board = ? AND j.external_id = ?",
        )
        .bind(decl.name)
        .bind(board)
        .bind(&posting.external_id)
        .fetch_optional(&db.pool)
        .await?;
        if let Some((id, raw, title, normalized)) = &existing
            && *raw == posting.raw
        {
            // A row the connector filled is read again, so one stored with team tags as skills
            // is mended. What a person, a model, or the rules set stays.
            sqlx::query(concat!(
                "UPDATE jobs SET last_seen_at = ",
                now!(),
                ", skills = CASE WHEN fields_by = 'connector' THEN ? ELSE skills END WHERE id = ?"
            ))
            .bind(skills_of(title, normalized, posting)?)
            .bind(id)
            .execute(&db.pool)
            .await?;
            return Ok(Upserted::Unchanged(id.clone()));
        }
        let text = knowledge::import_job_text(db, &posting.title, &posting.raw).await?;
        let written = match &existing {
            Some((id, ..)) => revise(db, id, &text, posting)
                .await
                .map(|()| Some(Upserted::Updated(id.clone()))),
            None => insert_posting(db, decl, board, source_id, &text, posting)
                .await
                .map(|won| won.then(|| Upserted::Added(text.id.clone()))),
        };
        match written {
            Ok(Some(done)) => {
                let (Upserted::Added(id) | Upserted::Updated(id) | Upserted::Unchanged(id)) = &done;
                apply_rules(db, id).await?;
                return Ok(done);
            }
            lost_or_failed => {
                // A text no job holds is left to nothing.
                let _ = sqlx::query("DELETE FROM job_texts WHERE id = ?")
                    .bind(&text.id)
                    .execute(&db.pool)
                    .await;
                lost_or_failed?;
            }
        }
    }
    Err(Error::Failed("posting_raced".into()))
}

async fn company_id(tx: &mut sqlx::SqliteConnection, name: &str) -> Result<Option<String>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        sqlx::query_scalar(concat!(
            "INSERT INTO companies (id, name, key, created_at) \
             VALUES (lower(hex(randomblob(16))), ?, ?, ",
            now!(),
            ") ON CONFLICT (key) DO UPDATE SET key = key RETURNING id"
        ))
        .bind(name)
        .bind(company_key(name))
        .fetch_one(tx)
        .await?,
    ))
}

/// The skills a posting names, read as a pasted job is: the title and text, and the tags only
/// where they are in the vocabulary. A team or department tag is not a skill.
fn skills_of(title: &str, text: &str, posting: &Posting) -> Result<String> {
    let found = rules::skills_in(
        &format!("{title}\n{text}"),
        &posting.tags,
        &rules::vocabulary(&[]),
    );
    serde_json::to_string(&found).map_err(|error| bad(&error.to_string()))
}

async fn insert_posting(
    db: &Db,
    decl: &ConnectorDecl,
    board: &str,
    source_id: Option<&str>,
    text: &knowledge::JobText,
    posting: &Posting,
) -> Result<bool> {
    let mut tx = db.pool.begin().await?;
    let company = company_id(&mut tx, &posting.company).await?;
    let pay = posting.pay.as_ref();
    // A source removed since the run began leaves the job with none.
    let seq: Option<i64> = sqlx::query_scalar(concat!(
        "INSERT INTO jobs (id, job_text_id, connector, source_id, board, external_id, title, \
                           company_id, location, country, work_mode, employment_type, url, \
                           apply_url, attribution, posted_on, pay_min, pay_max, pay_currency, \
                           pay_period, skills, fields_by, first_seen_at, last_seen_at) \
         VALUES (?1, ?1, ?2, (SELECT id FROM job_sources WHERE id = ?3), ?4, ?5, ?6, ?7, ?8, ?9, \
                 ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, 'connector', ",
        now!(),
        ", ",
        now!(),
        ") ON CONFLICT (connector, board, external_id) DO NOTHING RETURNING seq"
    ))
    .bind(&text.id)
    .bind(decl.name)
    .bind(source_id)
    .bind(board)
    .bind(&posting.external_id)
    .bind(&text.title)
    .bind(company)
    .bind(&posting.location)
    .bind(posting.country.as_deref())
    .bind(posting.work_mode)
    .bind(posting.employment_type.as_deref())
    .bind(&posting.url)
    .bind(posting.apply_url.as_deref())
    .bind(decl.attribution)
    .bind(posting.posted_on.as_deref())
    .bind(pay.and_then(|pay| pay.min))
    .bind(pay.and_then(|pay| pay.max))
    .bind(pay.and_then(|pay| pay.currency.as_deref()))
    .bind(pay.and_then(|pay| pay.period).map(PayPeriod::column))
    .bind(skills_of(&text.title, &text.normalized, posting)?)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(seq) = seq else {
        // Another handle stored this posting first.
        tx.rollback().await?;
        return Ok(false);
    };
    sqlx::query("INSERT INTO jobs_fts (rowid, title, company, body) VALUES (?, ?, ?, ?)")
        .bind(seq)
        .bind(&text.title)
        .bind(posting.company.trim())
        .bind(&text.normalized)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Moves a job to its new text and fields (the rules refill what the new text leaves blank), and drops its score and its old search row.
async fn revise(db: &Db, id: &str, text: &knowledge::JobText, posting: &Posting) -> Result<()> {
    let mut tx = db.pool.begin().await?;
    let pay = posting.pay.as_ref();
    // The text it leaves stays, tied to this job so removing the job removes it too.
    sqlx::query(
        "UPDATE job_texts SET version_of = ?1 \
         WHERE id = (SELECT job_text_id FROM jobs WHERE id = ?1) AND id <> ?2",
    )
    .bind(id)
    .bind(&text.id)
    .execute(&mut *tx)
    .await?;
    let by: String = sqlx::query_scalar("SELECT fields_by FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    // What a person set stays: only the text and the links follow the source. A `rules` or
    // `model` row came from the old text, so it goes back to `none` and the rules read the new
    // text afresh; its evaluation goes with the score below.
    let (seq, title, company_name): (i64, String, String) = if by == "user" {
        sqlx::query_as(concat!(
            "UPDATE jobs SET job_text_id = ?, url = ?, apply_url = ?, posted_on = ?, last_seen_at = ",
            now!(),
            " WHERE id = ? RETURNING seq, title, \
             coalesce((SELECT name FROM companies WHERE id = jobs.company_id), '')"
        ))
        .bind(&text.id)
        .bind(&posting.url)
        .bind(posting.apply_url.as_deref())
        .bind(posting.posted_on.as_deref())
        .bind(id)
        .fetch_one(&mut *tx)
        .await?
    } else {
        let company = company_id(&mut tx, &posting.company).await?;
        let seq: i64 = sqlx::query_scalar(concat!(
        "UPDATE jobs SET job_text_id = ?, title = ?, company_id = ?, location = ?, country = ?, \
                work_mode = ?, employment_type = ?, url = ?, apply_url = ?, posted_on = ?, \
                pay_min = ?, pay_max = ?, pay_currency = ?, pay_period = ?, skills = ?, \
                seniority = NULL, mentions_authorization = 0, dedupe_key = '', \
                fields_by = CASE WHEN fields_by IN ('rules','model') THEN 'none' ELSE fields_by END, \
                last_seen_at = ",
        now!(),
        " WHERE id = ? RETURNING seq"
    ))
    .bind(&text.id)
    .bind(&text.title)
    .bind(company)
    .bind(&posting.location)
    .bind(posting.country.as_deref())
    .bind(posting.work_mode)
    .bind(posting.employment_type.as_deref())
    .bind(&posting.url)
    .bind(posting.apply_url.as_deref())
    .bind(posting.posted_on.as_deref())
    .bind(pay.and_then(|pay| pay.min))
    .bind(pay.and_then(|pay| pay.max))
    .bind(pay.and_then(|pay| pay.currency.as_deref()))
    .bind(pay.and_then(|pay| pay.period).map(PayPeriod::column))
    .bind(skills_of(&text.title, &text.normalized, posting)?)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
        (seq, text.title.clone(), posting.company.trim().to_owned())
    };
    sqlx::query("DELETE FROM match_results WHERE job_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM jobs_fts WHERE rowid = ?")
        .bind(seq)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO jobs_fts (rowid, title, company, body) VALUES (?, ?, ?, ?)")
        .bind(seq)
        .bind(&title)
        .bind(&company_name)
        .bind(&text.normalized)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A source the connector can read, for a target its own request builder accepts. The register
/// gate runs first, so a source with no declared read is refused, and one the register does
/// not allow is too. Adding the same source again returns the stored one.
pub async fn add_source(db: &Db, connector: &str, target: &str, query: &str) -> Result<SourceRow> {
    let found = connectors::find(connector).ok_or_else(|| bad("not a known source"))?;
    let decl = found.decl();
    let read = if decl.operations.contains(&Operation::ListBoard) {
        Operation::ListBoard
    } else {
        Operation::ListFeed
    };
    gate(decl, read)?;
    let target = target.trim();
    match decl.target {
        TargetKind::None if !target.is_empty() => return Err(bad("this source takes no target")),
        TargetKind::None => {}
        TargetKind::Board | TargetKind::Feed => {
            // The connector's own builder is the check: `clean_slug`, or the feed list.
            found.request(
                &Target {
                    name: target.into(),
                },
                0,
            )?;
        }
    }
    let query = query.trim();
    if query.chars().count() > QUERY_LIMIT {
        return Err(bad("a filter is up to 200 characters"));
    }
    // One statement counts and inserts, so racing adds cannot pass the cap, and a repeat of
    // the same source returns its row. At the cap the select yields nothing, hence the lookup.
    let added: Option<SourceRow> = sqlx::query_as(concat!(
        "INSERT INTO job_sources (id, connector, target, query, created_at) \
         SELECT lower(hex(randomblob(16))), ?1, ?2, ?3, ",
        now!(),
        " WHERE (SELECT COUNT(*) FROM job_sources) < ?4 \
         ON CONFLICT (connector, target, query) DO UPDATE SET query = excluded.query \
         RETURNING ",
        source_columns!()
    ))
    .bind(connector)
    .bind(target)
    .bind(query)
    .bind(SOURCE_LIMIT)
    .fetch_optional(&db.pool)
    .await?;
    if let Some(row) = added {
        return Ok(row);
    }
    let stored: Option<SourceRow> = sqlx::query_as(concat!(
        "SELECT ",
        source_columns!(),
        " FROM job_sources WHERE connector = ? AND target = ? AND query = ?"
    ))
    .bind(connector)
    .bind(target)
    .bind(query)
    .fetch_optional(&db.pool)
    .await?;
    stored.ok_or_else(|| bad("up to 50 sources"))
}

pub async fn list_sources(db: &Db) -> Result<Vec<SourceRow>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        source_columns!(),
        " FROM job_sources ORDER BY seq"
    ))
    .fetch_all(&db.pool)
    .await?)
}

pub async fn get_source(db: &Db, id: &str) -> Result<SourceRow> {
    sqlx::query_as(concat!(
        "SELECT ",
        source_columns!(),
        " FROM job_sources WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)
}

/// Its jobs stay, with no source.
pub async fn remove_source(db: &Db, id: &str) -> Result<()> {
    let gone = sqlx::query("DELETE FROM job_sources WHERE id = ?")
        .bind(id)
        .execute(&db.pool)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

/// A person says this job is not a duplicate: it leaves its group and no later pass puts it
/// back. Jobs that were grouped under it regroup among themselves on the next pass.
pub async fn split(db: &Db, actor: &str, id: &str) -> Result<()> {
    runner::person(actor)?;
    let mut tx = db.pool.begin().await?;
    let done = sqlx::query("UPDATE jobs SET duplicate_of = NULL, not_duplicate = 1 WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    audit::record_to(
        &mut *tx,
        actor,
        "job_split",
        Some(id),
        serde_json::json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The leader of the job's group first, then the jobs grouped under it.
pub async fn group(db: &Db, id: &str) -> Result<Vec<Job>> {
    let job = get(db, id).await?;
    let lead = job.duplicate_of.unwrap_or(job.id);
    let rows: Vec<JobRow> = sqlx::query_as(concat!(
        select!(),
        "WHERE j.id = ?1 OR j.duplicate_of = ?1 \
         ORDER BY j.id = ?1 DESC, j.first_seen_at, j.seq"
    ))
    .bind(lead)
    .fetch_all(&db.pool)
    .await?;
    finish(db, rows).await
}

#[derive(FromRow)]
struct RulesRow {
    title: String,
    company: Option<String>,
    location: String,
    country: Option<String>,
    work_mode: Option<String>,
    pay_min: Option<i64>,
    pay_max: Option<i64>,
    skills: String,
    fields_by: String,
    dedupe_key: String,
    normalized: String,
}

/// Fills a job's empty columns by rules, so it is usable at once with no agent. A column that
/// holds anything stays as it is. `fields_by` becomes `rules` only from `none`: a job a
/// connector filled stays the connector's, and one a person or a model set is not touched at
/// all. The dedupe key is filled for every job, since it is not a field anyone sets.
// ponytail: the candidate's own skills do not join the vocabulary yet; nothing stores them in a
// form this can read without a profile call, so only `skills.txt` is matched.
pub async fn apply_rules(db: &Db, id: &str) -> Result<()> {
    let row: RulesRow = sqlx::query_as(
        "SELECT j.title, c.name AS company, j.location, j.country, j.work_mode, j.pay_min, \
                j.pay_max, j.skills, j.fields_by, j.dedupe_key, t.normalized \
         FROM jobs j JOIN job_texts t ON t.id = j.job_text_id \
         LEFT JOIN companies c ON c.id = j.company_id WHERE j.id = ?",
    )
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)?;
    let mut country = row.country.clone();
    let mut work_mode = row.work_mode.clone();
    let mut tx = db.pool.begin().await?;
    if matches!(row.fields_by.as_str(), "none" | "connector") {
        let found = rules::fields(
            &row.title,
            &row.location,
            &row.normalized,
            &[],
            &rules::vocabulary(&[]),
        );
        country = country.or(found.country);
        work_mode = work_mode.or_else(|| found.work_mode.map(str::to_owned));
        let empty_skills = row.skills.trim() == "[]";
        let skills = (empty_skills && !found.skills.is_empty())
            .then(|| serde_json::to_string(&found.skills))
            .transpose()
            .map_err(|error| bad(&error.to_string()))?;
        let pay = found
            .pay
            .filter(|_| row.pay_min.is_none() && row.pay_max.is_none());
        sqlx::query(
            "UPDATE jobs SET country = ?, work_mode = ?, \
                    seniority = coalesce(seniority, ?), skills = coalesce(?, skills), \
                    pay_min = coalesce(pay_min, ?), pay_max = coalesce(pay_max, ?), \
                    pay_currency = coalesce(pay_currency, ?), pay_period = coalesce(pay_period, ?), \
                    mentions_authorization = max(mentions_authorization, ?) WHERE id = ?",
        )
        .bind(&country)
        .bind(&work_mode)
        .bind(found.seniority.map(rules::Seniority::as_str))
        .bind(skills)
        .bind(pay.as_ref().and_then(|p| p.min))
        .bind(pay.as_ref().and_then(|p| p.max))
        .bind(pay.as_ref().and_then(|p| p.currency.as_deref()))
        .bind(pay.as_ref().and_then(|p| p.period).map(PayPeriod::column))
        .bind(found.mentions_authorization)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if row.fields_by == "none" {
            sqlx::query("UPDATE jobs SET fields_by = 'rules' WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }
    if row.dedupe_key.is_empty() {
        let key = dedupe::dedupe_key(
            row.company.as_deref().unwrap_or(""),
            &row.title,
            country.as_deref(),
            work_mode.as_deref(),
        );
        sqlx::query("UPDATE jobs SET dedupe_key = ? WHERE id = ?")
            .bind(key)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Runs the rules over each job they have not read. Only a job migrated from 0.4 waits here, and
/// the rules move it to `rules`, so a later call finds nothing.
pub(crate) async fn apply_rules_to_unread(db: &Db) -> Result<()> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM jobs WHERE fields_by = 'none'")
        .fetch_all(&db.pool)
        .await?;
    for id in ids {
        apply_rules(db, &id).await?;
    }
    Ok(())
}

/// What the profile says about work authorization for the job's country. It is only shown: no
/// score, filter, or draft step reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthNote {
    /// The job states no country.
    NoCountry,
    /// The profile has no row for exactly this country.
    NotStated { country: String },
    Stated {
        country: String,
        authorized: Stated,
        needs_sponsorship: Stated,
    },
    /// A row exists and the key that opens it is not available.
    Locked,
}

pub async fn authorization_note(db: &Db, country: Option<&str>) -> Result<AuthNote> {
    let Some(country) = country.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok(AuthNote::NoCountry);
    };
    match crate::memory::authorization_for(db, country).await {
        Ok(answer) if answer.stated => Ok(AuthNote::Stated {
            country: answer.country,
            authorized: answer.authorized,
            needs_sponsorship: answer.needs_sponsorship,
        }),
        Ok(answer) => Ok(AuthNote::NotStated {
            country: answer.country,
        }),
        Err(Error::Validation(_)) => Ok(AuthNote::NotStated {
            country: country.to_ascii_uppercase(),
        }),
        Err(error) => profile::degraded(error).map(|_| AuthNote::Locked),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobDetail {
    pub job: Job,
    pub text: String,
    pub findings: Vec<knowledge::Finding>,
    pub explanation: Option<score::Explanation>,
    pub semantic: Option<semantic::Semantic>,
    /// The other jobs in this job's group: the leader and its followers, never this job.
    pub group: Vec<Job>,
    pub authorization: AuthNote,
    /// The posting speaks of work authorization at all. Nothing reads its words.
    pub mentions_authorization: bool,
    /// The company's pasted notes and the model's summary, in the order they were added.
    pub notes: Vec<company::CompanyNote>,
}

pub async fn detail(db: &Db, id: &str) -> Result<JobDetail> {
    let job = get(db, id).await?;
    let text = knowledge::get_job_text(db, &job.job_text_id).await?;
    let stored: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT explanation, semantic FROM match_results WHERE job_id = ?")
            .bind(&job.id)
            .fetch_optional(&db.pool)
            .await?;
    let (explanation, semantic) = match stored {
        Some((explanation, semantic)) => (
            Some(
                serde_json::from_str(&explanation)
                    .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))?,
            ),
            semantic
                .as_deref()
                .map(|raw| {
                    serde_json::from_str(raw)
                        .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))
                })
                .transpose()?,
        ),
        None => (None, None),
    };
    let flagged: i64 = sqlx::query_scalar("SELECT mentions_authorization FROM jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(&db.pool)
        .await?;
    let notes = match &job.company_id {
        Some(company) => company::notes(db, company).await?,
        None => Vec::new(),
    };
    let mut others = group(db, &job.id).await?;
    others.retain(|other| other.id != job.id);
    Ok(JobDetail {
        group: others,
        authorization: authorization_note(db, job.country.as_deref()).await?,
        mentions_authorization: flagged != 0,
        notes,
        text: text.normalized,
        findings: text.findings,
        explanation,
        semantic,
        job,
    })
}
