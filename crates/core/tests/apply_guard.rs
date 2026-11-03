//! The page, value, upload, and argument guards. Pure: no browser, no network, no `claude`.

use std::collections::HashSet;
use std::path::Path;

use jobpilot_core::apply::brief::{AuthorizationFact, Brief, BriefAnswer, Fact};
use jobpilot_core::apply::guard::PauseKind::*;
use jobpilot_core::apply::guard::*;
use serde_json::json;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/forms/snapshots")
        .join(format!("{name}.yml"));
    std::fs::read_to_string(path).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn brief(authorization: Option<(&str, &str)>) -> Brief {
    Brief {
        application_id: "a1".into(),
        company: "Acme".into(),
        role: "Engineer".into(),
        link: "https://jobs.example.test/1".into(),
        country: "US".into(),
        facts: vec![Fact {
            name: "name",
            value: "Alex Doe".into(),
        }],
        answers: vec![],
        work_authorization: authorization.map(|(authorized, sponsor)| AuthorizationFact {
            country: "US".into(),
            authorized: authorized.into(),
            needs_sponsorship: sponsor.into(),
        }),
        files: vec![],
    }
}

#[test]
fn the_list_is_the_thirteen_and_no_code_tool() {
    assert_eq!(BROWSER_TOOLS.len(), 13);
    for tool in [
        "browser_evaluate",
        "browser_run_code_unsafe",
        "browser_drop",
        "browser_close",
    ] {
        assert!(!BROWSER_TOOLS.contains(&tool));
    }
}

#[test]
fn each_stop_is_found_on_its_fixture() {
    for (file, kind) in [
        ("captcha", Captcha),
        ("mfa", Mfa),
        ("identity", Identity),
        ("login", Login),
        ("legal", Legal),
    ] {
        assert_eq!(scan(&fixture(file), &HashSet::new()).unwrap().kind, kind);
    }
    assert!(scan(&fixture("form"), &HashSet::new()).is_none());
    assert!(scan(&fixture("injection"), &HashSet::new()).is_none());
}

#[test]
fn a_dialog_is_an_other_stop() {
    let hit = scan("- dialog \"Leave site?\" [ref=e1]", &HashSet::new()).unwrap();
    assert_eq!(hit.kind, Other);
}

#[test]
fn an_acknowledged_line_does_not_hit_and_another_does() {
    let page = fixture("captcha");
    let first = scan(&page, &HashSet::new()).unwrap();
    assert_eq!(first.kind, Captcha);
    let mut acked = HashSet::new();
    acked.insert(hit_key(&first));
    let next = scan(&page, &acked).unwrap();
    assert_eq!(next.kind, Captcha);
    assert_ne!(hit_key(&next), hit_key(&first));
    acked.insert(hit_key(&next));
    let last = scan(&page, &acked).unwrap();
    assert_eq!(last.kind, Other);
    acked.insert(hit_key(&last));
    assert!(scan(&page, &acked).is_none());
}

#[test]
fn legal_words_count_on_every_role() {
    for role in ["checkbox", "generic", "listitem", "heading"] {
        let line = format!("- {role} \"I agree to the terms\" [ref=e1]");
        assert_eq!(scan(&line, &HashSet::new()).unwrap().kind, Legal, "{role}");
    }
    for word in ["I confirm", "I consent to", "I authorize"] {
        let line = format!("- text: {word} the check");
        assert_eq!(scan(&line, &HashSet::new()).unwrap().kind, Legal, "{word}");
    }
}

#[test]
fn quoted_keys_and_hidden_characters_are_seen() {
    assert_eq!(
        scan(&fixture("quoted_legal"), &HashSet::new())
            .unwrap()
            .kind,
        Legal
    );
    assert_eq!(
        scan(&fixture("quoted_login"), &HashSet::new())
            .unwrap()
            .kind,
        Login
    );
    assert!(submit_target(&fixture("quoted_submit"), "e7"));
    assert_eq!(
        label_of(&fixture("quoted_submit"), "e7").as_deref(),
        Some("Submit: final step")
    );
    let hidden = "- paragraph: I cer\u{200c}tify this";
    assert_eq!(scan(hidden, &HashSet::new()).unwrap().kind, Legal);
    let hidden = "- text: cap\u{2060}tcha";
    assert_eq!(scan(hidden, &HashSet::new()).unwrap().kind, Captcha);
}

