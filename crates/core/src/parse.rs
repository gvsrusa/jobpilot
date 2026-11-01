//! Resume import from PDF and DOCX. The file is untrusted: its size is checked on the open
//! handle, parsing runs on its own thread under a real time limit, and nothing in the
//! file (a path, a relationship, a macro) is ever followed.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::db::Db;
use crate::error::{Error, Result};
use crate::render::{ResumeDoc, Section, lines};

pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_TEXT_CHARS: usize = 2_000_000;
pub const PARSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest `word/document.xml` read after inflating (a zip bomb stops here).
const MAX_XML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 10_000;
const MAX_ITEMS: usize = 5_000;
const ITEM_CHARS: usize = 20_000;
const TITLE_LIMIT: usize = 300;
// ponytail: a parser that ignores its time limit keeps its thread (Rust cannot kill one).
// After this many are stuck at once, imports are refused until one ends.
const MAX_STUCK_PARSERS: usize = 2;

#[derive(Debug, Clone, Serialize)]
pub struct Imported {
    pub draft_id: String,
    pub lineage_id: String,
    pub items: usize,
    pub warnings: Vec<String>,
}

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}

// --------------------------------------------------------------- limits

/// Parsers that missed their time limit and have not ended yet.
static STUCK_PARSERS: AtomicUsize = AtomicUsize::new(0);

const RUNNING: u8 = 0;
const FINISHED: u8 = 1;
const ABANDONED: u8 = 2;

/// Runs `work` on a dedicated thread (not the runtime's blocking pool, whose shutdown
/// would wait for a hung parser) and gives up after `limit`. A panic becomes an error.
// ponytail: with `panic = "abort"` in the release profile a parser panic still aborts the
// process; catch_unwind only helps while unwinding is on.
async fn run_limited<T, F>(limit: Duration, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    if STUCK_PARSERS.load(Ordering::SeqCst) >= MAX_STUCK_PARSERS {
        return Err(Error::Refused(
            "earlier imports are still stuck; restart JobPilot to clear them".into(),
        ));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let state = Arc::new(AtomicU8::new(RUNNING));
    let thread_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("jobpilot-parse".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err(bad("this file could not be read")));
            let _ = tx.send(result);
            if thread_state.swap(FINISHED, Ordering::SeqCst) == ABANDONED {
                STUCK_PARSERS.fetch_sub(1, Ordering::SeqCst);
            }
        })?;
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(Error::Failed("the file reader stopped".into())),
        Err(_) => {
            if state.swap(ABANDONED, Ordering::SeqCst) == RUNNING {
                STUCK_PARSERS.fetch_add(1, Ordering::SeqCst);
            }
            Err(Error::Timeout)
        }
    }
}

// --------------------------------------------------------------- extraction

/// Text of a `.pdf` or `.docx` (by extension, any case). Blocking: call it through
/// [`import_resume`] or from a thread of your own.
pub fn extract_text(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let pdf = match ext.as_deref() {
        Some("pdf") => true,
        Some("docx") => false,
        _ => return Err(bad("a resume is a .pdf or .docx file")),
    };
    let bytes = read_limited(path)?;
    let text = if pdf {
        pdf_extract::extract_text_from_mem(&bytes).map_err(|_| bad("unreadable_pdf"))?
    } else {
        docx_text(&bytes)?
    };
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(Error::TooLarge {
            limit: MAX_TEXT_CHARS as u64,
        });
    }
    if !text.chars().any(char::is_alphanumeric) {
        return Err(bad("no_text"));
    }
    Ok(text)
}

// ponytail: accepted residual, `pdf_extract` allocates without a bound while it parses, so
// a hostile PDF under the 20 MiB file limit can still use a lot of memory before the 30 s
// limit ends the wait. Ceiling: one file at a time per user action.
/// Opens once and judges the handle, not the path, so the file cannot change between a
/// size check and the read.
fn read_limited(path: &Path) -> Result<Vec<u8>> {
    // A FIFO would block `open` forever; only a regular file is opened.
    if !std::fs::metadata(path)?.is_file() {
        return Err(bad("not a file"));
    }
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(bad("not a file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    Ok(bytes)
}

/// Reads only `word/document.xml`. Relationships, macros, and every other part stay
/// unopened, so nothing the file points at is fetched or run.
fn docx_text(bytes: &[u8]) -> Result<String> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| bad("unreadable_docx"))?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(bad("unreadable_docx"));
    }
    let entry = zip
        .by_name("word/document.xml")
        .map_err(|_| bad("unreadable_docx"))?;
    if entry.size() > MAX_XML_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_XML_BYTES,
        });
    }
    let mut xml = Vec::new();
    entry
        .take(MAX_XML_BYTES + 1)
        .read_to_end(&mut xml)
        .map_err(|_| bad("unreadable_docx"))?;
    if xml.len() as u64 > MAX_XML_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_XML_BYTES,
        });
    }
    paragraph_text(&String::from_utf8_lossy(&xml))
}

