//! Copilot's form reading and its rules: what an aria snapshot holds, and what fills each field
//! with no model at all.

use jobpilot_core::forms::{
    AnswerText, Form, FormField, Mapped, ValueSource, fold_label, map_rules, parse_snapshot,
};
use jobpilot_core::profile::ProfileBasic;

/// The vendor facts sample, as `browser_snapshot` replies with it, plus a greenhouse-shape
/// file field: a hidden input behind an `Attach` button in a labelled group.
const PAGE: &str = "### Page
- Page URL: https://boards.greenhouse.io/acme/jobs/1?gh_src=x
- Page Title: Software Engineer
### Snapshot
```yaml
- generic [ref=e1]:
  - heading \"Software Engineer\" [level=1] [ref=e2]
  - textbox \"Full name *\" [ref=e3]: Alex Doe
  - textbox \"Email *\" [ref=e4]: prefill@x.io
  - textbox \"Phone\" [ref=e5]
  - combobox \"Country\" [ref=e8]:
    - option \"United States\"
    - option \"Canada\"
  - checkbox \"Send me job alerts\" [checked] [ref=e10]
  - button \"Resume\" [ref=e12]
  - group \"Cover letter\" [ref=e20]:
    - paragraph [ref=e21]: Cover letter
    - button \"Attach\" [ref=e22]
    - button \"Remove\" [ref=e23]
  - link \"Privacy\" [ref=e24]
  - button \"Apply now\" [ref=e13]
  - button \"Submit application\" [ref=e14]
```
";

fn field(target: &str, role: &str, label: &str) -> FormField {
    FormField {
        target: target.into(),
        role: role.into(),
        label: label.into(),
        value: None,
        checked: false,
        options: Vec::new(),
    }
}

fn form(fields: Vec<FormField>) -> Form {
    Form {
        url: "https://boards.greenhouse.io/acme/jobs/1".into(),
        origin: "https://boards.greenhouse.io".into(),
        fields,
        submit: None,
    }
}

fn basic() -> ProfileBasic {
    ProfileBasic {
        name: "Ada King Lovelace".into(),
        email: "ada@example.com".into(),
        cities: vec!["Lisbon".into()],
        countries: vec!["Canada".into()],
        regions: vec!["Ontario".into()],
        languages: vec!["en".into()],
        ..ProfileBasic::default()
    }
}

fn answer(id: &str, question: &str, text: &str) -> AnswerText {
    AnswerText {
        answer_id: id.into(),
        question: question.into(),
        text: text.into(),
    }
}

fn value_of(mapped: &[Mapped], label: &str) -> Option<(String, ValueSource)> {
    mapped.iter().find_map(|m| match m {
        Mapped::Value {
            field,
            text,
            source,
        } if field.label == label => Some((text.clone(), source.clone())),
        _ => None,
    })
}

fn person(mapped: &[Mapped], label: &str) -> Option<&'static str> {
    mapped.iter().find_map(|m| match m {
        Mapped::Person { field, why } if field.label == label => Some(*why),
        _ => None,
    })
}

#[test]
fn parse_snapshot_reads_each_role_and_value() {
    let form = parse_snapshot(PAGE);
    assert_eq!(
        form.url,
        "https://boards.greenhouse.io/acme/jobs/1?gh_src=x"
    );
    assert_eq!(form.origin, "https://boards.greenhouse.io");
    assert_eq!(form.submit.as_deref(), Some("e14"));

    let labels: Vec<(&str, &str, &str)> = form
        .fields
        .iter()
        .map(|f| (f.target.as_str(), f.role.as_str(), f.label.as_str()))
        .collect();
    assert_eq!(
        labels,
        [
            ("e3", "textbox", "Full name *"),
            ("e4", "textbox", "Email *"),
            ("e5", "textbox", "Phone"),
            ("e8", "combobox", "Country"),
            ("e10", "checkbox", "Send me job alerts"),
            ("e12", "button", "Resume"),
            // The group names the file; its first button stands for it, and "Remove" does not.
            ("e22", "button", "Cover letter"),
        ]
    );
    assert_eq!(form.fields[0].value.as_deref(), Some("Alex Doe"));
    assert_eq!(form.fields[1].value.as_deref(), Some("prefill@x.io"));
    assert_eq!(form.fields[2].value, None);
    assert_eq!(form.fields[3].options, ["United States", "Canada"]);
    assert!(form.fields[4].checked);
    assert!(!form.fields[0].checked);
    // A navigation button, a link, a heading, and the submit button are not fields. A hidden
    // input never shows in a snapshot, so nothing else is either.
    for absent in ["e1", "e2", "e13", "e14", "e20", "e21", "e23", "e24"] {
        assert!(form.fields.iter().all(|f| f.target != absent), "{absent}");
    }
}

