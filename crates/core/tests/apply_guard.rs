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

/// A quick-apply modal holds the form itself: the owner's run goes on through it. A dialog of
/// text and buttons, and any alertdialog, still stops.
#[test]
fn a_dialog_holding_a_form_is_no_stop() {
    let modal = "- dialog \"Apply to Fennick Labs\" [ref=e1]:\n\
                 \x20 - heading \"Contact info\" [level=2] [ref=e2]\n\
                 \x20 - generic [ref=e3]:\n\
                 \x20   - textbox \"Email *\" [ref=e4]\n\
                 \x20 - button \"Submit application\" [ref=e5]\n\
                 - button \"Elsewhere\" [ref=e6]";
    assert_eq!(scan(modal, &HashSet::new()), None);
    for control in [
        "textbox",
        "combobox",
        "listbox",
        "checkbox",
        "radio",
        "spinbutton",
        "slider",
        "searchbox",
    ] {
        let page = format!("- dialog \"Apply\" [ref=e1]:\n  - {control} \"Field\" [ref=e2]");
        assert_eq!(scan(&page, &HashSet::new()), None, "{control}");
    }

    let notice = "- dialog \"Leave site?\" [ref=e1]:\n\
                  \x20 - paragraph [ref=e2]: Changes you made may not be saved.\n\
                  \x20 - button \"Leave\" [ref=e3]\n\
                  - textbox \"Outside\" [ref=e4]";
    assert_eq!(scan(notice, &HashSet::new()).unwrap().kind, Other);
    let alert = "- alertdialog \"Session ending\" [ref=e1]:\n  - textbox \"Note\" [ref=e2]";
    assert_eq!(scan(alert, &HashSet::new()).unwrap().kind, Other);
}

