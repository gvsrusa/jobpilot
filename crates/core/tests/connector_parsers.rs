//! The Lever, RemoteOK, and We Work Remotely parsers, on synthetic recorded shapes. No socket.

use jobpilot_core::Error;
use jobpilot_core::connectors::{
    Ashby, Connector, Greenhouse, Lever, PolicyStatus, RemoteOk, Target, WeWorkRemotely, check,
    clean_slug, day, find, read_status,
};
use jobpilot_core::jobs::PayPeriod;
use serde_json::json;

const LEVER: &[u8] = include_bytes!("fixtures/connectors/lever.json");
const LEVER_EMPTY: &[u8] = include_bytes!("fixtures/connectors/lever_empty.json");
const LEVER_CHANGED: &[u8] = include_bytes!("fixtures/connectors/lever_changed.json");
const REMOTEOK: &[u8] = include_bytes!("fixtures/connectors/remoteok.json");
const GREENHOUSE: &[u8] = include_bytes!("fixtures/connectors/greenhouse.json");
const ASHBY: &[u8] = include_bytes!("fixtures/connectors/ashby.json");
const WWR: &[u8] = include_bytes!("fixtures/connectors/wwr.rss");

fn target(name: &str) -> Target {
    Target { name: name.into() }
}

fn refusal(result: jobpilot_core::Result<jobpilot_core::connectors::Page>) -> String {
    match result.unwrap_err() {
        Error::Refused(why) => why,
        other => other.code().to_string(),
    }
}

fn limit_of(name: &str) -> (u64, u32) {
    let limit = find(name).unwrap().decl().rate_limit.unwrap();
    (limit.min_interval.as_secs(), limit.per_day)
}

#[test]
fn lever_builds_only_the_documented_request() {
    let r = Lever.request(&target("eu:acme"), 2).unwrap();
    assert_eq!(r.host, "api.eu.lever.co");
    assert_eq!(r.path, "/v0/postings/acme?mode=json&skip=200&limit=100");
    let r = Lever.request(&target("acme"), 0).unwrap();
    assert_eq!(r.host, "api.lever.co");
}

#[test]
fn a_slug_cannot_leave_its_path() {
    for bad in ["../x", "a/b", "a?b=1", "", "a b", "%2e%2e", "eu:", "eu:a/b"] {
        assert_eq!(
            Lever.request(&target(bad), 0).unwrap_err().code(),
            "validation",
            "{bad}"
        );
    }
    assert!(clean_slug(&"a".repeat(100)).is_ok());
    assert!(clean_slug(&"a".repeat(101)).is_err());
    assert_eq!(clean_slug("a_b-C9").unwrap(), "a_b-C9");
}

#[test]
fn lever_maps_its_fields() {
    let page = Lever.parse(&target("acme"), LEVER).unwrap();
    assert_eq!(page.postings.len(), 5);
    let job = &page.postings[0];
    assert_eq!(job.company, "acme");
    assert_eq!(job.country.as_deref(), Some("US"));
    assert_eq!(job.work_mode, Some("hybrid"));
    assert_eq!(job.pay.as_ref().unwrap().max, Some(175000));
    assert_eq!(job.pay.as_ref().unwrap().currency.as_deref(), Some("USD"));
    assert_eq!(job.pay.as_ref().unwrap().period, Some(PayPeriod::Year));
    assert_eq!(job.posted_on.as_deref(), Some("2025-11-01"));
    assert!(
        job.raw.contains("<div"),
        "raw stays HTML so normalize can see hidden text"
    );
    assert!(job.raw.contains("display:none"));
    assert!(job.raw.contains("What you will do"));
    assert!(job.raw.contains("Sample closing paragraph"));
    let modes: Vec<_> = page.postings.iter().map(|p| p.work_mode).collect();
    assert_eq!(
        modes,
        [
            Some("hybrid"),
            Some("remote"),
            Some("onsite"),
            None,
            Some("remote")
        ]
    );
    assert!(page.postings[1].country.is_none());
}

#[test]
fn lever_stores_pay_only_for_a_known_interval() {
    let page = Lever.parse(&target("acme"), LEVER).unwrap();
    assert!(page.postings[1].pay.is_none(), "no salaryRange");
    assert_eq!(page.postings[2].pay.as_ref().unwrap().min, Some(3000));
    assert!(page.postings[3].pay.is_none(), "a bonus is not pay");
}

