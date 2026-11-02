//! The career memory bank: questions you are asked, and your answers to them.
//!
//! An answer is never evidence for a draft, and only a person approves one. A class above
//! normal seals the body in `body_enc`. Reading never asks the keychain: a list shows such
//! a body empty, whichever column holds it, so a body raised while the key was locked stops
//! showing at once and is sealed later by [`seal_pending`]. Work authorization has no value
//! here: that question is answered from the encrypted profile row, one country at a time.

pub mod rules;

use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteExecutor;

use crate::crypto::{self, DataKey, KeyState};
use crate::db::Db;
use crate::drafts::fold;
use crate::error::{Error, Result};
use crate::knowledge::{self, Finding};
use crate::{audit, profile};

/// An answer not reviewed for longer than this is stale.
pub const FRESH_DAYS: i64 = 180;
const QUESTION_LIMIT: usize = 2_000;
const ANSWER_LIMIT: usize = 20_000;

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

macro_rules! question_columns {
    () => {
        "id, text, class, class_source, country, source, source_ref, findings, classified_at, \
         created_at, updated_at"
    };
}

/// With the class in one statement, so a body is never read beside an older class. Binds
/// [`FRESH_DAYS`] as `?1`. A date that cannot be read counts as stale.
macro_rules! select_answers {
    () => {
        "SELECT a.id, a.question_id, a.body, a.body_enc, a.key_id, a.approved, a.version, \
         a.source, a.source_ref, a.reviewed_at, a.created_at, a.updated_at, q.class, \
         coalesce(julianday('now') - julianday(a.reviewed_at) > ?1, 1) AS stale \
         FROM memory_answers a JOIN memory_questions q ON q.id = a.question_id "
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Normal,
    Sensitive,
    Legal,
    WorkAuthorization,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Normal => "normal",
            Class::Sensitive => "sensitive",
            Class::Legal => "legal",
            Class::WorkAuthorization => "work_authorization",
        }
    }

    /// Everything above Normal: the answer body is sealed. WorkAuthorization takes no new
    /// answer, so there this covers a body left behind by a raise.
    pub fn sealed(self) -> bool {
        self != Class::Normal
    }
}

impl FromStr for Class {
    type Err = Error;