/// Every other stop is found inside a modal that holds a form.
#[test]
fn a_stop_inside_a_form_dialog_still_pauses() {
    for (line, kind) in [
        ("- paragraph [ref=e3]: Verify you are human", Captcha),
        ("- textbox \"Verification code\" [ref=e3]", Mfa),
        ("- paragraph [ref=e3]: Verify your identity", Identity),
        ("- textbox \"Password\" [ref=e3]", Login),
        ("- checkbox \"I certify my answers\" [ref=e3]", Legal),
    ] {
        let page =
            format!("- dialog \"Apply\" [ref=e1]:\n  - textbox \"Email *\" [ref=e2]\n  {line}");
        assert_eq!(scan(&page, &HashSet::new()).unwrap().kind, kind, "{line}");
    }
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
    let sponsor = "Will you require sponsorship to work in the US?";
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
    for key in ["Tab", "Shift+Tab", "Escape", " Tab "] {
        assert_eq!(
            refused_args("browser_press_key", &json!({"key": key}), false),
            None,
            "{key}"
        );
    }
    for key in [
        "Enter",
        "a",
        "1",
        "Space",
        "Control+a",
        "tab",
        "",
        "ArrowDown",
        "ArrowLeft",
        "Backspace",
        "Delete",
        "Home",
        "End",
        "PageUp",
        "PageDown",
    ] {
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

#[test]
fn a_ref_quoted_inside_a_name_is_not_the_element() {
    let spoof = "- checkbox \"Subscribe [ref=e9]\" [ref=e5]\n\
                 - checkbox \"I certify this\" [ref=e9]";
    assert!(target_known(spoof, "e9"));
    assert_eq!(label_of(spoof, "e9").as_deref(), Some("I certify this"));
    assert_eq!(label_of(spoof, "e5").as_deref(), Some("Subscribe [ref=e9]"));
    let escaped = "- checkbox \"say \\\"hi [ref=e9]\" [ref=e5]\n\
                   - checkbox \"Real\" [ref=e9]";
    assert_eq!(label_of(escaped, "e9").as_deref(), Some("Real"));
    let only_quoted = "- checkbox \"Subscribe [ref=e9]\" [ref=e5]";
    assert!(!target_known(only_quoted, "e9"));
    let twice = "- button \"Submit application\" [ref=e9]\n- checkbox \"Other\" [ref=e9]";
    assert!(!target_known(twice, "e9"));
    assert_eq!(label_of(twice, "e9"), None);
    assert_eq!(role_of(twice, "e9"), None);
    assert!(!submit_target(twice, "e9"));
    assert!(!unnamed_choice(twice, "e9"));
}

#[test]
fn a_bare_word_target_is_unknown() {
    let page = "- generic [ref=e3]: Next step [ref=button] here\n\
                - button \"Go\" [ref=e5x]\n\
                - button \"Id\" [ref=#id]\n\
                - button \"Frame\" [ref=f1e2]\n\
                - button \"Plain\" [ref=e7]";
    for bad in [
        "button", "select", "input", "#id", "e5x", "e", "f1", "ee5", "fe5",
    ] {
        assert!(!target_known(page, bad), "{bad}");
        assert_eq!(label_of(page, bad), None, "{bad}");
    }
    assert!(target_known(page, "e7"));
    assert!(target_known(page, "f1e2"));
}

#[test]
fn a_question_matches_on_whole_words() {
    let mut b = brief(None);
    b.answers = vec![
        BriefAnswer {
            question: "Do you identify as transgender?".into(),
            answer: "No".into(),
        },
        BriefAnswer {
            question: "Age".into(),
            answer: "Over 18".into(),
        },
    ];
    assert_eq!(field_guard("Gender", "No", &b), Err(Sensitive));
    assert_eq!(field_guard("Expected wage", "Over 18", &b), Err(Sensitive));
    assert_eq!(
        field_guard("Disability status (see page 2)", "Over 18", &b),
        Err(Sensitive)
    );
    assert_eq!(field_guard("Age", "Over 18", &b), Ok(()));
    assert_eq!(field_guard("Your age?", "over 18", &b), Ok(()));
}

fn groups() -> String {
    [
        "- form \"Apply\" [ref=e1]:",
        "  - radiogroup \"Gender\" [ref=e2]:",
        "    - radio \"Male\" [ref=e3]",
        "    - radio \"Female\" [ref=e4]",
        "  - radiogroup \"Are you authorized to work in the US?\" [ref=e5]:",
        "    - radio \"Yes\" [ref=e6]",
        "    - radio \"No\" [ref=e7]",
        "  - radiogroup \"Preferred shift\" [ref=e8]:",
        "    - radio \"Night\" [ref=e9]",
        "    - radio [ref=e10]",
        "  - checkbox \"I certify this\" [ref=e11]",
        "  - checkbox [ref=e12]",
        "- checkbox [ref=e13]",
        "- checkbox \"Remember me\" [ref=e14]",
    ]
    .join("\n")
}

#[test]
fn a_choice_is_judged_by_its_question() {
    let mut b = brief(Some(("yes", "no")));
    b.answers = vec![BriefAnswer {
        question: "Gender".into(),
        answer: "Female".into(),
    }];
    assert_eq!(choice_guard(&groups(), "e3", &b), Err(Sensitive));
    assert_eq!(choice_guard(&groups(), "e4", &b), Ok(()));
    // The question is searched within the choice's own field (round 5), so the groups above
    // do not weigh on later choices.
    assert_eq!(choice_guard(&groups(), "e6", &b), Ok(()));
    assert_eq!(choice_guard(&groups(), "e7", &b), Err(Sensitive));
    assert_eq!(choice_guard(&groups(), "e9", &b), Ok(()));
    assert_eq!(choice_guard(&groups(), "e10", &b), Err(Legal));
    assert_eq!(choice_guard(&groups(), "e11", &b), Err(Legal));
    assert_eq!(choice_guard(&groups(), "e12", &b), Err(Legal));
    assert_eq!(choice_guard(&groups(), "e13", &b), Err(Legal));
    assert_eq!(choice_guard(&groups(), "e14", &b), Ok(()));
    assert_eq!(choice_guard(&groups(), "e2", &b), Err(Sensitive));
    let alone = |lines: &[&str]| page(lines);
    let authorized = alone(&[
        "- radiogroup \"Are you authorized to work in the US?\" [ref=e5]:",
        "  - radio \"Yes\" [ref=e6]",
        "  - radio \"No\" [ref=e7]",
    ]);
    assert_eq!(choice_guard(&authorized, "e6", &b), Ok(()));
    assert_eq!(choice_guard(&authorized, "e7", &b), Err(Sensitive));
    let none = brief(None);
    assert_eq!(choice_guard(&authorized, "e6", &none), Err(Sensitive));
    let shift = alone(&[
        "- radiogroup \"Preferred shift\" [ref=e8]:",
        "  - radio \"Night\" [ref=e9]",
        "  - radio [ref=e10]",
    ]);
    assert_eq!(choice_guard(&shift, "e9", &b), Ok(()));
    assert_eq!(choice_guard(&shift, "e10", &b), Err(Legal));
}

#[test]
fn typing_slowly_and_scoped_snapshots_are_refused() {
    assert_eq!(
        refused_args(
            "browser_type",
            &json!({"target": "e1", "text": "a", "slowly": true}),
            false
        ),
        Some("slowly_refused")
    );
    assert_eq!(
        refused_args(
            "browser_type",
            &json!({"target": "e1", "text": "a", "slowly": false}),
            false
        ),
        None
    );
    for args in [
        json!({"target": "e1"}),
        json!({"depth": 2}),
        json!({"target": "#a", "depth": 1}),
    ] {
        assert_eq!(
            refused_args("browser_snapshot", &args, false),
            Some("snapshot_scope_refused")
        );
    }
    assert_eq!(refused_args("browser_snapshot", &json!({}), false), None);
}

#[test]
fn visa_and_negated_authorization_labels_pause() {
    let b = brief(Some(("yes", "yes")));
    for label in [
        "Do you require a visa to work in the US?",
        "Are you NOT authorized to work in the US?",
        "Aren't you authorized to work in the US?",
        "Are you authorized to work in the US without sponsorship?",
    ] {
        assert_eq!(field_guard(label, "yes", &b), Err(Sensitive), "{label}");
    }
    assert_eq!(
        field_guard("Will you require sponsorship to work in the US?", "yes", &b),
        Ok(())
    );
}

#[test]
fn authorization_must_name_the_facts_country() {
    let us = brief(Some(("yes", "no")));
    for label in [
        "Are you authorized to work in the United States?",
        "Are you authorized to work in the United States of America?",
        "Are you authorized to work in the U.S.?",
        "Are you authorized to work in the USA?",
    ] {
        assert_eq!(field_guard(label, "yes", &us), Ok(()), "{label}");
    }
    for label in [
        "Are you authorized to work in the UK?",
        "Are you authorized to work in Europe?",
        "Are you authorized to work in this country?",
        "Are you authorized to work?",
        "Are you authorized to work in England?",
        "Are you authorized to work in New Jersey?",
        "Are you authorized to work in New Jersey, US?",
        "Are you authorized to work in America?",
    ] {
        assert_eq!(field_guard(label, "yes", &us), Err(Sensitive), "{label}");
    }
    let mut gb = brief(Some(("yes", "no")));
    gb.work_authorization.as_mut().unwrap().country = "GB".into();
    assert_eq!(
        field_guard("Are you authorized to work in the UK?", "yes", &gb),
        Ok(())
    );
    assert_eq!(
        field_guard("Are you authorized to work in Great Britain?", "yes", &gb),
        Ok(())
    );
    assert_eq!(
        field_guard("Are you authorized to work in the US?", "yes", &gb),
        Err(Sensitive)
    );
}

#[test]
fn inline_text_is_not_a_name() {
    let page = "- textbox [ref=e5]: \"yes\"\n\
                - checkbox [ref=e6]: \"Note: I accept the terms\"\n\
                - checkbox \"Named\" [ref=e7]: \"x\"";
    assert_eq!(label_of(page, "e5"), None);
    assert_eq!(label_of(page, "e6"), None);
    assert!(unnamed_choice(page, "e6"));
    assert_eq!(label_of(page, "e7").as_deref(), Some("Named"));
}

#[test]
fn an_ack_survives_a_state_change() {
    let before = scan("- checkbox \"I certify this\" [ref=e6]", &HashSet::new()).unwrap();
    let mut acked = HashSet::new();
    acked.insert(hit_key(&before));
    for after in [
        "- checkbox \"I certify this\" [checked] [ref=e6]",
        "- checkbox \"I certify this\" [checked=mixed] [active] [ref=e6]",
        "- checkbox \"I certify this\" [focused] [disabled] [expanded] [pressed] [selected] [ref=e6]",
    ] {
        assert!(scan(after, &acked).is_none(), "{after}");
    }
    assert!(scan("- checkbox \"I certify that\" [ref=e6]", &acked).is_some());
}

fn page(lines: &[&str]) -> String {
    lines.join("\n")
}

fn gender_brief() -> Brief {
    let mut b = brief(Some(("yes", "no")));
    b.answers = vec![BriefAnswer {
        question: "Gender".into(),
        answer: "Female".into(),
    }];
    b
}

#[test]
fn every_choice_role_is_judged() {
    let b = gender_brief();
    let list = page(&[
        "- listbox \"Gender\" [ref=e1]:",
        "  - option \"Male\" [ref=e2]",
        "  - option \"Female\" [ref=e3]",
        "- menu \"Gender\" [ref=e4]:",
        "  - menuitemradio \"Male\" [ref=e5]",
        "- switch \"I am a protected veteran\" [ref=e6]",
        "- button \"Next\" [ref=e7]",
        "- generic [ref=e8]",
    ]);
    assert_eq!(choice_guard(&list, "e2", &b), Err(Sensitive));
    assert_eq!(choice_guard(&list, "e3", &b), Ok(()));
    assert_eq!(choice_guard(&list, "e5", &b), Err(Sensitive));
    assert_eq!(choice_guard(&list, "e6", &b), Err(Sensitive));
    for (target, choice) in [
        ("e2", true),
        ("e6", true),
        ("e7", false),
        ("e8", true),
        ("e99", true),
    ] {
        assert_eq!(is_choice(&list, target), choice, "{target}");
    }
    for role in ["button", "link", "tab", "combobox", "textbox", "searchbox"] {
        let one = format!("- {role} \"x\" [ref=e1]");
        assert!(!is_choice(&one, "e1"), "{role}");
    }
}

#[test]
fn the_options_own_class_counts() {
    let b = gender_brief();
    let steps = page(&[
        "- group \"Step 4 of 5\" [ref=e1]:",
        "  - radio \"I am a protected veteran\" [ref=e2]",
        "  - checkbox \"I require visa sponsorship\" [ref=e3]",
        "  - checkbox \"I agree to arbitration\" [ref=e4]",
        "  - radio \"Morning\" [ref=e5]",
    ]);
    assert_eq!(choice_guard(&steps, "e2", &b), Err(Sensitive));
    assert_eq!(choice_guard(&steps, "e3", &b), Err(Sensitive));
    assert_eq!(choice_guard(&steps, "e4", &b), Err(Legal));
    assert_eq!(choice_guard(&steps, "e5", &b), Ok(()));
}

#[test]
fn a_sibling_label_and_every_ancestor_are_questions() {
    let b = gender_brief();
    let sibling = page(&[
        "- generic [ref=e1]:",
        "  - text: Gender",
        "  - radiogroup [ref=e2]:",
        "    - radio \"Male\" [ref=e3]",
        "    - radio \"Female\" [ref=e4]",
    ]);
    assert_eq!(choice_guard(&sibling, "e3", &b), Err(Sensitive));
    assert_eq!(choice_guard(&sibling, "e4", &b), Ok(()));
    let heading = page(&[
        "- form \"Apply for Engineer\" [ref=e1]:",
        "  - heading \"Gender\" [level=3] [ref=e5]",
        "  - radiogroup [ref=e2]:",
        "    - radio \"Male\" [ref=e3]",
    ]);
    assert_eq!(choice_guard(&heading, "e3", &b), Err(Sensitive));
    let hostile = page(&[
        "- radiogroup \"Gender\" [ref=e1]:",
        "  - listitem \"Shift A\" [ref=e2]:",
        "    - radio \"Male\" [ref=e3]",
    ]);
    assert_eq!(choice_guard(&hostile, "e3", &b), Err(Sensitive));
    let legal = page(&[
        "- paragraph: I certify the above is true",
        "- checkbox \"Yes\" [ref=e1]",
    ]);
    assert_eq!(choice_guard(&legal, "e1", &b), Err(Legal));
}

#[test]
fn one_label_matching_two_answers_pauses() {
    let mut b = brief(None);
    b.answers = vec![
        BriefAnswer {
            question: "Gender".into(),
            answer: "Female".into(),
        },
        BriefAnswer {
            question: "Is your gender identity the same as assigned at birth?".into(),
            answer: "Yes".into(),
        },
    ];
    assert_eq!(field_guard("Gender", "Yes", &b), Err(Sensitive));
    assert_eq!(field_guard("Gender", "Female", &b), Err(Sensitive));
}

#[test]
fn authorization_uses_a_strict_form() {
    let us = brief(Some(("yes", "no")));
    let sponsor = brief(Some(("yes", "yes")));
    assert_eq!(
        field_guard(
            "Are you legally authorized to work in the United States?",
            "yes",
            &us
        ),
        Ok(())
    );
    let future =
        "Will you now or in the future require sponsorship for employment visa status in the U.S.?";
    assert_eq!(field_guard(future, "yes", &sponsor), Ok(()));
    assert_eq!(field_guard(future, "no", &sponsor), Err(Sensitive));
    for label in [
        "Are you a U.S. citizen?",
        "Are you a permanent resident of the United States?",
        "Do you hold a green card for the US?",
        "Do you need a work permit to work in the US?",
        "Are you unauthorized to work in the US?",
        "Are you authorized to work in the US and Canada?",
        "Are you authorized to work in the United States or the United Kingdom?",
        "Are you authorized to work in Latin America?",
        "Are you authorized to work in the U.S. Virgin Islands?",
        "Are you authorized to work for us in Canada?",
        "Are you authorized to work in Europe?",
        "Are you authorized to work in the EU?",
        "Are you authorized to work in the US, either now or later?",
        "Do you require a visa to work in the US?",
        "Are you authorized to work in the US or any territory?",
    ] {
        assert_eq!(field_guard(label, "yes", &us), Err(Sensitive), "{label}");
    }
    let mut india = brief(Some(("yes", "no")));
    india.work_authorization.as_mut().unwrap().country = "IN".into();
    assert_eq!(
        field_guard("ARE YOU AUTHORIZED TO WORK IN CANADA?", "yes", &india),
        Err(Sensitive)
    );
    assert_eq!(
        field_guard("Are you authorized to work in India?", "yes", &india),
        Ok(())
    );
    let mut gb = brief(Some(("yes", "no")));
    gb.work_authorization.as_mut().unwrap().country = "GB".into();
    assert_eq!(
        field_guard("Are you authorized to work in New South Wales?", "yes", &gb),
        Err(Sensitive)
    );
    assert_eq!(
        field_guard("Are you authorized to work in Britain?", "yes", &gb),
        Err(Sensitive)
    );
    assert_eq!(
        field_guard("Are you authorized to work in the U.K.?", "yes", &gb),
        Ok(())
    );
}

fn released() -> Brief {
    let mut b = brief(Some(("yes", "no")));
    b.answers = vec![
        BriefAnswer {
            question: "What is your gender?".into(),
            answer: "Female".into(),
        },
        BriefAnswer {
            question: "Are you a protected veteran?".into(),
            answer: "No".into(),
        },
    ];
    b
}

#[test]
fn work_authorization_off_the_templates_pauses() {
    let us = brief(Some(("yes", "no")));
    for label in [
        "Are you authorized to work in the US? (No sponsorship)",
        "Are you legally authorized to work in the U.S. with no sponsorship required?",
        "Are you legally authorized to work in the United States, excluding sponsorship?",
        "Are you authorized to work in the United States without the need for sponsorship?",
        "Are you authorized to work in the U.S. free of sponsorship?",
        "Are you eligible to work in the US with sponsorship?",
        "Are you authorized to work in the U.S. unless sponsored?",
        "Can you work in the US with no visa sponsorship?",
        "Is sponsorship unnecessary for you to work in the US?",
        "Would you need us to sponsor you to work in the U.S.?",
        "Are you currently authorized to work in the U.S. on an H-1B?",
        "Are you authorized to work in the United States on OPT?",
        "Are you authorized to work in the US only with an EAD?",
        "Have you ever been denied authorization to work in the US?",
        "Are you authorized to work in the US for the next 12 months?",
        "Are you authorized to work in the US but require sponsorship later?",
        "Are you authorized to work in the us?",
        "Are you authorized to work in the Us?",
        "Are you authorized to work in Britain?",
    ] {
        for value in ["yes", "no"] {
            assert_eq!(
                field_guard(label, value, &us),
                Err(Sensitive),
                "{label} {value}"
            );
        }
    }
}

#[test]
fn every_work_authorization_template_takes_its_stated_value() {
    let fact = brief(Some(("yes", "no")));
    let authorized = [
        "Are you legally authorized to work in {C}?",
        "Are you authorized to work in {C}?",
        "Are you legally eligible to work in {C}?",
        "Are you eligible to work in {C}?",
        "Are you legally permitted to work in {C}?",
        "Are you authorized to work in {C} for any employer?",
        "* Are you legally permitted to work in {C} for any employer?",
        "Required: Are you eligible to work in {C}?",
    ];
    let mut sponsorship = Vec::new();
    for verb in ["require", "need"] {
        for form in [
            "Will you {V} sponsorship to work in {C}?",
            "Will you {V} visa sponsorship to work in {C}?",
            "Will you {V} sponsorship for employment visa status in {C}?",
            "Will you now or in the future {V} sponsorship for employment visa status (e.g., H-1B visa status) in {C}?",
            "Do you {V} sponsorship to work in {C}?",
            "Do you {V} visa sponsorship to work in {C}?",
            "Will you {V} immigration sponsorship to work in {C}?",
        ] {
            sponsorship.push(form.replace("{V}", verb));
        }
    }
    for country in [
        "the United States",
        "United States",
        "the United States of America",
        "the US",
        "the U.S.",
        "the USA",
        "the U.S.A.",
    ] {
        for form in authorized {
            let label = form.replace("{C}", country);
            assert_eq!(field_guard(&label, "Yes", &fact), Ok(()), "{label}");
            assert_eq!(field_guard(&label, "no", &fact), Err(Sensitive), "{label}");
        }
        for form in &sponsorship {
            let label = form.replace("{C}", country);
            assert_eq!(field_guard(&label, "No", &fact), Ok(()), "{label}");
            assert_eq!(field_guard(&label, "yes", &fact), Err(Sensitive), "{label}");
        }
    }
    let mut gb = brief(Some(("yes", "no")));
    gb.work_authorization.as_mut().unwrap().country = "GB".into();
    for country in ["the United Kingdom", "the UK", "the U.K.", "Great Britain"] {
        let label = format!("Are you authorized to work in {country}?");
        assert_eq!(field_guard(&label, "yes", &gb), Ok(()), "{label}");
        assert_eq!(field_guard(&label, "no", &gb), Err(Sensitive), "{label}");
    }
}

#[test]
fn every_line_before_a_choice_is_its_question() {
    let b = released();
    for (layout, snap) in [
        (
            "label nested with asterisk",
            "- generic [ref=e0]:\n  - generic [ref=e1]:\n    - text: Gender\n    - generic [ref=e9]: \"*\"\n  - radiogroup [ref=e4]:\n    - radio \"Male\" [ref=e2]",
        ),
        (
            "help text between",
            "- generic [ref=e0]:\n  - generic [ref=e1]: Gender\n  - paragraph [ref=e8]: Select the option that best describes you\n  - radiogroup [ref=e4]:\n    - radio \"Male\" [ref=e2]",
        ),
        (
            "label as strong child",
            "- generic [ref=e0]:\n  - paragraph [ref=e1]:\n    - strong [ref=e7]: Gender\n  - radiogroup [ref=e4]:\n    - radio \"Male\" [ref=e2]",
        ),
        (
            "label after group",
            "- generic [ref=e0]:\n  - radiogroup [ref=e4]:\n    - radio \"Male\" [ref=e2]\n  - text: Gender",
        ),
        (
            "heading in a sibling container",
            "- generic [ref=e0]:\n  - generic [ref=e1]:\n    - heading \"Gender\" [level=3] [ref=e3]\n  - radio \"Male\" [ref=e2]",
        ),
        (
            "generic name at the root",
            "- generic \"Gender\" [ref=e1]\n- radio \"Male\" [ref=e2]",
        ),
        (
            "table cell",
            "- row [ref=e0]:\n  - cell \"Gender\" [ref=e1]\n  - cell [ref=e5]:\n    - radio \"Male\" [ref=e2]",
        ),
        (
            "strong sibling",
            "- generic [ref=e0]:\n  - strong [ref=e1]: Gender\n  - radio \"Male\" [ref=e2]",
        ),
        (
            "img sibling",
            "- generic [ref=e0]:\n  - img \"Gender\" [ref=e1]\n  - radio \"Male\" [ref=e2]",
        ),
        (
            "plain checkbox named Female",
            "- checkbox \"Female\" [ref=e2]",
        ),
        (
            "label after the group, Female",
            "- generic [ref=e0]:\n  - radiogroup [ref=e4]:\n    - radio \"Female\" [ref=e2]\n  - text: Gender",
        ),
    ] {
        assert!(is_choice(snap, "e2"), "{layout}");
        assert_eq!(choice_guard(snap, "e2", &b), Err(Sensitive), "{layout}");
    }
    let happy = "- group \"Gender\" [ref=e1]:\n  - radio \"Female\" [ref=e2]";
    assert_eq!(choice_guard(happy, "e2", &b), Ok(()));
    let help = "- generic [ref=e0]:\n  - generic [ref=e1]: Gender\n  - paragraph [ref=e8]: Select the option that best describes you\n  - radiogroup [ref=e4]:\n    - radio \"Female\" [ref=e2]";
    assert_eq!(choice_guard(help, "e2", &b), Ok(()));
    let far = page(&[
        "- text: Are you a protected veteran?",
        "- text: one",
        "- text: two",
        "- text: three",
        "- text: four",
        "- text: five",
        "- text: six",
        "- text: seven",
        "- text: eight",
        "- radio \"Yes\" [ref=e2]",
    ]);
    assert_eq!(choice_guard(&far, "e2", &b), Ok(()));
    let near = page(&[
        "- text: Are you a protected veteran?",
        "- text: one",
        "- radio \"Yes\" [ref=e2]",
    ]);
    assert_eq!(choice_guard(&near, "e2", &b), Err(Sensitive));
    // An earlier ungrouped option of the same role and depth is a sibling, so the question
    // above it still counts (final fix, E1).
    let sibling = page(&[
        "- text: Are you a protected veteran?",
        "- radio \"No\" [ref=e1]",
        "- radio \"Yes\" [ref=e2]",
    ]);
    assert_eq!(choice_guard(&sibling, "e2", &b), Err(Sensitive));
}

#[test]
fn a_sensitive_option_needs_one_sensitive_question() {
    let b = released();
    for option in [
        "Male",
        "Woman",
        "Non-binary",
        "Hispanic or Latino",
        "Two or More Races",
        "I don't wish to answer",
        "Decline to self-identify",
        "I am a disabled veteran",
    ] {
        let snap =
            format!("- group \"Preferred shift\" [ref=e1]:\n  - radio \"{option}\" [ref=e2]");
        assert_eq!(choice_guard(&snap, "e2", &b), Err(Sensitive), "{option}");
    }
    let two = "- group \"Gender\" [ref=e1]:\n  - group \"Sex\" [ref=e3]:\n    - radio \"Female\" [ref=e2]";
    assert_eq!(choice_guard(two, "e2", &b), Err(Sensitive));
    let plain = "- group \"Preferred shift\" [ref=e1]:\n  - radio \"Manager track\" [ref=e2]";
    assert_eq!(choice_guard(plain, "e2", &b), Ok(()));
}

#[test]
fn a_button_can_be_a_choice() {
    let b = released();
    let gender = "- group \"Gender\" [ref=e1]:\n  - button \"Male\" [ref=e2]";
    assert!(is_choice(gender, "e2"));
    assert_eq!(choice_guard(gender, "e2", &b), Err(Sensitive));
    let veteran = "- generic [ref=e1]:\n  - text: Are you a protected veteran?\n  - button \"Yes\" [pressed] [ref=e2]\n  - button \"No\" [ref=e3]";
    assert!(is_choice(veteran, "e2"));
    assert_eq!(choice_guard(veteran, "e2", &b), Err(Sensitive));
    assert_eq!(choice_guard(veteran, "e3", &b), Ok(()));
    let bare = "- button \"Yes\" [ref=e2]";
    assert!(is_choice(bare, "e2"));
    let female = "- button \"Female\" [ref=e2]";
    assert!(is_choice(female, "e2"));
    assert_eq!(choice_guard(female, "e2", &b), Err(Sensitive));
    let submit = "- form \"Voluntary self-identification\" [ref=e1]:\n  - text: Gender\n  - button \"Submit application\" [ref=e2]";
    assert!(!is_choice(submit, "e2"));
    assert_eq!(choice_guard(submit, "e2", &b), Ok(()));
    let next =
        "- form \"Apply\" [ref=e1]:\n  - textbox \"Name\" [ref=e3]\n  - button \"Next\" [ref=e2]";
    assert!(!is_choice(next, "e2"));
    assert_eq!(choice_guard(next, "e2", &b), Ok(()));
}

fn application_form() -> String {
    page(&[
        "- main [ref=e1]:",
        "  - form \"Application for Engineer\" [ref=e2]:",
        "    - heading \"Apply for Engineer\" [level=1] [ref=e3]",
        "    - text: Full name",
        "    - textbox \"Full name\" [ref=e4]",
        "    - radiogroup \"Are you legally authorized to work in the United States?\" [ref=e10]:",
        "      - radio \"Yes\" [ref=e11]",
        "      - radio \"No\" [ref=e12]",
        "    - radiogroup \"Will you now or in the future require sponsorship to work in the United States?\" [ref=e13]:",
        "      - radio \"Yes\" [ref=e14]",
        "      - radio \"No\" [ref=e15]",
        "    - heading \"Voluntary self-identification\" [level=2] [ref=e16]",
        "    - group \"Gender\" [ref=e17]:",
        "      - radio \"Male\" [ref=e18]",
        "      - radio \"Female\" [ref=e19]",
        "    - text: Shifts are scheduled weekly",
        "    - group \"Preferred shift\" [ref=e20]:",
        "      - radio \"Morning\" [ref=e21]",
        "      - radio \"Night\" [ref=e22]",
        "    - combobox \"How did you hear about us?\" [ref=e30]",
        "    - checkbox \"I certify that the information above is true\" [ref=e31]",
        "    - button \"Next\" [ref=e32]",
        "    - button \"Submit application\" [ref=e33]",
    ])
}

#[test]
fn a_realistic_form_pauses_only_where_it_must() {
    let form = application_form();
    let b = gender_brief();
    for (target, expected) in [
        ("e11", Ok(())),
        ("e12", Err(Sensitive)),
        ("e14", Err(Sensitive)),
        ("e15", Ok(())),
        ("e18", Err(Sensitive)),
        ("e19", Ok(())),
        ("e21", Ok(())),
        ("e22", Ok(())),
        ("e30", Ok(())),
        ("e31", Err(Legal)),
        ("e32", Ok(())),
        ("e33", Ok(())),
    ] {
        assert_eq!(choice_guard(&form, target, &b), expected, "{target}");
    }
    for plain in ["e4", "e30", "e32", "e33"] {
        assert!(!is_choice(&form, plain), "{plain}");
    }
    for choice in [
        "e11", "e12", "e14", "e15", "e18", "e19", "e21", "e22", "e31",
    ] {
        assert!(is_choice(&form, choice), "{choice}");
    }
}

#[test]
fn yes_and_no_buttons_share_the_question_above_them() {
    let toggles = page(&[
        "- generic [ref=e1]:",
        "  - text: Are you a protected veteran?",
        "  - button \"Yes\" [ref=e2]",
        "  - button \"No\" [ref=e3]",
    ]);
    for (released, ok, paused) in [("Yes", "e2", "e3"), ("No", "e3", "e2")] {
        let mut b = brief(Some(("yes", "no")));
        b.answers = vec![BriefAnswer {
            question: "Are you a protected veteran?".into(),
            answer: released.into(),
        }];
        assert_eq!(choice_guard(&toggles, ok, &b), Ok(()), "{released}");
        assert_eq!(
            choice_guard(&toggles, paused, &b),
            Err(Sensitive),
            "{released}"
        );
    }
}

#[test]
fn ungrouped_options_keep_their_question() {
    let b = released();
    let lever = page(&[
        "- generic [ref=e1]: Are you legally authorized to work in the United States?",
        "- list [ref=e5]:",
        "  - listitem [ref=e6]:",
        "    - generic [ref=e7]:",
        "      - radio \"Yes\" [ref=e2]",
        "      - text: \"Yes\"",
        "  - listitem [ref=e8]:",
        "    - generic [ref=e9]:",
        "      - radio \"No\" [ref=e3]",
        "      - text: \"No\"",
    ]);
    assert_eq!(choice_guard(&lever, "e2", &b), Ok(()));
    assert_eq!(choice_guard(&lever, "e3", &b), Err(Sensitive));
    for (question, first, second) in [
        (
            "Are you a protected veteran?",
            "radio \"No\" [ref=e3]",
            "radio \"Yes\" [ref=e2]",
        ),
        (
            "Will you require sponsorship to work in the United States?",
            "radio \"No\" [ref=e3]",
            "radio \"Yes\" [ref=e2]",
        ),
        (
            "Race",
            "checkbox \"Asian\" [ref=e3]",
            "checkbox \"Middle Eastern or North African\" [ref=e2]",
        ),
        (
            "Sexual orientation",
            "radio \"Heterosexual\" [ref=e3]",
            "radio \"Gay\" [ref=e2]",
        ),
    ] {
        let snap = format!("- generic [ref=e0]:\n  - text: {question}\n  - {first}\n  - {second}");
        assert_eq!(choice_guard(&snap, "e2", &b), Err(Sensitive), "{question}");
    }
    let veteran_no = "- generic [ref=e0]:\n  - text: Are you a protected veteran?\n  - radio \"Yes\" [ref=e3]\n  - radio \"No\" [ref=e2]";
    assert_eq!(choice_guard(veteran_no, "e2", &b), Ok(()));
}

#[test]
fn a_side_list_takes_its_comboboxs_question() {
    let b = released();
    let auth = page(&[
        "- generic [ref=e0]:",
        "  - text: Are you legally authorized to work in the United States?",
        "  - combobox \"Are you legally authorized to work in the United States?\" [expanded] [ref=e4]",
        "  - listbox [ref=e5]:",
        "    - option \"Yes\" [ref=e2]",
        "    - option \"No\" [ref=e3]",
    ]);
    assert_eq!(choice_guard(&auth, "e2", &b), Ok(()));
    assert_eq!(choice_guard(&auth, "e3", &b), Err(Sensitive));
    let gender = page(&[
        "- generic [ref=e0]:",
        "  - text: Gender",
        "  - combobox \"Gender\" [expanded] [ref=e4]",
        "  - listbox [ref=e5]:",
        "    - option \"Male\" [ref=e2]",
        "    - option \"Female\" [ref=e3]",
    ]);
    assert_eq!(choice_guard(&gender, "e2", &b), Err(Sensitive));
    assert_eq!(choice_guard(&gender, "e3", &b), Ok(()));
}

#[test]
fn a_helper_button_does_not_cut_off_the_question() {
    let b = released();
    let clear = page(&[
        "- generic [ref=e0]:",
        "  - text: Are you a protected veteran?",
        "  - radiogroup [ref=e4]:",
        "    - button \"Clear\" [ref=e9]",
        "    - radio \"Yes\" [ref=e2]",
        "    - radio \"No\" [ref=e3]",
    ]);
    assert_eq!(choice_guard(&clear, "e2", &b), Err(Sensitive));
    assert_eq!(choice_guard(&clear, "e3", &b), Ok(()));
    let info = page(&[
        "- generic [ref=e0]:",
        "  - text: Are you a protected veteran?",
        "  - button \"More info\" [ref=e9]",
        "  - radiogroup [ref=e4]:",
        "    - radio \"Yes\" [ref=e2]",
        "    - radio \"No\" [ref=e3]",
    ]);
    assert_eq!(choice_guard(&info, "e2", &b), Err(Sensitive));
    assert_eq!(choice_guard(&info, "e3", &b), Ok(()));
}
