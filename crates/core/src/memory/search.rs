//! Retrieval over the question side of career memory: full text always, and meaning when a
//! confirmed local embedding model is set up. Only a question's words are embedded. A
//! question of class work authorization is in neither index, so similarity can never carry
//! one country's authorization to another, and a sealed class indexes its question only.

use serde::Serialize;
use sqlx::SqliteConnection;

use super::{Class, FRESH_DAYS};
use crate::ai::CancellationToken;
use crate::ai::router::Router;
use crate::db::Db;
use crate::documents::fts_query;
use crate::embed::{self, VECTOR_CEILING};
use crate::error::{Error, Result};
use crate::knowledge::{cosine, from_blob, to_blob};

/// Questions sent to the model per call, so one failure costs one batch.
const BATCH: usize = 32;

#[derive(Debug, Clone, Serialize)]
pub struct MemoryHit {
    pub question_id: String,
    pub question: String,
    pub class: Class,
    pub answer_id: Option<String>,
    /// `Some` only for an approved answer in `Class::Normal`.
    pub answer: Option<String>,
    pub stale: bool,
    pub needs_confirmation: bool,
    pub source: Option<String>,
    pub score: f32,
    /// `"semantic"` or `"text"`.
    pub via: String,
}

/// Brings one question's rows in both indexes up to date inside the caller's write. The
/// `answer` column holds plain bodies of a normal class only: a sealed class, or a body
/// still waiting to be sealed, adds nothing. A work authorization question gets no row and
/// loses its vectors.
pub(crate) async fn reindex(write: &mut SqliteConnection, question_id: &str) -> Result<()> {
    unindex(&mut *write, question_id).await?;
    sqlx::query(
        "INSERT INTO memory_fts (rowid, question, answer) \
         SELECT q.seq, q.text, CASE WHEN q.class = 'normal' THEN coalesce(( \
             SELECT group_concat(a.body, char(10)) FROM memory_answers a \
             WHERE a.question_id = q.id AND a.body <> '' AND a.body_enc IS NULL), '') \
           ELSE '' END \
         FROM memory_questions q WHERE q.id = ? AND q.class <> 'work_authorization'",
    )
    .bind(question_id)
    .execute(&mut *write)
    .await?;
    sqlx::query(
        "DELETE FROM memory_embeddings WHERE question_id = ?1 AND EXISTS ( \
         SELECT 1 FROM memory_questions WHERE id = ?1 AND class = 'work_authorization')",
    )
    .bind(question_id)
    .execute(&mut *write)
    .await?;
    Ok(())
}