#[test]
fn a_nameless_choice_is_found() {
    let page = fixture("quoted_submit");
    assert!(unnamed_choice(&page, "e8"));
    assert!(unnamed_choice(&page, "e9"));
    assert!(!unnamed_choice(&page, "e10"));
    assert!(!unnamed_choice(&page, "e7"));
    assert_eq!(role_of(&page, "e8").as_deref(), Some("checkbox"));
}

#[test]
fn the_ack_key_holds_the_whole_line() {
    let long = "x".repeat(400);
    let a = format!("- checkbox \"Background {long}: I agree\" [ref=e6]");
    let b = format!("- checkbox \"Arbitration {long}: I agree\" [ref=e6]");
    let (a, b) = (
        scan(&a, &HashSet::new()).unwrap(),
        scan(&b, &HashSet::new()).unwrap(),
    );
    assert_ne!(hit_key(&a), hit_key(&b));
}

#[test]
fn scan_takes_at_most_300_characters_of_a_line() {
    let line = format!("- paragraph: {} i certify", "x".repeat(5000));
    let hit = scan(&line, &HashSet::new()).unwrap();
    assert_eq!(hit.kind, Legal);
    assert!(hit.line.chars().count() <= 300);
    assert!(hit.line.contains("i certify"));
}

#[test]
fn a_filename_or_a_file_url_is_refused() {
    assert_eq!(
        refused_args("browser_snapshot", &json!({"filename": "x.md"}), false),
        Some("filename_refused")
    );
    assert_eq!(
        refused_args(
            "browser_navigate",
            &json!({"url": "file:///etc/passwd"}),
            false
        ),
        Some("url_refused")
    );
    assert_eq!(
        refused_args(
            "browser_navigate",
            &json!({"url": "http://127.0.0.1:9/x"}),
            false
        ),
        Some("url_refused")
    );
    assert_eq!(
        refused_args(
            "browser_navigate",
            &json!({"url": "http://127.0.0.1:9/x"}),
            true
        ),
        None
    );
    for url in [
        "data:text/html,x",
        "javascript:alert(1)",
        "about:blank",
        "chrome://settings",
    ] {
        assert_eq!(
            refused_args("browser_tabs", &json!({"url": url}), true),
            Some("url_refused")
        );
    }
    assert_eq!(
        refused_args(
            "browser_navigate",
            &json!({"url": "https://jobs.example.test/a"}),
            false
        ),
        None
    );
}

#[test]
fn isolation_and_submit_keys_are_refused() {
    let nested = json!({"fields": [{"target": "e1", "filename": "x"}]});
    assert_eq!(
        refused_args("browser_fill_form", &nested, false),
        Some("filename_refused")
    );
    assert_eq!(
        refused_args("browser_tabs", &json!({"isolatedContext": true}), false),
        Some("isolated_context_refused")
    );
    assert_eq!(
        refused_args(
            "browser_type",
            &json!({"target": "e1", "text": "a", "submit": true}),
            false
        ),
        Some("submit_key_refused")
    );
    assert_eq!(
        refused_args(
            "browser_type",
            &json!({"target": "e1", "text": "a", "submit": false}),
            false
        ),
        None
    );
}

