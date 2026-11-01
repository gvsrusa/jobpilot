//! Embeddings over the knowledge base. The provider is a mock that hashes text into 8
//! dimensions, so equal text gives equal vectors and nothing reaches a real Ollama.
//! The `#[ignore]`d test marks an item sensitive, which needs the keychain.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::time::Duration;

use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    Availability, CancellationToken, Capability, Declaration, Prompt, Provider, ProviderId,
};
use jobpilot_core::knowledge::{self, cosine, from_blob, to_blob};
use jobpilot_core::{AppPaths, Db, Error, Result, embed, settings};
use serde_json::json;
use tempfile::TempDir;

const DIMS: usize = 8;

fn hash_vector(text: &str) -> Vec<f32> {
    (0..DIMS)
        .map(|i| {
            let mut hasher = DefaultHasher::new();
            (text, i).hash(&mut hasher);
            (hasher.finish() % 2001) as f32 / 1000.0 - 1.0
        })
        .collect()
}

/// `Good` hashes. `Short` drops the last vector, so vectors would misalign with items.
#[derive(Clone, Copy)]
enum Kind {
    Good,
    Short,
}

struct Mock(Kind);

impl Provider for Mock {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::Ollama,
            capabilities: vec![
                Capability::Embeddings,
                Capability::LocalExecution,
                Capability::NoTools,
                Capability::StructuredOutput,
            ],
            local: true,
            context_limit: 8192,
            timeout: Duration::from_secs(5),
            sign_in: "none".into(),
        }
    }

    fn available(&self) -> Pin<Box<dyn Future<Output = Availability> + Send + '_>> {
        Box::pin(async { Availability::Available })
    }

    fn complete<'a>(
        &'a self,
        _prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async { Err(Error::Failed("not used".into())) })
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send + 'a>> {
        Box::pin(async move {
            let mut vectors: Vec<Vec<f32>> = inputs.iter().map(|t| hash_vector(t)).collect();
            if matches!(self.0, Kind::Short) {
                vectors.pop();
            }
            Ok(vectors)
        })
    }
}

fn router(kind: Kind) -> Router {
    Router::with_providers(vec![Box::new(Mock(kind))])
}

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

async fn use_model(db: &Db, model: &str) {
    settings::set(db, "ai.embedding_model", &json!(model))
        .await
        .unwrap();
}

async fn add(db: &Db, body: &str) -> String {
    knowledge::add(db, "note", body, "user", None)
        .await
        .unwrap()
        .id
}

async fn rows(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM embeddings")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

#[tokio::test]
#[ignore = "marks an item sensitive, which needs the keychain"]
async fn index_skips_marked_sensitive_and_is_idempotent() {
    let (_tmp, db) = open().await;
    use_model(&db, "m1").await;
    let _a = add(&db, "alpha text").await;
    let _b = add(&db, "beta text").await;
    let secret = add(&db, "gamma secret").await;
    knowledge::mark_sensitive(&db, &secret, true).await.unwrap();

    let r = router(Kind::Good);
    assert_eq!(embed::index_missing(&db, &r, cancel()).await.unwrap(), 2);
    assert_eq!(rows(&db).await, 2);
    assert_eq!(embed::index_missing(&db, &r, cancel()).await.unwrap(), 0);

    // Marking an already embedded item drops its vector.
    let before = add(&db, "delta text").await;
    embed::index_missing(&db, &r, cancel()).await.unwrap();
    knowledge::mark_sensitive(&db, &before, true).await.unwrap();
    assert_eq!(rows(&db).await, 2);
}

#[tokio::test]
async fn marked_item_is_never_embedded() {
    let (_tmp, db) = open().await;
    use_model(&db, "m1").await;
    add(&db, "alpha text").await;
    let marked = add(&db, "beta text").await;
    // Set directly: the flag is what the indexer must respect, with or without a key.
    sqlx::query("UPDATE knowledge_items SET marked_sensitive = 1 WHERE id = ?")
        .bind(&marked)
        .execute(&db.pool)
        .await
        .unwrap();
    let n = embed::index_missing(&db, &router(Kind::Good), cancel())
        .await
        .unwrap();
    assert_eq!(n, 1);
    let ids: Vec<String> = sqlx::query_scalar("SELECT item_id FROM embeddings")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(!ids.contains(&marked));
}

#[tokio::test]
async fn search_ranks_by_cosine() {
    let (_tmp, db) = open().await;
    use_model(&db, "m1").await;
    add(&db, "worked on compilers").await;
    let b = add(&db, "led a payments team").await;
    add(&db, "taught chess to children").await;
    let r = router(Kind::Good);
    embed::index_missing(&db, &r, cancel()).await.unwrap();

    let hits = embed::search(&db, &r, "led a payments team", 2)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].item_id, b);
    assert_eq!(hits[0].body, "led a payments team");
    assert!((hits[0].score - 1.0).abs() < 1e-4);
    assert!(hits[0].score >= hits[1].score);
    assert!(
        embed::search(&db, &r, "x", 0).await.unwrap().is_empty(),
        "limit 0 returns nothing"
    );
}

