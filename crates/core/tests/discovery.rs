//! The discovery agent and duplicate groups. Every request goes to `tests/fake_http` on
//! 127.0.0.1; nothing here reaches a real host.

mod fake_http;

use std::time::{Duration, Instant};

use fake_http::{Canned, Fake};
use jobpilot_core::agents::{discover_with, resume_run_with};
use jobpilot_core::ai::CancellationToken;
use jobpilot_core::ai::router::Router;
use jobpilot_core::connectors::fetch::Fetcher;
use jobpilot_core::jobs::dedupe::{self, SIMILAR, company_key, dedupe_key, similar};
use jobpilot_core::jobs::{
    JobFilter, Upserted, add_source, group, list, list_sources, remove_source, split,
};
use jobpilot_core::runner::{self, RunState};
use jobpilot_core::{AppPaths, Db, Error, pause};
use serde_json::{Value, json};
use tempfile::TempDir;

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

/// A rerun in a test must not wait out a real interval.
async fn fresh_limits(db: &Db) {
    sqlx::query("DELETE FROM connector_limits")
        .execute(&db.pool)
        .await
        .unwrap();
}

fn lever_path(skip: usize) -> String {
    format!("/v0/postings/acme?mode=json&skip={skip}&limit=100")
}

fn route(skip: usize) -> &'static str {
    Box::leak(lever_path(skip).into_boxed_str())
}

fn lever_row(id: &str, title: &str, country: Option<&str>, text: &str) -> Value {
    json!({
        "id": id, "text": title, "country": country, "workplaceType": "remote",
        "categories": {"location": "Anywhere"}, "description": text,
        "hostedUrl": format!("https://jobs.lever.co/acme/{id}"),
    })
}

fn lever_rows(from: usize, count: usize) -> Value {
    Value::Array(
        (from..from + count)
            .map(|n| {
                lever_row(
                    &format!("id{n}"),
                    &format!("Role {n}"),
                    Some("US"),
                    "<p>x</p>",
                )
            })
            .collect(),
    )
}

fn remoteok_body(rows: &[(&str, &str, &[&str], &str)]) -> Value {
    let mut all = vec![json!({"last_updated": 1, "legal": "Link back to Remote OK."})];
    for (id, title, tags, text) in rows {
        all.push(json!({
            "id": id, "position": title, "company": "Sample Co", "tags": tags,
            "description": text, "location": "Worldwide",
            "url": format!("https://remoteOK.com/remote-jobs/{id}"),
        }));
    }
    Value::Array(all)
}

fn three() -> Value {
    remoteok_body(&[
        ("1", "Rust Engineer", &["rust"], "<p>Build things.</p>"),
        ("2", "Designer", &["design"], "<p>Draw things.</p>"),
        ("3", "Analyst", &["data"], "<p>Count things.</p>"),
    ])
}

fn fake_remoteok(body: &Value) -> (Fake, Fetcher) {
    let fake = Fake::start(vec![("/api", Canned::json(200, body))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    (fake, fetcher)
}

async fn source(db: &Db, connector: &str, target: &str, query: &str) -> String {
    add_source(db, connector, target, query).await.unwrap().id
}

#[tokio::test]
async fn a_run_stores_jobs_marked_with_their_source_and_attribution() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    let id = source(&db, "remoteok", "", "").await;
    let report = discover_with(&db, &fetcher, "cli", vec![id.clone()], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].added, 3);
    assert_eq!(report.sources[0].source_id, id);
    let jobs = list(&db, &JobFilter::default(), 50).await.unwrap();
    assert_eq!(jobs.len(), 3);
    assert!(
        jobs.iter()
            .all(|j| j.attribution.as_deref() == Some("Remote OK"))
    );
    assert!(jobs.iter().all(|j| j.connector == "remoteok"));
    let marked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE source_id = ?")
        .bind(&id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(marked, 3);
}

#[tokio::test]
async fn an_empty_source_list_means_every_source() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    source(&db, "remoteok", "", "").await;
    let report = discover_with(&db, &fetcher, "cli", vec![], token())
        .await
        .unwrap();
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].added, 3);
}

