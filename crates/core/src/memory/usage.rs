//! Taking an answer's text for one application. A sealed class needs a person's confirmation
//! of this answer version on this application, and a stale one needs a fresh confirmation.
//! Every check runs before any decrypt, and no audit row or return value but [`UsedAnswer`]
//! holds the text. Work authorization has no stored answer: [`authorization_for`] reads the
//! profile row of one exact country.

use serde::Serialize;
use serde_json::json;

use super::{Class, StoredAnswer, clean_country, is_cleared, person, plain_texts, stored_answer};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::profile::{self, Stated};
use crate::{applications, audit};

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

#[derive(Debug, Clone, Serialize)]
pub struct UsedAnswer {
    pub application_id: String,
    pub answer_id: String,
    pub class: Class,
    pub text: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppliedAnswer {
    pub answer_id: String,
    pub question: String,
    pub class: Class,
    pub confirmed_at: Option<String>,
    pub used_at: Option<String>,
    /// The confirmation still matches the answer's version, and the answer is not stale.
    pub current: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorizationAnswer {
    pub country: String,
    pub authorized: Stated,
    pub needs_sponsorship: Stated,
    pub note: String,
    pub stated: bool,
}

/// An approved answer, with no text.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovedRow {
    pub answer_id: String,
    pub question: String,
    pub class: Class,
    pub stale: bool,
}

/// Every approved answer that still has text, oldest first. Asks the keychain nothing.
pub async fn approved_for(db: &Db) -> Result<Vec<ApprovedRow>> {
    let rows: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT a.id, q.text, q.class, \
         coalesce(julianday('now') - julianday(a.reviewed_at) > ?1, 1) \
         FROM memory_answers a JOIN memory_questions q ON q.id = a.question_id \
         WHERE a.approved = 1 AND (a.body <> '' OR a.body_enc IS NOT NULL) ORDER BY a.seq",
    )
    .bind(super::FRESH_DAYS)
    .fetch_all(&db.pool)
    .await?;
    rows.into_iter()
        .map(|(answer_id, question, class, stale)| {
            Ok(ApprovedRow {
                answer_id,
                question,
                class: class.parse()?,
                stale,
            })
        })
        .collect()
}

/// The checks both calls share, in order: actor, answer, application, class, approval.
async fn checked(
    db: &Db,
    actor: &str,
    answer_id: &str,
    application_id: &str,
) -> Result<(StoredAnswer, Class)> {
    person(actor)?;
    let row = stored_answer(db, answer_id).await?;
    applications::get(db, application_id).await?;
    let class = row.class()?;
    if class == Class::WorkAuthorization {
        return Err(Error::Refused("use_profile_by_country".into()));
    }
    if !row.approved {
        return Err(Error::Refused("answer_not_approved".into()));
    }
    Ok((row, class))
}

/// A person confirms this answer version for this application, which also marks the answer
/// reviewed. Editing the answer voids it. The audit row holds ids and the class, no text.
pub async fn confirm_use(
    db: &Db,
    actor: &str,
    answer_id: &str,
    application_id: &str,
) -> Result<()> {
    let (row, class) = checked(db, actor, answer_id, application_id).await?;
    let mut write = db.pool.begin().await?;
    let reviewed = sqlx::query(concat!(
        "UPDATE memory_answers SET reviewed_at = ",
        now!(),
        ", updated_at = ",
        now!(),
        " WHERE id = ? AND version = ? AND approved = 1"
    ))
    .bind(answer_id)
    .bind(row.version)
    .execute(&mut *write)
    .await?
    .rows_affected();
    if reviewed == 0 {
        return Err(super::changed());
    }
    sqlx::query(
        "INSERT INTO application_answers (application_id, answer_id, answer_version, \
         confirmed_at, confirmed_by) \
         SELECT ?, id, ?, reviewed_at, ? FROM memory_answers WHERE id = ? \
         ON CONFLICT(application_id, answer_id) DO UPDATE SET \
         answer_version = excluded.answer_version, confirmed_at = excluded.confirmed_at, \
         confirmed_by = excluded.confirmed_by",
    )
    .bind(application_id)
    .bind(row.version)
    .bind(actor)
    .bind(answer_id)
    .execute(&mut *write)
    .await?;
    audit::record_to(
        &mut *write,
        actor,
        "answer_confirmed",
        Some(answer_id),
        json!({ "application": application_id, "class": class.as_str() }),
    )
    .await?;
    write.commit().await?;
    Ok(())
}

