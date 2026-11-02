//! The one place a connector's request leaves the machine. Order for each attempt: the pause,
//! the register gate, the declared host, the stored rate limit, then the request, raced
//! against the run's token. A refusal is final: no retry, no other route, no other client.
//! One honest `User-Agent` is sent. No cookie, no proxy, and no redirect is followed.

use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, RETRY_AFTER, USER_AGENT as UA};
use reqwest::{Client, Url};

use super::limit::{self, MAX_WAIT, Slot};
use super::{BodyKind, Connector, ConnectorDecl, Operation, Page, PageRequest, Target, gate};
use crate::ai::CancellationToken;
use crate::ai::net::{hosted_client, loopback_client, net, read_capped, retry_after};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::pause;

pub const MAX_FEED_BYTES: u64 = 16 * 1024 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const USER_AGENT: &str = concat!(
    "JobPilot/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/gvsrusa/jobpilot)"
);

/// Two attempts per request (decision 9).
const ATTEMPTS: u32 = 2;
/// A refusal keeps the source quiet for a day. A person cannot override it, by design.
const BLOCKED_FOR: Duration = Duration::from_secs(24 * 3600);
/// A 429 or 503 with no usable `Retry-After`.
const BUSY_FOR: Duration = Duration::from_secs(3600);
const POLL: Duration = Duration::from_millis(50);

pub struct Fetcher {
    client: Client,
    /// Set only by [`Fetcher::for_test`]: every request goes to this loopback origin.
    origin: Option<Url>,
}

/// What came back, before anything is parsed.
struct Reply {
    status: u16,
    retry_after: Option<Duration>,
    content_type: String,
    challenged: bool,
    body: Vec<u8>,
}

enum Step {
    Done(Page),
    Again,
}

impl Fetcher {
    pub fn hosted() -> Result<Fetcher> {
        Ok(Fetcher {
            client: hosted_client()?,
            origin: None,
        })
    }

    /// Loopback only: `loopback_client` refuses anything else, so a test build cannot reach a
    /// vendor.
    #[doc(hidden)]
    pub fn for_test(base: &str) -> Result<Fetcher> {
        let (origin, client) = loopback_client(base)?;
        Ok(Fetcher {
            client,
            origin: Some(origin),
        })
    }