/// Chromium names no group by its `aria-labelledby` in a snapshot: the caption is the group's
/// first text, and only its first button stands for the file.
#[test]
fn an_unnamed_group_is_captioned_by_its_first_text() {
    let form = parse_snapshot(
        "- group [ref=e21]:
  - paragraph [ref=e22]: Resume/CV *
  - button \"Attach\" [ref=e23]
  - button \"Remove\" [ref=e24]
- group [ref=e25]:
  - button \"Add another\" [ref=e26]
  - paragraph [ref=e27]: Cover letter
  - button \"Attach\" [ref=e28]
- group [ref=e29]:
  - text: Portfolio
  - button \"Attach\" [ref=e30]",
    );
    let found: Vec<(&str, &str)> = form
        .fields
        .iter()
        .map(|f| (f.target.as_str(), f.label.as_str()))
        .collect();
    assert_eq!(found, [("e23", "Resume/CV *")]);
}

#[test]
fn a_snapshot_with_no_page_url_has_no_origin() {
    let form = parse_snapshot("- textbox \"Email\" [ref=e1]");
    assert_eq!(form.origin, "");
    assert_eq!(form.fields.len(), 1);
}

#[test]
fn fold_label_keeps_letters_and_digits_only() {
    assert_eq!(fold_label("  Resume/CV *"), "resume cv");
    assert_eq!(fold_label("E-mail\u{200b} Address:"), "e mail address");
}

#[test]
fn rules_fill_common_fields_with_no_ai() {
    let mut country = field("e6", "combobox", "Country *");
    country.options = vec!["United States".into(), "Canada".into()];
    let mut prefilled = field("e2", "textbox", "Email *");
    prefilled.value = Some("someone@else.example".into());
    let form = form(vec![
        field("e1", "textbox", "Full name"),
        prefilled,
        field("e3", "textbox", "First name *"),
        field("e4", "textbox", "Last name *"),
        field("e5", "textbox", "City"),
        country,
        field("e7", "textbox", "Province"),
        field("e8", "textbox", "Language"),
        field("e9", "textbox", "Why this role?"),
        field("e10", "textbox", "Phone"),
        field("e11", "button", "Resume/CV"),
        field("e12", "button", "Cover letter"),
        field("e13", "checkbox", "Send me job alerts"),
    ]);
    let answers = [answer("a1", "Why this role?", "I enjoy building tools.")];
    let mapped = map_rules(&form, &basic(), &answers);
    assert_eq!(mapped.len(), form.fields.len());

    let profile = |text: &str| Some((text.to_owned(), ValueSource::Profile));
    assert_eq!(value_of(&mapped, "Full name"), profile("Ada King Lovelace"));
    // An ordinary prefilled value is the profile's, not the page's.
    assert_eq!(value_of(&mapped, "Email *"), profile("ada@example.com"));
    assert_eq!(value_of(&mapped, "First name *"), profile("Ada"));
    assert_eq!(value_of(&mapped, "Last name *"), profile("Lovelace"));
    assert_eq!(value_of(&mapped, "City"), profile("Lisbon"));
    assert_eq!(value_of(&mapped, "Country *"), profile("Canada"));
    assert_eq!(value_of(&mapped, "Province"), profile("Ontario"));
    assert_eq!(value_of(&mapped, "Language"), profile("en"));
    assert_eq!(
        value_of(&mapped, "Why this role?"),
        Some((
            "I enjoy building tools.".to_owned(),
            ValueSource::Answer("a1".into())
        ))
    );
    assert_eq!(person(&mapped, "Phone"), Some("unmapped"));
    assert_eq!(person(&mapped, "Send me job alerts"), Some("unmapped"));
    let files: Vec<(&str, &str)> = mapped
        .iter()
        .filter_map(|m| match m {
            Mapped::File { field, kind } => Some((field.target.as_str(), *kind)),
            _ => None,
        })
        .collect();
    assert_eq!(files, [("e11", "resume"), ("e12", "letter")]);
}

