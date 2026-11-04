//! What each automation mode may do on each site, in one place. Pure decisions from the mode,
//! the site, the action, and the grant. Nothing here opens a socket or a browser.
//!
//! Claude Code apply mode is the owner's recorded decision: every site, every action, no
//! register read, once the owner has started a run. Manual, Copilot, and Autonomous follow
//! the policy register's rows (`docs/policy-register.md`).

use crate::ai::register::{REGISTER, site_row};
use crate::connectors;
use crate::error::Result;
use crate::jobs::link::{LinkRoute, route};
use crate::{Db, audit, runner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Manual,
    Copilot,
    Autonomous,
    ClaudeCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OpenLink,
    Supply,
    Drive,
    Fill,
    Upload,
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Greenhouse,
    Lever,
    Ashby,
    Workday,
    Indeed,
    LinkedIn,
    RemoteOk,
    WeWorkRemotely,
    CompanyPage,
    /// A loopback test form, in a debug build only. A path name that is no site's is `Company`.
    Fixture(FixtureSite),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureSite {
    Greenhouse,
    Lever,
    Ashby,
    Workday,
    Indeed,
    LinkedIn,
    Company,
}

pub struct Grant {
    /// The owner started this run, through the GUI or the CLI.
    pub owner_started: bool,
    /// The owner authorized Autonomous mode on this connector.
    pub authorized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow { note: Option<&'static str> },
    Refuse(&'static str),
}

/// Recorded with a Copilot fill the register leaves open (decision D2).
pub const NO_CLAUSE: &str = "no clause found";

/// Sites whose Autonomous submit has been verified end to end. None has: Autonomous ships as
/// a gate with zero eligible sites (decision 24).
pub const VERIFIED: &[Site] = &[];

const COPILOT_OP: &str = "Copilot fill of the hosted form; user reviews and submits";
const SUBMIT_OP: &str = "Submit the hosted form with no human";
/// The register's Copilot row for each site: integration and operation. RemoteOK, We Work
/// Remotely, and company pages have none.
const COPILOT_ROWS: &[(&str, &str)] = &[
    ("Greenhouse", COPILOT_OP),
    ("Lever", COPILOT_OP),
    ("Ashby", COPILOT_OP),
    (
        "Workday",
        "Fill a career-site form; user reviews and submits",
    ),
    ("Indeed", "Automated or AI form filling and submission"),
    ("LinkedIn", "Submit through Easy Apply by automation"),
];
const SUBMIT_ROWS: &[(&str, &str)] = &[
    ("Greenhouse", SUBMIT_OP),
    ("Lever", SUBMIT_OP),
    ("Ashby", SUBMIT_OP),
];

/// The site a link belongs to. A `http://127.0.0.1:<port>/<name>/...` link is a fixture in a
/// debug build and an ordinary link in a release build.
pub fn site_of(url: &str) -> Result<Site> {
    if cfg!(debug_assertions)
        && let Some(fixture) = fixture_of(url)
    {
        return Ok(Site::Fixture(fixture));
    }
    Ok(match route(url)? {
        LinkRoute::Greenhouse { .. } => Site::Greenhouse,
        LinkRoute::Lever { .. } => Site::Lever,
        LinkRoute::Ashby { .. } => Site::Ashby,
        LinkRoute::Workday => Site::Workday,
        LinkRoute::Indeed => Site::Indeed,
        LinkRoute::LinkedIn => Site::LinkedIn,
        LinkRoute::RemoteOk => Site::RemoteOk,
        LinkRoute::WeWorkRemotely => Site::WeWorkRemotely,
        LinkRoute::CompanyPage => Site::CompanyPage,
    })
}

fn fixture_of(url: &str) -> Option<FixtureSite> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none() {
        return None;
    }
    Some(match url.path_segments()?.next()? {
        "greenhouse" => FixtureSite::Greenhouse,
        "lever" => FixtureSite::Lever,
        "ashby" => FixtureSite::Ashby,
        "workday" => FixtureSite::Workday,
        "indeed" => FixtureSite::Indeed,
        "linkedin" => FixtureSite::LinkedIn,
        // Any other fixture page (a captcha, a login, a rejected submit) is an ordinary company form.
        _ => FixtureSite::Company,
    })
}

/// `connectors::link_ok`, or a fixture link in a debug build.
pub fn link_allowed(url: &str) -> bool {
    connectors::link_ok(url, &[])
        || (cfg!(debug_assertions) && matches!(site_of(url), Ok(Site::Fixture(_))))
}

/// The register's integration name for a site; a fixture reads the rows of the site it names.
fn integration(site: Site) -> Option<&'static str> {
    Some(match site {
        Site::Greenhouse | Site::Fixture(FixtureSite::Greenhouse) => "Greenhouse",
        Site::Lever | Site::Fixture(FixtureSite::Lever) => "Lever",
        Site::Ashby | Site::Fixture(FixtureSite::Ashby) => "Ashby",
        Site::Workday | Site::Fixture(FixtureSite::Workday) => "Workday",
        Site::Indeed | Site::Fixture(FixtureSite::Indeed) => "Indeed",
        Site::LinkedIn | Site::Fixture(FixtureSite::LinkedIn) => "LinkedIn",
        _ => return None,
    })
}

/// The connectors a person can authorize for Autonomous mode: the sites with register rows.
pub const CONNECTORS: &[&str] = &[
    "greenhouse",
    "lever",
    "ashby",
    "workday",
    "indeed",
    "linkedin",
];

/// The connector name a grant for this site is stored under; a fixture reads as the site it
/// names. `None` for a site with no register row.
pub(crate) fn connector_of(site: Site) -> Option<&'static str> {
    let name = integration(site)?;
    CONNECTORS
        .iter()
        .copied()
        .find(|connector| connector.eq_ignore_ascii_case(name))
}

