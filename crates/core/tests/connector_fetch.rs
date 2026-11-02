//! The one fetch path: gate, pause, stored limits, and refusals. Every request goes to
//! `tests/fake_http` on 127.0.0.1; nothing here reaches a real host.

mod fake_http;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use fake_http::{Canned, Fake};
use jobpilot_core::ai::CancellationToken;
use jobpilot_core::connectors::fetch::{Fetcher, MAX_FEED_BYTES};
use jobpilot_core::connectors::limit::{self, MAX_WAIT, Slot};
use jobpilot_core::connectors::{
    Automation, BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus,
    RateLimit, Target, TargetKind,
};
use jobpilot_core::{AppPaths, Db, Error, Result, pause};
use serde_json::json;
use sqlx::Row;
use tempfile::TempDir;

const LEVER_READ: (&str, &str) = ("Lever", "Read public listings through Postings API");
const LEVER_HAND: (&str, &str) = (
    "Lever",
    "User applies on the hosted form in their own browser",
);

const fn decl(
    name: &'static str,
    interval: Duration,
    row: (&'static str, &'static str),
) -> ConnectorDecl {
    ConnectorDecl {
        name,
        label: "Test",
        operations: &[Operation::ListFeed],
        automation: &[Automation::ReadPublic],
        rate_limit: Some(RateLimit {
            min_interval: interval,
            per_day: 1000,
        }),
        policy: PolicyStatus::Allowed,
        read_row: Some(row),
        assisted_row: LEVER_HAND,
        hosts: &["lever.test"],
        attribution: None,
        target: TargetKind::Feed,
    }
}

static FAST: ConnectorDecl = decl("fast", Duration::from_millis(20), LEVER_READ);
static SLOW: ConnectorDecl = decl("slow", Duration::from_millis(400), LEVER_READ);
static HOURLY: ConnectorDecl = decl("hourly", Duration::from_secs(3600), LEVER_READ);
static NO_ROW: ConnectorDecl = decl("norow", Duration::from_millis(20), ("Nobody", "Nothing"));
static CAPPED: ConnectorDecl = ConnectorDecl {
    rate_limit: Some(RateLimit {
        min_interval: Duration::from_secs(1),
        per_day: 2,
    }),
    ..decl("capped", Duration::from_secs(1), LEVER_READ)
};

struct Test {
    decl: &'static ConnectorDecl,
    host: &'static str,
    kind: BodyKind,
    /// A path that replaces `/feed?page=N`, for a request that tries to leave its host.
    path: Option<&'static str>,
}

fn conn(decl: &'static ConnectorDecl) -> Test {
    Test {
        decl,
        host: "lever.test",
        kind: BodyKind::Json,
        path: None,
    }
}

impl Connector for Test {
    fn decl(&self) -> &'static ConnectorDecl {
        self.decl
    }
    fn request(&self, _: &Target, page: u32) -> Result<PageRequest> {
        Ok(PageRequest {
            host: self.host,
            path: self
                .path
                .map_or_else(|| format!("/feed?page={page}"), str::to_string),
            expects: self.kind,
        })
    }
    fn parse(&self, _: &Target, _: &[u8]) -> Result<Page> {
        Ok(Page {
            postings: Vec::new(),
            next: None,
            skipped: 0,
        })
    }
}

fn target() -> Target {
    Target {
        name: "acme".into(),
    }
}

fn reply(status: u16, content_type: &'static str, body: &str) -> Canned {
    Canned {
        status,
        headers: vec![("Content-Type", content_type.into())],
        body: body.into(),
        hold: Duration::ZERO,
        announced: None,
    }
}

fn ok() -> Canned {
    Canned::json(200, &json!([]))
}

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

/// The path is `/feed?page=0`, which the fake logs whole.
const FEED: &str = "/feed?page=0";

