//! Retrieval over career memory. The provider hashes text into 8 dimensions, so equal text
//! gives equal vectors and nothing reaches a real Ollama. A sealed answer here is seeded
//! under a key made of sevens, so no test touches the keychain.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    Availability, CancellationToken, Capability, Declaration, Prompt, Provider, ProviderId,
};
use jobpilot_core::crypto::{self, DataKey};
use jobpilot_core::memory::{self, Class};
use jobpilot_core::{AppPaths, Db, Error, Result, settings};
use serde_json::json;
use tempfile::TempDir;

const DIMS: usize = 8;
const KEY_ID: &str = "00112233aabbccdd";
const CANARY: &str = "ZXQ-CANARY-ANSWER";

fn hash_vector(text: &str) -> Vec<f32> {
    (0..DIMS)
        .map(|i| {
            let mut hasher = DefaultHasher::new();
            (text, i).hash(&mut hasher);
            (hasher.finish() % 2001) as f32 / 1000.0 - 1.0
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Good,
    /// Drops the last vector, so vectors would misalign with questions.
    Short,
    /// Local capabilities missing: an external provider.
    External,
}

struct Mock {
    kind: Kind,
    calls: Arc<AtomicUsize>,
    /// Cancelled during the second call, as a user pressing stop mid-run: that reply is lost,
    /// and the first batch is already stored.
    cancel_on_second: Option<CancellationToken>,
}

impl Provider for Mock {
    fn declaration(&self) -> Declaration {
        let external = self.kind == Kind::External;
        let mut capabilities = vec![Capability::StructuredOutput, Capability::NoTools];
        if !external {
            capabilities.extend([Capability::Embeddings, Capability::LocalExecution]);
        }
        Declaration {
            id: if external {
                ProviderId::ClaudeCode
            } else {
                ProviderId::Ollama
            },
            capabilities,
            local: !external,
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
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(cancel) = self.cancel_on_second.as_ref().filter(|_| call == 2) {
            cancel.cancel();
        }
        Box::pin(async move {
            let mut vectors: Vec<Vec<f32>> = inputs.iter().map(|t| hash_vector(t)).collect();
            if self.kind == Kind::Short {
                vectors.pop();
            }
            Ok(vectors)
        })
    }
}

fn router(kind: Kind) -> (Router, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mock = Mock {
        kind,
        calls: calls.clone(),
        cancel_on_second: None,
    };
    (Router::with_providers(vec![Box::new(mock)]), calls)
}

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

async fn use_model(db: &Db) {
    settings::set(db, "ai.embedding_model", &json!("test-embed"))
        .await
        .unwrap();
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

async fn count(db: &Db, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// An approved plain answer under a new normal question.
async fn answered(db: &Db, question: &str, answer: &str) -> (String, String) {
    let question = memory::add_question(db, question, None).await.unwrap();
    assert_eq!(question.class, Class::Normal);
    let answer = memory::add_answer(db, &question.id, answer, None)
        .await
        .unwrap();
    memory::approve(db, "cli", &answer.id, true).await.unwrap();
    (question.id, answer.id)
}

/// A sealed, approved answer under a sensitive question, sealed under [`key`].
async fn sealed_answer(db: &Db) -> (String, String) {
    let question = memory::add_question(db, "What is your salary?", None)
        .await
        .unwrap();
    assert_eq!(question.class, Class::Sensitive);
    let key = DataKey::from_bytes(KEY_ID, [7; 32]);
    let check = key
        .encrypt("data_keys", "key_check", KEY_ID, crypto::key_check_text())
        .unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) VALUES (?, 't', ?, 0)",
    )
    .bind(KEY_ID)
    .bind(check)
    .execute(&db.pool)
    .await
    .unwrap();
    let sealed = key
        .encrypt("memory_answers", "body_enc", "sealed1", CANARY.as_bytes())
        .unwrap();
    sqlx::query(
        "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, approved, source, \
         reviewed_at, created_at, updated_at) \
         VALUES ('sealed1', ?, '', ?, ?, 1, 'user', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 't', 't')",
    )
    .bind(&question.id)
    .bind(sealed)
    .bind(KEY_ID)
    .execute(&db.pool)
    .await
    .unwrap();
    (question.id, "sealed1".into())
}

#[tokio::test]
async fn search_finds_by_meaning_with_a_local_model() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    let (near, _) = answered(&db, "Why do you want this job?", "Because.").await;
    memory::add_question(&db, "Describe your leadership style", None)
        .await
        .unwrap();
    let (router, _) = router(Kind::Good);
    assert_eq!(
        memory::index_missing(&db, &router, cancel()).await.unwrap(),
        2
    );

    let hits = memory::search(&db, &router, "Why do you want this job?", 5)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].question_id, near);
    assert_eq!(hits[0].via, "semantic");
    assert!(hits[0].score > hits[1].score);
    assert_eq!(hits[0].answer.as_deref(), Some("Because."));
    db.close().await;
}