/// The text of one approved answer for one application. A sealed class needs a confirmation
/// that is current: made for this version, on this application, and not stale. A normal
/// answer needs none and comes back marked stale when it is.
pub async fn use_answer(
    db: &Db,
    actor: &str,
    answer_id: &str,
    application_id: &str,
) -> Result<UsedAnswer> {
    let (row, class) = checked(db, actor, answer_id, application_id).await?;
    if class.sealed() && !confirmed_now(db, &row, application_id).await? {
        return Err(Error::ConfirmationRequired);
    }
    if is_cleared(&row) {
        return Err(bad(
            "this text was cleared when its key was given up; remove the answer",
        ));
    }
    let text = plain_texts(db, std::slice::from_ref(&row)).await?.remove(0);

    // The write re-checks the version and approval, so an edit during the decrypt wins.
    let mut write = db.pool.begin().await?;
    let marked = if class.sealed() {
        sqlx::query(concat!(
            "UPDATE application_answers SET used_at = ",
            now!(),
            " WHERE application_id = ? AND answer_id = ? AND confirmed_at IS NOT NULL \
             AND answer_version = ? AND EXISTS (SELECT 1 FROM memory_answers \
             WHERE id = ? AND version = ? AND approved = 1)"
        ))
        .bind(application_id)
        .bind(answer_id)
        .bind(row.version)
        .bind(answer_id)
        .bind(row.version)
    } else {
        sqlx::query(concat!(
            "INSERT INTO application_answers (application_id, answer_id, answer_version, \
             used_at) SELECT ?, id, version, ",
            now!(),
            " FROM memory_answers WHERE id = ? AND version = ? AND approved = 1 \
             ON CONFLICT(application_id, answer_id) DO UPDATE SET used_at = excluded.used_at"
        ))
        .bind(application_id)
        .bind(answer_id)
        .bind(row.version)
    }
    .execute(&mut *write)
    .await?
    .rows_affected();
    if marked == 0 {
        return Err(if class.sealed() {
            Error::ConfirmationRequired
        } else {
            super::changed()
        });
    }
    audit::record_to(
        &mut *write,
        actor,
        "answer_used",
        Some(answer_id),
        json!({ "application": application_id, "class": class.as_str() }),
    )
    .await?;
    write.commit().await?;
    Ok(UsedAnswer {
        application_id: application_id.to_owned(),
        answer_id: answer_id.to_owned(),
        class,
        text,
        stale: row.stale,
    })
}

/// A confirmation counts while it matches the answer's version, the answer is approved and
/// not stale, and nothing that voids it came after: a review of the answer (which is per
/// answer, so one made for another application cannot revive this one) or a change of the
/// question's class. Binds [`super::FRESH_DAYS`] as `?1`. Times are one fixed text format.
macro_rules! current_sql {
    () => {
        "(x.confirmed_at IS NOT NULL AND x.answer_version = a.version AND a.approved = 1 \
         AND x.confirmed_at >= a.reviewed_at AND x.confirmed_at >= q.updated_at \
         AND coalesce(julianday('now') - julianday(a.reviewed_at) > ?1, 1) = 0)"
    };
}

async fn confirmed_now(db: &Db, row: &StoredAnswer, application_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(concat!(
        "SELECT coalesce(max(",
        current_sql!(),
        "), 0) FROM application_answers x JOIN memory_answers a ON a.id = x.answer_id \
         JOIN memory_questions q ON q.id = a.question_id \
         WHERE x.application_id = ?2 AND x.answer_id = ?3"
    ))
    .bind(super::FRESH_DAYS)
    .bind(application_id)
    .bind(&row.id)
    .fetch_one(&db.pool)
    .await?)
}

/// What this application has used or confirmed, with no answer text, oldest first. Asks the
/// keychain nothing.
pub async fn application_answers(db: &Db, application_id: &str) -> Result<Vec<AppliedAnswer>> {
    #[derive(sqlx::FromRow)]
    struct Stored {
        answer_id: String,
        question: String,
        class: String,
        confirmed_at: Option<String>,
        used_at: Option<String>,
        current: bool,
    }
    let stored: Vec<Stored> = sqlx::query_as(concat!(
        "SELECT x.answer_id, q.text AS question, q.class, x.confirmed_at, x.used_at, \
         ",
        current_sql!(),
        " AS current \
         FROM application_answers x JOIN memory_answers a ON a.id = x.answer_id \
         JOIN memory_questions q ON q.id = a.question_id \
         WHERE x.application_id = ?2 ORDER BY x.rowid"
    ))
    .bind(super::FRESH_DAYS)
    .bind(application_id)
    .fetch_all(&db.pool)
    .await?;
    stored
        .into_iter()
        .map(|row| {
            Ok(AppliedAnswer {
                current: row.current,
                class: row.class.parse()?,
                answer_id: row.answer_id,
                question: row.question,
                confirmed_at: row.confirmed_at,
                used_at: row.used_at,
            })
        })
        .collect()
}

/// What the profile says for one exact country. A country with no row is "not stated", and
/// asks the keychain nothing. Nothing is inferred from another country or from any field.
pub async fn authorization_for(db: &Db, country: &str) -> Result<AuthorizationAnswer> {
    let country = clean_country(country)?;
    Ok(match profile::authorization_row(db, &country).await? {
        Some(row) => AuthorizationAnswer {
            country,
            authorized: row.authorized,
            needs_sponsorship: row.needs_sponsorship,
            note: row.note,
            stated: true,
        },
        None => AuthorizationAnswer {
            country,
            authorized: Stated::NotStated,
            needs_sponsorship: Stated::NotStated,
            note: String::new(),
            stated: false,
        },
    })
}

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}