async fn go(fetcher: &Fetcher, db: &Db, c: &Test) -> Result<Page> {
    let token = CancellationToken::new();
    fetcher
        .page(db, c, &target(), 0, Operation::ListFeed, &token)
        .await
}

fn reason(result: Result<Page>) -> String {
    match result.unwrap_err() {
        Error::Refused(why) => why,
        other => other.code().to_string(),
    }
}

async fn row(db: &Db, name: &str) -> (i64, i64, i64) {
    let r = sqlx::query(
        "SELECT last_at_ms, not_before_ms, day_count FROM connector_limits WHERE connector = ?",
    )
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (r.get(0), r.get(1), r.get(2))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn a_page_arrives_with_the_honest_user_agent_and_no_cookie() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    go(&fetcher, &db, &conn(&FAST)).await.unwrap();
    let headers = fake.headers(FEED).join("\n");
    assert!(headers.contains("user-agent: jobpilot/"), "{headers}");
    assert!(headers.contains("accept: application/json"), "{headers}");
    assert!(!headers.contains("cookie") && !headers.contains("mozilla"));
    assert!(!headers.contains("authorization"));
}

#[tokio::test]
async fn an_unknown_row_sends_nothing() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&NO_ROW)).await),
        "policy_unknown"
    );
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn the_pause_sends_nothing() {
    let (_t, db) = open().await;
    pause::set(&db, "cli").await.unwrap();
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(reason(go(&fetcher, &db, &conn(&FAST)).await), "paused");
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn a_host_outside_the_declaration_sends_nothing() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let evil = Test {
        host: "evil.test",
        ..conn(&FAST)
    };
    assert_eq!(reason(go(&fetcher, &db, &evil).await), "validation");
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn a_challenge_page_stops_the_source() {
    let (_t, db) = open().await;
    let html = include_str!("fixtures/connectors/challenge.html");
    let fake = Fake::start(vec![(FEED, reply(200, "text/html", html))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "source_blocked"
    );
    assert_eq!(fake.paths().len(), 1);
    let again = go(&fetcher, &db, &conn(&FAST)).await.unwrap_err();
    assert!(
        matches!(again, Error::RateLimited { retry_after: Some(d) } if d > Duration::from_secs(23 * 3600))
    );
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn a_json_type_holding_html_is_refused() {
    let (_t, db) = open().await;
    let body = "  <!DOCTYPE html><html></html>";
    let fake = Fake::start(vec![(FEED, reply(200, "application/json", body))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "source_blocked"
    );
}

#[tokio::test]
async fn a_cf_mitigated_header_is_refused_whatever_the_body() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, ok().with_header("cf-mitigated", "challenge"))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "source_blocked"
    );
}

#[tokio::test]
async fn a_wrong_content_type_is_a_bad_feed() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, reply(200, "text/plain", "[]"))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(reason(go(&fetcher, &db, &conn(&FAST)).await), "bad_feed");
}

#[tokio::test]
async fn an_rss_body_needs_an_xml_type() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, reply(200, "application/rss+xml", "<rss/>"))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let rss = Test {
        kind: BodyKind::Rss,
        ..conn(&FAST)
    };
    go(&fetcher, &db, &rss).await.unwrap();
    assert!(
        fake.headers(FEED)
            .join("\n")
            .contains("accept: application/rss+xml")
    );
}

#[tokio::test]
async fn a_403_is_not_retried_or_rerouted() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, reply(403, "text/plain", "no"))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "source_blocked"
    );
    assert_eq!(fake.paths().len(), 1);
    let (_, not_before, _) = row(&db, "fast").await;
    assert!(not_before > now_ms() + 23 * 3600 * 1000);
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (_t, db) = open().await;
    let elsewhere = Fake::start(vec![("/feed?page=0", ok())]);
    let to = format!("{}{FEED}", elsewhere.url);
    let fake = Fake::start(vec![(
        FEED,
        reply(302, "text/plain", "").with_header("Location", &to),
    )]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "source_blocked"
    );
    assert_eq!(fake.paths().len(), 1);
    assert!(elsewhere.paths().is_empty());
}