#[tokio::test]
async fn search_falls_back_to_text_with_no_model() {
    let (_tmp, db) = open().await;
    let (id, _) = answered(&db, "Describe your leadership style", "I mentor.").await;
    answered(&db, "Why this job?", "Because.").await;
    let (router, calls) = router(Kind::Good);

    let hits = memory::search(&db, &router, "leadership", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].question_id, id);
    assert_eq!(hits[0].via, "text");
    // The answer's words are found too.
    let hits = memory::search(&db, &router, "mentor", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no model is set");
    db.close().await;
}

#[tokio::test]
async fn search_falls_back_to_text_when_nothing_is_indexed() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    let (id, _) = answered(&db, "Describe your leadership style", "I mentor.").await;
    let (router, _) = router(Kind::Good);

    let hits = memory::search(&db, &router, "leadership", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].question_id, id);
    assert_eq!(hits[0].via, "text");
    db.close().await;
}

#[tokio::test]
async fn a_sealed_answers_value_is_never_indexed_or_returned() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    let (question, answer) = sealed_answer(&db).await;
    let (router, _) = router(Kind::Good);
    memory::index_missing(&db, &router, cancel()).await.unwrap();

    let rows: Vec<(String, String)> = sqlx::query_as("SELECT question, answer FROM memory_fts")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].1.contains(CANARY) && rows[0].1.is_empty());

    for query in ["salary", CANARY] {
        let hits = memory::search(&db, &router, query, 5).await.unwrap();
        for hit in hits.iter().filter(|hit| hit.question_id == question) {
            assert_eq!(hit.answer, None);
            assert!(hit.needs_confirmation);
            assert_eq!(hit.answer_id.as_deref(), Some(answer.as_str()));
            assert!(!format!("{hit:?}").contains(CANARY));
        }
    }
    // By text, the value is not a word anywhere.
    let none = Router::with_providers(vec![]);
    sqlx::query("DELETE FROM settings WHERE key = 'ai.embedding_model'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        memory::search(&db, &none, CANARY, 5)
            .await
            .unwrap()
            .is_empty()
    );
    let hits = memory::search(&db, &none, "salary", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].answer, None);
    assert!(hits[0].needs_confirmation);
    db.close().await;
}

#[tokio::test]
async fn work_authorization_question_is_never_found_by_search() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    let text = "Are you legally authorized to work in the United States?";
    let question = memory::add_question(&db, text, Some("US")).await.unwrap();
    assert_eq!(question.class, Class::WorkAuthorization);
    answered(&db, "Why this job?", "Because.").await;
    let (router, _) = router(Kind::Good);

    // Only the other question is embedded.
    assert_eq!(
        memory::index_missing(&db, &router, cancel()).await.unwrap(),
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts").await, 1);
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        1
    );
    let seq: i64 = sqlx::query_scalar("SELECT seq FROM memory_questions WHERE id = ?")
        .bind(&question.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let in_fts: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_fts WHERE rowid = ?")
        .bind(seq)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(in_fts, 0);

    // The semantic path, for the exact text, and the text path.
    let hits = memory::search(&db, &router, text, 10).await.unwrap();
    assert!(hits.iter().all(|hit| hit.question_id != question.id));
    sqlx::query("DELETE FROM memory_embeddings")
        .execute(&db.pool)
        .await
        .unwrap();
    for query in [text, "authorized work United States"] {
        let hits = memory::search(&db, &router, query, 10).await.unwrap();
        assert!(hits.iter().all(|hit| hit.question_id != question.id));
    }
    db.close().await;
}

#[tokio::test]
async fn raising_a_class_removes_it_from_both_indexes() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    let (id, _) = answered(&db, "Tell me about your strengths", "Teamwork.").await;
    let (router, _) = router(Kind::Good);
    memory::index_missing(&db, &router, cancel()).await.unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts").await, 1);

    // A person lowers nothing here: this raises to work authorization, which seals nothing.
    sqlx::query("DELETE FROM memory_answers WHERE question_id = ?")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
    memory::set_class(&db, "cli", &id, Class::WorkAuthorization)
        .await
        .unwrap();

    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        0
    );
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts").await, 0);
    assert!(
        memory::search(&db, &router, "Tell me about your strengths", 5)
            .await
            .unwrap()
            .is_empty()
    );
    db.close().await;
}

