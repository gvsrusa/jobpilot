use jobpilot_core::documents::{self, fts_query};
use jobpilot_core::{AppPaths, Db, Error, audit, settings};
use serde_json::json;
use tempfile::TempDir;

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

#[tokio::test]
async fn crud_round_trip() {
    let (_tmp, db) = open().await;
    let first = documents::create(&db, "One", "first body").await.unwrap();
    assert_eq!(first.id.len(), 32);
    assert_eq!(documents::get(&db, &first.id).await.unwrap().title, "One");

    let second = documents::create(&db, "Two", "second body").await.unwrap();
    let updated = documents::update(&db, &first.id, "One b", "changed")
        .await
        .unwrap();
    assert_eq!(updated.title, "One b");
    assert_eq!(updated.body, "changed");
    assert_eq!(updated.created_at, first.created_at);

    let ids: Vec<_> = documents::list(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(ids, [second.id.clone(), first.id.clone()]);

    documents::delete(&db, &first.id).await.unwrap();
    assert!(matches!(
        documents::get(&db, &first.id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        documents::delete(&db, &first.id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        documents::update(&db, &first.id, "x", "y").await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn search_finds_title_and_body_with_diacritics() {
    let (_tmp, db) = open().await;
    let doc = documents::create(&db, "Résumé notes", "Rust engineer")
        .await
        .unwrap();
    for q in ["resume", "rust"] {
        let hits = documents::search(&db, q).await.unwrap();
        assert_eq!(hits.len(), 1, "{q}");
        assert_eq!(hits[0].id, doc.id);
    }
}

#[tokio::test]
async fn search_tracks_update_and_delete() {
    let (_tmp, db) = open().await;
    let doc = documents::create(&db, "Plan", "alpha").await.unwrap();
    documents::update(&db, &doc.id, "Plan", "bravo")
        .await
        .unwrap();
    assert!(documents::search(&db, "alpha").await.unwrap().is_empty());
    assert_eq!(documents::search(&db, "bravo").await.unwrap().len(), 1);
    documents::delete(&db, &doc.id).await.unwrap();
    assert!(documents::search(&db, "bravo").await.unwrap().is_empty());
}

#[tokio::test]
async fn search_accepts_any_user_text() {
    let (_tmp, db) = open().await;
    for q in [
        "c++ \"dev",
        "AND",
        "*",
        "-",
        "\"",
        "NEAR(",
        "title:x",
        "",
        "   ",
        "a\0b",
        "\0",
        "alpha\0",
    ] {
        assert!(documents::search(&db, q).await.is_ok(), "{q}");
    }
}

#[tokio::test]
async fn search_ignores_a_nul_byte() {
    let (_tmp, db) = open().await;
    let doc = documents::create(&db, "alpha", "body").await.unwrap();
    let hits = documents::search(&db, "alpha\0").await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, doc.id);
}

#[tokio::test]
async fn audit_limit_is_clamped() {
    let (_tmp, db) = open().await;
    for n in 0..3 {
        audit::record(&db, "gui", "act", None, json!({ "n": n }))
            .await
            .unwrap();
    }
    assert_eq!(audit::recent(&db, -1).await.unwrap().len(), 1);
    assert_eq!(audit::recent(&db, 0).await.unwrap().len(), 1);
    assert_eq!(audit::recent(&db, i64::MAX).await.unwrap().len(), 3);
}

#[test]
fn fts_query_quotes_terms() {
    assert_eq!(fts_query("c++ \"dev"), Some("\"c++\" \"\"\"dev\"".into()));
    assert_eq!(fts_query("  "), None);
}

#[tokio::test]
async fn settings_round_trip() {
    let (_tmp, db) = open().await;
    assert_eq!(settings::get(&db, "theme").await.unwrap(), None);
    settings::set(&db, "theme", &json!("dark")).await.unwrap();
    assert_eq!(
        settings::get(&db, "theme").await.unwrap(),
        Some(json!("dark"))
    );
    settings::set(&db, "theme", &json!({"mode": 2}))
        .await
        .unwrap();
    assert_eq!(
        settings::get(&db, "theme").await.unwrap(),
        Some(json!({"mode": 2}))
    );
}

#[tokio::test]
async fn audit_records_in_reverse_order() {
    let (_tmp, db) = open().await;
    audit::record(&db, "gui", "first", None, json!({}))
        .await
        .unwrap();
    audit::record(&db, "cli", "second", Some("t"), json!({"n": 1}))
        .await
        .unwrap();
    let latest = audit::recent(&db, 1).await.unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].action, "second");
    assert_eq!(latest[0].actor, "cli");
    assert_eq!(latest[0].target.as_deref(), Some("t"));
    assert_eq!(latest[0].detail, json!({"n": 1}));
    assert_eq!(audit::recent(&db, 10).await.unwrap().len(), 2);
}