    pub async fn page(
        &self,
        db: &Db,
        connector: &dyn Connector,
        target: &Target,
        page: u32,
        operation: Operation,
        cancel: &CancellationToken,
    ) -> Result<Page> {
        let decl = connector.decl();
        let mut attempt = 0;
        loop {
            attempt += 1;
            pause::ensure_off(db).await?;
            if operation == Operation::ImportPasted {
                // Import reads a paste. It has no request to make.
                return Err(Error::Refused("operation_not_declared".into()));
            }
            gate(decl, operation)?;
            let request = connector.request(target, page)?;
            if !decl.hosts.contains(&request.host) {
                return Err(Error::Validation(
                    "connector request names a host its declaration does not".into(),
                ));
            }
            wait_for_slot(db, decl, request.host, cancel).await?;
            // A pause set during the wait still stops this request.
            pause::ensure_off(db).await?;
            let sent = self.send(&request, cancel).await;
            match self
                .classify(db, connector, target, &request, attempt, cancel, sent)
                .await?
            {
                Step::Done(page) => return Ok(page),
                Step::Again => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn classify(
        &self,
        db: &Db,
        connector: &dyn Connector,
        target: &Target,
        request: &PageRequest,
        attempt: u32,
        cancel: &CancellationToken,
        sent: Result<Reply>,
    ) -> Result<Step> {
        let decl = connector.decl();
        let last = attempt >= ATTEMPTS;
        let reply = match sent {
            Ok(reply) => reply,
            // Connection trouble: one more try after the interval, then stop.
            Err(Error::Io(_)) if !last => return Ok(Step::Again),
            Err(Error::Io(_)) => return Err(Error::Refused("network".into())),
            Err(Error::Timeout) if !last => return Ok(Step::Again),
            Err(other) => return Err(other),
        };
        let blocked = || block(db, decl, request.host);
        if reply.challenged {
            return blocked().await;
        }
        match reply.status {
            200 => {}
            429 | 503 => {
                let wait = reply.retry_after;
                match wait {
                    Some(wait) if wait <= MAX_WAIT && !last => {
                        sleep_polling(wait, cancel).await?;
                        return Ok(Step::Again);
                    }
                    _ => {
                        let wait = wait.unwrap_or(BUSY_FOR);
                        limit::hold_off(db, decl, request.host, wait).await?;
                        return Err(Error::RateLimited {
                            retry_after: Some(wait),
                        });
                    }
                }
            }
            500..=599 if !last => return Ok(Step::Again),
            500..=599 => return Err(Error::Refused("network".into())),
            401 | 403 | 407 | 451 | 300..=399 => return blocked().await,
            404 => return Err(Error::Refused("source_not_found".into())),
            _ => return Err(Error::Refused("bad_feed".into())),
        }
        if looks_like_html(&reply.body) {
            return blocked().await;
        }
        let type_ok = match request.expects {
            BodyKind::Json => reply.content_type.contains("json"),
            BodyKind::Rss => {
                reply.content_type.contains("xml") || reply.content_type.contains("rss")
            }
        };
        if !type_ok {
            return Err(Error::Refused("bad_feed".into()));
        }
        connector.parse(target, &reply.body).map(Step::Done)
    }

    /// One request, bounded by [`REQUEST_TIMEOUT`] and ended at once by the token.
    async fn send(&self, request: &PageRequest, cancel: &CancellationToken) -> Result<Reply> {
        let url = self.url(request)?;
        let work = async {
            tokio::time::timeout(REQUEST_TIMEOUT, self.get(url, request.expects))
                .await
                .map_err(|_| Error::Timeout)?
        };
        tokio::select! {
            reply = work => reply,
            () = cancelled(cancel) => Err(Error::Cancelled),
        }
    }

    fn url(&self, request: &PageRequest) -> Result<Url> {
        let refused = || Error::Refused("not_loopback".into());
        match &self.origin {
            Some(origin) => origin.join(&request.path).map_err(|_| refused()),
            None => Url::parse(&format!("https://{}{}", request.host, request.path))
                .map_err(|_| Error::Validation("connector path is not a path".into())),
        }
    }

    async fn get(&self, url: Url, kind: BodyKind) -> Result<Reply> {
        let accept = match kind {
            BodyKind::Json => "application/json",
            BodyKind::Rss => "application/rss+xml, application/xml;q=0.9, text/xml;q=0.8",
        };
        let response = self
            .client
            .get(url)
            .header(UA, USER_AGENT)
            .header(ACCEPT, accept)
            .send()
            .await
            .map_err(net)?;
        let text = |name| {
            let value = response.headers().get(name);
            value
                .and_then(|v| v.to_str().ok())
                .map(str::to_ascii_lowercase)
        };
        let content_type = text(CONTENT_TYPE).unwrap_or_default();
        let retry = retry_after(text(RETRY_AFTER).as_deref());
        let challenged = response.headers().contains_key("cf-mitigated");
        let status = response.status().as_u16();
        // Only a 200 has a body worth reading; a page that says it is HTML is not read at all.
        let body = if status == 200 && !challenged && !content_type.contains("html") {
            read_capped(response, MAX_FEED_BYTES).await?
        } else {
            Vec::new()
        };
        let challenged = challenged || (status == 200 && content_type.contains("html"));
        Ok(Reply {
            status,
            retry_after: retry,
            content_type,
            challenged,
            body,
        })
    }
}

/// Stops the source for a day and says so. Nothing else is tried.
async fn block(db: &Db, decl: &ConnectorDecl, host: &str) -> Result<Step> {
    limit::hold_off(db, decl, host, BLOCKED_FOR).await?;
    Err(Error::Refused("source_blocked".into()))
}

fn looks_like_html(body: &[u8]) -> bool {
    let head = &body[..body.len().min(256)];
    let text = String::from_utf8_lossy(head);
    let text = text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_lowercase();
    text.starts_with("<!doctype html") || text.starts_with("<html")
}

/// Takes a slot, sleeping a short wait and refusing a long one.
async fn wait_for_slot(
    db: &Db,
    decl: &ConnectorDecl,
    host: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    for _ in 0..4 {
        match limit::take(db, decl, host).await? {
            Slot::Go => return Ok(()),
            Slot::Wait(wait) if wait > MAX_WAIT => {
                return Err(Error::RateLimited {
                    retry_after: Some(wait),
                });
            }
            Slot::Wait(wait) => sleep_polling(wait, cancel).await?,
        }
    }
    // Another handle kept winning the slot: say when to try again.
    Err(Error::RateLimited {
        retry_after: decl.rate_limit.map(|limit| limit.min_interval),
    })
}

async fn sleep_polling(total: Duration, cancel: &CancellationToken) -> Result<()> {
    let end = tokio::time::Instant::now() + total;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let left = end.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        tokio::time::sleep(left.min(POLL)).await;
    }
}

async fn cancelled(cancel: &CancellationToken) {
    while !cancel.is_cancelled() {
        tokio::time::sleep(POLL).await;
    }
}