#[tokio::test]
async fn only_a_person_starts_a_run() {
    let (_t, db) = open().await;
    let (fake, fetcher) = fake_remoteok(&three());
    source(&db, "remoteok", "", "").await;
    let error = discover_with(&db, &fetcher, "system", vec![], token())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Refused(r) if r == "only_a_person"));
    assert!(fake.paths().is_empty());
    assert!(runner::list(&db, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn no_run_starts_while_paused_and_no_request_goes() {
    let (_t, db) = open().await;
    let (fake, fetcher) = fake_remoteok(&three());
    source(&db, "remoteok", "", "").await;
    pause::set(&db, "cli").await.unwrap();
    let error = discover_with(&db, &fetcher, "cli", vec![], token())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Paused));
    assert!(fake.paths().is_empty());
    assert!(runner::list(&db, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_pause_halts_a_run_mid_request_and_a_resume_goes_on() {
    let (_t, db) = open().await;
    let first = Fake::start(vec![
        (route(0), Canned::json(200, &lever_rows(0, 100))),
        (route(100), Canned::stalled(Duration::from_secs(20))),
    ]);
    let fetcher = Fetcher::for_test(&first.url).unwrap();
    let id = source(&db, "lever", "acme", "").await;

    let run = async {
        let result = discover_with(&db, &fetcher, "cli", vec![id.clone()], token()).await;
        (result, Instant::now())
    };
    let control = async {
        let wanted = lever_path(100);
        let began = Instant::now();
        while !first.paths().contains(&wanted) {
            assert!(
                began.elapsed() < Duration::from_secs(15),
                "page 2 never asked"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let at = Instant::now();
        pause::set(&db, "cli").await.unwrap();
        at
    };
    let ((result, ended), paused_at) = tokio::join!(run, control);
    assert!(matches!(result, Err(Error::Paused)));
    assert!(
        ended - paused_at < Duration::from_secs(1),
        "{:?}",
        ended - paused_at
    );

    let runs = runner::list(&db, 10).await.unwrap();
    assert_eq!(runs[0].state, RunState::Halted);
    assert_eq!(runs[0].reason.as_deref(), Some("paused"));
    let stored = list(&db, &JobFilter::default(), 500).await.unwrap();
    assert_eq!(stored.len(), 100, "page 1's jobs stay");
    let cursor: i64 = sqlx::query_scalar("SELECT cursor FROM job_sources WHERE id = ?")
        .bind(&id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(cursor, 1);

    pause::clear(&db, "cli").await.unwrap();
    let second = Fake::start(vec![(route(100), Canned::json(200, &lever_rows(100, 30)))]);
    let fetcher = Fetcher::for_test(&second.url).unwrap();
    let router = Router::with_providers(vec![]);
    let done = resume_run_with(&db, &router, &fetcher, "cli", &runs[0].id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(second.paths(), [lever_path(100)], "no page repeats");
    let stored = list(&db, &JobFilter::default(), 500).await.unwrap();
    assert_eq!(stored.len(), 130);
    let cursor: i64 = sqlx::query_scalar("SELECT cursor FROM job_sources WHERE id = ?")
        .bind(&id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(cursor, 0, "a finished source starts over");
}

#[tokio::test]
async fn a_rerun_is_idempotent() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    let id = source(&db, "remoteok", "", "").await;
    discover_with(&db, &fetcher, "cli", vec![id.clone()], token())
        .await
        .unwrap();
    let before = list(&db, &JobFilter::default(), 50).await.unwrap();
    fresh_limits(&db).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let report = discover_with(&db, &fetcher, "cli", vec![id], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].added, 0);
    assert_eq!(report.sources[0].updated, 0);
    let after = list(&db, &JobFilter::default(), 50).await.unwrap();
    assert_eq!(before.len(), after.len());
    for job in &after {
        let old = before.iter().find(|b| b.id == job.id).unwrap();
        assert_eq!(old.job_text_id, job.job_text_id);
        assert_eq!(old.first_seen_at, job.first_seen_at);
        assert!(job.last_seen_at > old.last_seen_at);
    }
    let texts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_texts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(texts, 3);
}

#[tokio::test]
async fn a_changed_posting_gets_a_new_text_and_loses_its_score() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    let id = source(&db, "remoteok", "", "").await;
    discover_with(&db, &fetcher, "cli", vec![id.clone()], token())
        .await
        .unwrap();
    let job = list(&db, &JobFilter::default(), 50)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.title == "Designer")
        .unwrap();
    sqlx::query(
        "INSERT INTO match_results (job_id, score, compared_weight, explanation, inputs, \
         created_at) VALUES (?, 70, 10, 'x', '{}', '2026-01-01T00:00:00Z')",
    )
    .bind(&job.id)
    .execute(&db.pool)
    .await
    .unwrap();

    let changed = remoteok_body(&[
        ("1", "Rust Engineer", &["rust"], "<p>Build things.</p>"),
        (
            "2",
            "Designer",
            &["design"],
            "<p>Draw different things.</p>",
        ),
        ("3", "Analyst", &["data"], "<p>Count things.</p>"),
    ]);
    let (_fake, fetcher) = fake_remoteok(&changed);
    fresh_limits(&db).await;
    let report = discover_with(&db, &fetcher, "cli", vec![id], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].updated, 1);
    assert_eq!(report.sources[0].added, 0);

    let now = jobpilot_core::jobs::get(&db, &job.id).await.unwrap();
    assert_ne!(now.job_text_id, job.job_text_id);
    assert_eq!(now.score, None);
    let scored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM match_results WHERE job_id = ?")
        .bind(&job.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(scored, 0);
    let old: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_texts WHERE id = ?")
        .bind(&job.job_text_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(old, 1, "the old text stays");
    let found = list(
        &db,
        &JobFilter {
            query: Some("different".into()),
            ..JobFilter::default()
        },
        10,
    )
    .await
    .unwrap();
    assert_eq!(found.len(), 1, "search reads the new text");
}

#[tokio::test]
async fn one_refused_source_does_not_stop_the_next() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![
        (
            route(0),
            Canned {
                status: 403,
                headers: vec![("Content-Type", "text/plain".into())],
                body: String::new(),
                hold: Duration::ZERO,
                announced: None,
            },
        ),
        ("/api", Canned::json(200, &three())),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let blocked = source(&db, "lever", "acme", "").await;
    let good = source(&db, "remoteok", "", "").await;
    let report = discover_with(&db, &fetcher, "cli", vec![blocked, good], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].reason.as_deref(), Some("source_blocked"));
    assert_eq!(report.sources[0].added, 0);
    assert_eq!(report.sources[1].reason, None);
    assert_eq!(report.sources[1].added, 3);
}

#[tokio::test]
async fn adding_a_source_with_no_read_is_refused() {
    let (_t, db) = open().await;
    let e = add_source(&db, "linkedin", "acme", "").await.unwrap_err();
    assert!(matches!(e, Error::Refused(r) if r == "operation_not_declared"));
    let e = add_source(&db, "company_page", "x", "").await.unwrap_err();
    assert!(matches!(e, Error::Refused(r) if r == "operation_not_declared"));
    assert!(list_sources(&db).await.unwrap().is_empty());
}

#[tokio::test]
async fn adding_a_d3_source_is_stored() {
    let (_t, db) = open().await;
    assert_eq!(
        add_source(&db, "greenhouse", "acme", "")
            .await
            .unwrap()
            .connector,
        "greenhouse"
    );
    assert_eq!(list_sources(&db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_source_target_is_checked_and_sources_are_capped() {
    let (_t, db) = open().await;
    for bad in ["../x", "a/b", ""] {
        let e = add_source(&db, "lever", bad, "").await.unwrap_err();
        assert_eq!(e.code(), "validation", "{bad:?}");
    }
    add_source(&db, "lever", "eu:acme", "").await.unwrap();
    let e = add_source(&db, "wwr", "not-a-feed", "").await.unwrap_err();
    assert_eq!(e.code(), "validation");
    add_source(&db, "wwr", "remote-programming-jobs", "")
        .await
        .unwrap();
    let e = add_source(&db, "remoteok", "x", "").await.unwrap_err();
    assert_eq!(e.code(), "validation", "a feed with no target takes none");
    let again = add_source(&db, "lever", "eu:acme", "").await.unwrap();
    assert_eq!(list_sources(&db).await.unwrap().len(), 2, "{}", again.id);
    for n in 0..48 {
        add_source(&db, "lever", &format!("b{n}"), "")
            .await
            .unwrap();
    }
    let e = add_source(&db, "lever", "onemore", "").await.unwrap_err();
    assert_eq!(e.code(), "validation");
    let all = list_sources(&db).await.unwrap();
    assert_eq!(all.len(), 50);
    remove_source(&db, &all[0].id).await.unwrap();
    assert_eq!(list_sources(&db).await.unwrap().len(), 49);
    assert_eq!(
        remove_source(&db, &all[0].id).await.unwrap_err().code(),
        "not_found"
    );
}

#[tokio::test]
async fn a_run_row_holds_no_posting_text() {
    let (_t, db) = open().await;
    let body = remoteok_body(&[(
        "1",
        "Canary Title",
        &["zebracanary"],
        "<p>ZEBRACANARY body</p>",
    )]);
    let (_fake, fetcher) = fake_remoteok(&body);
    source(&db, "remoteok", "", "").await;
    discover_with(&db, &fetcher, "cli", vec![], token())
        .await
        .unwrap();
    let runs: Vec<String> = sqlx::query_scalar(
        "SELECT input || checkpoint || coalesce(reason, '') || agent || state FROM agent_runs",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let log: Vec<String> = sqlx::query_scalar(
        "SELECT agent || step || state || data_categories || outcome || coalesce(provider, '') \
         FROM agent_run_log",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(!runs.is_empty() && !log.is_empty());
    for row in runs.iter().chain(&log) {
        let row = row.to_lowercase();
        assert!(
            !row.contains("zebracanary") && !row.contains("canary title"),
            "{row}"
        );
    }
}

#[tokio::test]
async fn a_query_filters_locally_and_requests_the_same_url() {
    let (_t, db) = open().await;
    let (fake, fetcher) = fake_remoteok(&three());
    let plain = source(&db, "remoteok", "", "").await;
    let narrow = source(&db, "remoteok", "", "rust").await;
    let report = discover_with(&db, &fetcher, "cli", vec![narrow], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].added, 1);
    assert_eq!(list(&db, &JobFilter::default(), 50).await.unwrap().len(), 1);
    // Text counts too, not only tags and title.
    for job in list(&db, &JobFilter::default(), 50).await.unwrap() {
        jobpilot_core::jobs::remove(&db, "cli", &job.id)
            .await
            .unwrap();
    }
    let by_text = source(&db, "remoteok", "", "count things").await;
    fresh_limits(&db).await;
    let report = discover_with(&db, &fetcher, "cli", vec![by_text], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].added, 1);
    fresh_limits(&db).await;
    let report = discover_with(&db, &fetcher, "cli", vec![plain], token())
        .await
        .unwrap();
    assert_eq!(report.sources[0].added, 2, "the third is already stored");
    assert!(fake.paths().iter().all(|p| p == "/api"));
}

fn wwr_body(items: &[(&str, &str)]) -> String {
    let mut out =
        String::from("<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>x</title>");
    for (title, slug) in items {
        out.push_str(&format!(
            "<item><title>{title}</title><region>Anywhere</region><description>&lt;p&gt;Work.\
             &lt;/p&gt;</description><pubDate>Sat, 01 Nov 2026 12:00:00 +0000</pubDate>\
             <guid>https://weworkremotely.com/remote-jobs/{slug}</guid>\
             <link>https://weworkremotely.com/remote-jobs/{slug}</link></item>"
        ));
    }
    out.push_str("</channel></rss>");
    out
}

#[tokio::test]
async fn the_same_job_on_two_sources_groups() {
    let (_t, db) = open().await;
    let rss = Canned {
        status: 200,
        headers: vec![("Content-Type", "application/rss+xml".into())],
        body: wwr_body(&[("ACME: Engineer", "acme-engineer")]),
        hold: Duration::ZERO,
        announced: None,
    };
    let lever = json!([lever_row("l1", "Engineer", None, "<p>Same work.</p>")]);
    let fake = Fake::start(vec![
        (route(0), Canned::json(200, &lever)),
        ("/remote-jobs.rss", rss),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let a = source(&db, "lever", "acme", "").await;
    let b = source(&db, "wwr", "", "").await;
    let report = discover_with(&db, &fetcher, "cli", vec![a, b], token())
        .await
        .unwrap();
    assert_eq!(report.grouped, 1);
    let shown = list(&db, &JobFilter::default(), 50).await.unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].connector, "lever", "the oldest leads");
    assert_eq!(shown[0].duplicates, 1);
    let both = group(&db, &shown[0].id).await.unwrap();
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].id, shown[0].id);
    let from_follower = group(&db, &both[1].id).await.unwrap();
    assert_eq!(from_follower.len(), 2);
    assert_eq!(from_follower[0].id, shown[0].id);
    let all = list(
        &db,
        &JobFilter {
            include_duplicates: true,
            ..JobFilter::default()
        },
        50,
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 2, "nothing is deleted");
}

#[tokio::test]
async fn a_different_country_is_a_different_job() {
    let (_t, db) = open().await;
    let rows = json!([
        lever_row("d1", "Engineer", Some("DE"), "<p>Work.</p>"),
        lever_row("p1", "Engineer", Some("PT"), "<p>Work.</p>"),
    ]);
    let fake = Fake::start(vec![(route(0), Canned::json(200, &rows))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let id = source(&db, "lever", "acme", "").await;
    let report = discover_with(&db, &fetcher, "cli", vec![id], token())
        .await
        .unwrap();
    assert_eq!(report.grouped, 0);
    assert_eq!(list(&db, &JobFilter::default(), 50).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_split_survives_the_next_fetch() {
    let (_t, db) = open().await;
    let rss = Canned {
        status: 200,
        headers: vec![("Content-Type", "application/rss+xml".into())],
        body: wwr_body(&[("ACME: Engineer", "acme-engineer")]),
        hold: Duration::ZERO,
        announced: None,
    };
    let lever = json!([lever_row("l1", "Engineer", None, "<p>Same work.</p>")]);
    let fake = Fake::start(vec![
        (route(0), Canned::json(200, &lever)),
        ("/remote-jobs.rss", rss),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let a = source(&db, "lever", "acme", "").await;
    let b = source(&db, "wwr", "", "").await;
    discover_with(&db, &fetcher, "cli", vec![a.clone(), b.clone()], token())
        .await
        .unwrap();
    let follower = list(
        &db,
        &JobFilter {
            include_duplicates: true,
            ..JobFilter::default()
        },
        50,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|j| j.duplicate_of.is_some())
    .unwrap();

    assert!(matches!(
        split(&db, "system", &follower.id).await.unwrap_err(),
        Error::Refused(r) if r == "only_a_person"
    ));
    split(&db, "cli", &follower.id).await.unwrap();
    assert!(
        jobpilot_core::jobs::get(&db, &follower.id)
            .await
            .unwrap()
            .duplicate_of
            .is_none()
    );

    fresh_limits(&db).await;
    let report = discover_with(&db, &fetcher, "cli", vec![a, b], token())
        .await
        .unwrap();
    assert_eq!(report.grouped, 0);
    let again = jobpilot_core::jobs::get(&db, &follower.id).await.unwrap();
    assert!(again.duplicate_of.is_none());
    assert_eq!(list(&db, &JobFilter::default(), 50).await.unwrap().len(), 2);
}

#[tokio::test]
async fn upsert_says_what_it_did() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    let connector = jobpilot_core::connectors::find("remoteok").unwrap();
    let target = jobpilot_core::connectors::Target {
        name: String::new(),
    };
    let page = fetcher
        .page(
            &db,
            &*connector,
            &target,
            0,
            jobpilot_core::connectors::Operation::ListFeed,
            &token(),
        )
        .await
        .unwrap();
    let posting = &page.postings[0];
    let decl = connector.decl();
    let first = jobpilot_core::jobs::upsert_posting(&db, decl, "", None, posting)
        .await
        .unwrap();
    let Upserted::Added(id) = first else {
        panic!("{first:?}")
    };
    let second = jobpilot_core::jobs::upsert_posting(&db, decl, "", None, posting)
        .await
        .unwrap();
    assert!(matches!(second, Upserted::Unchanged(ref same) if *same == id));
    let mut changed = posting.clone();
    changed.raw.push_str(" and more");
    let third = jobpilot_core::jobs::upsert_posting(&db, decl, "", None, &changed)
        .await
        .unwrap();
    assert!(matches!(third, Upserted::Updated(ref same) if *same == id));
}

#[test]
fn company_keys_fold_legal_forms() {
    assert_eq!(company_key("Müller GmbH"), company_key("MULLER"));
    assert_eq!(company_key("Acme, Inc."), company_key("acme"));
    assert_eq!(company_key("Acme S.A."), company_key("ACME"));
    assert_eq!(company_key("Foo Corp"), company_key("foo ltd"));
    assert_eq!(company_key("Foo & Co"), company_key("Foo"));
    assert_ne!(company_key("Acme"), company_key("Acne"));
    assert_eq!(
        company_key("Inc"),
        "",
        "a name of only a legal form has no key"
    );
}

#[test]
fn dedupe_keys_and_similarity() {
    let a = dedupe_key("Acme Inc", "Senior  Engineer", Some("us"), Some("remote"));
    assert_eq!(
        a,
        dedupe_key("ACME", "senior engineer", Some("US"), Some("remote"))
    );
    assert_ne!(
        a,
        dedupe_key("Acme", "Senior Engineer", Some("DE"), Some("remote"))
    );
    assert_ne!(
        a,
        dedupe_key("Acme", "Senior Engineer", Some("US"), Some("hybrid"))
    );
    assert_eq!(dedupe_key("", "Engineer", None, None), "");
    assert_eq!(dedupe_key("Acme", "", None, None), "");
    assert!((similar("Senior Rust Engineer", "senior rust engineer") - 1.0).abs() < 1e-6);
    assert!(similar("a b c d e", "a b c d e f") < SIMILAR);
    assert!(similar("a b c d e f", "a b c d e f g") >= SIMILAR);
    assert!(
        similar(
            "a b c d e f g h i j k l m n o p q r s t",
            "a b c d e f g h i j k l m n o p q r s t u"
        ) >= SIMILAR
    );
    assert_eq!(similar("", ""), 0.0);
}

/// Inserts a bare job straight into the store, for the grouping rule alone.
async fn bare(
    db: &Db,
    title: &str,
    company: &str,
    url: Option<&str>,
    country: Option<&str>,
) -> String {
    bare_in(db, title, company, url, country, "").await
}

async fn bare_in(
    db: &Db,
    title: &str,
    company: &str,
    url: Option<&str>,
    country: Option<&str>,
    location: &str,
) -> String {
    let text = jobpilot_core::knowledge::import_job_text(db, title, "<p>body</p>")
        .await
        .unwrap();
    let company_id: String = sqlx::query_scalar(
        "INSERT INTO companies (id, name, key, created_at) VALUES (lower(hex(randomblob(16))), ?1, \
         lower(?1), '2026-01-01') ON CONFLICT (key) DO UPDATE SET key = key RETURNING id",
    )
    .bind(company)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, job_text_id, connector, title, company_id, url, country, location, \
         first_seen_at, last_seen_at) VALUES (?1, ?1, 'import', ?2, ?3, ?4, ?5, ?6, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
    )
    .bind(&text.id)
    .bind(title)
    .bind(company_id)
    .bind(url)
    .bind(country)
    .bind(location)
    .execute(&db.pool)
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(3)).await;
    text.id
}

#[tokio::test]
async fn the_rule_groups_by_link_key_or_near_title_and_never_deletes() {
    let (_t, db) = open().await;
    let lead = bare(
        &db,
        "Senior Rust Engineer",
        "Acme",
        Some("https://Jobs.Acme.test/a/1?utm_source=x&ref=y#top"),
        Some("US"),
    )
    .await;
    // Same link once host case, query, and fragment are dropped.
    let by_url = bare(
        &db,
        "Something else",
        "Other",
        Some("https://jobs.acme.test/a/1"),
        None,
    )
    .await;
    // Same company, same title.
    let by_key = bare(&db, "senior rust  engineer", "ACME Inc", None, Some("US")).await;
    // Same company, a near title (19 of 20 words shared), same country.
    let long = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike \
                november oscar papa quebec romeo sierra tango";
    let first = bare(&db, long, "Zed", None, Some("US")).await;
    let near = bare(&db, &format!("{long} uniform"), "Zed Ltd", None, Some("US")).await;
    // A near title in another country stays apart.
    let far = bare(&db, &format!("{long} uniform"), "Zed", None, Some("DE")).await;
    // No company and no link: no key, no group.
    let loose_a = bare(&db, "Loose", "", None, None).await;
    let loose_b = bare(&db, "Loose", "", None, None).await;

    let changed = dedupe::run(&db).await.unwrap();
    assert_eq!(changed, 3);
    let of = |id: &str| {
        let id = id.to_owned();
        let db = &db;
        async move {
            jobpilot_core::jobs::get(db, &id)
                .await
                .unwrap()
                .duplicate_of
        }
    };
    assert_eq!(of(&lead).await, None);
    assert_eq!(of(&by_url).await.as_deref(), Some(lead.as_str()));
    assert_eq!(of(&by_key).await.as_deref(), Some(lead.as_str()));
    assert_eq!(of(&first).await, None);
    assert_eq!(of(&near).await.as_deref(), Some(first.as_str()));
    assert_eq!(of(&far).await, None);
    assert_eq!(of(&loose_a).await, None);
    assert_eq!(of(&loose_b).await, None);
    assert_eq!(
        dedupe::run(&db).await.unwrap(),
        0,
        "a second pass changes nothing"
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 8);
}

async fn parent_of(db: &Db, id: &str) -> Option<String> {
    jobpilot_core::jobs::get(db, id).await.unwrap().duplicate_of
}

#[tokio::test]
async fn one_title_in_two_places_is_two_jobs() {
    let (_t, db) = open().await;
    let london = bare_in(&db, "Account Executive", "Acme", None, None, "London, UK").await;
    let york = bare_in(&db, "Account Executive", "Acme", None, None, "New York, NY").await;
    let again = bare_in(&db, "Account Executive", "Acme", None, None, "london uk").await;
    let unknown_a = bare_in(&db, "Sales Lead", "Acme", None, None, "").await;
    let unknown_b = bare_in(&db, "Sales Lead", "Acme", None, None, "").await;
    dedupe::run(&db).await.unwrap();
    assert_eq!(parent_of(&db, &london).await, None);
    assert_eq!(parent_of(&db, &york).await, None);
    assert_eq!(
        parent_of(&db, &again).await.as_deref(),
        Some(london.as_str())
    );
    assert_eq!(parent_of(&db, &unknown_a).await, None);
    assert_eq!(
        parent_of(&db, &unknown_b).await,
        None,
        "an unknown place never matches on title alone"
    );
    // The same holds for a near title.
    let long = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike";
    let a = bare_in(&db, long, "Zed", None, Some("US"), "Austin").await;
    let b = bare_in(
        &db,
        &format!("{long} november"),
        "Zed",
        None,
        Some("US"),
        "Denver",
    )
    .await;
    dedupe::run(&db).await.unwrap();
    assert_eq!(parent_of(&db, &a).await, None);
    assert_eq!(parent_of(&db, &b).await, None);
}

#[tokio::test]
async fn links_keep_their_job_ids_and_drop_only_tracking() {
    let (_t, db) = open().await;
    let one = bare(
        &db,
        "A",
        "X1",
        Some("https://boards.test/acme?gh_jid=1"),
        None,
    )
    .await;
    let two = bare(
        &db,
        "B",
        "X2",
        Some("https://boards.test/acme?gh_jid=2"),
        None,
    )
    .await;
    let tracked = bare(
        &db,
        "C",
        "X3",
        Some("https://Boards.test/acme?utm_medium=a&gh_jid=1&gclid=z&fbclid=q&ref=r&source=s#f"),
        None,
    )
    .await;
    dedupe::run(&db).await.unwrap();
    assert_eq!(parent_of(&db, &one).await, None);
    assert_eq!(parent_of(&db, &two).await, None);
    assert_eq!(
        parent_of(&db, &tracked).await.as_deref(),
        Some(one.as_str())
    );
}

#[test]
fn plus_and_hash_stay_in_a_title() {
    assert_ne!(
        dedupe_key("Acme", "C++ Developer", Some("US"), None),
        dedupe_key("Acme", "C# Developer", Some("US"), None)
    );
    assert_ne!(
        dedupe_key("Acme", "C++ Developer", None, None),
        dedupe_key("Acme", "C Developer", None, None)
    );
    assert!(similar("C++ Developer", "C# Developer") < SIMILAR);
    assert_ne!(company_key("C++ Inc"), company_key("C# Inc"));
}

#[tokio::test]
async fn removing_a_job_removes_every_version_of_its_text() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    let id = source(&db, "remoteok", "", "").await;
    discover_with(&db, &fetcher, "cli", vec![id.clone()], token())
        .await
        .unwrap();
    for text in ["<p>Draw v2.</p>", "<p>Draw v3.</p>"] {
        let body = remoteok_body(&[
            ("1", "Rust Engineer", &["rust"], "<p>Build things.</p>"),
            ("2", "Designer", &["design"], text),
            ("3", "Analyst", &["data"], "<p>Count things.</p>"),
        ]);
        let (_fake, fetcher) = fake_remoteok(&body);
        fresh_limits(&db).await;
        discover_with(&db, &fetcher, "cli", vec![id.clone()], token())
            .await
            .unwrap();
    }
    let texts = |db: &Db| {
        let db = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM job_texts")
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    assert_eq!(texts(&db).await, 5, "three jobs, two older versions");
    let job = list(&db, &JobFilter::default(), 50)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.title == "Designer")
        .unwrap();
    jobpilot_core::jobs::remove(&db, "cli", &job.id)
        .await
        .unwrap();
    assert_eq!(texts(&db).await, 2, "every version of that job went");
}

#[tokio::test]
async fn handles_racing_to_store_one_posting_end_cleanly() {
    let (_t, db) = open().await;
    let (_fake, fetcher) = fake_remoteok(&three());
    let connector = jobpilot_core::connectors::find("remoteok").unwrap();
    let target = jobpilot_core::connectors::Target {
        name: String::new(),
    };
    let page = fetcher
        .page(
            &db,
            &*connector,
            &target,
            0,
            jobpilot_core::connectors::Operation::ListFeed,
            &token(),
        )
        .await
        .unwrap();
    let decl = connector.decl();
    for posting in &page.postings {
        let runs =
            (0..8).map(|_| jobpilot_core::jobs::upsert_posting(&db, decl, "", None, posting));
        let done = futures_join(runs).await;
        let added = done
            .iter()
            .filter(|r| matches!(r, Ok(Upserted::Added(_))))
            .count();
        assert!(done.iter().all(|r| r.is_ok()), "{done:?}");
        assert_eq!(added, 1, "{done:?}");
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 3);
    let texts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_texts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(texts, 3, "a lost race leaves no text behind");
}

async fn futures_join<F: std::future::Future>(items: impl Iterator<Item = F>) -> Vec<F::Output> {
    let mut set = Vec::new();
    for item in items {
        set.push(Box::pin(item));
    }
    let mut out = Vec::new();
    let mut pending: Vec<_> = set.into_iter().map(Some).collect();
    out.resize_with(pending.len(), || None);
    std::future::poll_fn(|cx| {
        let mut left = false;
        for (at, slot) in pending.iter_mut().enumerate() {
            if let Some(fut) = slot {
                match fut.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => {
                        out[at] = Some(value);
                        *slot = None;
                    }
                    std::task::Poll::Pending => left = true,
                }
            }
        }
        if left {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    out.into_iter().map(Option::unwrap).collect()
}

#[tokio::test]
async fn racing_adds_stay_within_the_cap_and_a_repeat_returns_its_row() {
    let (_t, db) = open().await;
    let adds = (0..60).map(|n| {
        add_source(
            &db,
            "lever",
            Box::leak(format!("b{n}").into_boxed_str()),
            "",
        )
    });
    let done = futures_join(adds).await;
    let stored = done.iter().filter(|r| r.is_ok()).count();
    assert_eq!(stored, 50, "{done:?}");
    assert_eq!(list_sources(&db).await.unwrap().len(), 50);
    let same = futures_join((0..10).map(|_| add_source(&db, "lever", "b0", ""))).await;
    let ids: std::collections::HashSet<_> = same
        .iter()
        .map(|r| r.as_ref().unwrap().id.clone())
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "a repeat returns the stored row, even at the cap"
    );
    let (_t2, db2) = open().await;
    let same = futures_join((0..10).map(|_| add_source(&db2, "lever", "fresh", ""))).await;
    let ids: std::collections::HashSet<_> = same
        .iter()
        .map(|r| r.as_ref().unwrap().id.clone())
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(list_sources(&db2).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_resume_honours_the_stored_cursor_when_page_one_was_all_filtered_out() {
    let (_t, db) = open().await;
    let first = Fake::start(vec![
        (route(0), Canned::json(200, &lever_rows(0, 100))),
        (route(100), Canned::stalled(Duration::from_secs(20))),
    ]);
    let fetcher = Fetcher::for_test(&first.url).unwrap();
    // No row says "zzz", so nothing is stored and no job id reaches the checkpoint.
    let id = source(&db, "lever", "acme", "zzz").await;
    let run = discover_with(&db, &fetcher, "cli", vec![id.clone()], token());
    let control = async {
        let began = Instant::now();
        while !first.paths().contains(&lever_path(100)) {
            assert!(
                began.elapsed() < Duration::from_secs(15),
                "page 2 never asked"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        pause::set(&db, "cli").await.unwrap();
    };
    let (result, ()) = tokio::join!(run, control);
    assert!(matches!(result, Err(Error::Paused)));
    pause::clear(&db, "cli").await.unwrap();
    let runs = runner::list(&db, 10).await.unwrap();
    let second = Fake::start(vec![(route(100), Canned::json(200, &lever_rows(100, 3)))]);
    let fetcher = Fetcher::for_test(&second.url).unwrap();
    let router = Router::with_providers(vec![]);
    let done = resume_run_with(&db, &router, &fetcher, "cli", &runs[0].id, token())
        .await
        .unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(
        second.paths(),
        [lever_path(100)],
        "page 1 is not read again"
    );
}

#[tokio::test]
async fn a_source_removed_mid_run_is_skipped() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![
        (route(0), Canned::json(200, &lever_rows(0, 100))),
        (route(100), Canned::json(200, &lever_rows(100, 2))),
        ("/api", Canned::json(200, &three())),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let a = source(&db, "lever", "acme", "").await;
    let b = source(&db, "remoteok", "", "").await;
    let run = discover_with(&db, &fetcher, "cli", vec![a, b.clone()], token());
    let control = async {
        let began = Instant::now();
        while !fake.paths().contains(&lever_path(0)) {
            assert!(began.elapsed() < Duration::from_secs(15));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        remove_source(&db, &b).await.unwrap();
    };
    let (report, ()) = tokio::join!(run, control);
    let report = report.unwrap();
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].added, 102);
}