    fn from_str(text: &str) -> Result<Class> {
        match text {
            "normal" => Ok(Class::Normal),
            "sensitive" => Ok(Class::Sensitive),
            "legal" => Ok(Class::Legal),
            "work_authorization" => Ok(Class::WorkAuthorization),
            _ => Err(bad(
                "a class is normal, sensitive, legal, or work_authorization",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub class: Class,
    pub class_source: String,
    pub country: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
    pub findings: Vec<Finding>,
    pub classified_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub id: String,
    pub question_id: String,
    /// Empty under a sealed class, and for a body cleared by a discard.
    pub body: String,
    /// There is text to show on request: ciphertext, or a plain body waiting to be sealed.
    pub sealed: bool,
    pub approved: bool,
    pub version: i64,
    pub source: String,
    pub source_ref: Option<String>,
    pub reviewed_at: String,
    pub stale: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionRow {
    pub question: Question,
    pub answers: Vec<Answer>,
}

#[derive(sqlx::FromRow)]
struct StoredQuestion {
    id: String,
    text: String,
    class: String,
    class_source: String,
    country: Option<String>,
    source: String,
    source_ref: Option<String>,
    findings: String,
    classified_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl StoredQuestion {
    fn question(self) -> Result<Question> {
        Ok(Question {
            class: self.class.parse()?,
            findings: serde_json::from_str(&self.findings)
                .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))?,
            id: self.id,
            text: self.text,
            class_source: self.class_source,
            country: self.country,
            source: self.source,
            source_ref: self.source_ref,
            classified_at: self.classified_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// A row as stored, so it has no `Debug`: `body` may be sealed-class text.
#[derive(sqlx::FromRow)]
struct StoredAnswer {
    id: String,
    question_id: String,
    body: String,
    body_enc: Option<Vec<u8>>,
    key_id: Option<String>,
    approved: bool,
    version: i64,
    source: String,
    source_ref: Option<String>,
    reviewed_at: String,
    created_at: String,
    updated_at: String,
    class: String,
    stale: bool,
}

impl StoredAnswer {
    fn class(&self) -> Result<Class> {
        self.class.parse()
    }

    /// What every read shows: no body under a sealed class, whichever column holds it.
    fn listed(self) -> Result<Answer> {
        let sealed_class = self.class()?.sealed();
        let sealed = self.body_enc.is_some() || (sealed_class && !self.body.is_empty());
        Ok(Answer {
            body: if sealed { String::new() } else { self.body },
            sealed,
            id: self.id,
            question_id: self.question_id,
            approved: self.approved,
            version: self.version,
            source: self.source,
            source_ref: self.source_ref,
            reviewed_at: self.reviewed_at,
            stale: self.stale,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}

fn changed() -> Error {
    Error::Refused("this changed while it was being updated, and nothing was changed".into())
}

/// Only a person approves, reviews, sets a class, or opens a body, whatever the adapter does.
fn person(actor: &str) -> Result<()> {
    match actor {
        "gui" | "cli" => Ok(()),
        _ => Err(Error::Refused("only_a_person".into())),
    }
}

fn clean_country(country: &str) -> Result<String> {
    let country = country.trim().to_ascii_uppercase();
    if country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(country)
    } else {
        Err(bad("a country is two letters, like PT"))
    }
}

fn clean_answer(text: &str) -> Result<&str> {
    let text = text.trim();
    if text.is_empty() {
        return Err(bad("an answer needs some text"));
    }
    if text.chars().count() > ANSWER_LIMIT {
        return Err(bad("an answer is at most 20000 characters"));
    }
    Ok(text)
}

/// The text of a question is untrusted: it is normalized, and what was hidden in it is
/// reported. A `country` comes only from the caller, never from the words.
pub async fn add_question(db: &Db, text: &str, country: Option<&str>) -> Result<Question> {
    let (text, findings) = knowledge::normalize(text);
    let text = text.trim();
    if text.chars().count() > QUESTION_LIMIT {
        return Err(bad("a question is at most 2000 characters"));
    }
    let folded = fold(text);
    if folded.is_empty() {
        return Err(bad("a question needs some words"));
    }
    let country = country.map(clean_country).transpose()?;
    let key = format!("{folded}|{}", country.as_deref().unwrap_or(""));
    let findings =
        serde_json::to_string(&findings).map_err(|error| Error::Validation(error.to_string()))?;

    let row: Option<StoredQuestion> = sqlx::query_as(concat!(
        "INSERT INTO memory_questions (id, key, text, class, class_source, country, source, \
         findings, created_at, updated_at) \
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, 'rules', ?, 'user', ?, ",
        now!(),
        ", ",
        now!(),
        ") ON CONFLICT(key) DO NOTHING RETURNING ",
        question_columns!()
    ))
    .bind(key)
    .bind(text)
    .bind(rules::classify(text).as_str())
    .bind(country)
    .bind(findings)
    .fetch_optional(&db.pool)
    .await?;
    row.ok_or_else(|| bad("this question is already in your memory"))?
        .question()
}

/// Newest question first, its answers oldest first. Asks the keychain nothing.
pub async fn list(db: &Db, class: Option<Class>) -> Result<Vec<QuestionRow>> {
    let class = class.map(Class::as_str);
    let questions: Vec<StoredQuestion> = sqlx::query_as(concat!(
        "SELECT ",
        question_columns!(),
        " FROM memory_questions WHERE (?1 IS NULL OR class = ?1) ORDER BY seq DESC"
    ))
    .bind(class)
    .fetch_all(&db.pool)
    .await?;
    let stored: Vec<StoredAnswer> = sqlx::query_as(concat!(
        select_answers!(),
        "WHERE (?2 IS NULL OR q.class = ?2) ORDER BY a.seq"
    ))
    .bind(FRESH_DAYS)
    .bind(class)
    .fetch_all(&db.pool)
    .await?;

    let mut answers: HashMap<String, Vec<Answer>> = HashMap::new();
    for row in stored {
        let answer = row.listed()?;
        answers
            .entry(answer.question_id.clone())
            .or_default()
            .push(answer);
    }
    questions
        .into_iter()
        .map(|question| {
            let question = question.question()?;
            let answers = answers.remove(&question.id).unwrap_or_default();
            Ok(QuestionRow { question, answers })
        })
        .collect()
}

/// One question, as [`list`] shows it.
pub async fn get(db: &Db, question_id: &str) -> Result<QuestionRow> {
    let question = stored_question(db, question_id).await?.question()?;
    let answers = answers_of(db, question_id)
        .await?
        .into_iter()
        .map(StoredAnswer::listed)
        .collect::<Result<_>>()?;
    Ok(QuestionRow { question, answers })
}

/// One answer as [`list`] shows it, with its class, for an adapter to decide what to ask.
pub async fn find_answer(db: &Db, answer_id: &str) -> Result<(Class, Answer)> {
    let row = stored_answer(db, answer_id).await?;
    Ok((row.class()?, row.listed()?))
}

async fn stored_question(db: &Db, id: &str) -> Result<StoredQuestion> {
    sqlx::query_as(concat!(
        "SELECT ",
        question_columns!(),
        " FROM memory_questions WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)
}

async fn stored_answer(db: &Db, id: &str) -> Result<StoredAnswer> {
    sqlx::query_as(concat!(select_answers!(), "WHERE a.id = ?2"))
        .bind(FRESH_DAYS)
        .bind(id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or(Error::NotFound)
}

async fn answers_of(db: &Db, question_id: &str) -> Result<Vec<StoredAnswer>> {
    Ok(sqlx::query_as(concat!(
        select_answers!(),
        "WHERE a.question_id = ?2 ORDER BY a.seq"
    ))
    .bind(FRESH_DAYS)
    .bind(question_id)
    .fetch_all(&db.pool)
    .await?)
}

/// The three columns that hold a body.
struct Body {
    plain: String,
    sealed: Option<Vec<u8>>,
    key_id: Option<String>,
}

/// A sealed class seals first, so a key error stores nothing.
async fn body_for(db: &Db, class: Class, answer_id: &str, text: &str) -> Result<Body> {
    if !class.sealed() {
        return Ok(Body {
            plain: text.to_owned(),
            sealed: None,
            key_id: None,
        });
    }
    let key = crypto::unlocked(db).await?;
    let (table, column) = crypto::MEMORY_AAD;
    Ok(Body {
        plain: String::new(),
        sealed: Some(key.encrypt(table, column, answer_id, text.as_bytes())?),
        key_id: Some(key.key_id().to_owned()),
    })
}

/// The row as [`list`] shows it, so no sealed-class body comes back. A question of class
/// work authorization takes no answer. `source_ref` names a knowledge item that must exist.
pub async fn add_answer(
    db: &Db,
    question_id: &str,
    text: &str,
    source_ref: Option<&str>,
) -> Result<Answer> {
    let text = clean_answer(text)?;
    let class: Class = stored_question(db, question_id).await?.class.parse()?;
    no_stored_authorization(class)?;
    if let Some(item) = source_ref {
        let found: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM knowledge_items WHERE id = ?)")
                .bind(item)
                .fetch_one(&db.pool)
                .await?;
        if !found {
            return Err(bad("this knowledge item does not exist"));
        }
    }
    // The id is part of what is sealed, so it is chosen first.
    let id: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
        .fetch_one(&db.pool)
        .await?;
    let body = body_for(db, class, &id, text).await?;
    let source = if source_ref.is_some() {
        "knowledge"
    } else {
        "user"
    };
    let added = sqlx::query(concat!(
        "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, source, \
         source_ref, reviewed_at, created_at, updated_at) \
         SELECT ?, id, ?, ?, ?, ?, ?, ",
        now!(),
        ", ",
        now!(),
        ", ",
        now!(),
        " FROM memory_questions WHERE id = ? AND class = ?"
    ))
    .bind(&id)
    .bind(body.plain)
    .bind(body.sealed)
    .bind(body.key_id)
    .bind(source)
    .bind(source_ref)
    .bind(question_id)
    .bind(class.as_str())
    .execute(&db.pool)
    .await?
    .rows_affected();
    if added == 0 {
        return Err(changed());
    }
    stored_answer(db, &id).await?.listed()
}

fn no_stored_authorization(class: Class) -> Result<()> {
    if class == Class::WorkAuthorization {
        return Err(bad(
            "work authorization is answered from your profile, for one country at a time",
        ));
    }
    Ok(())
}

/// New text is not approved text: this unapproves and takes a new version, which voids any
/// confirmation of the old one. A person just wrote it, so it counts as reviewed.
pub async fn edit_answer(db: &Db, answer_id: &str, text: &str) -> Result<Answer> {
    let text = clean_answer(text)?;
    let row = stored_answer(db, answer_id).await?;
    let class = row.class()?;
    no_stored_authorization(class)?;
    let body = body_for(db, class, answer_id, text).await?;
    let edited = sqlx::query(concat!(
        "UPDATE memory_answers SET body = ?, body_enc = ?, key_id = ?, approved = 0, \
         version = version + 1, reviewed_at = ",
        now!(),
        ", updated_at = ",
        now!(),
        " WHERE id = ? AND version = ? AND \
         (SELECT class FROM memory_questions WHERE id = question_id) = ?"
    ))
    .bind(body.plain)
    .bind(body.sealed)
    .bind(body.key_id)
    .bind(answer_id)
    .bind(row.version)
    .bind(class.as_str())
    .execute(&db.pool)
    .await?
    .rows_affected();
    if edited == 0 {
        return Err(changed());
    }
    stored_answer(db, answer_id).await?.listed()
}

/// Only a person approves. Taking approval back needs no one. A plain body under a sealed
/// class is sealed first, so the key's error reaches the user and nothing is approved.
pub async fn approve(db: &Db, actor: &str, answer_id: &str, approved: bool) -> Result<()> {
    if !approved {
        return set_unapproved(db, actor, answer_id).await;
    }
    person(actor)?;
    let row = stored_answer(db, answer_id).await?;
    let class = row.class()?;
    if class == Class::WorkAuthorization {
        return Err(Error::Refused("use_profile_by_country".into()));
    }
    if row.body.is_empty() && row.body_enc.is_none() {
        return Err(bad("this text was cleared when its key was given up"));
    }
    if class.sealed() && !row.body.is_empty() {
        seal_pending(db).await?;
    }

    let mut write = db.pool.begin().await?;
    let done = sqlx::query(concat!(
        "UPDATE memory_answers SET approved = 1, updated_at = ",
        now!(),
        " WHERE id = ? AND version = ? AND \
         (SELECT class FROM memory_questions WHERE id = question_id) = ?"
    ))
    .bind(answer_id)
    .bind(row.version)
    .bind(class.as_str())
    .execute(&mut *write)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(changed());
    }
    audit::record_to(
        &mut *write,
        actor,
        "answer_approved",
        Some(answer_id),
        json!({ "version": row.version }),
    )
    .await?;
    write.commit().await?;
    Ok(())
}

async fn set_unapproved(db: &Db, actor: &str, answer_id: &str) -> Result<()> {
    let mut write = db.pool.begin().await?;
    let done = sqlx::query(concat!(
        "UPDATE memory_answers SET approved = 0, updated_at = ",
        now!(),
        " WHERE id = ?"
    ))
    .bind(answer_id)
    .execute(&mut *write)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(Error::NotFound);
    }
    audit::record_to(
        &mut *write,
        actor,
        "answer_unapproved",
        Some(answer_id),
        json!({}),
    )
    .await?;
    write.commit().await?;
    Ok(())
}

/// A person says the text still holds, which restarts [`FRESH_DAYS`].
pub async fn mark_reviewed(db: &Db, actor: &str, answer_id: &str) -> Result<()> {
    person(actor)?;
    let mut write = db.pool.begin().await?;
    let done = sqlx::query(concat!(
        "UPDATE memory_answers SET reviewed_at = ",
        now!(),
        ", updated_at = ",
        now!(),
        " WHERE id = ?"
    ))
    .bind(answer_id)
    .execute(&mut *write)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(Error::NotFound);
    }
    audit::record_to(
        &mut *write,
        actor,
        "answer_reviewed",
        Some(answer_id),
        json!({}),
    )
    .await?;
    write.commit().await?;
    Ok(())
}

/// A person's choice of class. A sealed class seals every plain body, and a raise unapproves.
/// Normal opens each body. It asks for a key only when a body needs one, and a key error
/// changes nothing. To the class it already has, nothing happens.
pub async fn set_class(db: &Db, actor: &str, question_id: &str, class: Class) -> Result<Question> {
    person(actor)?;
    let question = stored_question(db, question_id).await?.question()?;
    let from = question.class;
    if from == class {
        return Ok(question);
    }
    let (open, seal): (Vec<_>, Vec<_>) = answers_of(db, question_id)
        .await?
        .into_iter()
        .filter(|row| row.body_enc.is_some() != class.sealed() && !is_cleared(row))
        .partition(|row| row.body_enc.is_some());
    let key = match seal.is_empty() {
        true => None,
        false => Some(crypto::unlocked(db).await?),
    };
    let opened = plain_texts(db, &open).await?;

    let mut write = db.pool.begin().await?;
    let moved = sqlx::query(concat!(
        "UPDATE memory_questions SET class = ?, class_source = 'user', updated_at = ",
        now!(),
        " WHERE id = ? AND class = ?"
    ))
    .bind(class.as_str())
    .bind(question_id)
    .bind(from.as_str())
    .execute(&mut *write)
    .await?
    .rows_affected();
    if moved == 0 {
        return Err(changed());
    }
    if let Some(key) = &key
        && seal_rows(&mut write, key, &seal).await? != seal.len() as u64
    {
        return Err(changed());
    }
    for (row, body) in open.iter().zip(opened) {
        let done = sqlx::query(concat!(
            "UPDATE memory_answers SET body = ?, body_enc = NULL, key_id = NULL, updated_at = ",
            now!(),
            " WHERE id = ? AND body_enc = ?"
        ))
        .bind(body)
        .bind(&row.id)
        .bind(&row.body_enc)
        .execute(&mut *write)
        .await?
        .rows_affected();
        if done == 0 {
            return Err(changed());
        }
    }
    if class > from {
        unapprove_all(&mut write, question_id).await?;
    }
    audit::record_to(
        &mut *write,
        actor,
        "memory_class_set",
        Some(question_id),
        json!({ "from": from.as_str(), "to": class.as_str() }),
    )
    .await?;
    write.commit().await?;
    stored_question(db, question_id).await?.question()
}

fn is_cleared(row: &StoredAnswer) -> bool {
    row.body.is_empty() && row.body_enc.is_none()
}

async fn unapprove_all(write: &mut SqliteConnection, question_id: &str) -> Result<()> {
    sqlx::query(concat!(
        "UPDATE memory_answers SET approved = 0, updated_at = ",
        now!(),
        " WHERE question_id = ?"
    ))
    .bind(question_id)
    .execute(write)
    .await?;
    Ok(())
}

/// Raises a class on a word from outside a person: `source` is `rules` or `model`. `false`
/// unless `class` outranks the stored one, so this can never lower. It asks no key: the body
/// is hidden at once, because every read blanks it, and [`seal_pending`] seals it later.
// The career memory agent is its caller, in a later task of this release.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn raise_class(
    db: &Db,
    question_id: &str,
    class: Class,
    source: &str,
) -> Result<bool> {
    let from: Class = stored_question(db, question_id).await?.class.parse()?;
    if class <= from {
        return Ok(false);
    }
    let mut write = db.pool.begin().await?;
    let moved = sqlx::query(concat!(
        "UPDATE memory_questions SET class = ?, class_source = ?, updated_at = ",
        now!(),
        " WHERE id = ? AND class = ?"
    ))
    .bind(class.as_str())
    .bind(source)
    .bind(question_id)
    .bind(from.as_str())
    .execute(&mut *write)
    .await?
    .rows_affected();
    if moved == 0 {
        return Err(changed());
    }
    unapprove_all(&mut write, question_id).await?;
    audit::record_to(
        &mut *write,
        "system",
        "memory_class_raised",
        Some(question_id),
        json!({ "agent": "career_memory", "from": from.as_str(), "to": class.as_str() }),
    )
    .await?;
    write.commit().await?;
    Ok(true)
}

/// The text of one answer, sealed or not, for a person, and audited with no text. A body
/// that waits to be sealed needs no key. A key that cannot be had is its own error.
pub async fn reveal(db: &Db, actor: &str, answer_id: &str) -> Result<String> {
    person(actor)?;
    let row = stored_answer(db, answer_id).await?;
    if is_cleared(&row) {
        return Err(bad(
            "this text was cleared when its key was given up; remove the answer",
        ));
    }
    let text = plain_texts(db, std::slice::from_ref(&row)).await?.remove(0);
    let detail = json!({ "version": row.version });
    audit::record(db, actor, "answer_revealed", Some(answer_id), detail).await?;
    Ok(text)
}

/// Each row's text, in order. Each key is read once, and the first error ends it.
async fn plain_texts(db: &Db, rows: &[StoredAnswer]) -> Result<Vec<String>> {
    let (table, column) = crypto::MEMORY_AAD;
    let mut keys: Vec<DataKey> = Vec::new();
    let mut texts = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(sealed), Some(key_id)) = (&row.body_enc, &row.key_id) else {
            texts.push(row.body.clone());
            continue;
        };
        let at = match keys.iter().position(|key| key.key_id() == key_id) {
            Some(at) => at,
            None => {
                keys.push(crypto::key_by_id(db, key_id).await?);
                keys.len() - 1
            }
        };
        let plain = keys[at].decrypt(table, column, &row.id, sealed)?;
        texts.push(String::from_utf8(plain).map_err(|_| bad("the text is not valid UTF-8"))?);
    }
    Ok(texts)
}

/// Needs no key: it only deletes. Its answers go with it.
pub async fn remove_question(db: &Db, id: &str) -> Result<()> {
    remove(db, "DELETE FROM memory_questions WHERE id = ?", id).await
}

/// Needs no key: it only deletes.
pub async fn remove_answer(db: &Db, id: &str) -> Result<()> {
    remove(db, "DELETE FROM memory_answers WHERE id = ?", id).await
}

async fn remove(db: &Db, sql: &'static str, id: &str) -> Result<()> {
    let done = sqlx::query(sql).bind(id).execute(&db.pool).await?;
    if done.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

/// Plain bodies under a sealed class, oldest first.
async fn pending(db: &Db) -> Result<Vec<StoredAnswer>> {
    Ok(sqlx::query_as(concat!(
        select_answers!(),
        "WHERE q.class <> 'normal' AND a.body <> '' ORDER BY a.seq"
    ))
    .bind(FRESH_DAYS)
    .fetch_all(&db.pool)
    .await?)
}

/// Seals what a raise left in plain. With none, it returns 0 and asks no key; otherwise it
/// returns the count, or the key's error.
pub async fn seal_pending(db: &Db) -> Result<usize> {
    let rows = pending(db).await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let key = crypto::unlocked(db).await?;
    let mut write = db.pool.begin().await?;
    let sealed = seal_rows(&mut write, &key, &rows).await?;
    write.commit().await?;
    Ok(sealed as usize)
}

/// A row edited or lowered since it was read is left alone, and left out of the count.
async fn seal_rows(
    write: &mut SqliteConnection,
    key: &DataKey,
    rows: &[StoredAnswer],
) -> Result<u64> {
    let (table, column) = crypto::MEMORY_AAD;
    let mut sealed = 0;
    for row in rows {
        let text = key.encrypt(table, column, &row.id, row.body.as_bytes())?;
        sealed += sqlx::query(concat!(
            "UPDATE memory_answers SET body = '', body_enc = ?, key_id = ?, updated_at = ",
            now!(),
            " WHERE id = ? AND body = ? AND body_enc IS NULL AND \
             (SELECT class FROM memory_questions WHERE id = question_id) <> 'normal'"
        ))
        .bind(text)
        .bind(key.key_id())
        .bind(&row.id)
        .bind(&row.body)
        .execute(&mut *write)
        .await?
        .rows_affected();
    }
    Ok(sealed)
}

/// What `recovery::discard_locked` gives up with a key: the row stays, with no text and
/// unapproved, and a new version so that no confirmation of the lost text still counts.
pub(crate) async fn clear_sealed(executor: impl SqliteExecutor<'_>, key_id: &str) -> Result<()> {
    sqlx::query(concat!(
        "UPDATE memory_answers SET body_enc = NULL, key_id = NULL, approved = 0, \
         version = version + 1, updated_at = ",
        now!(),
        " WHERE key_id = ? AND body_enc IS NOT NULL"
    ))
    .bind(key_id)
    .execute(executor)
    .await?;
    Ok(())
}

#[derive(Serialize)]
struct ExportedAnswer<'a> {
    id: &'a str,
    question_id: &'a str,
    class: &'a str,
    version: i64,
    approved: bool,
    body: String,
}

/// Rows for `transfer::export_all`, and whether plaintext was written. With no sealed row,
/// nothing is asked of the keychain. A raised body can be the only one on a root with no
/// key: an export must not make a key, so there it asks nothing either.
pub async fn export_sealed(db: &Db) -> Result<(String, bool)> {
    let rows: Vec<StoredAnswer> = sqlx::query_as(concat!(
        select_answers!(),
        "WHERE a.body_enc IS NOT NULL OR (q.class <> 'normal' AND a.body <> '') ORDER BY a.seq"
    ))
    .bind(FRESH_DAYS)
    .fetch_all(&db.pool)
    .await?;
    if rows.is_empty() {
        return Ok(("[]".into(), false));
    }
    let key_state = match crypto::known_state(db).await? {
        KeyState::NotLoaded => crypto::peek(db).await?,
        no_key_yet => no_key_yet,
    };
    if !matches!(key_state, KeyState::Unlocked { .. }) {
        return Ok((json!({ "locked": key_state }).to_string(), false));
    }
    let texts = match plain_texts(db, &rows).await {
        Ok(texts) => texts,
        Err(error) => {
            let locked = json!({ "locked": profile::degraded(error)? });
            return Ok((locked.to_string(), false));
        }
    };
    let exported: Vec<ExportedAnswer> = rows
        .iter()
        .zip(texts)
        .map(|(row, body)| ExportedAnswer {
            id: &row.id,
            question_id: &row.question_id,
            class: &row.class,
            version: row.version,
            approved: row.approved,
            body,
        })
        .collect();
    let text = serde_json::to_string(&exported).map_err(|e| Error::Validation(e.to_string()))?;
    Ok((text, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::key_check_text;
    use crate::paths::AppPaths;
    use crate::transfer;

    const KEY_ID: &str = "00112233aabbccdd";
    const CANARY: &str = "ZXQ-CANARY-ANSWER";

    async fn open() -> (tempfile::TempDir, Db) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&AppPaths::new(tmp.path().join("data")))
            .await
            .unwrap();
        (tmp, db)
    }

    fn key() -> DataKey {
        DataKey::from_bytes(KEY_ID, [7; 32])
    }

    async fn add_key_row(db: &Db) {
        let check = key()
            .encrypt("data_keys", "key_check", KEY_ID, key_check_text())
            .unwrap();
        sqlx::query(
            "INSERT INTO data_keys (key_id, created_at, key_check, active) \
             VALUES (?, 't', ?, 1)",
        )
        .bind(KEY_ID)
        .bind(check)
        .execute(&db.pool)
        .await
        .unwrap();
    }

    async fn sensitive_question(db: &Db) -> String {
        let question = add_question(db, "What is your salary?", None)
            .await
            .unwrap();
        assert_eq!(question.class, Class::Sensitive);
        question.id
    }

    async fn normal_answer(db: &Db) -> (String, String) {
        let question = add_question(db, "Why this job?", None).await.unwrap();
        let answer = add_answer(db, &question.id, CANARY, None).await.unwrap();
        approve(db, "cli", &answer.id, true).await.unwrap();
        (question.id, answer.id)
    }

    async fn raw_body(db: &Db, id: &str) -> (String, Option<Vec<u8>>) {
        sqlx::query_as("SELECT body, body_enc FROM memory_answers WHERE id = ?")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn raise_never_lowers() {
        let (_tmp, db) = open().await;
        let id = sensitive_question(&db).await;

        assert!(!raise_class(&db, &id, Class::Normal, "model").await.unwrap());
        assert!(
            !raise_class(&db, &id, Class::Sensitive, "model")
                .await
                .unwrap()
        );

        let question = get(&db, &id).await.unwrap().question;
        assert_eq!(question.class, Class::Sensitive);
        assert_eq!(question.class_source, "rules");
        assert!(audit::recent(&db, 10).await.unwrap().is_empty());
        db.close().await;
    }

    #[tokio::test]
    async fn raise_with_a_locked_key_hides_the_body_until_sealed() {
        let (_tmp, db) = open().await;
        let (question, answer) = normal_answer(&db).await;

        assert!(
            raise_class(&db, &question, Class::Sensitive, "model")
                .await
                .unwrap()
        );

        // Everything that shows a row hides it, and nothing asked for a key.
        assert_eq!(db.key_state(), KeyState::NotLoaded);
        let listed = list(&db, None).await.unwrap();
        let row = &listed[0].answers[0];
        assert_eq!(row.body, "");
        assert!(row.sealed);
        assert!(!row.approved);
        let row = &get(&db, &question).await.unwrap().answers[0];
        assert_eq!(row.body, "");
        assert!(row.sealed);
        let (class, row) = find_answer(&db, &answer).await.unwrap();
        assert_eq!(class, Class::Sensitive);
        assert_eq!(row.body, "");

        let (body, sealed) = raw_body(&db, &answer).await;
        assert_eq!(body, CANARY, "the plain column holds it for seal_pending");
        assert_eq!(sealed, None);

        let question = get(&db, &question).await.unwrap().question;
        assert_eq!(question.class_source, "model");
        let log = audit::recent(&db, 10).await.unwrap();
        let raised = log
            .iter()
            .find(|entry| entry.action == "memory_class_raised")
            .unwrap();
        assert_eq!(raised.actor, "system");
        assert_eq!(
            raised.detail,
            json!({ "agent": "career_memory", "from": "normal", "to": "sensitive" })
        );
        assert!(!format!("{log:?}").contains(CANARY));
        db.close().await;
    }

    #[tokio::test]
    async fn a_raised_body_stays_out_of_data_json() {
        let (_tmp, db) = open().await;
        let (question, answer) = normal_answer(&db).await;
        raise_class(&db, &question, Class::Sensitive, "model")
            .await
            .unwrap();

        let (text, plain) = export_sealed(&db).await.unwrap();
        assert!(!plain, "no key is unlocked, so no plain text goes out");
        assert!(!text.contains(CANARY), "{text}");

        let dir = transfer::export_all(&db, "cli").await.unwrap();

        let data = std::fs::read_to_string(dir.join("data.json")).unwrap();
        assert!(!data.contains(CANARY));
        assert!(data.contains(&answer), "the row stays, with no body");
        let sealed = std::fs::read_to_string(dir.join("memory_sealed_answers.json")).unwrap();
        assert!(!sealed.contains(CANARY));
        assert_eq!(db.key_state(), KeyState::NotLoaded, "and no key was made");
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM data_keys")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        db.close().await;
    }

    #[tokio::test]
    async fn a_raised_row_reveals_with_no_key() {
        let (_tmp, db) = open().await;
        let (question, answer) = normal_answer(&db).await;
        raise_class(&db, &question, Class::Legal, "model")
            .await
            .unwrap();

        assert_eq!(reveal(&db, "gui", &answer).await.unwrap(), CANARY);
        assert_eq!(db.key_state(), KeyState::NotLoaded);
        db.close().await;
    }

    #[tokio::test]
    async fn sealing_a_waiting_row_leaves_ciphertext_a_reveal_can_read() {
        let (_tmp, db) = open().await;
        add_key_row(&db).await;
        let (question, answer) = normal_answer(&db).await;
        raise_class(&db, &question, Class::Sensitive, "model")
            .await
            .unwrap();

        let rows = pending(&db).await.unwrap();
        assert_eq!(rows.len(), 1);
        let mut write = db.pool.begin().await.unwrap();
        assert_eq!(seal_rows(&mut write, &key(), &rows).await.unwrap(), 1);
        write.commit().await.unwrap();

        assert!(pending(&db).await.unwrap().is_empty());
        let (body, sealed) = raw_body(&db, &answer).await;
        assert_eq!(body, "");
        let sealed = sealed.unwrap();
        assert!(!sealed.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()));
        let (table, column) = crypto::MEMORY_AAD;
        let plain = key().decrypt(table, column, &answer, &sealed).unwrap();
        assert_eq!(plain, CANARY.as_bytes());
        db.close().await;
    }

    #[tokio::test]
    async fn a_row_lowered_or_edited_since_it_was_read_is_not_sealed() {
        let (_tmp, db) = open().await;
        add_key_row(&db).await;
        let (question, answer) = normal_answer(&db).await;
        raise_class(&db, &question, Class::Sensitive, "model")
            .await
            .unwrap();
        let rows = pending(&db).await.unwrap();

        sqlx::query("UPDATE memory_questions SET class = 'normal'")
            .execute(&db.pool)
            .await
            .unwrap();
        let mut write = db.pool.begin().await.unwrap();
        assert_eq!(seal_rows(&mut write, &key(), &rows).await.unwrap(), 0);
        write.rollback().await.unwrap();

        sqlx::query("UPDATE memory_questions SET class = 'sensitive'")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE memory_answers SET body = 'Newer' WHERE id = ?")
            .bind(&answer)
            .execute(&db.pool)
            .await
            .unwrap();
        let mut write = db.pool.begin().await.unwrap();
        assert_eq!(seal_rows(&mut write, &key(), &rows).await.unwrap(), 0);
        write.rollback().await.unwrap();
        db.close().await;
    }

    // `discard_locked` needs a keychain, so its clearing block is pinned here as well.
    #[tokio::test]
    async fn clearing_takes_only_the_bodies_under_that_key() {
        let (_tmp, db) = open().await;
        add_key_row(&db).await;
        let (question, answer) = normal_answer(&db).await;
        raise_class(&db, &question, Class::Sensitive, "model")
            .await
            .unwrap();
        let rows = pending(&db).await.unwrap();
        let mut write = db.pool.begin().await.unwrap();
        seal_rows(&mut write, &key(), &rows).await.unwrap();
        write.commit().await.unwrap();
        sqlx::query("UPDATE memory_answers SET approved = 1")
            .execute(&db.pool)
            .await
            .unwrap();

        clear_sealed(&db.pool, "99887766ffeeddcc").await.unwrap();
        assert!(raw_body(&db, &answer).await.1.is_some(), "another key's id");

        clear_sealed(&db.pool, KEY_ID).await.unwrap();

        assert_eq!(raw_body(&db, &answer).await, (String::new(), None));
        let (_, row) = find_answer(&db, &answer).await.unwrap();
        assert!(!row.sealed);
        assert!(!row.approved);
        assert_eq!(row.body, "");
        assert_eq!(row.version, 2);
        let key_id: Option<String> =
            sqlx::query_scalar("SELECT key_id FROM memory_answers WHERE id = ?")
                .bind(&answer)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(key_id, None);
        db.close().await;
    }
}
