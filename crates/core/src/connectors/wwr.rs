//! We Work Remotely's public RSS. quick-xml expands no custom entity, and a DOCTYPE, the only
//! place one could be declared, is refused outright.

use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::{
    Automation, BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus,
    Posting, RateLimit, Target, TargetKind, bad_feed, day,
};
use crate::error::{Error, Result};
use crate::knowledge::decode_entities;

pub struct WeWorkRemotely;

const FEEDS: [&str; 11] = [
    "remote-customer-support-jobs",
    "remote-product-jobs",
    "remote-full-stack-programming-jobs",
    "remote-back-end-programming-jobs",
    "remote-front-end-programming-jobs",
    "remote-programming-jobs",
    "remote-sales-and-marketing-jobs",
    "remote-management-and-finance-jobs",
    "remote-design-jobs",
    "remote-devops-sysadmin-jobs",
    "all-other-remote-jobs",
];

static DECL: ConnectorDecl = ConnectorDecl {
    name: "wwr",
    label: "We Work Remotely",
    operations: &[Operation::ListFeed, Operation::ImportPasted],
    automation: &[Automation::ReadPublic],
    rate_limit: Some(RateLimit {
        min_interval: Duration::from_secs(3600),
        per_day: 24,
    }),
    policy: PolicyStatus::Allowed,
    read_row: Some(("We Work Remotely", "Read listings through public RSS")),
    assisted_row: ("We Work Remotely", "Apply"),
    hosts: &["weworkremotely.com"],
    attribution: Some("We Work Remotely"),
    target: TargetKind::Feed,
};

/// Child elements of each `<item>`, in order, with their text.
fn items(body: &[u8]) -> Result<Vec<Vec<(String, String)>>> {
    let text = std::str::from_utf8(body).map_err(|_| bad_feed())?;
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let (mut saw_rss, mut in_item) = (false, false);
    let mut field: Option<String> = None;
    let mut items = Vec::new();
    let mut item: Vec<(String, String)> = Vec::new();
    loop {
        let event = reader.read_event().map_err(|_| bad_feed())?;
        match event {
            Event::DocType(_) => return Err(Error::Refused("bad_feed".into())),
            Event::Start(start) => {
                depth += 1;
                let name = start.name().as_ref().to_string();
                if depth == 1 {
                    if name != "rss" {
                        return Err(bad_feed());
                    }
                    saw_rss = true;
                } else if name == "item" && depth == 3 {
                    in_item = true;
                    item = Vec::new();
                } else if in_item && depth == 4 {
                    item.push((name.clone(), String::new()));
                    field = Some(name);
                }
            }
            Event::Empty(empty) => {
                let name = empty.name().as_ref().to_string();
                if in_item && depth == 3 {
                    item.push((name, String::new()));
                }
            }
            Event::End(_) => {
                if depth == 3 && in_item {
                    in_item = false;
                    items.push(std::mem::take(&mut item));
                }
                if depth == 4 {
                    field = None;
                }
                depth = depth.checked_sub(1).ok_or_else(bad_feed)?;
            }
            Event::Text(t) => {
                if let (Some(_), Some(last)) = (&field, item.last_mut()) {
                    last.1.push_str(&t.into_inner());
                }
            }
            Event::CData(c) => {
                if let (Some(_), Some(last)) = (&field, item.last_mut()) {
                    last.1.push_str(&c.into_inner());
                }
            }
            Event::GeneralRef(r) => {
                if let (Some(_), Some(last)) = (&field, item.last_mut()) {
                    let ch = match r.resolve_char_ref().map_err(|_| bad_feed())? {
                        Some(ch) => ch,
                        None => match r.into_inner().as_ref() {
                            "amp" => '&',
                            "lt" => '<',
                            "gt" => '>',
                            "quot" => '"',
                            "apos" => '\'',
                            _ => return Err(bad_feed()),
                        },
                    };
                    last.1.push(ch);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_rss || depth != 0 {
        return Err(bad_feed());
    }
    Ok(items)
}

fn get<'a>(item: &'a [(String, String)], key: &str) -> &'a str {
    item.iter()
        .find(|(name, _)| name == key)
        .map_or("", |(_, value)| value.trim())
}

impl Connector for WeWorkRemotely {
    fn decl(&self) -> &'static ConnectorDecl {
        &DECL
    }

    fn request(&self, target: &Target, _: u32) -> Result<PageRequest> {
        let path = match target.name.as_str() {
            "" => "/remote-jobs.rss".to_string(),
            slug if FEEDS.contains(&slug) => format!("/categories/{slug}.rss"),
            _ => return Err(Error::Validation("not a We Work Remotely feed".into())),
        };
        Ok(PageRequest {
            host: "weworkremotely.com",
            path,
            expects: BodyKind::Rss,
        })
    }

    fn parse(&self, _: &Target, body: &[u8]) -> Result<Page> {
        let rows = items(body)?;
        let mut postings = Vec::new();
        let mut skipped = 0;
        for item in &rows {
            let title = decode_entities(get(item, "title"));
            let (guid, link) = (get(item, "guid"), get(item, "link"));
            if title.is_empty() || guid.is_empty() || link.is_empty() {
                skipped += 1;
                continue;
            }
            let (company, role) = match title.split_once(": ") {
                Some((company, role)) => (company.to_string(), role.to_string()),
                None => (String::new(), title),
            };
            let place: Vec<&str> = ["region", "state", "country"]
                .iter()
                .map(|key| get(item, key))
                .filter(|part| !part.is_empty())
                .collect();
            postings.push(Posting {
                external_id: guid.to_string(),
                title: role,
                company,
                location: decode_entities(&place.join(", ")),
                country: None,
                work_mode: Some("remote"),
                employment_type: Some(get(item, "type").to_string()).filter(|t| !t.is_empty()),
                url: link.to_string(),
                apply_url: None,
                posted_on: day(get(item, "pubDate")),
                pay: None,
                tags: get(item, "skills")
                    .split(',')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_string)
                    .collect(),
                raw: get(item, "description").to_string(),
            });
        }
        if postings.is_empty() && skipped > 0 {
            return Err(bad_feed());
        }
        Ok(Page {
            postings,
            next: None,
            skipped,
        })
    }
}
