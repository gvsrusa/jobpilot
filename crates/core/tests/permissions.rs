//! The central permissions engine: pure decisions from the mode, the site, the action, and
//! the grant. No browser, no network, no model.

use jobpilot_core::permissions::{
    self, Action, Decision, FixtureSite, Grant, Mode, NO_CLAUSE, Site, decide, decide_on,
};
use jobpilot_core::{AppPaths, Db, Error, audit};
use tempfile::TempDir;

const ALL_SITES: [Site; 16] = [
    Site::Greenhouse,
    Site::Lever,
    Site::Ashby,
    Site::Workday,
    Site::Indeed,
    Site::LinkedIn,
    Site::RemoteOk,
    Site::WeWorkRemotely,
    Site::CompanyPage,
    Site::Fixture(FixtureSite::Greenhouse),
    Site::Fixture(FixtureSite::Lever),
    Site::Fixture(FixtureSite::Ashby),
    Site::Fixture(FixtureSite::Workday),
    Site::Fixture(FixtureSite::Indeed),
    Site::Fixture(FixtureSite::LinkedIn),
    Site::Fixture(FixtureSite::Company),
];
const ALL_ACTIONS: [Action; 6] = [
    Action::OpenLink,
    Action::Supply,
    Action::Drive,
    Action::Fill,
    Action::Upload,
    Action::Submit,
];

fn started() -> Grant {
    Grant {
        owner_started: true,
        authorized: false,
    }
}
fn not_started() -> Grant {
    Grant {
        owner_started: false,
        authorized: false,
    }
}
fn authorized() -> Grant {
    Grant {
        owner_started: true,
        authorized: true,
    }
}

#[track_caller]
fn assert_refused(decision: Decision, code: &str) {
    assert!(
        matches!(decision, Decision::Refuse(found) if found == code),
        "wanted Refuse({code:?})"
    );
}

#[test]
fn claude_code_is_allowed_on_every_site_and_every_action_once_started() {
    for site in ALL_SITES {
        for action in ALL_ACTIONS {
            assert!(
                matches!(
                    decide(Mode::ClaudeCode, site, action, &started()),
                    Decision::Allow { .. }
                ),
                "{site:?} {action:?}"
            );
        }
    }
}

#[test]
fn claude_code_reads_no_register() {
    // An empty register has no row for any site, and the mode still allows.
    for site in ALL_SITES {
        let decision = decide_on("", &[], Mode::ClaudeCode, site, Action::Submit, &started());
        assert!(
            matches!(decision, Decision::Allow { note: None }),
            "{site:?}"
        );
    }
}

#[test]
fn claude_code_needs_a_run_the_owner_started() {
    for site in ALL_SITES {
        for action in ALL_ACTIONS {
            assert_refused(
                decide(Mode::ClaudeCode, site, action, &not_started()),
                "run_not_started",
            );
        }
    }
}

#[test]
fn the_override_touches_no_other_mode() {
    assert_refused(
        decide(Mode::Copilot, Site::Indeed, Action::Fill, &started()),
        "policy_blocked",
    );
    assert_refused(
        decide(Mode::Copilot, Site::LinkedIn, Action::Fill, &started()),
        "policy_blocked",
    );
    assert_refused(
        decide(Mode::Copilot, Site::Workday, Action::Fill, &started()),
        "policy_unknown",
    );
    assert_refused(
        decide(
            Mode::Autonomous,
            Site::Indeed,
            Action::Submit,
            &authorized(),
        ),
        "policy_blocked",
    );
    assert_refused(
        decide(Mode::Manual, Site::Indeed, Action::Fill, &started()),
        "mode_does_not_allow",
    );
}

#[test]
fn copilot_records_no_clause_found_on_the_three_d2_sites() {
    for site in [
        Site::Greenhouse,
        Site::Lever,
        Site::Ashby,
        Site::Fixture(FixtureSite::Greenhouse),
        Site::Fixture(FixtureSite::Lever),
        Site::Fixture(FixtureSite::Ashby),
    ] {
        assert!(
            matches!(
                decide(Mode::Copilot, site, Action::Fill, &started()),
                Decision::Allow {
                    note: Some("no clause found")
                }
            ),
            "{site:?}"
        );
        assert_refused(
            decide(Mode::Copilot, site, Action::Submit, &started()),
            "mode_does_not_allow",
        );
    }
    assert_eq!(NO_CLAUSE, "no clause found");
}

#[test]
fn a_fixture_follows_the_rows_of_the_site_it_names() {
    assert_refused(
        decide(
            Mode::Copilot,
            Site::Fixture(FixtureSite::Indeed),
            Action::Fill,
            &started(),
        ),
        "policy_blocked",
    );
    assert_refused(
        decide(
            Mode::Copilot,
            Site::Fixture(FixtureSite::Workday),
            Action::Fill,
            &started(),
        ),
        "policy_unknown",
    );
    assert_refused(
        decide(
            Mode::Copilot,
            Site::Fixture(FixtureSite::Company),
            Action::Fill,
            &started(),
        ),
        "policy_unknown",
    );
}

#[test]
fn autonomous_has_zero_eligible_sites() {
    assert!(permissions::VERIFIED.is_empty());
    for site in ALL_SITES {
        assert!(
            matches!(
                decide(Mode::Autonomous, site, Action::Submit, &authorized()),
                Decision::Refuse(_)
            ),
            "{site:?}"
        );
    }
}

const ALLOWED_LEVER: &str = "| Integration | Operation | Primary source URL | Date read | Quoted \
clause | Verdict | Consequence |\n|---|---|---|---|---|---|---|\n| Lever | Submit the hosted \
form with no human | https://example.invalid | 2026-11-01 | No clause found. | `allowed` | x |\n";

