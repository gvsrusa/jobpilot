//! Routing a pasted link to the connector that owns its host. Nothing here opens a socket.

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRoute {
    Lever { site: String, eu: bool },
    Greenhouse { board: String },
    Ashby { board: String },
    Workday,
    Indeed,
    LinkedIn,
    RemoteOk,
    WeWorkRemotely,
    CompanyPage,
}

/// The route for an `http` or `https` link. The host decides: it is what follows the scheme
/// and any user info, up to the port, and a suffix counts only on a dot boundary or as the whole
/// host. A backslash ends the authority, as a browser reads it.
pub fn route(url: &str) -> Result<LinkRoute> {
    let (host, path) = split(url)?;
    let first = path.split('/').find(|part| !part.is_empty()).unwrap_or("");
    let name = || first.to_string();
    Ok(match host.as_str() {
        "jobs.lever.co" => LinkRoute::Lever {
            site: name(),
            eu: false,
        },
        "jobs.eu.lever.co" => LinkRoute::Lever {
            site: name(),
            eu: true,
        },
        "boards.greenhouse.io" | "job-boards.greenhouse.io" => {
            LinkRoute::Greenhouse { board: name() }
        }
        "jobs.ashbyhq.com" => LinkRoute::Ashby { board: name() },
        "remoteok.com" => LinkRoute::RemoteOk,
        "weworkremotely.com" => LinkRoute::WeWorkRemotely,
        _ if under(&host, "myworkdayjobs.com") => LinkRoute::Workday,
        _ if under(&host, "indeed.com") => LinkRoute::Indeed,
        _ if under(&host, "linkedin.com") => LinkRoute::LinkedIn,
        _ => LinkRoute::CompanyPage,
    })
}

/// The connector name stored on a job and shown to the person.
pub fn connector_of(route: &LinkRoute) -> &'static str {
    match route {
        LinkRoute::Lever { .. } => "lever",
        LinkRoute::Greenhouse { .. } => "greenhouse",
        LinkRoute::Ashby { .. } => "ashby",
        LinkRoute::Workday => "workday",
        LinkRoute::Indeed => "indeed",
        LinkRoute::LinkedIn => "linkedin",
        LinkRoute::RemoteOk => "remoteok",
        LinkRoute::WeWorkRemotely => "weworkremotely",
        LinkRoute::CompanyPage => "company_page",
    }
}

/// Credit a feed asks to be shown beside what it supplied; `None` where none is asked.
pub(crate) fn attribution_of(route: &LinkRoute) -> Option<&'static str> {
    match route {
        LinkRoute::RemoteOk => Some("Remote OK"),
        LinkRoute::WeWorkRemotely => Some("We Work Remotely"),
        _ => None,
    }
}

fn under(host: &str, suffix: &str) -> bool {
    host == suffix
        || host
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.ends_with('.'))
}

fn bad() -> Error {
    Error::Validation("a link is an http or https address".into())
}

/// Lowercase host without user info or port, and the path.
fn split(url: &str) -> Result<(String, String)> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(bad());
    }
    let end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let authority = authority.rsplit('@').next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(_) => authority.to_string(),
        None => authority.split(':').next().unwrap_or("").to_string(),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let valid = !host.is_empty()
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '[' | ']' | ':'));
    if !valid {
        return Err(bad());
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    // A browser reads `\` as `/` in an http link.
    Ok((host, path.replace('\\', "/")))
}