/// Deletes the text row of a question that is about to go: `ON DELETE CASCADE` does not
/// reach an FTS table. Vectors go with the question row itself.
pub(crate) async fn unindex(write: &mut SqliteConnection, question_id: &str) -> Result<()> {
    sqlx::query(
        "DELETE FROM memory_fts WHERE rowid = (SELECT seq FROM memory_questions WHERE id = ?)",
    )
    .bind(question_id)
    .execute(&mut *write)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Candidate {
    id: String,
    text: String,
    class: String,
    score: f64,
}

#[derive(sqlx::FromRow)]
struct Newest {
    id: String,
    body: String,
    source_ref: Option<String>,
    stale: bool,
}

/// Best matches first. Meaning when a local model embeds the query and some stored vector
/// belongs to that model; otherwise words, with the best match by `bm25` first.
pub async fn search(db: &Db, router: &Router, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
    let Some(terms) = fts_query(query) else {
        return Ok(Vec::new());
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    match router
        .embed(db, &[query.to_string()], CancellationToken::new())
        .await
    {
        Ok((model, mut vectors)) => {
            let dims = embed::check(&vectors, 1)?;
            let hits = by_meaning(db, &model, dims, &vectors.remove(0), limit).await?;
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        Err(Error::ManualRequired(_)) => {}
        Err(error) => return Err(error),
    }
    by_text(db, &terms, limit).await
}

async fn by_meaning(
    db: &Db,
    model: &str,
    dims: usize,
    query: &[f32],
    limit: usize,
) -> Result<Vec<MemoryHit>> {
    #[derive(sqlx::FromRow)]
    struct Stored {
        id: String,
        text: String,
        class: String,
        dims: i64,
        vector: Vec<u8>,
    }
    // The ceiling bounds memory; rows past it are not searched.
    let rows: Vec<Stored> = sqlx::query_as(
        "SELECT q.id, q.text, q.class, e.dims, e.vector FROM memory_embeddings e \
         JOIN memory_questions q ON q.id = e.question_id \
         WHERE e.model = ? AND q.class <> 'work_authorization' LIMIT ?",
    )
    .bind(model)
    .bind(i64::try_from(VECTOR_CEILING).unwrap_or(i64::MAX))
    .fetch_all(&db.pool)
    .await?;

    let mut scored: Vec<Candidate> = rows
        .into_iter()
        .filter(|row| usize::try_from(row.dims) == Ok(dims) && row.vector.len() == dims * 4)
        .map(|row| Candidate {
            score: f64::from(cosine(query, &from_blob(&row.vector))),
            id: row.id,
            text: row.text,
            class: row.class,
        })
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(limit);
    finish(db, scored, "semantic").await
}

async fn by_text(db: &Db, terms: &str, limit: usize) -> Result<Vec<MemoryHit>> {
    let rows: Vec<Candidate> = sqlx::query_as(
        "SELECT q.id, q.text, q.class, -bm25(memory_fts) AS score FROM memory_fts \
         JOIN memory_questions q ON q.seq = memory_fts.rowid \
         WHERE memory_fts MATCH ? AND q.class <> 'work_authorization' \
         ORDER BY bm25(memory_fts) LIMIT ?",
    )
    .bind(terms)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(&db.pool)
    .await?;
    finish(db, rows, "text").await
}

/// Each question takes its newest approved answer. A body comes back only for the normal
/// class, never from a sealed row, and a sealed class asks for confirmation instead.
async fn finish(db: &Db, rows: Vec<Candidate>, via: &str) -> Result<Vec<MemoryHit>> {
    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        let class: Class = row.class.parse()?;
        let newest: Option<Newest> = sqlx::query_as(
            "SELECT id, body, source_ref, \
             coalesce(julianday('now') - julianday(reviewed_at) > ?, 1) AS stale \
             FROM memory_answers WHERE question_id = ? AND approved = 1 \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(FRESH_DAYS)
        .bind(&row.id)
        .fetch_optional(&db.pool)
        .await?;
        let shows = class == Class::Normal;
        hits.push(MemoryHit {
            question_id: row.id,
            question: row.text,
            class,
            answer_id: newest.as_ref().map(|n| n.id.clone()),
            answer: newest
                .as_ref()
                .filter(|n| shows && !n.body.is_empty())
                .map(|n| n.body.clone()),
            stale: newest.as_ref().is_some_and(|n| n.stale),
            needs_confirmation: class.sealed(),
            source: newest.and_then(|n| n.source_ref),
            score: row.score as f32,
            via: via.to_owned(),
        });
    }
    Ok(hits)
}

/// Embeds the question text of every question that has no vector for the configured model,
/// and returns how many were stored. Work authorization questions are never sent. A batch
/// is stored whole or not at all, and a cancel ends the run between batches with
/// `Cancelled`, so the next call embeds the rest. `ManualRequired` when no local embedding
/// model is set up.
pub async fn index_missing(db: &Db, router: &Router, cancel: CancellationToken) -> Result<usize> {
    let model = embed::model_setting(db).await?;
    let mut stored = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let batch: Vec<(String, String)> = sqlx::query_as(
            "SELECT q.id, q.text FROM memory_questions q \
             WHERE q.class <> 'work_authorization' AND NOT EXISTS ( \
               SELECT 1 FROM memory_embeddings e WHERE e.question_id = q.id AND e.model = ?) \
             ORDER BY q.seq LIMIT ?",
        )
        .bind(&model)
        .bind(i64::try_from(BATCH).unwrap_or(i64::MAX))
        .fetch_all(&db.pool)
        .await?;
        if batch.is_empty() {
            return Ok(stored);
        }

        let inputs: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
        let (used, vectors) = router.embed(db, &inputs, cancel.clone()).await?;
        let dims = embed::check(&vectors, inputs.len())?;

        let mut write = db.pool.begin().await?;
        let mut done = 0;
        for ((id, _), vector) in batch.iter().zip(&vectors) {
            // The WHERE re-checks the class: a question raised while the model ran gets no row.
            done += sqlx::query(
                "INSERT OR REPLACE INTO memory_embeddings \
                 (question_id, model, dims, vector, created_at) \
                 SELECT id, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 FROM memory_questions WHERE id = ? AND class <> 'work_authorization'",
            )
            .bind(&used)
            .bind(i64::try_from(dims).unwrap_or(i64::MAX))
            .bind(to_blob(vector))
            .bind(id)
            .execute(&mut *write)
            .await?
            .rows_affected();
        }
        write.commit().await?;
        stored += usize::try_from(done).unwrap_or(0);
        // Nothing stored means every question left or was raised: the next pass would
        // find the same ones missing only if they still qualified, so stop here.
        if done == 0 {
            return Ok(stored);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{add_answer, add_question, approve, raise_class};
    use crate::paths::AppPaths;

    async fn held(db: &Db) -> String {
        sqlx::query_scalar("SELECT answer FROM memory_fts")
            .fetch_one(&db.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_raise_takes_the_plain_body_out_of_the_text_index() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&AppPaths::new(tmp.path().join("data")))
            .await
            .unwrap();
        let question = add_question(&db, "Why this job?", None).await.unwrap();
        let answer = add_answer(&db, &question.id, "ZXQ-CANARY-ANSWER", None)
            .await
            .unwrap();
        approve(&db, "cli", &answer.id, true).await.unwrap();
        assert_eq!(held(&db).await, "ZXQ-CANARY-ANSWER");

        // The body waits in plain for a key, and the index must not keep it.
        assert!(
            raise_class(&db, &question.id, Class::Sensitive, "model")
                .await
                .unwrap()
        );
        assert_eq!(held(&db).await, "");
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_fts")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "the question stays findable");
        db.close().await;
    }
}
