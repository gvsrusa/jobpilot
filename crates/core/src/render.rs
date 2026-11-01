//! Resume document model, two templates, and DOCX / PDF rendering.
//!
//! Output is plain text runs only: no hidden text, no images, no colour tricks (M-17).

use std::io::Cursor;

use docx_rs::{AlignmentType, Docx, PageMargin, Paragraph, Run, Tab, TabValueType};
use printpdf::{
    Op, ParsedFont, PdfDocument, PdfFontHandle, PdfPage, PdfSaveOptions, Point, Pt, TextItem,
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// DejaVu Sans 2.37, vendored with its licence in `assets/LICENSE-DejaVu.txt`.
// ponytail: DejaVu Sans has no CJK glyphs, so such names come out blank in PDF;
// add a CJK font when a user needs one.
const FONT: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ResumeDoc {
    pub name: String,
    pub contact: Vec<String>,
    pub summary: Vec<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Section {
    pub heading: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Entry {
    pub title: String,
    pub subtitle: String,
    pub dates: String,
    pub bullets: Vec<String>,
}

/// FR-7.4: Classic is headings and bullets; Compact has a dates column and smaller type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    Classic,
    Compact,
}

pub fn templates() -> &'static [Template] {
    &[Template::Classic, Template::Compact]
}

/// Flattened claim units in order: summary lines, then for each entry its non-empty
/// title, subtitle, dates, and bullets. Draft claims key on the index (Task 10).
pub fn lines(doc: &ResumeDoc) -> Vec<String> {
    let mut out: Vec<String> = doc
        .summary
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect();
    for entry in doc.sections.iter().flat_map(|s| &s.entries) {
        for field in [&entry.title, &entry.subtitle, &entry.dates] {
            if !field.is_empty() {
                out.push(field.clone());
            }
        }
        out.extend(entry.bullets.iter().filter(|b| !b.is_empty()).cloned());
    }
    out
}

/// A cover letter is the paragraphs in `summary`; it has no sections.
pub fn cover_letter_doc(name: &str, contact: &[String], paragraphs: &[String]) -> ResumeDoc {
    ResumeDoc {
        name: name.to_string(),
        contact: contact.to_vec(),
        summary: paragraphs.to_vec(),
        sections: Vec::new(),
    }
}

struct Style {
    name: f32,
    heading: f32,
    title: f32,
    body: f32,
    wrap: usize,
    dates_column: bool,
}

fn style(template: Template) -> Style {
    match template {
        Template::Classic => Style {
            name: 20.0,
            heading: 13.0,
            title: 11.0,
            body: 10.0,
            wrap: 90,
            dates_column: false,
        },
        Template::Compact => Style {
            name: 16.0,
            heading: 11.0,
            title: 9.5,
            body: 9.0,
            wrap: 90,
            dates_column: true,
        },
    }
}

fn bullet(text: &str) -> String {
    format!("\u{2022} {text}")
}

// ---------------------------------------------------------------- DOCX

fn para(text: &str, half_points: usize, bold: bool) -> Paragraph {
    let mut run = Run::new().add_text(text).size(half_points);
    if bold {
        run = run.bold();
    }
    Paragraph::new().add_run(run)
}

fn half(points: f32) -> usize {
    (points * 2.0).round() as usize
}

pub fn to_docx(doc: &ResumeDoc, template: Template) -> Result<Vec<u8>> {
    let st = style(template);
    // Twentieths of a point: 9638 twips is the text width of A4 with 0.75 inch margins.
    const RIGHT_TAB: usize = 9638;
    let mut docx = Docx::new().page_size(11906, 16838).page_margin(
        PageMargin::new()
            .top(1080)
            .bottom(1080)
            .left(1080)
            .right(1080),
    );

    if !doc.name.is_empty() {
        docx = docx.add_paragraph(para(&doc.name, half(st.name), true));
    }
    if !doc.contact.is_empty() {
        docx = docx.add_paragraph(para(&doc.contact.join(" | "), half(st.body), false));
    }
    for line in doc.summary.iter().filter(|s| !s.is_empty()) {
        docx = docx.add_paragraph(para(line, half(st.body), false));
    }
    for section in &doc.sections {
        if !section.heading.is_empty() {
            docx = docx.add_paragraph(para(&section.heading, half(st.heading), true));
        }
        for entry in &section.entries {
            if st.dates_column && !entry.dates.is_empty() {
                let mut p = Paragraph::new()
                    .add_tab(Tab::new().val(TabValueType::Right).pos(RIGHT_TAB))
                    .add_run(
                        Run::new()
                            .add_text(&entry.title)
                            .size(half(st.title))
                            .bold(),
                    )
                    .add_run(
                        Run::new()
                            .add_tab()
                            .add_text(&entry.dates)
                            .size(half(st.body)),
                    );
                if entry.title.is_empty() {
                    p = p.align(AlignmentType::Right);
                }
                docx = docx.add_paragraph(p);
                if !entry.subtitle.is_empty() {
                    docx = docx.add_paragraph(para(&entry.subtitle, half(st.body), false));
                }
            } else {
                if !entry.title.is_empty() {
                    docx = docx.add_paragraph(para(&entry.title, half(st.title), true));
                }
                if !entry.subtitle.is_empty() {
                    docx = docx.add_paragraph(para(&entry.subtitle, half(st.body), false));
                }
                if !entry.dates.is_empty() {
                    docx = docx.add_paragraph(para(&entry.dates, half(st.body), false));
                }
            }
            for b in entry.bullets.iter().filter(|b| !b.is_empty()) {
                docx = docx.add_paragraph(para(&bullet(b), half(st.body), false).indent(
                    Some(360),
                    None,
                    None,
                    None,
                ));
            }
        }
    }

    let mut cursor = Cursor::new(Vec::new());
    docx.build()
        .pack(&mut cursor)
        .map_err(|e| Error::Failed(format!("DOCX could not be written: {e}")))?;
    Ok(cursor.into_inner())
}

// ----------------------------------------------------------------- PDF

const PAGE_W: f32 = 595.28;
const PAGE_H: f32 = 841.89;
const MARGIN: f32 = 50.0;
const DATES_X: f32 = 430.0;
const BULLET_INDENT: f32 = 12.0;

/// Greedy wrap by whitespace at `width` characters; a word longer than `width` is split.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() > width {
            if count > 0 {
                out.push(std::mem::take(&mut current));
                count = 0;
            }
            out.push(word.drain(..width).collect());
        }
        let len = word.len();
        if len == 0 {
            continue;
        }
        if count > 0 && count + 1 + len > width {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
        if count > 0 {
            current.push(' ');
            count += 1;
        }
        current.extend(word);
        count += len;
    }
    if count > 0 {
        out.push(current);
    }
    out
}