#[tokio::test]
async fn a_404_says_the_source_is_not_found() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, reply(404, "text/plain", "gone"))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "source_not_found"
    );
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn a_429_with_a_short_retry_after_waits_then_tries_once_more() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![
        (
            FEED,
            reply(429, "text/plain", "slow").with_header("Retry-After", "1"),
        ),
        (FEED, ok()),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let began = Instant::now();
    go(&fetcher, &db, &conn(&FAST)).await.unwrap();
    assert_eq!(fake.paths().len(), 2);
    assert!(began.elapsed() >= Duration::from_millis(900));
}

#[tokio::test]
async fn a_429_with_a_long_retry_after_stops_and_remembers() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(
        FEED,
        reply(429, "text/plain", "").with_header("Retry-After", "3600"),
    )]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let err = go(&fetcher, &db, &conn(&FAST)).await.unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { retry_after: Some(d) } if d >= Duration::from_secs(3590))
    );
    assert_eq!(fake.paths().len(), 1);
    let fresh = Fetcher::for_test(&fake.url).unwrap();
    assert!(matches!(
        go(&fresh, &db, &conn(&FAST)).await.unwrap_err(),
        Error::RateLimited { .. }
    ));
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn a_429_with_no_header_holds_off_an_hour() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, reply(429, "text/plain", ""))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(
        reason(go(&fetcher, &db, &conn(&FAST)).await),
        "rate_limited"
    );
    let (_, not_before, _) = row(&db, "fast").await;
    let ahead = not_before - now_ms();
    assert!((3_500_000..=3_600_000).contains(&ahead), "{ahead}");
}

#[tokio::test]
async fn a_500_gets_one_more_attempt_and_no_third() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, reply(500, "text/plain", "oops"))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(reason(go(&fetcher, &db, &conn(&FAST)).await), "network");
    assert_eq!(fake.paths().len(), 2);
}

#[tokio::test]
async fn a_closed_port_reads_network_and_sends_no_third_attempt() {
    let (_t, db) = open().await;
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let fetcher = Fetcher::for_test(&closed).unwrap();
    assert_eq!(reason(go(&fetcher, &db, &conn(&FAST)).await), "network");
    // One slot per attempt, so exactly two were taken.
    assert_eq!(row(&db, "fast").await.2, 2);
}

#[tokio::test]
async fn a_body_over_the_cap_stores_nothing() {
    let (_t, db) = open().await;
    let mut big = ok();
    big.announced = Some(MAX_FEED_BYTES as usize + 1);
    let fake = Fake::start(vec![(FEED, big)]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    assert_eq!(reason(go(&fetcher, &db, &conn(&FAST)).await), "too_large");
}

#[tokio::test]
async fn cancel_ends_a_request_in_flight() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, Canned::stalled(Duration::from_secs(5)))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let token = CancellationToken::new();
    let stop = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
    });
    let began = Instant::now();
    let c = conn(&FAST);
    let result = fetcher
        .page(&db, &c, &target(), 0, Operation::ListFeed, &token)
        .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(began.elapsed() < Duration::from_secs(1));
}

/// A real-looking clock: a fresh row has `last_at_ms` 0.
const T0: i64 = 1_700_000_000_000;

#[tokio::test]
async fn a_too_soon_second_press_sends_nothing() {
    let (_t, db) = open().await;
    assert!(matches!(
        limit::take_at(&db, &HOURLY, "lever.test", T0)
            .await
            .unwrap(),
        Slot::Go
    ));
    let Slot::Wait(wait) = limit::take_at(&db, &HOURLY, "lever.test", T0 + 5_000)
        .await
        .unwrap()
    else {
        panic!("expected a wait");
    };
    assert_eq!(wait, Duration::from_secs(3595));
    // A refused press does not move the clock on.
    let Slot::Wait(again) = limit::take_at(&db, &HOURLY, "lever.test", T0 + 5_000)
        .await
        .unwrap()
    else {
        panic!("expected a wait");
    };
    assert_eq!(again, wait);
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    go(&fetcher, &db, &conn(&HOURLY)).await.unwrap();
    let err = go(&fetcher, &db, &conn(&HOURLY)).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { retry_after: Some(d) } if d > MAX_WAIT));
    assert_eq!(fake.paths().len(), 1);
}