#[tokio::test]
async fn an_unapproved_answer_is_not_returned() {
    let (_tmp, db) = open().await;
    let question = memory::add_question(&db, "Why this job?", None)
        .await
        .unwrap();
    let answer = memory::add_answer(&db, &question.id, "Because.", None)
        .await
        .unwrap();
    let (router, _) = router(Kind::Good);

    let hits = memory::search(&db, &router, "job", 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].answer, None);
    assert_eq!(hits[0].answer_id, None);
    assert!(!hits[0].needs_confirmation);

    memory::approve(&db, "cli", &answer.id, true).await.unwrap();
    let hits = memory::search(&db, &router, "job", 5).await.unwrap();
    assert_eq!(hits[0].answer.as_deref(), Some("Because."));
    assert_eq!(hits[0].answer_id.as_deref(), Some(answer.id.as_str()));
    assert!(!hits[0].stale);
    db.close().await;
}

#[tokio::test]
async fn no_memory_text_reaches_an_external_provider() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    answered(&db, "Why this job?", "Because.").await;
    let (router, calls) = router(Kind::External);

    let error = memory::index_missing(&db, &router, cancel())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::ManualRequired(_)), "{error:?}");
    let hits = memory::search(&db, &router, "job", 5).await.unwrap();
    assert_eq!(hits[0].via, "text");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        0
    );
    db.close().await;
}

#[tokio::test]
async fn index_missing_skips_work_authorization_and_rebuilds_after_cancel() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    for n in 0..70 {
        memory::add_question(&db, &format!("Question number {n} about teamwork"), None)
            .await
            .unwrap();
    }
    for country in ["US", "CA", "PT"] {
        memory::add_question(
            &db,
            "Are you authorized to work in this country?",
            Some(country),
        )
        .await
        .unwrap();
    }

    let stop = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let first = Router::with_providers(vec![Box::new(Mock {
        kind: Kind::Good,
        calls: calls.clone(),
        cancel_on_second: Some(stop.clone()),
    })]);
    let error = memory::index_missing(&db, &first, stop).await.unwrap_err();
    assert!(matches!(error, Error::Cancelled), "{error:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        32
    );

    let (router, _) = router(Kind::Good);
    assert_eq!(
        memory::index_missing(&db, &router, cancel()).await.unwrap(),
        38
    );
    assert_eq!(
        memory::index_missing(&db, &router, cancel()).await.unwrap(),
        0
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        70
    );
    let authz: i64 = count(
        &db,
        "SELECT count(*) FROM memory_embeddings e JOIN memory_questions q ON q.id = e.question_id \
         WHERE q.class = 'work_authorization'",
    )
    .await;
    assert_eq!(authz, 0);
    db.close().await;
}

#[tokio::test]
async fn a_vector_count_mismatch_stores_nothing() {
    let (_tmp, db) = open().await;
    use_model(&db).await;
    answered(&db, "Why this job?", "Because.").await;
    memory::add_question(&db, "Describe your leadership style", None)
        .await
        .unwrap();
    let (router, _) = router(Kind::Short);

    assert!(memory::index_missing(&db, &router, cancel()).await.is_err());
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_embeddings").await,
        0
    );
    db.close().await;
}

#[tokio::test]
async fn an_empty_query_returns_nothing() {
    let (_tmp, db) = open().await;
    answered(&db, "Why this job?", "Because.").await;
    let (router, calls) = router(Kind::Good);

    for query in ["", "   ", "\0"] {
        assert!(
            memory::search(&db, &router, query, 5)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        memory::search(&db, &router, "job", 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    db.close().await;
}

#[tokio::test]
async fn removing_a_question_or_an_answer_updates_the_text_index() {
    let (_tmp, db) = open().await;
    let (question, answer) = answered(&db, "Why this job?", "Because zebra.").await;
    let (router, _) = router(Kind::Good);
    assert_eq!(
        memory::search(&db, &router, "zebra", 5)
            .await
            .unwrap()
            .len(),
        1
    );

    memory::remove_answer(&db, &answer).await.unwrap();
    assert!(
        memory::search(&db, &router, "zebra", 5)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        memory::search(&db, &router, "job", 5).await.unwrap().len(),
        1
    );

    memory::remove_question(&db, &question).await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts").await, 0);
    db.close().await;
}

#[tokio::test]
async fn editing_an_answer_reindexes_it() {
    let (_tmp, db) = open().await;
    let (_, answer) = answered(&db, "Why this job?", "Because zebra.").await;
    let (router, _) = router(Kind::Good);

    memory::edit_answer(&db, &answer, "Because giraffe.")
        .await
        .unwrap();
    assert!(
        memory::search(&db, &router, "zebra", 5)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        memory::search(&db, &router, "giraffe", 5)
            .await
            .unwrap()
            .len(),
        1
    );
    db.close().await;
}
