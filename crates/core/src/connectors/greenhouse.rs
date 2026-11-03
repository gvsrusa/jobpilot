//! Greenhouse's public Job Board API, read under ruling R1: one JSON object, no paging.

use std::time::Duration;

use serde_json::Value;

use super::{
    Automation, BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus,
    Posting, RateLimit, Target, TargetKind, bad_feed, clean_slug, day, text,
};
use crate::error::Result;
use crate::jobs::PayRange;
use crate::knowledge::decode_entities;

pub struct Greenhouse;

static DECL: ConnectorDecl = ConnectorDecl {
    name: "greenhouse",
    label: "Greenhouse",
    operations: &[Operation::ListBoard, Operation::ImportPasted],
    automation: &[Automation::ReadPublic],
    rate_limit: Some(RateLimit {
        min_interval: Duration::from_secs(2),
        per_day: 300,
    }),
    policy: PolicyStatus::SilentRead,
    read_row: Some(("Greenhouse", "Read public listings through Job Board API")),
    assisted_row: (
        "Greenhouse",
        "User applies on the hosted form in their own browser",
    ),
    hosts: &["boards-api.greenhouse.io"],
    attribution: None,
    target: TargetKind::Board,
};

/// The docs sample shows `content` entity-encoded twice; a live board may send it once. It is
/// decoded once, and a second time only while it still holds no tag but does hold an encoded
/// one, so text that really says `&amp;` is never decoded past two passes.
fn body(content: &str) -> String {
    let once = decode_entities(content);
    if !once.contains('<') && (once.contains("&lt;") || once.contains("&gt;")) {
        decode_entities(&once)
    } else {
        once
    }
}

fn pay(row: &Value) -> Option<PayRange> {
    let range = row.get("pay_input_ranges")?.as_array()?.first()?;
    let dollars = |key: &str| range.get(key)?.as_i64().map(|cents| cents / 100);
    let (min, max) = (dollars("min_cents"), dollars("max_cents"));
    if min.is_none() && max.is_none() {
        return None;
    }
    Some(PayRange {
        min,
        max,
        currency: Some(text(range, "currency_type")).filter(|c| !c.is_empty()),
        // The API sends no period, so none is stored.
        period: None,
    })
}

impl Connector for Greenhouse {
    fn decl(&self) -> &'static ConnectorDecl {
        &DECL
    }

    fn request(&self, target: &Target, _: u32) -> Result<PageRequest> {
        let name = clean_slug(&target.name)?;
        Ok(PageRequest {
            host: "boards-api.greenhouse.io",
            path: format!("/v1/boards/{name}/jobs?content=true&pay_transparency=true"),
            expects: BodyKind::Json,
        })
    }

    fn parse(&self, target: &Target, body_bytes: &[u8]) -> Result<Page> {
        let board = clean_slug(&target.name)?;
        let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(body_bytes) else {
            return Err(bad_feed());
        };
        let Some(Value::Array(rows)) = top.get("jobs") else {
            return Err(bad_feed());
        };
        let mut postings = Vec::new();
        let mut skipped = 0;
        for row in rows {
            let id = match row.get("id") {
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::String(s)) => s.trim().to_string(),
                _ => String::new(),
            };
            let (title, url) = (
                decode_entities(&text(row, "title")),
                text(row, "absolute_url"),
            );
            if id.is_empty() || title.is_empty() || url.is_empty() {
                skipped += 1;
                continue;
            }
            let company = Some(decode_entities(&text(row, "company_name")))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| board.clone());
            postings.push(Posting {
                external_id: id,
                title,
                company,
                location: decode_entities(
                    row.get("location")
                        .map(|place| text(place, "name"))
                        .unwrap_or_default()
                        .as_str(),
                ),
                country: None,
                work_mode: None,
                employment_type: None,
                url,
                apply_url: None,
                posted_on: day(&text(row, "first_published")),
                pay: pay(row),
                tags: Vec::new(),
                raw: body(&text(row, "content")),
            });
        }
        // A non-empty reply with no usable row is a changed shape, not an empty board.
        if !rows.is_empty() && postings.is_empty() {
            return Err(bad_feed());
        }
        Ok(Page {
            postings,
            next: None,
            skipped,
        })
    }
}