/// End of the tag that starts at `from` (the index of its `>`), skipping quoted
/// attribute values and a DOCTYPE's bracketed subset.
fn tag_end(xml: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    for (offset, c) in xml[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => depth += 1,
            (None, ']') => depth = depth.saturating_sub(1),
            (None, '>') if depth == 0 => return Some(from + offset),
            _ => {}
        }
    }
    None
}

/// The text of `w:t` runs, one line per `w:p`. Entities other than the five XML ones and
/// numeric references are dropped, never expanded.
fn paragraph_text(xml: &str) -> Result<String> {
    let cap = MAX_TEXT_CHARS * 4;
    let mut out = String::new();
    let mut in_text = false;
    // Start of the current run in `out`, and whether the run is hidden (`w:vanish`).
    let mut run_start = 0;
    let mut vanished = false;
    let mut i = 0;
    while i < xml.len() {
        if out.len() > cap {
            return Err(Error::TooLarge {
                limit: MAX_TEXT_CHARS as u64,
            });
        }
        let Some(lt) = xml[i..].find('<').map(|n| i + n) else {
            if in_text {
                decode_into(&xml[i..], &mut out);
            }
            break;
        };
        if in_text {
            decode_into(&xml[i..lt], &mut out);
        }
        let rest = &xml[lt..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(xml.len(), |n| lt + n + 3);
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").map_or(xml.len(), |n| lt + n);
            if in_text {
                out.push_str(&xml[lt + 9..end.max(lt + 9)]);
            }
            i = (end + 3).min(xml.len());
        } else if rest.starts_with("<?") {
            i = rest.find("?>").map_or(xml.len(), |n| lt + n + 2);
        } else if rest.starts_with("<!") {
            i = tag_end(xml, lt).map_or(xml.len(), |n| n + 1);
        } else {
            let Some(end) = tag_end(xml, lt) else { break };
            let inner = &xml[lt + 1..end];
            let self_closing = inner.ends_with('/');
            let closing = inner.starts_with('/');
            let name = inner
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("");
            match (closing, name) {
                (false, "w:r") if !self_closing => {
                    run_start = out.len();
                    vanished = false;
                }
                (true, "w:r") => {
                    // Hidden text is not on the page, so it is not in the resume.
                    if vanished {
                        out.truncate(run_start);
                    }
                    vanished = false;
                }
                (false, "w:vanish") => {
                    vanished = !(inner.contains("\"0\"") || inner.contains("false"));
                }
                (false, "w:t") => in_text = !self_closing,
                (true, "w:t") => in_text = false,
                (true, "w:p") | (false, "w:br" | "w:cr") => out.push('\n'),
                (false, "w:p") if self_closing => out.push('\n'),
                (false, "w:tab") => out.push('\t'),
                _ => {}
            }
            i = end + 1;
        }
    }
    Ok(out)
}

fn decode_into(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        let Some(semi) = rest.bytes().take(11).position(|b| b == b';') else {
            out.push('&');
            continue;
        };
        let name = &rest[..semi];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            out.push(c);
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
}

// --------------------------------------------------------------- structure

pub(crate) const SUMMARY_HEADINGS: [&str; 4] =
    ["summary", "profile", "objective", "professional summary"];
pub(crate) const KNOWN_HEADINGS: [&str; 18] = [
    "experience",
    "work experience",
    "professional experience",
    "employment",
    "employment history",
    "education",
    "skills",
    "technical skills",
    "projects",
    "certifications",
    "achievements",
    "awards",
    "languages",
    "interests",
    "publications",
    "volunteering",
    "references",
    "work history",
];

