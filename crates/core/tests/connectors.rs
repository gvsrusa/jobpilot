//! The connector SDK and its gate. Most rows come from an inline register; the R1 and
//! assisted-source tests read the real one.

use std::time::Duration;

use jobpilot_core::ai::register::REGISTER;
use jobpilot_core::connectors::{
    Automation, ConnectorDecl, Operation, PolicyStatus, RateLimit, TargetKind, all, check, find,
    gate, gate_on, read_status, read_status_on, statuses,
};

const TABLE: &str = "# Register\n\nx\n\n\
| Integration | Operation | Source | Date read | Quote | Verdict | Notes |\n\
| --- | --- | --- | --- | --- | --- | --- |\n\
| Lever | Read public listings through Postings API | https://x.test | 2026-11-01 | \"q\" | `silent-read` | n |\n\
| Lever | User applies on the hosted form in their own browser | https://x.test | 2026-11-01 | \"q\" | `user-assisted` | n |\n";

fn decl(policy: PolicyStatus) -> ConnectorDecl {
    ConnectorDecl {
        name: "t",
        label: "T",
        operations: &[Operation::ListFeed],
        automation: &[Automation::ReadPublic],
        rate_limit: Some(RateLimit {
            min_interval: Duration::from_secs(1),
            per_day: 10,
        }),
        policy,
        read_row: Some(("Lever", "Read public listings through Postings API")),
        assisted_row: (
            "Lever",
            "User applies on the hosted form in their own browser",
        ),
        hosts: &["h.test"],
        attribution: None,
        target: TargetKind::Feed,
    }
}

fn code(result: jobpilot_core::Result<()>) -> String {
    match result.unwrap_err() {
        jobpilot_core::Error::Refused(code) => code,
        other => other.code().to_string(),
    }
}