#[test]
fn lever_pages_stop_at_a_short_page_and_at_five() {
    let rows: Vec<_> = (0..100)
        .map(|i| json!({"id": format!("id{i}"), "text": "T", "hostedUrl": "https://jobs.lever.co/a/b"}))
        .collect();
    let full = serde_json::to_vec(&rows).unwrap();
    let t = target("acme");
    assert_eq!(Lever.parse_page(&t, &full, 0).unwrap().next, Some(1));
    assert_eq!(Lever.parse_page(&t, &full, 3).unwrap().next, Some(4));
    assert_eq!(Lever.parse_page(&t, &full, 4).unwrap().next, None);
    let short = serde_json::to_vec(&rows[..99]).unwrap();
    assert_eq!(Lever.parse_page(&t, &short, 0).unwrap().next, None);
    let empty = Lever.parse_page(&t, LEVER_EMPTY, 0).unwrap();
    assert!(empty.postings.is_empty() && empty.next.is_none());
}

#[test]
fn remoteok_skips_the_legal_row_and_stores_no_pay_at_zero() {
    let page = RemoteOk.parse(&target(""), REMOTEOK).unwrap();
    assert!(page.postings.iter().all(|p| !p.external_id.is_empty()));
    assert_eq!(page.postings.len(), 2);
    assert_eq!(
        page.skipped, 1,
        "the row with no id is counted, the legal row is not"
    );
    let unpaid = page
        .postings
        .iter()
        .find(|p| p.external_id == "1001")
        .unwrap();
    assert!(unpaid.pay.is_none());
    assert_eq!(unpaid.work_mode, Some("remote"));
    assert_eq!(unpaid.tags, ["rust", "backend"]);
    assert!(unpaid.raw.contains("display:none"));
    let paid = page
        .postings
        .iter()
        .find(|p| p.external_id == "1002")
        .unwrap();
    assert_eq!(paid.pay.as_ref().unwrap().currency, None);
    // The feed names no period, so none is stored: a figure is never read as yearly by guess.
    assert_eq!(paid.pay.as_ref().unwrap().period, None);
    assert_eq!(paid.pay.as_ref().unwrap().min, Some(90000));
}

#[test]
fn remoteok_carries_attribution_and_a_lower_case_link() {
    assert_eq!(RemoteOk.decl().attribution, Some("Remote OK"));
    let page = RemoteOk.parse(&target(""), REMOTEOK).unwrap();
    assert_eq!(
        page.postings[0].url,
        "https://remoteok.com/remote-jobs/remote-sample-engineer-1001"
    );
    let body = br#"[{"last_updated":1,"legal":"x"},{"id":"1","position":"P","url":"https://evil.example/x"}]"#;
    assert_eq!(refusal(RemoteOk.parse(&target(""), body)), "bad_feed");
}

#[test]
fn wwr_splits_company_and_role_and_keeps_the_html() {
    let page = WeWorkRemotely.parse(&target(""), WWR).unwrap();
    assert_eq!(page.postings.len(), 3);
    let first = &page.postings[0];
    assert_eq!(first.company, "ACME");
    assert_eq!(first.title, "Engineer | Remote");
    assert_eq!(first.location, "Anywhere in the World, Ontario, Canada");
    assert_eq!(first.country, None);
    assert_eq!(first.tags, ["Rust", "SQL"]);
    assert_eq!(
        first.external_id,
        "https://weworkremotely.com/remote-jobs/acme-engineer"
    );
    assert_eq!(first.posted_on.as_deref(), Some("2026-11-01"));
    assert_eq!(first.work_mode, Some("remote"));
    assert!(
        first
            .raw
            .starts_with("<p>Build sample things &amp; ship.</p>")
    );
    assert!(first.raw.contains("display:none"));
    let second = &page.postings[1];
    assert_eq!(
        (second.company.as_str(), second.title.as_str()),
        ("Example Ltd", "Support Lead")
    );
    assert_eq!(second.location, "Europe Only");
    assert!(second.tags.is_empty());
    let third = &page.postings[2];
    assert_eq!(third.company, "");
    assert_eq!(third.title, "Freelance Sample Writer");
}