fn tidy(line: &str) -> String {
    line.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn bullet_text(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(['\u{2022}', '\u{25AA}', '\u{00B7}', '\u{2013}', '-', '*'])?;
    Some(rest.trim_start())
}

fn is_known(line: &str) -> bool {
    let plain = line.trim_end_matches(':').trim().to_lowercase();
    KNOWN_HEADINGS.contains(&plain.as_str()) || SUMMARY_HEADINGS.contains(&plain.as_str())
}

fn is_heading(line: &str) -> bool {
    if is_known(line) {
        return true;
    }
    let letters = line.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 3
        && line.chars().count() <= 40
        && !line.chars().any(char::is_lowercase)
        && line
            .chars()
            .all(|c| c.is_alphabetic() || matches!(c, ' ' | '&' | '/' | '-' | ':'))
}

fn looks_like_contact(line: &str) -> bool {
    line.contains('@')
        || line.contains('|')
        || line.contains("http")
        || line.chars().filter(char::is_ascii_digit).count() >= 7
}

/// Heading heuristics: the first line is the name; lines before the first heading with an
/// address, `|`, link, or phone number are contact, the rest summary; a known or ALL CAPS
/// line starts a section; everything else in a section becomes a bullet of one entry
/// (a wrapped line joins the bullet before it).
// ponytail: titles, employers, and dates stay inside bullets; no entry detection.
pub fn sectionize(text: &str) -> ResumeDoc {
    let mut doc = ResumeDoc::default();
    let mut in_summary = false;
    let mut current: Option<usize> = None;
    let mut open_bullet = false;
    let mut first = true;
    for raw in text.lines() {
        let line = tidy(raw);
        if line.is_empty() {
            continue;
        }
        let heading = bullet_text(&line).is_none() && is_heading(&line);
        if first {
            first = false;
            // An ALL CAPS first line is a name; only a known heading starts a section.
            if !(heading && is_known(&line)) {
                doc.name = line;
                continue;
            }
        }
        if heading {
            let plain = line.trim_end_matches(':').trim().to_lowercase();
            open_bullet = false;
            if SUMMARY_HEADINGS.contains(&plain.as_str()) {
                in_summary = true;
                current = None;
            } else {
                in_summary = false;
                doc.sections.push(Section {
                    heading: line.trim_end_matches(':').trim().to_string(),
                    entries: vec![crate::render::Entry::default()],
                });
                current = Some(doc.sections.len() - 1);
            }
            continue;
        }
        if let Some(index) = current {
            let bullets = &mut doc.sections[index].entries[0].bullets;
            match bullet_text(&line) {
                Some("") => {}
                Some(body) => {
                    bullets.push(body.to_string());
                    open_bullet = true;
                }
                None => match bullets.last_mut().filter(|_| open_bullet) {
                    Some(last) => {
                        last.push(' ');
                        last.push_str(&line);
                    }
                    None => bullets.push(line),
                },
            }
        } else if in_summary {
            doc.summary
                .push(bullet_text(&line).unwrap_or(&line).to_string());
        } else if looks_like_contact(&line) {
            doc.contact
                .extend(line.split('|').map(tidy).filter(|p| !p.is_empty()));
        } else {
            doc.summary.push(line);
        }
    }
    doc
}

fn kind_of(heading: &str) -> &'static str {
    let h = heading.to_lowercase();
    if h.contains("experience") || h.contains("employment") || h.contains("work") {
        "experience"
    } else if h.contains("education") {
        "education"
    } else if h.contains("skill") {
        "skill"
    } else if h.contains("project") {
        "project"
    } else if h.contains("achievement") || h.contains("award") {
        "achievement"
    } else {
        "note"
    }
}

/// Knowledge kind for each entry of `lines(doc)`, in the same order (summary first, then
/// each section's bullets).
fn kinds(doc: &ResumeDoc) -> Vec<&'static str> {
    let mut out = vec!["note"; doc.summary.iter().filter(|s| !s.is_empty()).count()];
    for section in &doc.sections {
        let kind = kind_of(&section.heading);
        for entry in &section.entries {
            let n = [&entry.title, &entry.subtitle, &entry.dates]
                .iter()
                .filter(|f| !f.is_empty())
                .count()
                + entry.bullets.iter().filter(|b| !b.is_empty()).count();
            out.extend(std::iter::repeat_n(kind, n));
        }
    }
    out
}

// --------------------------------------------------------------- import