#[test]
fn autonomous_needs_row_verification_and_authorization_together() {
    let run = |verified: &[Site], grant: &Grant| {
        decide_on(
            ALLOWED_LEVER,
            verified,
            Mode::Autonomous,
            Site::Lever,
            Action::Submit,
            grant,
        )
    };
    assert!(matches!(
        run(&[Site::Lever], &authorized()),
        Decision::Allow { .. }
    ));
    assert_refused(run(&[Site::Lever], &started()), "not_authorized");
    assert_refused(run(&[], &authorized()), "policy_unknown");
    // Another site's row does not carry Lever.
    assert_refused(
        decide_on(
            ALLOWED_LEVER,
            &[Site::Lever, Site::Ashby],
            Mode::Autonomous,
            Site::Ashby,
            Action::Submit,
            &authorized(),
        ),
        "policy_unknown",
    );
}

#[test]
fn manual_opens_and_supplies_and_drives_nothing() {
    for site in ALL_SITES {
        for action in ALL_ACTIONS {
            let decision = decide(Mode::Manual, site, action, &started());
            if matches!(action, Action::OpenLink | Action::Supply) {
                assert!(matches!(decision, Decision::Allow { .. }), "{site:?}");
            } else {
                assert_refused(decision, "mode_does_not_allow");
            }
        }
    }
}

#[test]
fn a_site_with_no_row_reads_unknown() {
    for site in [Site::RemoteOk, Site::WeWorkRemotely, Site::CompanyPage] {
        assert_refused(
            decide(Mode::Copilot, site, Action::Fill, &started()),
            "policy_unknown",
        );
    }
    // A register that lacks the rows altogether fails closed too.
    assert_refused(
        decide_on(
            "",
            &[],
            Mode::Copilot,
            Site::Greenhouse,
            Action::Fill,
            &started(),
        ),
        "policy_unknown",
    );
}

#[test]
fn fixture_sites_exist_only_in_debug_builds() {
    assert!(matches!(
        permissions::site_of("http://127.0.0.1:9/greenhouse/form").unwrap(),
        Site::Fixture(FixtureSite::Greenhouse)
    ));
    assert!(matches!(
        permissions::site_of("http://127.0.0.1:9/company/form").unwrap(),
        Site::Fixture(FixtureSite::Company)
    ));
    assert_eq!(
        permissions::site_of("http://evil.example/greenhouse/form").unwrap(),
        Site::CompanyPage
    );
    // Any other fixture page is the company fixture; no port or no loopback is no fixture.
    for name in [
        "legal",
        "injection",
        "captcha",
        "mfa",
        "identity",
        "login",
        "two_pages",
        "fake_success",
        "reject_once",
    ] {
        let url = format!("http://127.0.0.1:9/{name}/form");
        assert_eq!(
            permissions::site_of(&url).unwrap(),
            Site::Fixture(FixtureSite::Company),
            "{name}"
        );
        assert!(permissions::link_allowed(&url), "{name}");
    }
    assert_eq!(
        permissions::site_of("http://127.0.0.1/greenhouse/form").unwrap(),
        Site::CompanyPage
    );
    assert_eq!(
        permissions::site_of("https://boards.greenhouse.io/acme/jobs/1").unwrap(),
        Site::Greenhouse
    );
    assert_eq!(
        permissions::site_of("https://www.indeed.com/viewjob?jk=1").unwrap(),
        Site::Indeed
    );
    assert!(permissions::site_of("ftp://x/y").is_err());
}

#[test]
fn a_link_is_allowed_by_the_connector_rule_or_as_a_debug_fixture() {
    assert!(permissions::link_allowed(
        "https://boards.greenhouse.io/acme/jobs/1"
    ));
    assert!(!permissions::link_allowed("http://evil.example/x"));
    assert!(!permissions::link_allowed("https://user@evil.example/x"));
    assert!(permissions::link_allowed("http://127.0.0.1:9/lever/form"));
    assert!(!permissions::link_allowed("http://127.0.0.1/lever/form"));
    assert!(!permissions::link_allowed("http://localhost:9/lever/form"));
}

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

#[tokio::test]
async fn a_grant_is_a_persons_act_and_audited() {
    let (_tmp, db) = setup().await;
    assert!(permissions::grants(&db).await.unwrap().is_empty());

    for actor in ["system", "model", ""] {
        let error = permissions::grant(&db, actor, "lever").await.unwrap_err();
        assert!(matches!(error, Error::Refused(_)), "{actor}");
    }
    assert!(permissions::grants(&db).await.unwrap().is_empty());

    permissions::grant(&db, "gui", "lever").await.unwrap();
    permissions::grant(&db, "cli", "lever").await.unwrap();
    permissions::grant(&db, "cli", "ashby").await.unwrap();
    assert_eq!(permissions::grants(&db).await.unwrap(), ["ashby", "lever"]);

    let error = permissions::revoke(&db, "system", "lever")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Refused(_)));
    permissions::revoke(&db, "gui", "lever").await.unwrap();
    permissions::revoke(&db, "gui", "lever").await.unwrap();
    assert_eq!(permissions::grants(&db).await.unwrap(), ["ashby"]);

    let rows = audit::recent(&db, 20).await.unwrap();
    let of = |action: &str| rows.iter().filter(|r| r.action == action).count();
    assert_eq!(of("autonomous_granted"), 3);
    assert_eq!(of("autonomous_revoked"), 1);
    let revoked = rows
        .iter()
        .find(|r| r.action == "autonomous_revoked")
        .unwrap();
    assert_eq!(revoked.actor, "gui");
    assert_eq!(revoked.target.as_deref(), Some("lever"));
}