#[test]
fn wwr_asks_only_for_its_known_feeds() {
    assert_eq!(
        WeWorkRemotely.request(&target(""), 0).unwrap().path,
        "/remote-jobs.rss"
    );
    assert_eq!(
        WeWorkRemotely
            .request(&target("remote-design-jobs"), 0)
            .unwrap()
            .path,
        "/categories/remote-design-jobs.rss"
    );
    assert_eq!(
        WeWorkRemotely
            .request(&target("../x"), 0)
            .unwrap_err()
            .code(),
        "validation"
    );
    assert_eq!(RemoteOk.request(&target(""), 0).unwrap().path, "/api");
}

#[test]
fn wwr_refuses_a_doctype() {
    let bomb = br#"<?xml version="1.0"?><!DOCTYPE r [<!ENTITY a "aaaa">]><rss><channel/></rss>"#;
    assert_eq!(
        WeWorkRemotely.parse(&target(""), bomb).unwrap_err().code(),
        "refused"
    );
    assert_eq!(refusal(WeWorkRemotely.parse(&target(""), bomb)), "bad_feed");
}

#[test]
fn a_changed_feed_stores_nothing() {
    let cases: [(Box<dyn Connector>, &[u8]); 3] = [
        (Box::new(Lever), LEVER_CHANGED),
        (Box::new(RemoteOk), b"{\"jobs\":[]}"),
        (Box::new(WeWorkRemotely), b"not xml"),
    ];
    for (conn, body) in cases {
        assert!(
            matches!(conn.parse(&target("acme"), body), Err(Error::Refused(r)) if r == "bad_feed"),
            "{}",
            conn.decl().name
        );
    }
    // Rows that all lack a field are a changed shape too.
    assert_eq!(
        refusal(Lever.parse(&target("acme"), b"[{\"x\":1}]")),
        "bad_feed"
    );
    let truncated = b"<rss><channel><item><title>A: B</title>";
    assert_eq!(
        refusal(WeWorkRemotely.parse(&target(""), truncated)),
        "bad_feed"
    );
}

#[test]
fn the_three_allowed_sources_pass_the_gate() {
    for name in ["lever", "remoteok", "wwr"] {
        let c = find(name).unwrap();
        assert_eq!(read_status(c.decl()), PolicyStatus::Allowed, "{name}");
        assert!(check(c.decl()).is_ok());
    }
}

#[test]
fn a_rate_limit_is_declared_for_each_as_pinned() {
    assert_eq!(limit_of("lever"), (2, 300));
    assert_eq!(limit_of("remoteok"), (900, 24));
    assert_eq!(limit_of("wwr"), (3600, 24));
}

#[test]
fn dates_become_days_or_nothing() {
    assert_eq!(
        day("2026-11-01T23:30:00-05:00").as_deref(),
        Some("2026-11-01")
    );
    assert_eq!(
        day("Sat, 01 Nov 2026 12:00:00 +0000").as_deref(),
        Some("2026-11-01")
    );
    assert_eq!(day("1761955200000").as_deref(), Some("2025-11-01"));
    assert_eq!(day("1761955200").as_deref(), Some("2025-11-01"));
    assert_eq!(day("2026-02-30"), None);
    assert_eq!(day("soon"), None);
    assert_eq!(day(""), None);
}

#[test]
fn greenhouse_builds_only_the_documented_request() {
    let r = Greenhouse.request(&target("acme"), 0).unwrap();
    assert_eq!(r.host, "boards-api.greenhouse.io");
    assert_eq!(
        r.path,
        "/v1/boards/acme/jobs?content=true&pay_transparency=true"
    );
    for bad in ["../x", "a/b", "", "a?b"] {
        assert_eq!(
            Greenhouse.request(&target(bad), 0).unwrap_err().code(),
            "validation"
        );
    }
}

#[test]
fn greenhouse_decodes_a_double_encoded_body_and_pay_in_cents() {
    let page = Greenhouse.parse(&target("acme"), GREENHOUSE).unwrap();
    assert_eq!(page.postings.len(), 2);
    assert_eq!(page.skipped, 1, "the row with no title");
    assert!(page.next.is_none());
    let job = &page.postings[0];
    assert_eq!(job.external_id, "4001");
    assert_eq!(job.company, "Acme Sample");
    assert_eq!(job.location, "Remote - US");
    assert_eq!(job.posted_on.as_deref(), Some("2026-10-01"));
    assert!(
        job.raw
            .starts_with("<p>Build sample things &amp; ship.</p>")
    );
    assert!(job.raw.contains("display:none"));
    let pay = job.pay.as_ref().unwrap();
    assert_eq!((pay.min, pay.max), (Some(50000), Some(70000)));
    assert_eq!(pay.currency.as_deref(), Some("USD"));
    assert_eq!(pay.period, None, "the API names no period");
    assert!(job.work_mode.is_none() && job.country.is_none());
}

