//! The connector SDK and the gate every connector reads through. A connector is a declaration
//! plus two pure functions: build a request, parse a body. It opens no socket. The policy
//! register is `include_str!`, so a verdict is fixed at build time and no setting reaches it.
//!
//! `silent-read` is controller ruling R1, not an owner decision: the vendor is silent on
//! third-party readers. It passes the gate and is never reported as `allowed`.

use std::time::Duration;

pub mod fetch;
mod lever;
pub mod limit;
mod remoteok;
mod wwr;

pub use lever::Lever;
pub use remoteok::RemoteOk;
pub use wwr::WeWorkRemotely;

use serde::Serialize;

use crate::ai::register::{REGISTER, site_row};
use crate::error::{Error, Result};
use crate::jobs::PayRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    ListBoard,
    ListFeed,
    ImportPasted,
}

impl Operation {
    fn reads(self) -> bool {
        matches!(self, Self::ListBoard | Self::ListFeed)
    }
}

/// No fill or submit variant exists in 0.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Automation {
    ReadPublic,
    UserAssisted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStatus {
    Allowed,
    SilentRead,
    UserAssisted,
    Blocked,
    Unknown,
}

impl PolicyStatus {
    fn from_verdict(verdict: &str) -> Self {
        match verdict {
            "allowed" => Self::Allowed,
            "silent-read" => Self::SilentRead,
            "user-assisted" => Self::UserAssisted,
            "blocked" => Self::Blocked,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub min_interval: Duration,
    pub per_day: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Board,
    Feed,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Json,
    Rss,
}

#[derive(Debug, Clone)]
pub struct ConnectorDecl {
    pub name: &'static str,
    pub label: &'static str,
    pub operations: &'static [Operation],
    pub automation: &'static [Automation],
    pub rate_limit: Option<RateLimit>,
    pub policy: PolicyStatus,
    /// Integration and Operation cells of the register row that governs the read.
    pub read_row: Option<(&'static str, &'static str)>,
    /// The row for the person applying by hand.
    pub assisted_row: (&'static str, &'static str),
    pub hosts: &'static [&'static str],
    pub attribution: Option<&'static str>,
    pub target: TargetKind,
}

#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct PageRequest {
    pub host: &'static str,
    pub path: String,
    pub expects: BodyKind,
}

#[derive(Debug, Clone)]
pub struct Posting {
    pub external_id: String,
    pub title: String,
    pub company: String,
    pub location: String,
    pub country: Option<String>,
    pub work_mode: Option<&'static str>,
    pub employment_type: Option<String>,
    pub url: String,
    pub apply_url: Option<String>,
    pub posted_on: Option<String>,
    pub pay: Option<PayRange>,
    pub tags: Vec<String>,
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub postings: Vec<Posting>,
    pub next: Option<u32>,
    /// Rows the source sent that lacked a field this connector needs. Counted, never guessed.
    pub skipped: usize,
}

pub trait Connector: Send + Sync {
    fn decl(&self) -> &'static ConnectorDecl;
    fn request(&self, target: &Target, page: u32) -> Result<PageRequest>;
    fn parse(&self, target: &Target, body: &[u8]) -> Result<Page>;
    /// `parse` for a connector whose paging rule needs the page it just read. The fetcher calls
    /// this one; the default ignores the page.
    fn parse_page(&self, target: &Target, body: &[u8], _page: u32) -> Result<Page> {
        self.parse(target, body)
    }
    /// Hosts a stored link may name, when the vendor serves its postings from known ones.
    /// Empty means any host, over https.
    fn link_hosts(&self) -> &'static [&'static str] {
        &[]
    }
}

// ponytail: field caps are fixed here for every connector: a title over 300 characters skips
// the row; company and location are cut at 200 characters; description at 200 KB (204,800
// bytes), always on a character boundary.
const TITLE_MAX: usize = 300;
const NAME_MAX: usize = 200;
const RAW_MAX: usize = 200 * 1024;

fn cut_chars(text: &mut String, max: usize) {
    if let Some((at, _)) = text.char_indices().nth(max) {
        text.truncate(at);
    }
}