fn row(register: &str, rows: &[(&str, &str)], site: Site) -> Option<crate::ai::register::SiteRow> {
    let name = integration(site)?;
    let (_, op) = rows.iter().find(|(integration, _)| *integration == name)?;
    site_row(register, name, op)
}

pub fn decide(mode: Mode, site: Site, action: Action, grant: &Grant) -> Decision {
    decide_on(REGISTER, VERIFIED, mode, site, action, grant)
}

/// [`decide`] on any register text and verified list, so a test can change them.
#[doc(hidden)]
pub fn decide_on(
    register: &str,
    verified: &[Site],
    mode: Mode,
    site: Site,
    action: Action,
    grant: &Grant,
) -> Decision {
    match mode {
        Mode::ClaudeCode if grant.owner_started => Decision::Allow { note: None },
        Mode::ClaudeCode => Decision::Refuse("run_not_started"),
        Mode::Manual => match action {
            Action::OpenLink | Action::Supply => Decision::Allow { note: None },
            _ => Decision::Refuse("mode_does_not_allow"),
        },
        Mode::Copilot if action == Action::Submit => Decision::Refuse("mode_does_not_allow"),
        Mode::Copilot => match row(register, COPILOT_ROWS, site) {
            Some(row) if row.verdict == "blocked" => Decision::Refuse("policy_blocked"),
            Some(row) if row.verdict == "allowed" => Decision::Allow { note: None },
            Some(row) if row.verdict == "unknown" && row.quote.starts_with("No clause") => {
                Decision::Allow {
                    note: Some(NO_CLAUSE),
                }
            }
            _ => Decision::Refuse("policy_unknown"),
        },
        Mode::Autonomous => {
            let submit = row(register, SUBMIT_ROWS, site);
            let blocked = |row: &Option<crate::ai::register::SiteRow>| {
                row.as_ref().is_some_and(|row| row.verdict == "blocked")
            };
            if blocked(&submit) || blocked(&row(register, COPILOT_ROWS, site)) {
                Decision::Refuse("policy_blocked")
            } else if !verified.contains(&site) || submit.is_none_or(|row| row.verdict != "allowed")
            {
                Decision::Refuse("policy_unknown")
            } else if !grant.authorized {
                Decision::Refuse("not_authorized")
            } else {
                Decision::Allow { note: None }
            }
        }
    }
}

/// Connectors the owner authorized for Autonomous mode.
pub async fn grants(db: &Db) -> Result<Vec<String>> {
    let names = sqlx::query_scalar("SELECT connector FROM autonomous_grants ORDER BY connector")
        .fetch_all(&db.pool)
        .await?;
    Ok(names)
}

/// A connector name as typed, folded to the stored form, or `Validation` for one not in
/// [`CONNECTORS`].
pub fn known_connector(connector: &str) -> Result<&'static str> {
    let folded = connector.trim().to_ascii_lowercase();
    CONNECTORS
        .iter()
        .copied()
        .find(|known| *known == folded)
        .ok_or_else(|| {
            crate::error::Error::Validation(format!(
                "unknown connector; one of: {}",
                CONNECTORS.join(", ")
            ))
        })
}

/// Whether the owner's stored grant covers this site's connector, read fresh each call.
pub async fn granted(db: &Db, site: Site) -> Result<bool> {
    let Some(connector) = connector_of(site) else {
        return Ok(false);
    };
    let found: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM autonomous_grants WHERE connector = ?)")
            .bind(connector)
            .fetch_one(&db.pool)
            .await?;
    Ok(found)
}

/// Only a person grants, only a connector in [`CONNECTORS`], and the audit row lands with the
/// grant.
pub async fn grant(db: &Db, actor: &str, connector: &str) -> Result<()> {
    runner::person(actor)?;
    let connector = known_connector(connector)?;
    let mut tx = db.pool.begin().await?;
    sqlx::query(
        "INSERT INTO autonomous_grants (connector, granted_by, granted_at) \
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT (connector) DO UPDATE SET granted_by = excluded.granted_by, \
         granted_at = excluded.granted_at",
    )
    .bind(connector)
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    let detail = serde_json::json!({});
    audit::record_to(
        &mut *tx,
        actor,
        "autonomous_granted",
        Some(connector),
        detail,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Only a person revokes. Revoking what was never granted writes nothing.
pub async fn revoke(db: &Db, actor: &str, connector: &str) -> Result<()> {
    runner::person(actor)?;
    let connector = known_connector(connector)?;
    let mut tx = db.pool.begin().await?;
    let removed = sqlx::query("DELETE FROM autonomous_grants WHERE connector = ?")
        .bind(connector)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 1 {
        let detail = serde_json::json!({});
        audit::record_to(
            &mut *tx,
            actor,
            "autonomous_revoked",
            Some(connector),
            detail,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
