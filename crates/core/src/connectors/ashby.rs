//! Ashby's public Job Postings API, read under ruling R1: one JSON object, no paging.

use std::time::Duration;

use serde_json::Value;

use super::{
    Automation, BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus,
    Posting, RateLimit, Target, TargetKind, bad_feed, clean_slug, day, text,
};
use crate::error::Result;
use crate::jobs::{PayPeriod, PayRange};

pub struct Ashby;

static DECL: ConnectorDecl = ConnectorDecl {
    name: "ashby",
    label: "Ashby",
    operations: &[Operation::ListBoard, Operation::ImportPasted],
    automation: &[Automation::ReadPublic],
    rate_limit: Some(RateLimit {
        min_interval: Duration::from_secs(2),
        per_day: 300,
    }),
    policy: PolicyStatus::SilentRead,
    read_row: Some(("Ashby", "Read public listings through Job Postings API")),
    assisted_row: (
        "Ashby",
        "User applies on the hosted form in their own browser",
    ),
    hosts: &["api.ashbyhq.com"],
    attribution: None,
    target: TargetKind::Board,
};

/// The `Salary` row of `compensation.summaryComponents`. A bonus or equity row is not pay.
fn pay(row: &Value) -> Option<PayRange> {
    let salary = row
        .get("compensation")?
        .get("summaryComponents")?
        .as_array()?
        .iter()
        .find(|part| text(part, "compensationType") == "Salary")?;
    let whole = |key: &str| {
        let n = salary.get(key)?;
        n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))
    };
    let (min, max) = (whole("minValue"), whole("maxValue"));
    if min.is_none() && max.is_none() {
        return None;
    }
    // ponytail: only `1 YEAR` is read as a period. Any other interval leaves it unknown.
    let period = (text(salary, "interval") == "1 YEAR").then_some(PayPeriod::Year);
    Some(PayRange {
        min,
        max,
        currency: Some(text(salary, "currencyCode")).filter(|c| !c.is_empty()),
        period,
    })
}

/// A two-letter code only: the API may send a name, and a name is not guessed into a code.
fn country(row: &Value) -> Option<String> {
    let code = text(row.get("address")?.get("postalAddress")?, "addressCountry");
    (code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase())).then_some(code)
}

impl Connector for Ashby {
    fn decl(&self) -> &'static ConnectorDecl {
        &DECL
    }

    fn link_hosts(&self) -> &'static [&'static str] {
        &["jobs.ashbyhq.com"]
    }

    fn request(&self, target: &Target, _: u32) -> Result<PageRequest> {
        let name = clean_slug(&target.name)?;
        Ok(PageRequest {
            host: "api.ashbyhq.com",
            path: format!("/posting-api/job-board/{name}?includeCompensation=true"),
            expects: BodyKind::Json,
        })
    }

    fn parse(&self, target: &Target, body: &[u8]) -> Result<Page> {
        let board = clean_slug(&target.name)?;
        let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(body) else {
            return Err(bad_feed());
        };
        let Some(Value::Array(rows)) = top.get("jobs") else {
            return Err(bad_feed());
        };
        let mut postings = Vec::new();
        let mut skipped = 0;
        let mut listed = 0;
        for row in rows {
            // An unlisted posting is not public. It is left out and not counted.
            if row.get("isListed") == Some(&Value::Bool(false)) {
                continue;
            }
            listed += 1;
            // The field table lists no id, so the posting's own link is its id.
            let (url, title) = (text(row, "jobUrl"), text(row, "title"));
            if url.is_empty() || title.is_empty() {
                skipped += 1;
                continue;
            }
            // `isRemote` is true for a hybrid role too, so only `workplaceType` is read.
            let work_mode = match text(row, "workplaceType").as_str() {
                "Remote" => Some("remote"),
                "Hybrid" => Some("hybrid"),
                "OnSite" => Some("onsite"),
                _ => None,
            };
            let tags = ["team", "department"]
                .iter()
                .map(|key| text(row, key))
                .filter(|tag| !tag.is_empty())
                .collect();
            postings.push(Posting {
                external_id: url.clone(),
                title,
                company: board.clone(),
                location: text(row, "location"),
                country: country(row),
                work_mode,
                employment_type: Some(text(row, "employmentType")).filter(|t| !t.is_empty()),
                url,
                apply_url: Some(text(row, "applyUrl")).filter(|u| !u.is_empty()),
                posted_on: day(&text(row, "publishedAt")),
                pay: pay(row),
                tags,
                raw: text(row, "descriptionHtml"),
            });
        }
        if listed > 0 && postings.is_empty() {
            return Err(bad_feed());
        }
        Ok(Page {
            postings,
            next: None,
            skipped,
        })
    }
}
