use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct SearchHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
}

// sqlx 0.9 takes only `&'static str` SQL, so shared fragments are macros.
macro_rules! columns {
    () => {
        "id, title, body, created_at, updated_at"
    };
}
macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}
// ponytail: search returns at most 50 hits and has no paging.
const SEARCH_LIMIT: i64 = 50;

pub async fn create(db: &Db, title: &str, body: &str) -> Result<Document> {
    Ok(sqlx::query_as(concat!(
        "INSERT INTO documents (id, title, body, created_at, updated_at) ",
        "VALUES (lower(hex(randomblob(16))), ?, ?, ",
        now!(),
        ", ",
        now!(),
        ") ",
        "RETURNING ",
        columns!()
    ))
    .bind(title)
    .bind(body)
    .fetch_one(&db.pool)
    .await?)
}

pub async fn get(db: &Db, id: &str) -> Result<Document> {
    sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM documents WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)
}

/// Newest first.
pub async fn list(db: &Db) -> Result<Vec<Document>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM documents ORDER BY seq DESC"
    ))
    .fetch_all(&db.pool)
    .await?)
}

pub async fn update(db: &Db, id: &str, title: &str, body: &str) -> Result<Document> {
    sqlx::query_as(concat!(
        "UPDATE documents SET title = ?, body = ?, updated_at = ",
        now!(),
        " WHERE id = ? RETURNING ",
        columns!()
    ))
    .bind(title)
    .bind(body)
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    let done = sqlx::query("DELETE FROM documents WHERE id = ?")
        .bind(id)
        .execute(&db.pool)
        .await?;
    if done.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub async fn search(db: &Db, query: &str) -> Result<Vec<SearchHit>> {
    let Some(query) = fts_query(query) else {
        return Ok(Vec::new());
    };
    Ok(sqlx::query_as(
        "SELECT d.id AS id, d.title AS title, \
                snippet(documents_fts, -1, '', '', '...', 12) AS snippet \
         FROM documents_fts JOIN documents d ON d.seq = documents_fts.rowid \
         WHERE documents_fts MATCH ? ORDER BY bm25(documents_fts) LIMIT ?",
    )
    .bind(query)
    .bind(SEARCH_LIMIT)
    .fetch_all(&db.pool)
    .await?)
}

/// Turns user text into an FTS5 query of quoted terms, so no input is parsed as
/// FTS syntax. `None` when no terms remain.
pub fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}
