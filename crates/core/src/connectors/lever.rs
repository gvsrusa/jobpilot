//! Lever's public Postings API: one JSON array per page, a hundred rows at most.

use std::time::Duration;

use serde_json::Value;

use super::{
    Automation, BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus,
    Posting, RateLimit, Target, TargetKind, bad_feed, clean_slug, day,
};
use crate::error::Result;
use crate::jobs::{PayPeriod, PayRange};

const PAGE_ROWS: usize = 100;
/// ponytail: five pages, so 500 postings, is the ceiling for one Lever board.
const LAST_PAGE: u32 = 4;

pub struct Lever;

static DECL: ConnectorDecl = ConnectorDecl {
    name: "lever",
    label: "Lever",
    operations: &[Operation::ListBoard, Operation::ImportPasted],
    automation: &[Automation::ReadPublic],
    rate_limit: Some(RateLimit {
        min_interval: Duration::from_secs(2),
        per_day: 300,
    }),
    policy: PolicyStatus::Allowed,
    read_row: Some(("Lever", "Read public listings through Postings API")),
    assisted_row: (
        "Lever",
        "User applies on the hosted form in their own browser",
    ),
    hosts: &["api.lever.co", "api.eu.lever.co"],
    attribution: None,
    target: TargetKind::Board,
};

/// `acme` or `eu:acme`.
fn site(target: &Target) -> Result<(bool, String)> {
    match target.name.strip_prefix("eu:") {
        Some(name) => Ok((true, clean_slug(name)?)),
        None => Ok((false, clean_slug(&target.name)?)),
    }
}

fn text(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn pay(row: &Value) -> Option<PayRange> {
    let range = row.get("salaryRange")?;
    let interval = range.get("interval")?.as_str()?;
    let period = ["year", "month", "week", "day", "hour"]
        .iter()
        .zip([
            PayPeriod::Year,
            PayPeriod::Month,
            PayPeriod::Week,
            PayPeriod::Day,
            PayPeriod::Hour,
        ])
        .find(|(word, _)| interval.contains(**word))?
        .1;
    let whole = |key: &str| {
        let n = range.get(key)?;
        n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))
    };
    let (min, max) = (whole("min"), whole("max"));
    if min.is_none() && max.is_none() {
        return None;
    }
    let currency = range
        .get("currency")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Some(PayRange {
        min,
        max,
        currency,
        period,
    })
}

fn raw(row: &Value) -> String {
    let mut parts = vec![text(row, "description")];
    for list in row
        .get("lists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        parts.push(format!(
            "<h3>{}</h3>{}",
            text(list, "text"),
            text(list, "content")
        ));
    }
    parts.push(text(row, "additional"));
    parts.retain(|part| !part.is_empty());
    parts.join("\n")
}

impl Connector for Lever {
    fn decl(&self) -> &'static ConnectorDecl {
        &DECL
    }

    fn link_hosts(&self) -> &'static [&'static str] {
        &["jobs.lever.co", "jobs.eu.lever.co"]
    }

    fn request(&self, target: &Target, page: u32) -> Result<PageRequest> {
        let (eu, name) = site(target)?;
        Ok(PageRequest {
            host: if eu {
                "api.eu.lever.co"
            } else {
                "api.lever.co"
            },
            path: format!(
                "/v0/postings/{name}?mode=json&skip={}&limit={PAGE_ROWS}",
                page as usize * PAGE_ROWS
            ),
            expects: BodyKind::Json,
        })
    }

    fn parse(&self, target: &Target, body: &[u8]) -> Result<Page> {
        self.parse_page(target, body, 0)
    }

    fn parse_page(&self, target: &Target, body: &[u8], page: u32) -> Result<Page> {
        let (_, company) = site(target)?;
        let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(body) else {
            return Err(bad_feed());
        };
        let mut postings = Vec::new();
        let mut skipped = 0;
        for row in &rows {
            let (id, title, url) = (text(row, "id"), text(row, "text"), text(row, "hostedUrl"));
            if id.is_empty() || title.is_empty() || url.is_empty() {
                skipped += 1;
                continue;
            }
            let categories = row.get("categories").cloned().unwrap_or(Value::Null);
            let work_mode = match text(row, "workplaceType").as_str() {
                "remote" => Some("remote"),
                "hybrid" => Some("hybrid"),
                "on-site" => Some("onsite"),
                _ => None,
            };
            let tags = ["team", "department"]
                .iter()
                .map(|key| text(&categories, key))
                .filter(|tag| !tag.is_empty())
                .collect();
            postings.push(Posting {
                external_id: id,
                title,
                company: company.clone(),
                location: text(&categories, "location"),
                country: Some(text(row, "country")).filter(|c| !c.is_empty()),
                work_mode,
                employment_type: Some(text(&categories, "commitment")).filter(|c| !c.is_empty()),
                url,
                apply_url: Some(text(row, "applyUrl")).filter(|u| !u.is_empty()),
                posted_on: row.get("createdAt").and_then(|at| day(&at.to_string())),
                pay: pay(row),
                tags,
                raw: raw(row),
            });
        }
        // A non-empty reply with no usable row is a changed shape, not an empty board.
        if !rows.is_empty() && postings.is_empty() {
            return Err(bad_feed());
        }
        let next = (rows.len() == PAGE_ROWS && page < LAST_PAGE).then_some(page + 1);
        Ok(Page {
            postings,
            next,
            skipped,
        })
    }
}