#[tokio::test]
async fn a_short_wait_is_slept_and_a_long_one_is_refused() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    go(&fetcher, &db, &conn(&SLOW)).await.unwrap();
    let began = Instant::now();
    go(&fetcher, &db, &conn(&SLOW)).await.unwrap();
    assert!(began.elapsed() >= Duration::from_millis(300));
    assert_eq!(fake.paths().len(), 2);
    go(&fetcher, &db, &conn(&HOURLY)).await.unwrap();
    assert!(matches!(
        go(&fetcher, &db, &conn(&HOURLY)).await,
        Err(Error::RateLimited { .. })
    ));
}

#[tokio::test]
async fn the_day_cap_refuses_and_a_new_utc_day_opens() {
    let (_t, db) = open().await;
    let day = 86_400_000_i64;
    let noon = 20_000 * day + 12 * 3_600_000;
    assert!(matches!(
        limit::take_at(&db, &CAPPED, "h", noon).await.unwrap(),
        Slot::Go
    ));
    assert!(matches!(
        limit::take_at(&db, &CAPPED, "h", noon + 2_000)
            .await
            .unwrap(),
        Slot::Go
    ));
    // The cap is met: the wait runs to the next UTC midnight.
    let Slot::Wait(wait) = limit::take_at(&db, &CAPPED, "h", noon + 4_000)
        .await
        .unwrap()
    else {
        panic!("the cap should refuse");
    };
    assert_eq!(wait, Duration::from_millis((12 * 3_600_000 - 4_000) as u64));
    assert!(matches!(
        limit::take_at(&db, &CAPPED, "h", 21_000 * day)
            .await
            .unwrap(),
        Slot::Go
    ));
}

#[tokio::test]
async fn two_handles_cannot_share_a_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let a = Db::open(&paths).await.unwrap();
    let b = Db::open(&paths).await.unwrap();
    for _ in 0..5 {
        let (x, y) = tokio::join!(limit::take(&a, &HOURLY, "h"), limit::take(&b, &HOURLY, "h"));
        let goes = [x.unwrap(), y.unwrap()]
            .iter()
            .filter(|s| matches!(s, Slot::Go))
            .count();
        assert_eq!(goes, 1);
        sqlx::query("DELETE FROM connector_limits")
            .execute(&a.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_reopen_keeps_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    assert!(matches!(
        limit::take(&db, &HOURLY, "h").await.unwrap(),
        Slot::Go
    ));
    db.close().await;
    let db = Db::open(&paths).await.unwrap();
    assert!(matches!(
        limit::take(&db, &HOURLY, "h").await.unwrap(),
        Slot::Wait(_)
    ));
}

#[tokio::test]
async fn a_path_cannot_move_the_request_to_another_host() {
    let (_t, db) = open().await;
    let hosted = Fetcher::hosted().unwrap();
    for path in ["@evil.test/x", ".evil.test/x", ":80@evil.test/x"] {
        let sneaky = Test {
            path: Some(path),
            ..conn(&FAST)
        };
        assert_eq!(
            reason(go(&hosted, &db, &sneaky).await),
            "validation",
            "{path}"
        );
    }
    let fake = Fake::start(vec![(FEED, ok())]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let sneaky = Test {
        path: Some("//other.test/x"),
        ..conn(&FAST)
    };
    assert_eq!(reason(go(&fetcher, &db, &sneaky).await), "validation");
    assert!(fake.paths().is_empty());
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM connector_limits")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a refused URL takes no slot");
}

#[tokio::test]
async fn a_huge_retry_after_holds_a_day_at_most() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(
        FEED,
        reply(429, "text/plain", "").with_header("Retry-After", "99999999999999"),
    )]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let err = go(&fetcher, &db, &conn(&FAST)).await.unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(86_400))
    );
    let (_, not_before, _) = row(&db, "fast").await;
    assert!(
        not_before - now_ms() <= 86_400_000,
        "{}",
        not_before - now_ms()
    );
}

