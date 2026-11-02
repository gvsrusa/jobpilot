//! The sources JobPilot never reads: Workday, Indeed, LinkedIn, and a company's own page. The
//! person pastes a posting, or applies in their own browser. There is no request to build.

use super::{
    Automation, Connector, ConnectorDecl, Operation, Page, PageRequest, PolicyStatus, Target,
    TargetKind,
};
use crate::error::{Error, Result};

/// A declaration that can only import a paste.
pub struct Assisted(&'static ConnectorDecl);

const fn decl(
    name: &'static str,
    label: &'static str,
    policy: PolicyStatus,
    read_row: (&'static str, &'static str),
    assisted_row: (&'static str, &'static str),
) -> ConnectorDecl {
    ConnectorDecl {
        name,
        label,
        operations: &[Operation::ImportPasted],
        automation: &[Automation::UserAssisted],
        rate_limit: None,
        policy,
        read_row: Some(read_row),
        assisted_row,
        hosts: &[],
        attribution: None,
        target: TargetKind::None,
    }
}

static WORKDAY: ConnectorDecl = decl(
    "workday",
    "Workday",
    PolicyStatus::Unknown,
    (
        "Workday",
        "Read an employer's `myworkdayjobs.com` career site",
    ),
    (
        "Workday",
        "User applies in their own browser with JobPilot's documents",
    ),
);
static INDEED: ConnectorDecl = decl(
    "indeed",
    "Indeed",
    PolicyStatus::Blocked,
    ("Indeed", "Read listings by connector or crawler"),
    (
        "Indeed",
        "User applies themselves, or pastes a posting into JobPilot",
    ),
);
static LINKEDIN: ConnectorDecl = decl(
    "linkedin",
    "LinkedIn",
    PolicyStatus::Unknown,
    ("LinkedIn", "Automated read or logged-in automation"),
    (
        "LinkedIn",
        "User pastes a posting, or applies in their own browser",
    ),
);
static COMPANY_PAGE: ConnectorDecl = decl(
    "company_page",
    "Company career page",
    PolicyStatus::Unknown,
    (
        "Company career pages",
        "Automated read of an arbitrary site",
    ),
    (
        "Company career pages",
        "User pastes a posting's text or link",
    ),
);

pub(super) fn sources() -> [Assisted; 4] {
    [
        Assisted(&WORKDAY),
        Assisted(&INDEED),
        Assisted(&LINKEDIN),
        Assisted(&COMPANY_PAGE),
    ]
}

fn refused<T>() -> Result<T> {
    Err(Error::Refused("op_not_declared".into()))
}

impl Connector for Assisted {
    fn decl(&self) -> &'static ConnectorDecl {
        self.0
    }

    fn request(&self, _: &Target, _: u32) -> Result<PageRequest> {
        refused()
    }

    fn parse(&self, _: &Target, _: &[u8]) -> Result<Page> {
        refused()
    }
}