#[test]
fn a_read_with_no_rate_limit_cannot_register() {
    let bad = ConnectorDecl {
        rate_limit: None,
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(check(&bad).unwrap_err().code(), "validation");
    assert!(check(&decl(PolicyStatus::Allowed)).is_ok());
}

#[test]
fn no_operations_no_hosts_and_no_read_row_cannot_register() {
    let none = ConnectorDecl {
        operations: &[],
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(check(&none).unwrap_err().code(), "validation");
    let none = ConnectorDecl {
        hosts: &[],
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(check(&none).unwrap_err().code(), "validation");
    let none = ConnectorDecl {
        read_row: None,
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(check(&none).unwrap_err().code(), "validation");
}

#[test]
fn a_user_assisted_decl_cannot_declare_a_read() {
    let bad = ConnectorDecl {
        operations: &[Operation::ListBoard],
        automation: &[Automation::UserAssisted],
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(check(&bad).unwrap_err().code(), "validation");
}

#[test]
fn the_row_sets_the_status() {
    // The real register says `allowed` for Lever's read.
    assert_eq!(
        read_status(&decl(PolicyStatus::Allowed)),
        PolicyStatus::Allowed
    );
    // A declaration that disagrees with the row is not trusted.
    assert_eq!(
        read_status(&decl(PolicyStatus::Blocked)),
        PolicyStatus::Unknown
    );
}

#[test]
fn a_connector_claiming_more_than_its_row_counts_as_unknown() {
    let row = ("Greenhouse", "Read public listings through Job Board API");
    let claims = ConnectorDecl {
        read_row: Some(row),
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(read_status(&claims), PolicyStatus::Unknown);
}

#[test]
fn a_row_that_moves_without_the_declaration_counts_as_unknown() {
    assert_eq!(
        read_status_on(TABLE, &decl(PolicyStatus::Unknown)),
        PolicyStatus::Unknown
    );
    assert_eq!(
        read_status_on(TABLE, &decl(PolicyStatus::Allowed)),
        PolicyStatus::Unknown
    );
}

#[test]
fn silent_read_passes_the_gate_and_never_reads_as_allowed() {
    let d = decl(PolicyStatus::SilentRead);
    assert_eq!(read_status_on(TABLE, &d), PolicyStatus::SilentRead);
    assert_ne!(read_status_on(TABLE, &d), PolicyStatus::Allowed);
    assert!(gate_on(TABLE, &d, Operation::ListFeed).is_ok());
}

#[test]
fn a_missing_row_counts_as_unknown() {
    let d = ConnectorDecl {
        read_row: Some(("Nobody", "Read nothing")),
        ..decl(PolicyStatus::Allowed)
    };
    assert_eq!(read_status_on(TABLE, &d), PolicyStatus::Unknown);
    assert_eq!(
        code(gate_on(TABLE, &d, Operation::ListFeed)),
        "policy_unknown"
    );
}

#[test]
fn the_gate_refuses_by_status_with_its_own_code() {
    let table = |verdict: &str| TABLE.replace("`silent-read`", &format!("`{verdict}`"));
    let cases = [
        ("unknown", PolicyStatus::Unknown, "policy_unknown"),
        ("blocked", PolicyStatus::Blocked, "policy_blocked"),
        (
            "user-assisted",
            PolicyStatus::UserAssisted,
            "policy_user_assisted",
        ),
    ];
    for (verdict, status, expected) in cases {
        let d = decl(status);
        assert_eq!(
            code(gate_on(&table(verdict), &d, Operation::ListFeed)),
            expected
        );
    }
    let allowed = decl(PolicyStatus::Allowed);
    assert!(gate_on(&table("allowed"), &allowed, Operation::ListFeed).is_ok());
}

#[test]
fn an_undeclared_operation_is_refused() {
    let d = decl(PolicyStatus::SilentRead);
    assert_eq!(
        code(gate_on(TABLE, &d, Operation::ListBoard)),
        "operation_not_declared"
    );
    // The op check comes before the status check.
    let unknown = decl(PolicyStatus::Unknown);
    assert_eq!(
        code(gate_on(TABLE, &unknown, Operation::ListBoard)),
        "operation_not_declared"
    );
}

#[test]
fn import_passes_on_any_declared_connector() {
    let d = ConnectorDecl {
        operations: &[Operation::ImportPasted],
        read_row: None,
        rate_limit: None,
        automation: &[Automation::UserAssisted],
        ..decl(PolicyStatus::UserAssisted)
    };
    assert!(check(&d).is_ok());
    assert!(gate_on(TABLE, &d, Operation::ImportPasted).is_ok());
}

#[test]
fn every_registered_connector_passes_check() {
    for connector in all() {
        check(connector.decl()).unwrap();
        assert!(find(connector.decl().name).is_some());
        let _ = gate(connector.decl(), Operation::ImportPasted);
    }
    assert_eq!(statuses().len(), all().len());
}

#[test]
fn a_duplicate_row_with_another_verdict_reads_unknown() {
    let d = decl(PolicyStatus::SilentRead);
    let dup = format!(
        "{TABLE}| Lever | Read public listings through Postings API | https://x.test | 2026-11-01 | \"q\" | `blocked` | n |\n"
    );
    assert_eq!(read_status_on(&dup, &d), PolicyStatus::Unknown);
    let same = format!(
        "{TABLE}| Lever | Read public listings through Postings API | https://x.test | 2026-11-01 | \"q\" | `silent-read` | n |\n"
    );
    assert_eq!(read_status_on(&same, &d), PolicyStatus::SilentRead);
}

fn target(name: &str) -> jobpilot_core::connectors::Target {
    jobpilot_core::connectors::Target { name: name.into() }
}

#[test]
fn greenhouse_and_ashby_read_under_r1_and_never_read_as_allowed() {
    for name in ["greenhouse", "ashby"] {
        let c = find(name).unwrap();
        assert_eq!(read_status(c.decl()), PolicyStatus::SilentRead);
        assert_ne!(read_status(c.decl()), PolicyStatus::Allowed);
        assert!(gate(c.decl(), Operation::ListBoard).is_ok());
        assert!(gate(c.decl(), Operation::ListFeed).is_err());
    }
}

/// Integration and Operation of every register row whose verdict is `silent-read`.
fn silent_rows() -> Vec<(String, String)> {
    REGISTER
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line
                .strip_prefix("| ")?
                .strip_suffix(" |")?
                .split(" | ")
                .collect();
            (cells.get(5) == Some(&"`silent-read`")).then(|| (cells[0].into(), cells[1].into()))
        })
        .collect()
}

#[test]
fn r1_covers_only_those_two_rows() {
    assert_eq!(
        silent_rows(),
        [
            (
                "Greenhouse".to_string(),
                "Read public listings through Job Board API".to_string()
            ),
            (
                "Ashby".to_string(),
                "Read public listings through Job Postings API".to_string()
            ),
        ]
    );
}

#[test]
fn the_register_calls_r1_a_controller_ruling_not_an_owner_decision() {
    let heading = REGISTER
        .lines()
        .find(|line| line.starts_with('#') && line.contains("R1"))
        .expect("an R1 heading");
    assert!(heading.contains("Controller ruling"), "{heading}");
    assert!(REGISTER.contains("not an owner decision"));
    assert!(!REGISTER.contains("is `unknown`, matching the read rows"));
}

#[test]
fn a_row_set_back_to_unknown_refuses_both() {
    for (name, row) in [
        (
            "greenhouse",
            "| Greenhouse | Read public listings through Job Board API |",
        ),
        (
            "ashby",
            "| Ashby | Read public listings through Job Postings API |",
        ),
    ] {
        let changed: String = REGISTER
            .lines()
            .map(|line| {
                if line.starts_with(row) {
                    line.replace("`silent-read`", "`unknown`")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(changed, REGISTER, "{name}: the row was found");
        let c = find(name).unwrap();
        assert_eq!(read_status_on(&changed, c.decl()), PolicyStatus::Unknown);
        assert_eq!(
            code(gate_on(&changed, c.decl(), Operation::ListBoard)),
            "policy_unknown"
        );
    }
}

#[test]
fn indeed_is_blocked_and_declares_no_read_and_no_host() {
    let c = find("indeed").unwrap();
    assert_eq!(read_status(c.decl()), PolicyStatus::Blocked);
    assert!(c.decl().hosts.is_empty());
    assert!(gate(c.decl(), Operation::ListFeed).is_err());
    assert!(gate(c.decl(), Operation::ImportPasted).is_ok());
}

#[test]
fn every_assisted_src_can_only_import() {
    for name in ["workday", "indeed", "linkedin", "company_page"] {
        let c = find(name).unwrap();
        assert_eq!(c.decl().operations, &[Operation::ImportPasted], "{name}");
        assert!(c.decl().hosts.is_empty() && c.decl().rate_limit.is_none());
        assert!(check(c.decl()).is_ok());
        let err = c.request(&target("x"), 0).unwrap_err();
        assert!(
            matches!(&err, jobpilot_core::Error::Refused(code) if code == "operation_not_declared")
        );
        assert!(gate(c.decl(), Operation::ImportPasted).is_ok());
        assert!(gate(c.decl(), Operation::ListBoard).is_err());
        assert!(
            c.parse(&target("x"), b"[]").is_err(),
            "{name} parses nothing"
        );
    }
    let unknown = |name: &str| read_status(find(name).unwrap().decl());
    assert_eq!(unknown("workday"), PolicyStatus::Unknown);
    assert_eq!(unknown("linkedin"), PolicyStatus::Unknown);
    assert_eq!(unknown("company_page"), PolicyStatus::Unknown);
}

#[test]
fn an_import_only_decl_needs_a_user_assisted_row() {
    let base = ConnectorDecl {
        operations: &[Operation::ImportPasted],
        read_row: None,
        rate_limit: None,
        hosts: &[],
        automation: &[Automation::UserAssisted],
        ..decl(PolicyStatus::UserAssisted)
    };
    assert!(check(&base).is_ok());
    let missing = ConnectorDecl {
        assisted_row: ("Nobody", "Nothing"),
        ..base.clone()
    };
    assert_eq!(check(&missing).unwrap_err().code(), "validation");
    // A row with another verdict does not count.
    let wrong = ConnectorDecl {
        assisted_row: ("Greenhouse", "Submit through Job Board API"),
        ..base
    };
    assert_eq!(check(&wrong).unwrap_err().code(), "validation");
}

#[test]
fn no_row_ever_grants_more_than_a_status_says() {
    let all = statuses();
    let fetchable: Vec<_> = all
        .iter()
        .filter(|s| s.can_fetch)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        fetchable,
        ["lever", "remoteok", "wwr", "greenhouse", "ashby"]
    );
    let noted: Vec<_> = all
        .iter()
        .filter(|s| s.note.is_some())
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(noted, ["greenhouse", "ashby"]);
    let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "lever",
            "remoteok",
            "wwr",
            "greenhouse",
            "ashby",
            "workday",
            "indeed",
            "linkedin",
            "company_page"
        ]
    );
}
