use std::io::{Cursor, Read};

use jobpilot_core::render::{
    Entry, ResumeDoc, Section, Template, cover_letter_doc, lines, templates, to_docx, to_pdf,
};

fn doc() -> ResumeDoc {
    ResumeDoc {
        name: "Ada Lovelace".into(),
        contact: vec!["ada@example.com".into(), "London".into()],
        summary: vec!["Analyst who writes the first programs.".into()],
        sections: vec![
            Section {
                heading: "Experience".into(),
                entries: vec![
                    Entry {
                        title: "Engineer".into(),
                        subtitle: "Analytical Engines Ltd".into(),
                        dates: "1842 - 1843".into(),
                        bullets: vec!["Wrote the Bernoulli program.".into(), String::new()],
                    },
                    Entry {
                        title: String::new(),
                        subtitle: "Independent".into(),
                        dates: String::new(),
                        bullets: vec![],
                    },
                ],
            },
            Section {
                heading: "Education".into(),
                entries: vec![Entry {
                    title: "Tutor study".into(),
                    ..Entry::default()
                }],
            },
        ],
    }
}

fn document_xml(docx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx)).unwrap();
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

/// Concatenated `<w:t>` text of the document, runs joined by a newline.
fn text_of(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<w:t") {
        rest = &rest[i + 4..];
        let Some(start) = rest.find('>') else { break };
        if rest.starts_with(' ') || rest.starts_with('>') {
            let body = &rest[start + 1..];
            let end = body.find("</w:t>").unwrap();
            out.push_str(&body[..end]);
            out.push('\n');
        }
    }
    out
}

#[test]
fn docx_contains_every_line_as_plain_runs() {
    let d = doc();
    for template in templates() {
        let bytes = to_docx(&d, *template).unwrap();
        assert!(docx_rs::read_docx(&bytes).is_ok());
        let xml = document_xml(&bytes);
        assert!(!xml.contains("<w:vanish"));
        let text = text_of(&xml);
        for line in lines(&d) {
            assert!(text.contains(&line), "missing {line:?} in {template:?}");
        }
    }
}

#[test]
fn pdf_starts_with_header_and_embeds_font() {
    let mut d = doc();
    // About two pages of bullets.
    d.sections[0].entries[0].bullets = (0..90)
        .map(|i| format!("Delivered result number {i} with a measured effect on the team."))
        .collect();
    let bytes = to_pdf(&d, Template::Classic).unwrap();
    assert!(bytes.starts_with(b"%PDF-1."));
    assert!(bytes.windows(10).any(|w| w == b"DejaVuSans"));
    assert!(bytes.len() < 2 * 1024 * 1024);
    assert!(
        bytes
            .windows(8)
            .any(|w| w == b"/Count 3" || w == b"/Count 2"),
        "expected a multi-page document"
    );
}

#[test]
fn pdf_renders_non_ascii_names() {
    let d = ResumeDoc {
        name: "José Ñandú Жуков".into(),
        ..ResumeDoc::default()
    };
    for template in templates() {
        assert!(to_pdf(&d, *template).unwrap().starts_with(b"%PDF-1."));
    }
}

#[test]
fn both_templates_render_the_same_lines() {
    let d = doc();
    let classic = to_docx(&d, Template::Classic).unwrap();
    let compact = to_docx(&d, Template::Compact).unwrap();
    let (a, b) = (document_xml(&classic), document_xml(&compact));
    assert_ne!(a, b);
    let (ta, tb) = (text_of(&a), text_of(&b));
    for line in lines(&d) {
        assert!(ta.contains(&line) && tb.contains(&line), "missing {line:?}");
    }
}

#[test]
fn lines_order_is_stable() {
    assert_eq!(
        lines(&doc()),
        vec![
            "Analyst who writes the first programs.",
            "Engineer",
            "Analytical Engines Ltd",
            "1842 - 1843",
            "Wrote the Bernoulli program.",
            "Independent",
            "Tutor study",
        ]
    );
    let letter = cover_letter_doc("A", &["a@b".into()], &["One.".into(), "Two.".into()]);
    assert!(letter.sections.is_empty());
    assert_eq!(lines(&letter), vec!["One.", "Two."]);
}
