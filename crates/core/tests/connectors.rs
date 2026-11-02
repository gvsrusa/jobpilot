//! The connector SDK and its gate. Rows come from an inline register where no real row reads
//! `silent-read` yet.

use std::time::Duration;

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
fn an_undeclared_op_is_refused() {
    let d = decl(PolicyStatus::SilentRead);
    assert_eq!(
        code(gate_on(TABLE, &d, Operation::ListBoard)),
        "op_not_declared"
    );
    // The op check comes before the status check.
    let unknown = decl(PolicyStatus::Unknown);
    assert_eq!(
        code(gate_on(TABLE, &unknown, Operation::ListBoard)),
        "op_not_declared"
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
