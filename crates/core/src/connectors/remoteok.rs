//! RemoteOK's JSON API: one array of about a hundred rows, row 0 being a legal notice.

use std::time::Duration;

use serde_json::Value;

use super::{
    Automation, BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus,
    Posting, RateLimit, Target, TargetKind, bad_feed, day,
};
use crate::error::Result;
use crate::jobs::{PayPeriod, PayRange};
use crate::knowledge::decode_entities;

pub struct RemoteOk;

static DECL: ConnectorDecl = ConnectorDecl {
    name: "remoteok",
    label: "RemoteOK",
    operations: &[Operation::ListFeed, Operation::ImportPasted],
    automation: &[Automation::ReadPublic],
    rate_limit: Some(RateLimit {
        min_interval: Duration::from_secs(900),
        per_day: 24,
    }),
    policy: PolicyStatus::Allowed,
    read_row: Some(("RemoteOK", "Read listings through the JSON API")),
    assisted_row: ("RemoteOK", "Apply"),
    hosts: &["remoteok.com"],
    attribution: Some("Remote OK"),
    target: TargetKind::None,
};

fn text(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The feed sends `remoteOK.com`. A link to any other host is dropped, not followed.
fn own_link(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    (host == "remoteok.com" || host == "www.remoteok.com")
        .then(|| format!("https://remoteok.com/{path}"))
}

fn id(row: &Value) -> String {
    match row.get("id") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn pay(row: &Value) -> Option<PayRange> {
    let whole = |key: &str| row.get(key).and_then(Value::as_i64).filter(|n| *n > 0);
    let (min, max) = (whole("salary_min"), whole("salary_max"));
    if min.is_none() && max.is_none() {
        return None;
    }
    // ponytail: the feed names no currency and no period. The currency stays unknown and is
    // never compared; the period is a yearly figure, the way the board lists salaries.
    Some(PayRange {
        min,
        max,
        currency: None,
        period: PayPeriod::Year,
    })
}

impl Connector for RemoteOk {
    fn decl(&self) -> &'static ConnectorDecl {
        &DECL
    }

    fn request(&self, _: &Target, _: u32) -> Result<PageRequest> {
        Ok(PageRequest {
            host: "remoteok.com",
            path: "/api".into(),
            expects: BodyKind::Json,
        })
    }

    fn parse(&self, _: &Target, body: &[u8]) -> Result<Page> {
        let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(body) else {
            return Err(bad_feed());
        };
        let mut postings = Vec::new();
        let mut skipped = 0;
        for (at, row) in rows.iter().enumerate() {
            let (id, title) = (id(row), decode_entities(&text(row, "position")));
            let url = own_link(&text(row, "url"));
            let (Some(url), false, false) = (url, id.is_empty(), title.is_empty()) else {
                // Row 0 is the legal notice, not a job.
                if !(at == 0 && row.get("legal").is_some()) {
                    skipped += 1;
                }
                continue;
            };
            let tags = row
                .get("tags")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let posted = row
                .get("epoch")
                .map(|v| v.to_string())
                .and_then(|v| day(&v))
                .or_else(|| day(&text(row, "date")));
            postings.push(Posting {
                external_id: id,
                title,
                company: decode_entities(&text(row, "company")),
                location: decode_entities(&text(row, "location")),
                country: None,
                work_mode: Some("remote"),
                employment_type: None,
                url,
                apply_url: own_link(&text(row, "apply_url")),
                posted_on: posted,
                pay: pay(row),
                tags,
                raw: text(row, "description"),
            });
        }
        if postings.is_empty() && (skipped > 0 || rows.is_empty()) {
            return Err(bad_feed());
        }
        Ok(Page {
            postings,
            next: None,
            skipped,
        })
    }
}