#[tokio::test]
async fn a_short_retry_after_is_stored_before_the_sleep() {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![
        (
            FEED,
            reply(429, "text/plain", "").with_header("Retry-After", "1"),
        ),
        (FEED, ok()),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let began = now_ms();
    go(&fetcher, &db, &conn(&FAST)).await.unwrap();
    let (_, not_before, _) = row(&db, "fast").await;
    assert!(
        not_before >= began + 900,
        "the hold was stored: {}",
        not_before - began
    );
}

// A parsed page is vetted once, in the fetcher, whatever connector produced it.

const LEVER_PATH: &str = "/v0/postings/acme?mode=json&skip=0&limit=100";

fn lever_row(id: &str, title: &str, url: &str) -> serde_json::Value {
    json!({"id": id, "text": title, "hostedUrl": url, "categories": {"location": "Austin"}})
}

async fn lever_page(rows: serde_json::Value) -> Page {
    let (_t, db) = open().await;
    let fake = Fake::start(vec![(LEVER_PATH, Canned::json(200, &rows))]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let lever = jobpilot_core::connectors::find("lever").unwrap();
    fetcher
        .page(
            &db,
            &*lever,
            &target(),
            0,
            Operation::ListBoard,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_link_off_https_or_off_the_declared_hosts_skips_the_row() {
    let good = "https://jobs.lever.co/acme/1";
    let mut with_bad_apply = lever_row("9", "T", good);
    with_bad_apply["applyUrl"] = json!("javascript:alert(1)");
    let page = lever_page(json!([
        lever_row("1", "Ok", good),
        lever_row("2", "T", "javascript:alert(1)"),
        lever_row("3", "T", "file:///etc/passwd"),
        lever_row("4", "T", "https://evil.test/acme/4"),
        lever_row("5", "T", "http://jobs.lever.co/acme/5"),
        lever_row("6", "T", "https://jobs.lever.co@evil.test/acme/6"),
        lever_row("7", "T", "https://jobs.lever.co:8443/acme/7"),
        lever_row("8", "T", "https://jobs.eu.lever.co/acme/8"),
        with_bad_apply,
    ]))
    .await;
    let ids: Vec<_> = page
        .postings
        .iter()
        .map(|p| p.external_id.as_str())
        .collect();
    assert_eq!(ids, ["1", "8"]);
    assert_eq!(page.skipped, 7);
}

#[tokio::test]
async fn a_repeated_id_keeps_the_first_and_counts_the_rest() {
    let url = "https://jobs.lever.co/acme/1";
    let page = lever_page(json!([
        lever_row("1", "First", url),
        lever_row("1", "Second", url),
        lever_row("2", "Other", url),
        lever_row("1", "Third", url),
    ]))
    .await;
    let titles: Vec<_> = page.postings.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["First", "Other"]);
    assert_eq!(page.skipped, 2);
}

#[tokio::test]
async fn negative_or_empty_pay_is_unknown() {
    let url = "https://jobs.lever.co/acme/1";
    let pay = |min: i64, max: i64| {
        let mut row = lever_row("1", "T", url);
        row["salaryRange"] =
            json!({"currency": "USD", "interval": "per-year-salary", "min": min, "max": max});
        row
    };
    for (min, max) in [(-5, 10), (5, -10), (0, 0), (-1, -1)] {
        let page = lever_page(json!([pay(min, max)])).await;
        assert!(page.postings[0].pay.is_none(), "{min} {max}");
    }
    let page = lever_page(json!([pay(0, 90000)])).await;
    assert_eq!(page.postings[0].pay.as_ref().unwrap().max, Some(90000));
}

#[tokio::test]
async fn long_fields_are_cut_and_a_long_title_skips_the_row() {
    let url = "https://jobs.lever.co/acme/1";
    let mut long = lever_row("1", "T", url);
    long["categories"]["location"] = json!("é".repeat(250));
    long["description"] = json!("é".repeat(150_000));
    let exact = lever_row("2", &"t".repeat(300), url);
    let over = lever_row("3", &"t".repeat(301), url);
    let page = lever_page(json!([long, exact, over])).await;
    assert_eq!(page.postings.len(), 2);
    assert_eq!(page.skipped, 1);
    assert_eq!(page.postings[0].location.chars().count(), 200);
    assert!(page.postings[0].raw.len() <= 200 * 1024);
    assert!(
        page.postings[0].raw.len() > 200 * 1024 - 4,
        "cut near the cap"
    );
    assert_eq!(page.postings[1].title.chars().count(), 300);
}

const GREENHOUSE: &str = include_str!("fixtures/connectors/greenhouse.json");
const ASHBY: &str = include_str!("fixtures/connectors/ashby.json");

#[tokio::test]
async fn a_board_list_parses_from_the_doc_sample_through_the_fetcher() {
    let (_t, db) = open().await;
    let gh = "/v1/boards/acme/jobs?content=true&pay_transparency=true";
    let ab = "/posting-api/job-board/acme?includeCompensation=true";
    let body = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
    let fake = Fake::start(vec![
        (gh, Canned::json(200, &body(GREENHOUSE))),
        (ab, Canned::json(200, &body(ASHBY))),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let token = CancellationToken::new();
    for (name, path, rows, skipped) in [("greenhouse", gh, 2, 1), ("ashby", ab, 3, 1)] {
        let c = jobpilot_core::connectors::find(name).unwrap();
        let page = fetcher
            .page(&db, &*c, &target(), 0, Operation::ListBoard, &token)
            .await
            .unwrap();
        assert_eq!(
            (page.postings.len(), page.skipped),
            (rows, skipped),
            "{name}"
        );
        assert_eq!(fake.hits(path).len(), 1, "{name}: one request");
    }
}

#[tokio::test]
async fn a_greenhouse_link_needs_https_only_and_an_ashby_link_needs_its_host() {
    let (_t, db) = open().await;
    let gh = "/v1/boards/acme/jobs?content=true&pay_transparency=true";
    let ab = "/posting-api/job-board/acme?includeCompensation=true";
    let gh_rows = json!({"jobs": [
        {"id": 1, "title": "A", "absolute_url": "https://careers.example.com/a"},
        {"id": 2, "title": "B", "absolute_url": "http://careers.example.com/b"},
        {"id": 3, "title": "C", "absolute_url": "javascript:alert(1)"},
    ]});
    let ab_rows = json!({"jobs": [
        {"title": "A", "jobUrl": "https://jobs.ashbyhq.com/acme/1"},
        {"title": "B", "jobUrl": "https://evil.test/acme/2"},
    ]});
    let fake = Fake::start(vec![
        (gh, Canned::json(200, &gh_rows)),
        (ab, Canned::json(200, &ab_rows)),
    ]);
    let fetcher = Fetcher::for_test(&fake.url).unwrap();
    let token = CancellationToken::new();
    for (name, kept, skipped) in [("greenhouse", 1, 2), ("ashby", 1, 1)] {
        let c = jobpilot_core::connectors::find(name).unwrap();
        let page = fetcher
            .page(&db, &*c, &target(), 0, Operation::ListBoard, &token)
            .await
            .unwrap();
        assert_eq!(
            (page.postings.len(), page.skipped),
            (kept, skipped),
            "{name}"
        );
    }
}