#[test]
fn only_the_two_files_upload() {
    let tmp = tempfile::tempdir().unwrap();
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("resume.pdf"), b"r").unwrap();
    std::fs::write(files.join("cover-letter.pdf"), b"c").unwrap();
    std::fs::write(files.join("other.pdf"), b"o").unwrap();
    std::fs::write(tmp.path().join("secret"), b"s").unwrap();
    let p = |name: &str| files.join(name).to_str().unwrap().to_owned();

    assert!(upload_ok(&[], &files));
    assert!(upload_ok(&[p("resume.pdf")], &files));
    assert!(upload_ok(&[p("resume.pdf"), p("cover-letter.pdf")], &files));
    assert!(!upload_ok(&[p("other.pdf")], &files));
    assert!(!upload_ok(&[p("resume.pdf"), p("other.pdf")], &files));
    assert!(!upload_ok(&[p("../secret")], &files));
    assert!(!upload_ok(&[p("missing.pdf")], &files));
    assert!(!upload_ok(&["~/.ssh/id_rsa".into()], &files));
    assert!(!upload_ok(
        &[tmp.path().join("secret").to_str().unwrap().into()],
        &files
    ));
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_files_pointing_out_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let secret = tmp.path().join("secret.pdf");
    std::fs::write(&secret, b"s").unwrap();
    std::os::unix::fs::symlink(&secret, files.join("resume.pdf")).unwrap();
    let path = files.join("resume.pdf").to_str().unwrap().to_owned();
    assert!(!upload_ok(&[path], &files));
}

#[test]
fn a_value_must_be_a_fact() {
    let allowed = strings(&["Alex Doe", "alex@example.test", "+1 415 555 0100"]);
    assert!(value_ok("alex doe", &allowed));
    assert!(value_ok("Alex", &allowed));
    assert!(value_ok("  Alex   Doe ", &allowed));
    assert!(value_ok("", &allowed));
    assert!(value_ok("4155550100", &["+1 415 555 0100".into()]));
    assert!(!value_ok("A", &allowed));
    assert!(!value_ok("lex", &allowed));
    assert!(!value_ok("10", &["+1 415 555 0100".into()]));
    assert!(!value_ok("15", &["+1 415 555 0100".into()]));
    assert!(!value_ok("4155", &["+1 415 555 0100".into()]));
    assert!(!value_ok("Call 4155550100 now", &allowed));
    assert!(!value_ok("10 years of Kubernetes", &allowed));
    assert!(!value_ok("Alex Doe, Staff Engineer", &allowed));
    let long = "a".repeat(2500);
    assert!(!value_ok(&long, &[format!("{long} b")]));
    assert!(value_ok(&long, std::slice::from_ref(&long)));
}

#[test]
fn a_submit_button_is_seen_and_a_filter_button_is_not() {
    let page = "- button \"Submit application\" [ref=e1]\n\
                - button \"Send application\" [ref=e2]\n\
                - button \"Finish application\" [ref=e3]\n\
                - button \"Apply\" [ref=e4]\n\
                - button \"Apply now\" [ref=e5]\n\
                - button \"Filter\" [ref=e6]\n\
                - link \"Submit\" [ref=e7]\n\
                - textbox \"Submit notes\" [ref=e8]";
    for yes in ["e1", "e2", "e3", "e7"] {
        assert!(submit_target(page, yes), "{yes}");
    }
    for no in ["e4", "e5", "e6", "e8", "e99"] {
        assert!(!submit_target(page, no), "{no}");
    }
    assert!(submit_target(&fixture("form"), "e14"));
    assert!(!submit_target(&fixture("form"), "e13"));
}

#[test]
fn a_label_comes_from_the_ref_line() {
    let page = fixture("form");
    assert_eq!(label_of(&page, "e4").as_deref(), Some("Email *"));
    assert_eq!(label_of(&page, "e8").as_deref(), Some("Country"));
    assert_eq!(label_of(&page, "e44"), None);
}