#[tokio::test]
async fn search_leaves_out_unapproved_and_marked_items() {
    let (_tmp, db) = open().await;
    use_model(&db, "m1").await;
    let ok = add(&db, "approved text").await;
    let parsed = knowledge::add(&db, "note", "parsed text", "parsed", None)
        .await
        .unwrap();
    let marked = add(&db, "marked text").await;
    let r = router(Kind::Good);
    embed::index_missing(&db, &r, cancel()).await.unwrap();
    sqlx::query("UPDATE knowledge_items SET marked_sensitive = 1 WHERE id = ?")
        .bind(&marked)
        .execute(&db.pool)
        .await
        .unwrap();
    let hits = embed::search(&db, &r, "parsed text", 10).await.unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.item_id.as_str()).collect();
    assert_eq!(ids, vec![ok.as_str()]);
    assert!(!ids.contains(&parsed.id.as_str()));
}

#[tokio::test]
async fn no_embedding_model_is_manual_required() {
    let (_tmp, db) = open().await;
    add(&db, "alpha").await;
    let none = Router::with_providers(Vec::new());
    for model in [None, Some("m1")] {
        if let Some(model) = model {
            use_model(&db, model).await;
        }
        let indexed = embed::index_missing(&db, &none, cancel()).await;
        assert!(
            matches!(&indexed, Err(Error::ManualRequired(r)) if r == "no_embedding_model"),
            "{indexed:?}"
        );
        let found = embed::search(&db, &none, "alpha", 5).await;
        assert!(
            matches!(&found, Err(Error::ManualRequired(r)) if r == "no_embedding_model"),
            "{found:?}"
        );
    }
    assert_eq!(rows(&db).await, 0);
}

#[tokio::test]
async fn a_reply_with_the_wrong_count_stores_nothing() {
    let (_tmp, db) = open().await;
    use_model(&db, "m1").await;
    add(&db, "alpha").await;
    add(&db, "beta").await;
    let result = embed::index_missing(&db, &router(Kind::Short), cancel()).await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(rows(&db).await, 0);
}

#[test]
fn blob_round_trip_and_cosine_bounds() {
    let v = [0.5_f32, -1.25, 3.0, 0.0, 1e-3, -7.5, 2.0, 0.125];
    let blob = to_blob(&v);
    assert_eq!(blob.len(), v.len() * 4);
    assert_eq!(blob[..4], 0.5_f32.to_le_bytes());
    assert_eq!(from_blob(&blob), v);
    let neg: Vec<f32> = v.iter().map(|x| -x).collect();
    assert!((cosine(&v, &v) - 1.0).abs() < 1e-5);
    assert!((cosine(&v, &neg) + 1.0).abs() < 1e-5);
    // Degenerate input scores 0.0, never NaN.
    assert_eq!(cosine(&[0.0; 3], &[1.0, 2.0, 3.0]), 0.0);
    assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
    assert_eq!(cosine(&[], &[]), 0.0);
    // A torn blob drops its trailing bytes rather than panicking.
    assert_eq!(from_blob(&blob[..6]).len(), 1);
}

#[tokio::test]
async fn model_change_reindexes_only_that_model() {
    let (_tmp, db) = open().await;
    add(&db, "alpha").await;
    add(&db, "beta").await;
    let r = router(Kind::Good);
    use_model(&db, "m1").await;
    assert_eq!(embed::index_missing(&db, &r, cancel()).await.unwrap(), 2);
    use_model(&db, "m2").await;
    assert_eq!(embed::index_missing(&db, &r, cancel()).await.unwrap(), 2);
    assert_eq!(rows(&db).await, 4);
    assert_eq!(embed::index_missing(&db, &r, cancel()).await.unwrap(), 0);
    let per_model: Vec<(String, i64)> =
        sqlx::query_as("SELECT model, count(*) FROM embeddings GROUP BY model ORDER BY model")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(per_model, vec![("m1".into(), 2), ("m2".into(), 2)]);
}

#[tokio::test]
async fn search_skips_rows_of_other_dimensions() {
    let (_tmp, db) = open().await;
    use_model(&db, "m1").await;
    let id = add(&db, "alpha").await;
    sqlx::query(
        "INSERT INTO embeddings (item_id, model, dims, vector, created_at) \
         VALUES (?, 'm1', 3, ?, 'now')",
    )
    .bind(&id)
    .bind(to_blob(&[1.0, 2.0, 3.0]))
    .execute(&db.pool)
    .await
    .unwrap();
    let hits = embed::search(&db, &router(Kind::Good), "alpha", 5)
        .await
        .unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn text_search_covers_evidence_only() {
    let (_tmp, db) = open().await;
    let ok = add(&db, "Built a Rust compiler").await;
    knowledge::add(&db, "note", "Rust parsed claim", "parsed", None)
        .await
        .unwrap();
    let hits = knowledge::search_text(&db, "rust", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item_id, ok);
    assert_eq!(hits[0].score, 0.0);
    // LIKE wildcards in the query are literal.
    assert!(
        knowledge::search_text(&db, "%", 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        knowledge::search_text(&db, "  ", 10)
            .await
            .unwrap()
            .is_empty()
    );
}
