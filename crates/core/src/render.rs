//! Resume document model, two templates, and DOCX / PDF rendering.
//!
//! Output is plain text runs only: no hidden text, no images, no colour tricks (M-17).

use std::io::Cursor;
use std::sync::OnceLock;

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

/// Indexes into [`lines`] in the order Compact prints them: the dates share the title's row, so
/// they come before the subtitle.
pub(crate) fn compact_order(doc: &ResumeDoc) -> Vec<usize> {
    let summary = doc.summary.iter().filter(|s| !s.is_empty()).count();
    let mut order: Vec<usize> = (0..summary).collect();
    for entry in doc.sections.iter().flat_map(|s| &s.entries) {
        let fields = [&entry.title, &entry.subtitle, &entry.dates];
        let head = fields.iter().filter(|field| !field.is_empty()).count();
        let bullets = entry.bullets.iter().filter(|b| !b.is_empty()).count();
        let at = order.len();
        order.extend(at..at + head + bullets);
        if !entry.subtitle.is_empty() && !entry.dates.is_empty() {
            order.swap(at + head - 2, at + head - 1);
        }
    }
    order
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
    dates_column: bool,
}

fn style(template: Template) -> Style {
    match template {
        Template::Classic => Style {
            name: 20.0,
            heading: 13.0,
            title: 11.0,
            body: 10.0,
            dates_column: false,
        },
        Template::Compact => Style {
            name: 16.0,
            heading: 11.0,
            title: 9.5,
            body: 9.0,
            dates_column: true,
        },
    }
}

fn bullet(text: &str) -> String {
    format!("\u{2022} {text}")
}

// ---------------------------------------------------------------- DOCX

/// Drops characters XML 1.0 cannot hold (control characters, U+FFFE, U+FFFF); a stray
/// one would make Word report the whole file as corrupt.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\0'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}'))
        .collect()
}

fn para(text: &str, half_points: usize, bold: bool) -> Paragraph {
    let mut run = Run::new().add_text(clean(text)).size(half_points);
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
                            .add_text(clean(&entry.title))
                            .size(half(st.title))
                            .bold(),
                    )
                    .add_run(
                        Run::new()
                            .add_tab()
                            .add_text(clean(&entry.dates))
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

pub const PAGE_W: f32 = 595.28;
pub const PAGE_H: f32 = 841.89;
pub const MARGIN: f32 = 50.0;
const BULLET_INDENT: f32 = 12.0;
/// Compact: the dates column starts here and runs to the right margin.
const DATES_X: f32 = 395.0;
const COLUMN_GAP: f32 = 8.0;

fn font() -> &'static ParsedFont {
    static FONT_CELL: OnceLock<ParsedFont> = OnceLock::new();
    FONT_CELL.get_or_init(|| {
        let mut parsed = ParsedFont::from_bytes(FONT, 0, &mut Vec::new())
            .expect("the bundled font is a valid TrueType file");
        // The crate leaves the PDF font name unset, which would print a random id.
        parsed.font_name = Some("DejaVuSans".into());
        parsed
    })
}

/// Advance width in points of `text` at `size`, from the font's own glyph metrics.
/// A character the font lacks is measured as `.notdef`, which is also what gets drawn.
pub fn text_width(text: &str, size: f32) -> f32 {
    let f = font();
    let units: u32 = text
        .chars()
        .map(|c| {
            let gid = f.lookup_glyph_index(c as u32).unwrap_or(0);
            u32::from(f.get_glyph_width(gid).unwrap_or(0))
        })
        .sum();
    units as f32 * size / f32::from(f.units_per_em.max(1))
}