#[test]
fn values_in_leaves_every_choice_out_and_fields_in_keeps_it() {
    let select = json!({"target": "e8", "values": ["United States"]});
    assert!(values_in("browser_select_option", &select).is_empty());
    assert_eq!(
        fields_in("browser_select_option", &select),
        vec![("e8".to_string(), "United States".to_string())]
    );

    let fill = json!({"fields": [
        {"target": "e3", "name": "Name", "type": "textbox", "value": "Alex Doe"},
        {"target": "e8", "name": "Country", "type": "combobox", "value": "Canada"},
        {"target": "e10", "name": "Alerts", "type": "checkbox", "value": true},
    ]});
    assert_eq!(
        values_in("browser_fill_form", &fill),
        vec!["Alex Doe".to_string()]
    );
    let fields = fields_in("browser_fill_form", &fill);
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[1], ("e8".to_string(), "Canada".to_string()));

    let typed = json!({"target": "e3", "text": "Alex"});
    assert_eq!(values_in("browser_type", &typed), vec!["Alex".to_string()]);
    assert_eq!(
        fields_in("browser_type", &typed),
        vec![("e3".into(), "Alex".into())]
    );
    let prompt = json!({"accept": true, "promptText": "hello"});
    assert_eq!(
        values_in("browser_handle_dialog", &prompt),
        vec!["hello".to_string()]
    );
    assert!(values_in("browser_click", &json!({"target": "e1"})).is_empty());
}

#[test]
fn work_authorization_needs_the_stated_value_and_never_infers() {
    let authorized = "Are you authorized to work in the US?";
    let sponsor = "Will you require sponsorship?";
    let none = brief(None);
    assert_eq!(field_guard(authorized, "yes", &none), Err(Sensitive));
    assert_eq!(field_guard(sponsor, "no", &none), Err(Sensitive));

    let stated = brief(Some(("yes", "no")));
    assert_eq!(field_guard(authorized, "Yes", &stated), Ok(()));
    assert_eq!(field_guard(authorized, "no", &stated), Err(Sensitive));
    assert_eq!(field_guard(sponsor, "no", &stated), Ok(()));
    assert_eq!(field_guard(sponsor, "yes", &stated), Err(Sensitive));

    let not_stated = brief(Some(("not_stated", "no")));
    assert_eq!(field_guard(authorized, "yes", &not_stated), Err(Sensitive));
}

#[test]
fn a_legal_label_is_never_filled() {
    let b = brief(None);
    for value in ["yes", "I agree", "", "Alex Doe"] {
        assert_eq!(
            field_guard("I certify the above is true", value, &b),
            Err(Legal)
        );
        assert_eq!(field_guard("Electronic signature", value, &b), Err(Legal));
    }
}

#[test]
fn a_sensitive_label_takes_only_the_released_answer_for_its_question() {
    let mut b = brief(Some(("yes", "no")));
    b.answers = vec![
        BriefAnswer {
            question: "Gender".into(),
            answer: "Female".into(),
        },
        BriefAnswer {
            question: "I am not a protected veteran".into(),
            answer: "yes".into(),
        },
    ];
    assert_eq!(field_guard("Gender", "Female", &b), Ok(()));
    assert_eq!(field_guard("What is your gender?", "female", &b), Ok(()));
    assert_eq!(field_guard("Gender", "male", &b), Err(Sensitive));
    assert_eq!(field_guard("Gender", "no", &b), Err(Sensitive));
    assert_eq!(
        field_guard("Do you have a disability?", "no", &b),
        Err(Sensitive)
    );
    assert_eq!(
        field_guard("I am not a protected veteran", "a protected veteran", &b),
        Err(Sensitive)
    );
    assert_eq!(
        field_guard("I am not a protected veteran", "yes", &b),
        Ok(())
    );
}

#[test]
fn a_sensitive_label_takes_only_an_approved_value() {
    let mut b = brief(None);
    b.facts.push(Fact {
        name: "email",
        value: "alex@example.test".into(),
    });
    assert_eq!(
        field_guard("Date of birth", "1990-01-01", &b),
        Err(Sensitive)
    );
    assert_eq!(field_guard("Preferred name", "Anything", &b), Ok(()));
}

