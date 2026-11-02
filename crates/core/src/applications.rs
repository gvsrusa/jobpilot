//! Applications the user is tracking, and the history of each.
//!
//! Plain Personal-class rows: no AI, no keychain. Every write and its history event share
//! one transaction. A status can be corrected to any other; the history keeps both ends.

use serde::{Deserialize, Serialize};

use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::pause;

pub const STATUSES: [&str; 8] = [
    "saved",
    "applied",
    "screening",
    "interviewing",
    "offer",
    "accepted",
    "rejected",
    "withdrawn",
];

const NAME_LIMIT: usize = 300;
const NOTES_LIMIT: usize = 20_000;
const LINK_LIMIT: usize = 2_000;

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

macro_rules! columns {
    () => {
        "id, company, role, job_text_id, resume_draft_id, letter_draft_id, link, country, \
         notes, status, applied_on, created_at, updated_at"
    };
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApplicationInput {
    pub company: String,
    pub role: String,
    pub job_text_id: Option<String>,
    pub resume_draft_id: Option<String>,
    pub letter_draft_id: Option<String>,
    pub link: String,
    pub country: String,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Application {
    pub id: String,
    pub company: String,
    pub role: String,
    pub job_text_id: Option<String>,
    pub resume_draft_id: Option<String>,
    pub letter_draft_id: Option<String>,
    pub link: String,
    pub country: String,
    pub notes: String,
    pub status: String,
    pub applied_on: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ApplicationEvent {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub note: String,
}

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}

fn too_long(text: &str, limit: usize) -> bool {
    text.chars().count() > limit
}

/// Trimmed, checked, and with the country upper-cased. Nothing is stored before this passes.
async fn clean(db: &Db, input: ApplicationInput) -> Result<ApplicationInput> {
    let company = input.company.trim().to_string();
    let role = input.role.trim().to_string();
    if company.is_empty() || role.is_empty() {
        return Err(bad("an application needs a company and a role"));
    }
    if too_long(&company, NAME_LIMIT) || too_long(&role, NAME_LIMIT) {
        return Err(bad("a company or role is at most 300 characters"));
    }
    if too_long(&input.notes, NOTES_LIMIT) {
        return Err(bad("notes are at most 20000 characters"));
    }
    let link = input.link.trim().to_string();
    // ponytail: a link is text to read and copy; only the scheme and length are checked.
    let lower = link.to_ascii_lowercase();
    if !link.is_empty() && !(lower.starts_with("https://") || lower.starts_with("http://"))
        || too_long(&link, LINK_LIMIT)
        || link.chars().any(char::is_control)
    {
        return Err(bad(
            "a link begins with https:// or http:// and is at most 2000 characters",
        ));
    }
    let country = input.country.trim().to_ascii_uppercase();
    if !country.is_empty()
        && !(country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()))
    {
        return Err(bad("a country is two letters, like PT"));
    }
    let (job, resume, letter): (bool, bool, bool) = sqlx::query_as(
        "SELECT ?1 IS NULL OR EXISTS (SELECT 1 FROM job_texts WHERE id = ?1), \
         ?2 IS NULL OR EXISTS (SELECT 1 FROM drafts WHERE id = ?2 AND kind = 'resume'), \
         ?3 IS NULL OR EXISTS (SELECT 1 FROM drafts WHERE id = ?3 AND kind = 'cover_letter')",
    )
    .bind(&input.job_text_id)
    .bind(&input.resume_draft_id)
    .bind(&input.letter_draft_id)
    .fetch_one(&db.pool)
    .await?;
    if !(job && resume && letter) {
        return Err(bad("a linked job text or draft does not exist"));
    }
    Ok(ApplicationInput {
        company,
        role,
        link,
        country,
        ..input
    })
}

fn check_status(status: &str) -> Result<()> {
    if STATUSES.contains(&status) {
        Ok(())
    } else {
        Err(bad(
            "a status is saved, applied, screening, interviewing, offer, accepted, rejected, or withdrawn",
        ))
    }
}

/// `YYYY-MM-DD` and a real calendar day, checked by SQLite.
async fn check_date(db: &Db, on: &str) -> Result<()> {
    let shaped = on.len() == 10
        && on.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if shaped {
        let (same,): (bool,) = sqlx::query_as("SELECT date(?1) = ?1")
            .bind(on)
            .fetch_one(&db.pool)
            .await
            .unwrap_or((false,));
        if same {
            return Ok(());
        }
    }
    Err(bad("a date is written YYYY-MM-DD"))
}

#[allow(clippy::too_many_arguments)]
async fn event(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    actor: &str,
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    note: &str,
) -> Result<()> {
    sqlx::query(concat!(
        "INSERT INTO application_events \
         (application_id, at, actor, kind, from_status, to_status, note) \
         VALUES (?, ",
        now!(),
        ", ?, ?, ?, ?, ?)"
    ))
    .bind(id)
    .bind(actor)
    .bind(kind)
    .bind(from)
    .bind(to)
    .bind(note)
    .execute(tx)
    .await?;
    Ok(())
}

pub async fn add(db: &Db, actor: &str, input: ApplicationInput) -> Result<Application> {
    let input = clean(db, input).await?;
    let mut tx = db.pool.begin().await?;
    let app: Application = sqlx::query_as(concat!(
        "INSERT INTO applications (id, company, role, job_text_id, resume_draft_id, \
         letter_draft_id, link, country, notes, status, created_at, updated_at) \
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ?, ?, ?, 'saved', ",
        now!(),
        ", ",
        now!(),
        ") RETURNING ",
        columns!()
    ))
    .bind(&input.company)
    .bind(&input.role)
    .bind(&input.job_text_id)
    .bind(&input.resume_draft_id)
    .bind(&input.letter_draft_id)
    .bind(&input.link)
    .bind(&input.country)
    .bind(&input.notes)
    .fetch_one(&mut *tx)
    .await?;
    event(&mut tx, &app.id, actor, "created", None, Some("saved"), "").await?;
    tx.commit().await?;
    Ok(app)
}

pub async fn get(db: &Db, id: &str) -> Result<Application> {
    sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM applications WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)
}