fn cut_bytes(text: &mut String, max: usize) {
    if text.len() > max {
        let mut at = max;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
}

/// A stored link is https, names no user or port, and sits on one of `hosts` when any are given.
fn link_ok(link: &str, hosts: &[&str]) -> bool {
    let Ok(url) = reqwest::Url::parse(link) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|host| hosts.is_empty() || hosts.contains(&host))
}

/// The one check every parsed page passes through, in the fetcher. A row with a bad link or an
/// over-long title is skipped and counted; a repeated `external_id` keeps its first; a pay
/// range that is negative or empty becomes unknown; long fields are cut.
pub fn vet(mut page: Page, link_hosts: &[&str]) -> Page {
    let mut seen = std::collections::HashSet::new();
    let rows = std::mem::take(&mut page.postings);
    for mut posting in rows {
        let links_ok = link_ok(&posting.url, link_hosts)
            && posting
                .apply_url
                .as_deref()
                .is_none_or(|link| link_ok(link, link_hosts));
        if !links_ok
            || posting.title.chars().count() > TITLE_MAX
            || !seen.insert(posting.external_id.clone())
        {
            page.skipped += 1;
            continue;
        }
        if let Some(pay) = &posting.pay {
            let negative = pay.min.is_some_and(|n| n < 0) || pay.max.is_some_and(|n| n < 0);
            let empty = pay.min.unwrap_or(0) == 0 && pay.max.unwrap_or(0) == 0;
            if negative || empty {
                posting.pay = None;
            }
        }
        cut_chars(&mut posting.company, NAME_MAX);
        cut_chars(&mut posting.location, NAME_MAX);
        cut_bytes(&mut posting.raw, RAW_MAX);
        page.postings.push(posting);
    }
    page
}

fn invalid(why: &str) -> Error {
    Error::Validation(format!("connector declaration: {why}"))
}

/// A declaration that cannot register. Read connectors need a rate limit, a read row, and the
/// public-read automation; an assisted one declares no read.
pub fn check(decl: &ConnectorDecl) -> Result<()> {
    if decl.name.is_empty() || decl.label.is_empty() {
        return Err(invalid("a name and a label"));
    }
    if decl.operations.is_empty() {
        return Err(invalid("no operations"));
    }
    if decl.hosts.is_empty() {
        return Err(invalid("no hosts"));
    }
    if decl.operations.iter().any(|op| op.reads()) {
        if decl.automation.contains(&Automation::UserAssisted) {
            return Err(invalid("a user-assisted connector cannot declare a read"));
        }
        if !decl.automation.contains(&Automation::ReadPublic) {
            return Err(invalid("a read needs the read-public automation"));
        }
        if decl.read_row.is_none() {
            return Err(invalid("a read needs a register row"));
        }
        match decl.rate_limit {
            Some(limit) if !limit.min_interval.is_zero() && limit.per_day > 0 => {}
            _ => return Err(invalid("a read needs a rate limit")),
        }
    }
    Ok(())
}

/// Every connector. Task 5 adds Greenhouse and Ashby.
pub fn all() -> Vec<Box<dyn Connector>> {
    vec![
        Box::new(Lever),
        Box::new(RemoteOk),
        Box::new(WeWorkRemotely),
    ]
}

/// A board or feed name that cannot leave its path: `[A-Za-z0-9_-]{1,100}`.
pub fn clean_slug(text: &str) -> Result<String> {
    let ok = (1..=100).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(text.to_string())
    } else {
        Err(Error::Validation(
            "a name holds letters, digits, - and _ only, up to 100".into(),
        ))
    }
}