/// Stores the file's text as an uploaded resume draft (version 1, new lineage) and one
/// unapproved `parsed` knowledge item per claim unit, with `source_ref`
/// `draft:<id>#line:<n>`. Creates nothing when anything fails.
pub async fn import_resume(db: &Db, path: &Path, title: &str) -> Result<Imported> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_LIMIT {
        return Err(bad("a title is 1 to 300 characters"));
    }
    let owned = path.to_path_buf();
    let text = run_limited(PARSE_TIMEOUT, move || extract_text(&owned)).await?;
    // The same cleaning job text gets: controls, bidi, zero-width and tag characters go,
    // and the text is NFC. A page break must not glue two words together.
    let mut findings = Vec::new();
    let text = crate::knowledge::clean_chars(&text.replace('\u{c}', "\n"), &mut findings);
    let text: String = text.nfc().collect();
    let doc = sectionize(&text);
    let units = lines(&doc);
    let kinds = kinds(&doc);
    if units.is_empty() || units.len() != kinds.len() {
        return Err(bad("no_text"));
    }
    if units.len() > MAX_ITEMS {
        return Err(bad("too_many_lines"));
    }
    let mut warnings: Vec<String> = findings.into_iter().map(|f| f.excerpt).collect();
    if doc.sections.is_empty() {
        warnings.push("no section headings were recognised".to_string());
    }
    let body = serde_json::to_string(&doc).map_err(|e| Error::Failed(e.to_string()))?;

    let mut tx = db.pool.begin().await?;
    let (draft_id, lineage_id): (String, String) = sqlx::query_as(
        "INSERT INTO drafts (id, kind, lineage_id, version, origin, title, body, created_at) \
         VALUES (lower(hex(randomblob(16))), 'resume', lower(hex(randomblob(16))), 1, 'uploaded', ?, ?, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) RETURNING id, lineage_id",
    )
    .bind(title)
    .bind(&body)
    .fetch_one(&mut *tx)
    .await?;
    for (n, (kind, unit)) in kinds.iter().zip(&units).enumerate() {
        let unit: String = unit.chars().take(ITEM_CHARS).collect();
        sqlx::query(
            "INSERT INTO knowledge_items \
             (id, kind, body, source, source_ref, approved, created_at, updated_at) \
             VALUES (lower(hex(randomblob(16))), ?, ?, 'parsed', ?, 0, \
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(kind)
        .bind(unit)
        .bind(format!("draft:{draft_id}#line:{n}"))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Imported {
        draft_id,
        lineage_id,
        items: units.len(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hung_parser_times_out_and_is_released() {
        let before = STUCK_PARSERS.load(Ordering::SeqCst);
        let result = run_limited(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(400));
            Ok(())
        })
        .await;
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(STUCK_PARSERS.load(Ordering::SeqCst) > before);
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(STUCK_PARSERS.load(Ordering::SeqCst), before);
    }

    #[tokio::test]
    async fn a_panicking_parser_is_an_error() {
        let result: Result<()> = run_limited(Duration::from_secs(5), || panic!("bad input")).await;
        assert!(matches!(result, Err(Error::Validation(_))));
    }

    #[test]
    fn many_ampersands_decode_in_linear_time() {
        let text = "&".repeat(2 * 1024 * 1024);
        let start = std::time::Instant::now();
        let mut out = String::new();
        decode_into(&text, &mut out);
        assert_eq!(out.len(), text.len());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn vanished_runs_are_skipped() {
        let xml = "<w:p><w:r><w:t>shown</w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>secret</w:t></w:r><w:r><w:rPr><w:vanish w:val=\"0\"/></w:rPr><w:t> ok</w:t></w:r></w:p>";
        assert_eq!(paragraph_text(xml).unwrap(), "shown ok\n");
    }

    #[test]
    fn sectionize_finds_name_contact_and_headings() {
        let doc = sectionize(
            "JANE ROE\njane@x.org | Paris\nSummary\nBuilds things.\nWORK EXPERIENCE\n• Did a\n  long thing\n• Did b\nSkills:\nRust\n",
        );
        assert_eq!(doc.name, "JANE ROE");
        assert_eq!(doc.contact, ["jane@x.org", "Paris"]);
        assert_eq!(doc.summary, ["Builds things."]);
        assert_eq!(
            doc.sections[0].entries[0].bullets,
            ["Did a long thing", "Did b"]
        );
        assert_eq!(doc.sections[1].heading, "Skills");
        assert_eq!(kinds(&doc), ["note", "experience", "experience", "skill"]);
    }
}