struct Layout {
    pages: Vec<Vec<Op>>,
    y: f32,
}

impl Layout {
    fn new() -> Self {
        Layout {
            pages: vec![Vec::new()],
            y: PAGE_H - MARGIN,
        }
    }

    /// Move down one line of `size`, starting a page when the bottom margin is reached.
    fn advance(&mut self, size: f32) -> f32 {
        let lh = size * 1.35;
        if self.y - lh < MARGIN {
            self.pages.push(Vec::new());
            self.y = PAGE_H - MARGIN;
        }
        self.y -= lh;
        self.y
    }

    fn gap(&mut self, points: f32) {
        self.y -= points;
    }

    fn put(&mut self, font: &PdfFontHandle, x: f32, y: f32, size: f32, text: &str) {
        let ops = self.pages.last_mut().expect("at least one page");
        ops.push(Op::StartTextSection);
        ops.push(Op::SetFont {
            font: font.clone(),
            size: Pt(size),
        });
        ops.push(Op::SetTextCursor {
            pos: Point { x: Pt(x), y: Pt(y) },
        });
        ops.push(Op::ShowText {
            items: vec![TextItem::Text(text.to_string())],
        });
        ops.push(Op::EndTextSection);
    }

    /// Wrapped text, one printed row per wrapped line.
    fn block(&mut self, font: &PdfFontHandle, x: f32, size: f32, wrap_at: usize, text: &str) {
        for row in wrap(text, wrap_at) {
            let y = self.advance(size);
            self.put(font, x, y, size, &row);
        }
    }
}