/// Most recently changed first.
pub async fn list(db: &Db, status: Option<&str>) -> Result<Vec<Application>> {
    if let Some(status) = status {
        check_status(status)?;
    }
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM applications WHERE (?1 IS NULL OR status = ?1) ORDER BY updated_at DESC, seq DESC"
    ))
    .bind(status)
    .fetch_all(&db.pool)
    .await?)
}

/// Replaces the fields; the status stays.
pub async fn update(
    db: &Db,
    actor: &str,
    id: &str,
    input: ApplicationInput,
) -> Result<Application> {
    let input = clean(db, input).await?;
    let mut tx = db.pool.begin().await?;
    let app: Application = sqlx::query_as(concat!(
        "UPDATE applications SET company = ?, role = ?, job_text_id = ?, resume_draft_id = ?, \
         letter_draft_id = ?, link = ?, country = ?, notes = ?, updated_at = ",
        now!(),
        " WHERE id = ? RETURNING ",
        columns!()
    ))
    .bind(&input.company)
    .bind(&input.role)
    .bind(&input.job_text_id)
    .bind(&input.resume_draft_id)
    .bind(&input.letter_draft_id)
    .bind(&input.link)
    .bind(&input.country)
    .bind(&input.notes)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    event(&mut tx, id, actor, "edited", None, None, "").await?;
    tx.commit().await?;
    Ok(app)
}

/// What `set_status` refuses without touching a row, so a caller can ask before it starts.
pub(crate) async fn check_change(
    db: &Db,
    status: &str,
    on: Option<&str>,
    note: &str,
) -> Result<()> {
    check_status(status)?;
    if let Some(on) = on {
        check_date(db, on).await?;
    }
    if too_long(note.trim(), NOTES_LIMIT) {
        return Err(bad("a note is at most 20000 characters"));
    }
    Ok(())
}

/// The same status again writes nothing. The first move to `applied` stores `on`, or today's
/// UTC date; a later one keeps the date already there.
pub async fn set_status(
    db: &Db,
    actor: &str,
    id: &str,
    status: &str,
    on: Option<&str>,
    note: &str,
) -> Result<Application> {
    // Public, so it gates itself: the tracking agent is one caller, not the only possible one.
    pause::ensure_off(db).await?;
    check_change(db, status, on, note).await?;
    let note = note.trim();
    let mut tx = db.pool.begin().await?;
    let current: Application = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM applications WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    if current.status == status {
        // No status event, but a note the person typed is not dropped.
        if note.is_empty() {
            return Ok(current);
        }
        let app: Application = sqlx::query_as(concat!(
            "UPDATE applications SET updated_at = ",
            now!(),
            " WHERE id = ? RETURNING ",
            columns!()
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        event(&mut tx, id, actor, "note", None, None, note).await?;
        tx.commit().await?;
        return Ok(app);
    }
    let app: Application = sqlx::query_as(concat!(
        "UPDATE applications SET status = ?1, updated_at = ",
        now!(),
        ", applied_on = CASE WHEN ?1 = 'applied' AND applied_on IS NULL \
         THEN coalesce(?2, date('now', 'localtime')) ELSE applied_on END \
         WHERE id = ?3 RETURNING ",
        columns!()
    ))
    .bind(status)
    .bind(on)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    event(
        &mut tx,
        id,
        actor,
        "status",
        Some(&current.status),
        Some(status),
        note,
    )
    .await?;
    tx.commit().await?;
    Ok(app)
}

/// `since` is a stored timestamp, so the two compare as text. The same millisecond counts.
pub(crate) async fn status_changed_since(db: &Db, id: &str, since: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM application_events \
         WHERE application_id = ? AND kind = 'status' AND at >= ?)",
    )
    .bind(id)
    .bind(since)
    .fetch_one(&db.pool)
    .await?)
}

pub async fn add_note(db: &Db, actor: &str, id: &str, note: &str) -> Result<()> {
    let note = note.trim();
    if note.is_empty() || too_long(note, NOTES_LIMIT) {
        return Err(bad("a note is 1 to 20000 characters"));
    }
    let mut tx = db.pool.begin().await?;
    let touched = sqlx::query(concat!(
        "UPDATE applications SET updated_at = ",
        now!(),
        " WHERE id = ?"
    ))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if touched.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    event(&mut tx, id, actor, "note", None, None, note).await?;
    tx.commit().await?;
    Ok(())
}

/// Oldest first.
pub async fn history(db: &Db, id: &str) -> Result<Vec<ApplicationEvent>> {
    get(db, id).await?;
    Ok(sqlx::query_as(
        "SELECT id, at, actor, kind, from_status, to_status, note FROM application_events \
         WHERE application_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(&db.pool)
    .await?)
}

/// The events go with it. The audit row holds the id and a count, never the text.
pub async fn remove(db: &Db, actor: &str, id: &str) -> Result<()> {
    let mut tx = db.pool.begin().await?;
    let (events,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM application_events WHERE application_id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let gone = sqlx::query("DELETE FROM applications WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    audit::record_to(
        &mut *tx,
        actor,
        "application_removed",
        Some(id),
        serde_json::json!({ "events": events }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
