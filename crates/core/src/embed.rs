//! Embeddings over the knowledge base: little-endian `f32` BLOBs, cosine in Rust.
//! Only `Router::embed` makes vectors, so a confirmed local model is the only source.

use serde::Serialize;

use crate::ai::CancellationToken;
use crate::ai::router::Router;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::knowledge::{self, cosine, from_blob, to_blob};
use crate::settings;

/// ponytail: brute force scans every vector of a model on each search. Move to
/// `sqlite-vec` past this many.
pub const VECTOR_CEILING: usize = 50_000;

/// Items sent to the model per call, so one failure costs one batch.
const BATCH: usize = 32;

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub item_id: String,
    pub score: f32,
    pub body: String,
}

fn no_model() -> Error {
    Error::ManualRequired("no_embedding_model".into())
}

async fn model_setting(db: &Db) -> Result<String> {
    settings::get(db, "ai.embedding_model")
        .await?
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|model| !model.is_empty())
        .ok_or_else(no_model)
}

/// A reply the caller can trust to line up with its inputs. The router checks this too;
/// a vector stored against the wrong item would poison every later search.
fn check(vectors: &[Vec<f32>], inputs: usize) -> Result<usize> {
    let dims = vectors.first().map_or(0, Vec::len);
    let sound = vectors.len() == inputs
        && dims > 0
        && vectors
            .iter()
            .all(|v| v.len() == dims && v.iter().all(|x| x.is_finite()));
    if !sound {
        return Err(Error::Validation(
            "embedding reply does not match its inputs".into(),
        ));
    }
    Ok(dims)
}

/// Embeds every unmarked item that has no row for the configured model, and returns how
/// many. Marked items are never sent, and have no rows. `ManualRequired` when no local
/// embedding model is set up. A batch is stored whole or not at all.
pub async fn index_missing(db: &Db, router: &Router, cancel: CancellationToken) -> Result<usize> {
    let model = model_setting(db).await?;
    let done: Vec<String> = sqlx::query_scalar("SELECT item_id FROM embeddings WHERE model = ?")
        .bind(&model)
        .fetch_all(&db.pool)
        .await?;
    let pending: Vec<_> = knowledge::list(db)
        .await?
        .into_iter()
        .filter(|item| !item.marked_sensitive && !item.locked && !item.body.is_empty())
        .filter(|item| !done.contains(&item.id))
        .collect();

    let mut stored = 0;
    for batch in pending.chunks(BATCH) {
        let inputs: Vec<String> = batch.iter().map(|item| item.body.clone()).collect();
        let (used, vectors) = router.embed(db, &inputs, cancel.clone()).await?;
        let dims = check(&vectors, inputs.len())?;

        let mut write = db.pool.begin().await?;
        for (item, vector) in batch.iter().zip(&vectors) {
            // The WHERE re-checks the mark: an item marked while the model ran gets no row.
            let done = sqlx::query(
                "INSERT OR REPLACE INTO embeddings (item_id, model, dims, vector, created_at) \
                 SELECT id, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 FROM knowledge_items WHERE id = ? AND marked_sensitive = 0",
            )
            .bind(&used)
            .bind(i64::try_from(dims).unwrap_or(i64::MAX))
            .bind(to_blob(vector))
            .bind(&item.id)
            .execute(&mut *write)
            .await?;
            stored += usize::try_from(done.rows_affected()).unwrap_or(0);
        }
        write.commit().await?;
    }
    Ok(stored)
}

#[derive(sqlx::FromRow)]
struct Stored {
    item_id: String,
    dims: i64,
    vector: Vec<u8>,
    body: String,
}

/// Best matches first, over approved unmarked items only. `Err(ManualRequired)` means the
/// caller should use `knowledge::search_text`.
pub async fn search(db: &Db, router: &Router, query: &str, limit: usize) -> Result<Vec<Hit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let (model, mut vectors) = router
        .embed(db, &[query.to_string()], CancellationToken::new())
        .await?;
    let dims = check(&vectors, 1)?;
    let query = vectors.remove(0);

    // The ceiling bounds memory; rows past it are not searched.
    let rows: Vec<Stored> = sqlx::query_as(
        "SELECT e.item_id, e.dims, e.vector, k.body FROM embeddings e \
         JOIN knowledge_items k ON k.id = e.item_id \
         WHERE e.model = ? AND k.approved = 1 AND k.marked_sensitive = 0 LIMIT ?",
    )
    .bind(&model)
    .bind(i64::try_from(VECTOR_CEILING).unwrap_or(i64::MAX))
    .fetch_all(&db.pool)
    .await?;

    let mut hits: Vec<Hit> = rows
        .into_iter()
        .filter(|row| usize::try_from(row.dims) == Ok(dims) && row.vector.len() == dims * 4)
        .map(|row| Hit {
            score: cosine(&query, &from_blob(&row.vector)),
            item_id: row.item_id,
            body: row.body,
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    Ok(hits)
}