#[test]
fn only_quiet_keys_are_pressed() {
    for key in [
        "Tab",
        "Shift+Tab",
        "Escape",
        "ArrowDown",
        "Backspace",
        "PageUp",
        " Tab ",
    ] {
        assert_eq!(
            refused_args("browser_press_key", &json!({"key": key}), false),
            None,
            "{key}"
        );
    }
    for key in ["Enter", "a", "1", "Space", "Control+a", "tab", ""] {
        assert_eq!(
            refused_args("browser_press_key", &json!({"key": key}), false),
            Some("key_refused"),
            "{key}"
        );
    }
    assert_eq!(
        refused_args("browser_press_key", &json!({}), false),
        Some("key_refused")
    );
}

#[test]
fn a_slider_is_value_checked() {
    let fill = json!({"fields": [
        {"target": "e4", "type": "slider", "value": "20 years of Kubernetes"},
    ]});
    assert_eq!(
        values_in("browser_fill_form", &fill),
        vec!["20 years of Kubernetes".to_string()]
    );
}

#[test]
fn a_target_is_a_ref_the_snapshot_holds() {
    let page = fixture("form");
    assert!(target_known(&page, "e4"));
    assert!(!target_known(&page, "e44"));
    assert!(!target_known(&page, "e"));
    assert!(!target_known(&page, ""));
    assert!(!target_known(&page, "button[type=submit]"));
    assert!(!target_known(
        &page,
        "internal:role=button[name=\"Submit application\"]"
    ));
    assert!(!target_known(&page, "e4]: x [ref=e4"));
    let click = json!({"target": "button[type=submit]"});
    assert_eq!(
        targets_in("browser_click", &click),
        vec!["button[type=submit]".to_string()]
    );
    let fill =
        json!({"fields": [{"ref": "e3", "type": "textbox", "value": "a"}, {"target": "e8"}]});
    assert_eq!(
        targets_in("browser_fill_form", &fill),
        vec!["e3".to_string(), "e8".to_string()]
    );
    assert_eq!(
        targets_in("browser_select_option", &json!({"target": "e8"})),
        vec!["e8".to_string()]
    );
    assert!(targets_in("browser_navigate", &json!({"url": "https://a.test"})).is_empty());
    assert_eq!(
        targets_in("browser_hover", &json!({"target": 5})),
        vec!["".to_string()]
    );
}

#[test]
fn authorization_polarity_and_country_are_checked() {
    let us = brief(Some(("yes", "no")));
    let mixed = "Are you legally authorized to work in the United States without sponsorship?";
    assert_eq!(field_guard(mixed, "yes", &us), Err(Sensitive));
    assert_eq!(field_guard(mixed, "no", &us), Err(Sensitive));
    assert_eq!(
        field_guard("Are you authorized to work in the US?", "yes", &us),
        Ok(())
    );
    assert_eq!(
        field_guard(
            "Are you authorized to work in the United States?",
            "yes",
            &us
        ),
        Ok(())
    );
    let canada = "Are you authorized to work in Canada?";
    assert_eq!(field_guard(canada, "yes", &us), Err(Sensitive));
    let mut ca = brief(Some(("yes", "no")));
    ca.work_authorization.as_mut().unwrap().country = "CA".into();
    assert_eq!(field_guard(canada, "yes", &ca), Ok(()));
}

#[test]
fn legal_is_checked_before_authorization() {
    let b = brief(Some(("yes", "no")));
    assert_eq!(
        field_guard("I certify I am authorized to work", "yes", &b),
        Err(Legal)
    );
}

#[test]
fn urls_with_userinfo_and_relative_uploads_are_refused() {
    for url in [
        "https://user:pass@x.test/",
        "https://jobs.example.com@evil.test/",
    ] {
        assert_eq!(
            refused_args("browser_navigate", &json!({"url": url}), false),
            Some("url_refused")
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("resume.pdf"), b"r").unwrap();
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&files).unwrap();
    let relative = upload_ok(&["resume.pdf".into()], &files);
    std::env::set_current_dir(cwd).unwrap();
    assert!(!relative);
}