/// `YYYY-MM-DD` from an RFC 3339 or RFC 2822 date, a plain date, or an epoch in seconds or
/// milliseconds. Anything else is `None`: a date is never guessed.
pub fn day(text: &str) -> Option<String> {
    use time::format_description::well_known::{Rfc2822, Rfc3339};
    use time::{Date, Month, OffsetDateTime};
    let text = text.trim();
    let date = if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        let n: i64 = text.parse().ok()?;
        let secs = if n >= 100_000_000_000 { n / 1000 } else { n };
        OffsetDateTime::from_unix_timestamp(secs).ok()?.date()
    } else if let Ok(at) = OffsetDateTime::parse(text, &Rfc3339) {
        at.date()
    } else if let Ok(at) = OffsetDateTime::parse(text, &Rfc2822) {
        at.date()
    } else {
        let mut parts = text.get(..10)?.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: u8 = parts.next()?.parse().ok()?;
        let dom: u8 = parts.next()?.parse().ok()?;
        Date::from_calendar_date(year, Month::try_from(month).ok()?, dom).ok()?
    };
    Some(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    ))
}

fn bad_feed() -> Error {
    Error::Refused("bad_feed".into())
}

pub fn find(name: &str) -> Option<Box<dyn Connector>> {
    all().into_iter().find(|c| c.decl().name == name)
}

/// M-33: the row's verdict when the declaration agrees, otherwise `Unknown`.
pub fn read_status(decl: &ConnectorDecl) -> PolicyStatus {
    read_status_on(REGISTER, decl)
}

/// [`read_status`] on any register text, so a test can set the rows.
#[doc(hidden)]
pub fn read_status_on(register: &str, decl: &ConnectorDecl) -> PolicyStatus {
    let (integration, op) = decl.read_row.unwrap_or(decl.assisted_row);
    let Some(row) = site_row(register, integration, op) else {
        return PolicyStatus::Unknown;
    };
    let verdict = PolicyStatus::from_verdict(&row.verdict);
    if verdict == decl.policy {
        verdict
    } else {
        PolicyStatus::Unknown
    }
}

/// Passes only a declared operation whose status is `Allowed` or `SilentRead`.
pub fn gate(decl: &ConnectorDecl, op: Operation) -> Result<()> {
    gate_on(REGISTER, decl, op)
}

#[doc(hidden)]
pub fn gate_on(register: &str, decl: &ConnectorDecl, op: Operation) -> Result<()> {
    if !decl.operations.contains(&op) {
        return Err(Error::Refused("operation_not_declared".into()));
    }
    if op == Operation::ImportPasted {
        return Ok(());
    }
    let code = match read_status_on(register, decl) {
        PolicyStatus::Allowed | PolicyStatus::SilentRead => return Ok(()),
        PolicyStatus::Unknown => "policy_unknown",
        PolicyStatus::Blocked => "policy_blocked",
        PolicyStatus::UserAssisted => "policy_user_assisted",
    };
    Err(Error::Refused(code.into()))
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceStatus {
    pub name: String,
    pub label: String,
    pub read: PolicyStatus,
    pub can_fetch: bool,
    pub operations: Vec<Operation>,
    pub attribution: Option<String>,
    pub date_read: Option<String>,
    pub target: TargetKind,
    pub min_interval_secs: Option<u64>,
    pub per_day: Option<u32>,
    pub note: Option<&'static str>,
}

pub const R1_NOTE: &str = "Vendor is silent on third-party readers. Read under ruling R1.";

pub fn statuses() -> Vec<SourceStatus> {
    all()
        .iter()
        .map(|connector| {
            let decl = connector.decl();
            let read = read_status(decl);
            let (integration, op) = decl.read_row.unwrap_or(decl.assisted_row);
            let can_fetch = decl
                .operations
                .iter()
                .any(|op| op.reads() && gate(decl, *op).is_ok());
            SourceStatus {
                name: decl.name.into(),
                label: decl.label.into(),
                read,
                can_fetch,
                operations: decl.operations.to_vec(),
                attribution: decl.attribution.map(str::to_string),
                date_read: site_row(REGISTER, integration, op).map(|row| row.date_read),
                target: decl.target,
                min_interval_secs: decl.rate_limit.map(|limit| limit.min_interval.as_secs()),
                per_day: decl.rate_limit.map(|limit| limit.per_day),
                note: (read == PolicyStatus::SilentRead).then_some(R1_NOTE),
            }
        })
        .collect()
}