pub fn to_pdf(doc: &ResumeDoc, template: Template) -> Result<Vec<u8>> {
    let st = style(template);
    let mut parsed = ParsedFont::from_bytes(FONT, 0, &mut Vec::new())
        .ok_or_else(|| Error::Failed("the bundled font could not be read".into()))?;
    // The crate leaves the PDF font name unset, which would print a random id.
    parsed.font_name = Some("DejaVuSans".into());
    let mut pdf = PdfDocument::new(if doc.name.is_empty() {
        "Document"
    } else {
        &doc.name
    });
    let font = PdfFontHandle::External(pdf.add_font(&parsed));
    let mut l = Layout::new();

    if !doc.name.is_empty() {
        l.block(&font, MARGIN, st.name, st.wrap, &doc.name);
    }
    if !doc.contact.is_empty() {
        l.block(&font, MARGIN, st.body, st.wrap, &doc.contact.join(" | "));
    }
    if !doc.summary.is_empty() {
        l.gap(4.0);
    }
    for line in doc.summary.iter().filter(|s| !s.is_empty()) {
        l.block(&font, MARGIN, st.body, st.wrap, line);
        l.gap(st.body * 0.5);
    }
    for section in &doc.sections {
        l.gap(8.0);
        if !section.heading.is_empty() {
            l.block(&font, MARGIN, st.heading, st.wrap, &section.heading);
        }
        for entry in &section.entries {
            l.gap(3.0);
            if st.dates_column && !entry.dates.is_empty() {
                // Dates sit in a fixed right column on the first row of the title.
                let rows = wrap(&entry.title, 60);
                let first = rows.first().cloned().unwrap_or_default();
                let y = l.advance(st.title);
                if !first.is_empty() {
                    l.put(&font, MARGIN, y, st.title, &first);
                }
                l.put(&font, DATES_X, y, st.body, &entry.dates);
                for row in rows.iter().skip(1) {
                    let y = l.advance(st.title);
                    l.put(&font, MARGIN, y, st.title, row);
                }
                l.block(&font, MARGIN, st.body, st.wrap, &entry.subtitle);
            } else {
                l.block(&font, MARGIN, st.title, st.wrap, &entry.title);
                l.block(&font, MARGIN, st.body, st.wrap, &entry.subtitle);
                l.block(&font, MARGIN, st.body, st.wrap, &entry.dates);
            }
            for b in entry.bullets.iter().filter(|b| !b.is_empty()) {
                let rows = wrap(b, st.wrap - 4);
                for (i, row) in rows.iter().enumerate() {
                    let y = l.advance(st.body);
                    let text = if i == 0 { bullet(row) } else { row.clone() };
                    let x = MARGIN + if i == 0 { 0.0 } else { BULLET_INDENT };
                    l.put(&font, x, y, st.body, &text);
                }
            }
        }
    }

    let pages = l
        .pages
        .into_iter()
        .map(|ops| {
            PdfPage::new(
                printpdf::Mm(PAGE_W * 25.4 / 72.0),
                printpdf::Mm(PAGE_H * 25.4 / 72.0),
                ops,
            )
        })
        .collect();
    Ok(pdf
        .with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wrap_splits_on_whitespace_and_long_words() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefg", 3), vec!["abc", "def", "g"]);
        assert!(wrap("   ", 5).is_empty());
    }
}