/// Greedy wrap by whitespace so every row is at most `max` points wide at `size`;
/// a single word wider than `max` is split between characters.
fn wrap(text: &str, size: f32, max: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if text_width(&candidate, size) <= max {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if text_width(word, size) <= max {
            current = word.to_string();
            continue;
        }
        // One word too wide for a row: break it between characters (at least one per row).
        let mut piece = String::new();
        for c in word.chars() {
            piece.push(c);
            if text_width(&piece, size) > max && piece.chars().count() > 1 {
                piece.pop();
                out.push(std::mem::take(&mut piece));
                piece.push(c);
            }
        }
        current = piece;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// One positioned run of text in the PDF. Origin is the bottom left of the page.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedRun {
    pub page: usize,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: String,
}

struct Layout {
    pages: usize,
    y: f32,
    runs: Vec<PlacedRun>,
}

impl Layout {
    fn new() -> Self {
        Layout {
            pages: 1,
            y: PAGE_H - MARGIN,
            runs: Vec::new(),
        }
    }

    /// Move down one line of `size`, starting a page when the bottom margin is reached.
    fn advance(&mut self, size: f32) -> (usize, f32) {
        let lh = size * 1.35;
        if self.y - lh < MARGIN {
            self.pages += 1;
            self.y = PAGE_H - MARGIN;
        }
        self.y -= lh;
        (self.pages - 1, self.y)
    }

    fn gap(&mut self, points: f32) {
        self.y -= points;
    }

    fn put(&mut self, at: (usize, f32), x: f32, size: f32, text: &str) {
        self.runs.push(PlacedRun {
            page: at.0,
            x,
            y: at.1,
            size,
            text: text.to_string(),
        });
    }

    /// Wrapped text, one printed row per wrapped line, across the full text width.
    fn block(&mut self, size: f32, text: &str) {
        for row in wrap(text, size, PAGE_W - 2.0 * MARGIN) {
            let at = self.advance(size);
            self.put(at, MARGIN, size, &row);
        }
    }

    /// Title rows on the left and dates rows in the right column, sharing the same rows.
    /// Titles are emitted first so each stays contiguous in the content stream.
    fn two_column(&mut self, st: &Style, title: &str, dates: &str) {
        let titles = wrap(title, st.title, DATES_X - COLUMN_GAP - MARGIN);
        let dates = wrap(dates, st.body, PAGE_W - MARGIN - DATES_X);
        let rows: Vec<(usize, f32)> = (0..titles.len().max(dates.len()))
            .map(|_| self.advance(st.title))
            .collect();
        for (at, row) in rows.iter().zip(&titles) {
            self.put(*at, MARGIN, st.title, row);
        }
        for (at, row) in rows.iter().zip(&dates) {
            self.put(*at, DATES_X, st.body, row);
        }
    }
}

/// Positions every run of the PDF. Used by `to_pdf`, and by tests to check the margins.
#[doc(hidden)]
pub fn layout_runs(doc: &ResumeDoc, template: Template) -> Vec<PlacedRun> {
    let st = style(template);
    let mut l = Layout::new();
    if !doc.name.is_empty() {
        l.block(st.name, &clean(&doc.name));
    }
    if !doc.contact.is_empty() {
        l.block(st.body, &clean(&doc.contact.join(" | ")));
    }
    if !doc.summary.is_empty() {
        l.gap(4.0);
    }
    for line in doc.summary.iter().filter(|s| !s.is_empty()) {
        l.block(st.body, &clean(line));
        l.gap(st.body * 0.5);
    }
    for section in &doc.sections {
        l.gap(8.0);
        l.block(st.heading, &clean(&section.heading));
        for entry in &section.entries {
            l.gap(3.0);
            if st.dates_column && !entry.dates.is_empty() {
                l.two_column(&st, &clean(&entry.title), &clean(&entry.dates));
                l.block(st.body, &clean(&entry.subtitle));
            } else {
                l.block(st.title, &clean(&entry.title));
                l.block(st.body, &clean(&entry.subtitle));
                l.block(st.body, &clean(&entry.dates));
            }
            for b in entry.bullets.iter().filter(|b| !b.is_empty()) {
                let rows = wrap(
                    &bullet(&clean(b)),
                    st.body,
                    PAGE_W - 2.0 * MARGIN - BULLET_INDENT,
                );
                for (i, row) in rows.iter().enumerate() {
                    let at = l.advance(st.body);
                    // The first row carries the bullet mark and starts at the margin.
                    let x = if i == 0 {
                        MARGIN
                    } else {
                        MARGIN + BULLET_INDENT
                    };
                    l.put(at, x, st.body, row);
                }
            }
        }
    }
    l.runs
}

pub fn to_pdf(doc: &ResumeDoc, template: Template) -> Result<Vec<u8>> {
    let runs = layout_runs(doc, template);
    let mut pdf = PdfDocument::new(if doc.name.is_empty() {
        "Document"
    } else {
        &doc.name
    });
    let handle = PdfFontHandle::External(pdf.add_font(font()));
    let page_count = runs.iter().map(|r| r.page + 1).max().unwrap_or(1);
    let mut pages: Vec<Vec<Op>> = vec![Vec::new(); page_count];
    for run in runs {
        let ops = &mut pages[run.page];
        ops.push(Op::StartTextSection);
        ops.push(Op::SetFont {
            font: handle.clone(),
            size: Pt(run.size),
        });
        ops.push(Op::SetTextCursor {
            pos: Point {
                x: Pt(run.x),
                y: Pt(run.y),
            },
        });
        ops.push(Op::ShowText {
            items: vec![TextItem::Text(run.text)],
        });
        ops.push(Op::EndTextSection);
    }
    let pages = pages
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
    use super::{text_width, wrap};

    #[test]
    fn wrap_splits_on_whitespace_and_long_words() {
        let w = text_width("aaaa", 10.0);
        assert_eq!(
            wrap("aa bb cc", 10.0, text_width("aa bb", 10.0)),
            vec!["aa bb", "cc"]
        );
        let rows = wrap("aaaaaaaaaa", 10.0, w);
        assert!(rows.len() > 1 && rows.iter().all(|r| text_width(r, 10.0) <= w));
        assert_eq!(rows.concat(), "aaaaaaaaaa");
        assert!(wrap("   ", 10.0, 50.0).is_empty());
    }
}