#[test]
fn a_combobox_takes_only_one_of_its_own_options() {
    let mut country = field("e1", "combobox", "Country");
    country.options = vec!["Portugal".into(), "Spain".into()];
    let mapped = map_rules(&form(vec![country]), &basic(), &[]);
    assert_eq!(person(&mapped, "Country"), Some("unmapped"));
}

#[test]
fn rules_never_map_a_sensitive_or_legal_label() {
    let form = form(vec![
        field("e1", "textbox", "Gender"),
        field(
            "e2",
            "checkbox",
            "I certify that my answers are true and complete",
        ),
        field(
            "e3",
            "textbox",
            "Are you legally authorized to work in Canada?",
        ),
        field("e4", "textbox", "Salary expectation"),
        field("e5", "textbox", "Do you agree to our declaration?"),
    ]);
    // An answer for the legal label is never used: legal answers go to no automation, and
    // a label that classifies legal is a person's.
    let answers = [
        answer("a1", "Salary expectation", "90000 EUR"),
        answer("a2", "Do you agree to our declaration?", "Yes"),
    ];
    let mapped = map_rules(&form, &basic(), &answers);
    assert_eq!(person(&mapped, "Gender"), Some("sensitive"));
    assert_eq!(
        person(&mapped, "I certify that my answers are true and complete"),
        Some("legal")
    );
    assert_eq!(
        person(&mapped, "Are you legally authorized to work in Canada?"),
        Some("work_authorization")
    );
    assert_eq!(
        person(&mapped, "Do you agree to our declaration?"),
        Some("legal")
    );
    // A sensitive label takes only an answer whose question is its own, which reached this
    // list through `use_answer`, so a sealed one was confirmed for this application.
    assert_eq!(
        value_of(&mapped, "Salary expectation"),
        Some(("90000 EUR".into(), ValueSource::Answer("a1".into())))
    );

    // A rule's own label, worded as a sensitive question, is not the rule's.
    let named = form_with("Country of birth");
    let mapped = map_rules(&named, &basic(), &[]);
    assert_eq!(person(&mapped, "Country of birth"), Some("unmapped"));
}

fn form_with(label: &str) -> Form {
    form(vec![field("e1", "textbox", label)])
}

#[test]
fn a_prefilled_sensitive_value_goes_to_a_person() {
    let mut birth = field("e1", "textbox", "Date of birth");
    birth.value = Some("1990-01-01".into());
    let mut gender = field("e2", "textbox", "Gender");
    gender.value = Some("x".into());
    let form = form(vec![birth, gender, field("e3", "textbox", "Ethnicity")]);
    // Even a matching answer does not overwrite what the page put there.
    let answers = [answer("a1", "Gender", "Woman")];
    let mapped = map_rules(&form, &basic(), &answers);
    assert_eq!(person(&mapped, "Date of birth"), Some("prefilled"));
    assert_eq!(person(&mapped, "Gender"), Some("prefilled"));
    assert_eq!(person(&mapped, "Ethnicity"), Some("sensitive"));
}

#[test]
fn a_submit_inside_a_file_group_is_not_a_field() {
    let form = parse_snapshot(
        "- group [ref=e21]:
  - paragraph [ref=e22]: Resume
  - button \"Submit application\" [ref=e23]
- button \"Upload resume and apply\" [ref=e24]
- button \"Send your CV\" [ref=e25]
- button \"Resume\" [ref=e26]",
    );
    let found: Vec<&str> = form.fields.iter().map(|f| f.target.as_str()).collect();
    assert_eq!(found, ["e26"]);
    assert_eq!(form.submit.as_deref(), Some("e23"));
}

#[test]
fn a_field_inside_a_frame_is_left_to_the_person() {
    let form = parse_snapshot(
        "- textbox \"Email\" [ref=e1]
- iframe [ref=e2]:
  - textbox \"Email\" [ref=f1e3]
  - button \"Resume\" [ref=f1e4]",
    );
    let mapped = map_rules(&form, &basic(), &[]);
    assert_eq!(
        value_of(&mapped, "Email").map(|(text, _)| text),
        Some("ada@example.com".into())
    );
    let framed: Vec<&'static str> = mapped
        .iter()
        .filter_map(|m| match m {
            Mapped::Person { field, why } if field.target.starts_with('f') => Some(*why),
            _ => None,
        })
        .collect();
    assert_eq!(framed, ["frame", "frame"]);
}