#[test]
fn greenhouse_decodes_a_single_encoded_body_too() {
    let page = Greenhouse.parse(&target("acme"), GREENHOUSE).unwrap();
    let job = &page.postings[1];
    assert_eq!(job.raw, "<p>Single &amp; sample.</p>");
    assert!(job.pay.is_none());
    assert_eq!(job.url, "https://careers.example.com/jobs/4002?gh_jid=4002");
}

#[test]
fn greenhouse_with_a_changed_shape_stores_nothing() {
    for body in [
        &b"[]"[..],
        b"{\"jobs\":5}",
        b"{\"jobs\":[{\"x\":1}]}",
        b"nope",
    ] {
        assert_eq!(refusal(Greenhouse.parse(&target("acme"), body)), "bad_feed");
    }
    let empty = Greenhouse.parse(&target("acme"), b"{\"jobs\":[]}").unwrap();
    assert!(empty.postings.is_empty());
}

#[test]
fn ashby_builds_only_the_documented_request() {
    let r = Ashby.request(&target("acme"), 0).unwrap();
    assert_eq!(r.host, "api.ashbyhq.com");
    assert_eq!(
        r.path,
        "/posting-api/job-board/acme?includeCompensation=true"
    );
    assert_eq!(
        Ashby.request(&target("a/b"), 0).unwrap_err().code(),
        "validation"
    );
}

#[test]
fn ashby_uses_workplace_type_and_skips_unlisted() {
    let page = Ashby.parse(&target("acme"), ASHBY).unwrap();
    let ids: Vec<_> = page.postings.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(ids, ["Platform Engineer", "Office Manager", "Designer"]);
    assert_eq!(page.skipped, 1, "no link counts, an unlisted row does not");
    let modes: Vec<_> = page.postings.iter().map(|p| p.work_mode).collect();
    assert_eq!(modes, [Some("hybrid"), Some("onsite"), Some("remote")]);
    let job = &page.postings[0];
    assert_eq!(
        job.external_id,
        "https://jobs.ashbyhq.com/acme/00000000-0000-4000-8000-0000000000a1"
    );
    assert_eq!(job.company, "acme");
    assert_eq!(job.country.as_deref(), Some("US"));
    assert_eq!(page.postings[1].country, None, "a name is not a code");
    assert_eq!(job.employment_type.as_deref(), Some("FullTime"));
    assert_eq!(job.posted_on.as_deref(), Some("2026-10-01"));
    assert!(job.raw.contains("display:none"));
    assert_eq!(job.tags, ["Platform", "Engineering"]);
    assert!(job.apply_url.as_deref().unwrap().ends_with("/application"));
}

#[test]
fn ashby_pay_is_the_salary_row_by_year_only() {
    let page = Ashby.parse(&target("acme"), ASHBY).unwrap();
    let pay = page.postings[0].pay.as_ref().unwrap();
    assert_eq!((pay.min, pay.max), (Some(140000), Some(175000)));
    assert_eq!(pay.currency.as_deref(), Some("USD"));
    assert_eq!(pay.period, Some(PayPeriod::Year));
    assert!(page.postings[1].pay.is_none(), "equity is not pay");
    assert!(page.postings[2].pay.is_none());
}

#[test]
fn ashby_with_a_changed_shape_stores_nothing() {
    for body in [
        &b"[]"[..],
        b"{\"jobs\":{}}",
        b"{\"jobs\":[{\"x\":1}]}",
        b"nope",
    ] {
        assert_eq!(refusal(Ashby.parse(&target("acme"), body)), "bad_feed");
    }
}

#[test]
fn both_declare_the_pinned_limit() {
    assert_eq!(limit_of("greenhouse"), (2, 300));
    assert_eq!(limit_of("ashby"), (2, 300));
}

#[test]
fn the_two_r1_sources_pass_check_and_read_silent() {
    for name in ["greenhouse", "ashby"] {
        let c = find(name).unwrap();
        assert_eq!(read_status(c.decl()), PolicyStatus::SilentRead, "{name}");
        assert!(check(c.decl()).is_ok());
    }
}
