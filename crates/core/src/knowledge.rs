//! The knowledge base and pasted job texts.
//!
//! Knowledge items are what a draft may be built from. An item is evidence only when the
//! user approved it and did not mark it sensitive; a marked body is sealed in `body_enc`
//! and never reaches a model. Job text is untrusted: [`normalize`] makes it plain text and
//! reports what looks like hidden or instruction-like content, and removes none of the
//! latter. `raw` is stored as pasted and is never returned; only `normalized` is.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::crypto;
use crate::db::Db;
use crate::embed::Hit;
use crate::error::{Error, Result};

const KINDS: [&str; 6] = [
    "experience",
    "education",
    "skill",
    "project",
    "achievement",
    "note",
];
// ponytail: an item body is at most 20,000 characters and a title 300; no editing yet.
const BODY_LIMIT: usize = 20_000;
const TITLE_LIMIT: usize = 300;
const SOURCE_REF_LIMIT: usize = 200;

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeItem {
    pub id: String,
    pub kind: String,
    /// Empty while `sealed`, and for a marked item whose text was discarded.
    pub body: String,
    pub source: String,
    pub source_ref: Option<String>,
    pub approved: bool,
    pub marked_sensitive: bool,
    /// The body is ciphertext. A list never opens it: [`reveal`] does, one item at a time.
    pub sealed: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    kind: String,
    body: String,
    body_enc: Option<Vec<u8>>,
    key_id: Option<String>,
    source: String,
    source_ref: Option<String>,
    approved: bool,
    marked_sensitive: bool,
}

macro_rules! columns {
    () => {
        "id, kind, body, body_enc, key_id, source, source_ref, approved, marked_sensitive"
    };
}

impl Row {
    fn plain(self) -> KnowledgeItem {
        KnowledgeItem {
            id: self.id,
            kind: self.kind,
            body: self.body,
            source: self.source,
            source_ref: self.source_ref,
            approved: self.approved,
            marked_sensitive: self.marked_sensitive,
            sealed: self.body_enc.is_some(),
        }
    }
}

/// `source` `"user"` is approved at once; `"parsed"` waits for [`approve`].
pub async fn add(
    db: &Db,
    kind: &str,
    body: &str,
    source: &str,
    source_ref: Option<&str>,
) -> Result<KnowledgeItem> {
    if !KINDS.contains(&kind) {
        return Err(bad(
            "an item is an experience, education, skill, project, achievement, or note",
        ));
    }
    if !matches!(source, "user" | "parsed") {
        return Err(bad("a source is user or parsed"));
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(bad("an item needs some text"));
    }
    if body.chars().count() > BODY_LIMIT {
        return Err(bad("an item is at most 20000 characters"));
    }
    if source_ref.is_some_and(|text| text.chars().count() > SOURCE_REF_LIMIT) {
        return Err(bad("a source reference is at most 200 characters"));
    }
    let row: Row = sqlx::query_as(concat!(
        "INSERT INTO knowledge_items \
         (id, kind, body, source, source_ref, approved, created_at, updated_at) \
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?, ",
        now!(),
        ", ",
        now!(),
        ") RETURNING ",
        columns!()
    ))
    .bind(kind)
    .bind(body)
    .bind(source)
    .bind(source_ref)
    .bind(source == "user")
    .fetch_one(&db.pool)
    .await?;
    Ok(row.plain())
}

/// Newest first. A sealed body is listed empty and `sealed`: a list asks the keychain
/// nothing, so it works with the key gone and never prompts. [`reveal`] opens one.
pub async fn list(db: &Db) -> Result<Vec<KnowledgeItem>> {
    let rows: Vec<Row> = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM knowledge_items ORDER BY seq DESC"
    ))
    .fetch_all(&db.pool)
    .await?;
    Ok(rows.into_iter().map(Row::plain).collect())
}

/// One item's text. A sealed body is opened under the key its row names; when that key
/// cannot be had, the key's own error comes back. `NotFound` for an unknown id, and a
/// marked item whose text was discarded is a validation error.
pub async fn reveal(db: &Db, id: &str) -> Result<String> {
    let row: Row = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM knowledge_items WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)?;
    let (Some(sealed), Some(key_id)) = (row.body_enc, row.key_id) else {
        if row.marked_sensitive {
            return Err(bad(
                "this text was cleared when its key was given up; remove the item",
            ));
        }
        return Ok(row.body);
    };
    let key = crypto::key_by_id(db, &key_id).await?;
    let (table, column) = crypto::KNOWLEDGE_AAD;
    let plain = key.decrypt(table, column, &row.id, &sealed)?;
    String::from_utf8(plain).map_err(|_| bad("the text is not valid UTF-8"))
}

/// Every marked body, opened, for a check that must not miss marked text. The first key
/// error ends it, so a caller never works from some of the text. With nothing sealed the
/// keychain is not asked. A body that will not open for another reason is `Locked`. A
/// marked row still holding plain text (the app never writes one) counts as it is.
pub(crate) async fn marked_bodies(db: &Db) -> Result<Vec<String>> {
    let rows: Vec<Row> = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM knowledge_items WHERE body_enc IS NOT NULL OR marked_sensitive = 1 ORDER BY seq"
    ))
    .fetch_all(&db.pool)
    .await?;
    let (table, column) = crypto::KNOWLEDGE_AAD;
    let mut keys: Vec<crypto::DataKey> = Vec::new();
    let mut bodies = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(sealed), Some(key_id)) = (row.body_enc, row.key_id) else {
            bodies.extend(Some(row.body).filter(|body| !body.is_empty()));
            continue;
        };
        if !keys.iter().any(|key| key.key_id() == key_id) {
            keys.push(crypto::key_by_id(db, &key_id).await?);
        }
        let key = keys.iter().find(|key| key.key_id() == key_id);
        let opened = key
            .and_then(|key| key.decrypt(table, column, &row.id, &sealed).ok())
            .and_then(|plain| String::from_utf8(plain).ok());
        bodies.push(opened.ok_or(Error::Locked)?);
    }
    Ok(bodies)
}

pub async fn approve(db: &Db, id: &str, approved: bool) -> Result<()> {
    let done = sqlx::query(concat!(
        "UPDATE knowledge_items SET approved = ?, updated_at = ",
        now!(),
        " WHERE id = ?"
    ))
    .bind(approved)
    .bind(id)
    .execute(&db.pool)
    .await?;
    found(done.rows_affected())
}

/// `true` seals the body into `body_enc` and empties `body`, and drops the item's
/// embedding, which was made from the plain text. `false` reverses it. Either needs the
/// key (`Err(Locked)` or the key's own error). Marking an item that is marked, or
/// unmarking one that is not, changes nothing.
pub async fn mark_sensitive(db: &Db, id: &str, marked: bool) -> Result<()> {
    let row: Row = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM knowledge_items WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&db.pool)
    .await?
    .ok_or(Error::NotFound)?;
    let (table, column) = crypto::KNOWLEDGE_AAD;

    if marked {
        let key = crypto::unlocked(db).await?;
        if row.marked_sensitive {
            return Ok(());
        }
        let sealed = key.encrypt(table, column, id, row.body.as_bytes())?;
        let mut write = db.pool.begin().await?;
        // The old body is part of the match, so an item edited since the read is left alone.
        let done = sqlx::query(concat!(
            "UPDATE knowledge_items SET body = '', body_enc = ?, key_id = ?, \
             marked_sensitive = 1, updated_at = ",
            now!(),
            " WHERE id = ? AND marked_sensitive = 0 AND body = ?"
        ))
        .bind(sealed)
        .bind(key.key_id())
        .bind(id)
        .bind(&row.body)
        .execute(&mut *write)
        .await?
        .rows_affected();
        if done == 0 {
            return Err(changed());
        }
        sqlx::query("DELETE FROM embeddings WHERE item_id = ?")
            .bind(id)
            .execute(&mut *write)
            .await?;
        write.commit().await?;
        return Ok(());
    }

    if !row.marked_sensitive {
        return Ok(());
    }
    let (Some(sealed), Some(key_id)) = (row.body_enc, row.key_id) else {
        return Err(bad(
            "this text was cleared when its key was given up; remove the item",
        ));
    };
    let key = crypto::key_by_id(db, &key_id).await?;
    let plain = key.decrypt(table, column, id, &sealed)?;
    let body = String::from_utf8(plain).map_err(|_| bad("the text is not valid UTF-8"))?;
    let done = sqlx::query(concat!(
        "UPDATE knowledge_items SET body = ?, body_enc = NULL, key_id = NULL, \
         marked_sensitive = 0, updated_at = ",
        now!(),
        " WHERE id = ? AND body_enc = ?"
    ))
    .bind(body)
    .bind(id)
    .bind(sealed)
    .execute(&db.pool)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(changed());
    }
    Ok(())
}

/// What a draft may cite: approved, and not marked sensitive. Nothing else.
pub async fn evidence(db: &Db) -> Result<Vec<KnowledgeItem>> {
    let rows: Vec<Row> = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM knowledge_items WHERE approved = 1 AND marked_sensitive = 0 ORDER BY seq"
    ))
    .fetch_all(&db.pool)
    .await?;
    Ok(rows.into_iter().map(Row::plain).collect())
}

/// Case-insensitive substring match over what a draft may cite, newest last, score 0.0.
/// Knowledge is not in FTS5. The fallback when no embedding model is set up.
pub async fn search_text(db: &Db, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    Ok(evidence(db)
        .await?
        .into_iter()
        .filter(|item| item.body.to_lowercase().contains(&needle))
        .take(limit)
        .map(|item| Hit {
            item_id: item.id,
            score: 0.0,
            body: item.body,
        })
        .collect())
}

/// Cosine similarity, 0.0 when the lengths differ, either is empty, or either has no
/// length, so a bad vector never ranks high and never yields NaN.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let norm = (na * nb).sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return 0.0;
    }
    dot / norm
}

/// Little-endian `f32`, the same bytes on every platform.
pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Trailing bytes short of a whole `f32` are dropped.
pub fn from_blob(b: &[u8]) -> Vec<f32> {
    let (whole, _) = b.as_chunks::<4>();
    whole.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// Needs no key: it only deletes. Its embedding goes with it.
pub async fn remove(db: &Db, id: &str) -> Result<()> {
    let done = sqlx::query("DELETE FROM knowledge_items WHERE id = ?")
        .bind(id)
        .execute(&db.pool)
        .await?;
    found(done.rows_affected())
}

fn found(rows: u64) -> Result<()> {
    if rows == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}

fn changed() -> Error {
    Error::Refused("the item changed while it was being updated, and nothing was changed".into())
}

// ---- job texts ----------------------------------------------------------------------

/// Characters kept after normalization.
pub const JOB_TEXT_LIMIT: usize = 200_000;
// ponytail: a pasted text over 2 MB is refused, so the HTML pass stays quick.
const RAW_LIMIT: usize = 2_000_000;
// ponytail: at most 50 findings are kept per text; later ones are dropped.
const FINDINGS_LIMIT: usize = 50;
const EXCERPT_CHARS: usize = 120;
/// A tag longer than this is not read as a tag, which bounds the scan from every `<`.
const TAG_SCAN: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    /// `"instruction_like"`, `"hidden_text"`, or `"truncated"`.
    pub kind: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobText {
    pub id: String,
    pub title: String,
    pub normalized: String,
    pub findings: Vec<Finding>,
}

fn finding(kind: &str, excerpt: String) -> Finding {
    Finding {
        kind: kind.into(),
        excerpt,
    }
}

/// Plain text from untrusted input, plus what was noticed.
///
/// In order: terminal escapes (CSI, OSC and the like) go first; HTML becomes text, with
/// `<script>`, `<style>`, `<template>`, `<noscript>`, comments, and elements hidden by
/// their `style` or `hidden` attribute dropped and reported; control characters other than
/// `\n` and `\t`, and invisible formatting characters, go; the text is NFC; blank runs
/// collapse; the result is cut at [`JOB_TEXT_LIMIT`]. Lines that read like instructions to
/// a model are reported and left in place.
// ponytail: a style set by a stylesheet or a class is not seen; only inline style is.
pub fn normalize(raw: &str) -> (String, Vec<Finding>) {
    let mut findings = Vec::new();
    let text = strip_terminal_escapes(raw);
    let (text, saw_tag) = html_to_text(&text, &mut findings);
    let text = if saw_tag {
        decode_entities(&text)
    } else {
        text
    };
    let text = clean_chars(&text, &mut findings);
    let text: String = text.nfc().collect();
    let mut text = collapse_blank_lines(&text);

    let length = text.chars().count();
    let mut cut = None;
    if length > JOB_TEXT_LIMIT {
        text = text.chars().take(JOB_TEXT_LIMIT).collect();
        cut = Some(finding(
            "truncated",
            format!("kept {JOB_TEXT_LIMIT} of {length} characters"),
        ));
    }
    for line in text.lines().filter(|line| instruction_like(line)) {
        findings.push(finding("instruction_like", excerpt(line, EXCERPT_CHARS)));
    }
    // Cut text is the finding the reader needs most, so it keeps its slots.
    findings.extend(cut);
    let (cuts, mut findings): (Vec<_>, Vec<_>) =
        findings.into_iter().partition(|f| f.kind == "truncated");
    findings.truncate(FINDINGS_LIMIT.saturating_sub(cuts.len()));
    findings.extend(cuts);
    (text, findings)
}

const INSTRUCTION_MARKERS: [&str; 12] = [
    "ignore previous",
    "ignore all previous",
    "ignore the previous",
    "ignore prior",
    "ignore the above",
    "you are",
    "system:",
    "assistant:",
    "disregard",
    "as an ai",
    "include the word",
    "forget your instructions",
];

fn instruction_like(line: &str) -> bool {
    let lower = line.to_lowercase();
    INSTRUCTION_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn excerpt(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", &flat[..end]),
        None => flat,
    }
}

/// Removes CSI (`ESC [` or U+009B), OSC and the other string sequences (`ESC ] P X ^ _`,
/// U+009D), and two-byte escapes, whole. An unterminated string loses only its introducer,
/// so a stray escape cannot swallow the posting.
fn strip_terminal_escapes(input: &str) -> String {
    let c: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut cache = None;
    while i < c.len() {
        match c[i] {
            '\u{1b}' | '\u{9b}' | '\u{9d}' => i = skip_escape(&c, i, &mut cache),
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// What was last found by looking for a string's end: scanning from `.0` found the first
/// terminator at `.1` (`None`: there is none). It answers every later start up to that
/// terminator without another scan, so a run of unterminated strings costs one pass.
type Terminator = Option<(usize, Option<(usize, usize)>)>;

/// The first BEL, U+009C, or ESC `\` at or after `from`, and the index after it.
fn terminator_after(c: &[char], from: usize, cache: &mut Terminator) -> Option<(usize, usize)> {
    if let Some((scanned, found)) = *cache
        && from >= scanned
        && found.is_none_or(|(at, _)| from <= at)
    {
        return found;
    }
    let found = (from..c.len()).find_map(|j| match c[j] {
        '\u{7}' | '\u{9c}' => Some((j, j + 1)),
        '\u{1b}' if c.get(j + 1) == Some(&'\\') => Some((j, j + 2)),
        _ => None,
    });
    *cache = Some((from, found));
    found
}

/// The index after the escape sequence that starts at `i`.
fn skip_escape(c: &[char], i: usize, cache: &mut Terminator) -> usize {
    let (csi, string) = match c[i] {
        '\u{9b}' => (true, false),
        '\u{9d}' => (false, true),
        _ => match c.get(i + 1) {
            Some('[') => (true, false),
            Some(']' | 'P' | 'X' | '^' | '_') => (false, true),
            _ => (false, false),
        },
    };
    let body = if c[i] == '\u{1b}' { i + 2 } else { i + 1 };
    if csi {
        let mut j = body;
        while j < c.len() && ('\u{20}'..='\u{3f}').contains(&c[j]) {
            j += 1;
        }
        return if j < c.len() && ('\u{40}'..='\u{7e}').contains(&c[j]) {
            j + 1
        } else {
            body
        };
    }
    if string {
        return match terminator_after(c, body, cache) {
            Some((at, after)) if at - body <= TAG_SCAN => after,
            _ => body,
        };
    }
    if c[i] != '\u{1b}' {
        return i + 1;
    }
    // ESC, optional intermediates, then a final byte: charset selection and the like.
    let mut j = i + 1;
    while j < c.len() && ('\u{20}'..='\u{2f}').contains(&c[j]) {
        j += 1;
    }
    match c.get(j) {
        Some(ch) if ('\u{30}'..='\u{7e}').contains(ch) => j + 1,
        _ => i + 1,
    }
}

/// Controls other than `\n` and `\t` go; `\r\n` and a lone `\r` become `\n`. So do the
/// characters that hide text without a tag ([`is_invisible`]).
pub(crate) fn clean_chars(input: &str, findings: &mut Vec<Finding>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut hidden = 0usize;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c if is_invisible(c) => hidden += 1,
            c => out.push(c),
        }
    }
    if hidden > 0 {
        findings.push(finding(
            "hidden_text",
            format!("{hidden} invisible characters removed"),
        ));
    }
    out
}

/// Unicode's format characters (category Cf) and the characters that draw as a blank but
/// count as letters or symbols, so nothing can hide between two letters. ZWNJ and ZWJ are
/// left out because scripts and emoji use them; a caller matching words drops them itself.
/// Combining marks are not here, variation selectors among them: they carry meaning in text
/// (an emoji's style, the glyph of a Japanese name), which is why plane 14 skips U+E0100 to
/// U+E01EF. A caller matching words drops them with
/// [`unicode_normalization::char::is_combining_mark`].
pub(crate) fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}'
        | '\u{890}'..='\u{891}' | '\u{8e2}' | '\u{115f}'..='\u{1160}' | '\u{180e}'
        | '\u{200b}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{feff}' | '\u{ffa0}'
        | '\u{fff0}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}'
        | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e00ff}' | '\u{e01f0}'..='\u{e0fff}')
}

fn collapse_blank_lines(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end();
        if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n")
}

// ---- HTML ---------------------------------------------------------------------------

/// Characters the HTML pass may look at, beyond reading each once. Hostile markup that
/// makes every `<` rescan a long stretch spends it, and then the rest of the text is
/// dropped, not shown, because what was left unread may be hidden.
struct Budget(usize);

impl Budget {
    fn for_input(len: usize) -> Budget {
        Budget(32 * len + 2_000_000)
    }

    fn spend(&mut self, n: usize) {
        self.0 = self.0.saturating_sub(n);
    }

    fn spent(&self) -> bool {
        self.0 == 0
    }
}

struct Tag {
    name: String,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(String, String)>,
}

fn starts_with_at(c: &[char], i: usize, pattern: &str) -> bool {
    pattern
        .chars()
        .enumerate()
        .all(|(k, p)| c.get(i + k).is_some_and(|ch| ch.eq_ignore_ascii_case(&p)))
}

fn find_from(c: &[char], from: usize, pattern: &str, budget: &mut Budget) -> Option<usize> {
    let found = (from..c.len()).find(|&i| starts_with_at(c, i, pattern));
    budget.spend(found.unwrap_or(c.len()).saturating_sub(from));
    found
}

/// A `<` starts markup only before a letter, `/`, or `!`, so "a < b" stays text.
fn tag_start(c: &[char], i: usize) -> bool {
    c.get(i) == Some(&'<')
        && c.get(i + 1)
            .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/' || *n == '!')
}

/// Reads the tag at `i` (a `<`). `None` when it does not close within [`TAG_SCAN`].
fn read_tag(c: &[char], i: usize, budget: &mut Budget) -> Option<(Tag, usize)> {
    let limit = (i + TAG_SCAN).min(c.len());
    let mut j = i + 1;
    let closing = c.get(j) == Some(&'/');
    if closing {
        j += 1;
    }
    let name_start = j;
    while j < limit && !c[j].is_whitespace() && !matches!(c[j], '/' | '>') {
        j += 1;
    }
    let name: String = c[name_start..j]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();

    let attrs_start = j;
    let mut quote: Option<char> = None;
    while j < limit {
        match (quote, c[j]) {
            (Some(q), ch) if ch == q => quote = None,
            (None, '"' | '\'') => quote = Some(c[j]),
            (None, '>') => break,
            _ => {}
        }
        j += 1;
    }
    // Only what was looked at is charged: a short tag is cheap, a failed one costs its window.
    if j >= limit || c[j] != '>' {
        budget.spend(limit - i);
        return None;
    }
    budget.spend(j + 1 - i);
    let attr_text = &c[attrs_start..j];
    let self_closing = attr_text.last() == Some(&'/');
    Some((
        Tag {
            name,
            closing,
            self_closing,
            attrs: parse_attrs(attr_text),
        },
        j + 1,
    ))
}

fn parse_attrs(c: &[char]) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && (c[i].is_whitespace() || c[i] == '/') {
            i += 1;
        }
        let start = i;
        while i < c.len() && !c[i].is_whitespace() && !matches!(c[i], '=' | '/') {
            i += 1;
        }
        let name: String = c[start..i].iter().collect::<String>().to_ascii_lowercase();
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if c.get(i) == Some(&'=') {
            i += 1;
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            match c.get(i) {
                Some(&q @ ('"' | '\'')) => {
                    i += 1;
                    while i < c.len() && c[i] != q {
                        value.push(c[i]);
                        i += 1;
                    }
                    i += 1;
                }
                _ => {
                    while i < c.len() && !c[i].is_whitespace() {
                        value.push(c[i]);
                        i += 1;
                    }
                }
            }
        }
        if !name.is_empty() {
            attrs.push((name, value));
        }
    }
    attrs
}

/// Hidden by inline style, or by the `hidden` attribute.
fn is_hidden(attrs: &[(String, String)]) -> bool {
    attrs.iter().any(|(name, value)| match name.as_str() {
        "hidden" => true,
        "style" => value.split(';').any(|declaration| {
            let Some((property, value)) = declaration.split_once(':') else {
                return false;
            };
            let value = decode_entities(&value.to_ascii_lowercase());
            let value = value.replace("!important", "");
            let value = value.trim();
            match property.trim().to_ascii_lowercase().as_str() {
                "display" => value == "none",
                "visibility" => value == "hidden",
                "font-size" | "opacity" => leading_number(value) == Some(0.0),
                _ => false,
            }
        }),
        _ => false,
    })
}

fn leading_number(value: &str) -> Option<f64> {
    let end = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    value[..end].parse().ok()
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr"
            | "img"
            | "input"
            | "meta"
            | "link"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "ul"
            | "ol"
            | "table"
            | "section"
            | "article"
            | "tr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "footer"
            | "blockquote"
    )
}

/// Script and style hold raw text, so their end is the next `</name`, whatever is inside.
fn is_raw_text(name: &str) -> bool {
    matches!(name, "script" | "style")
}

fn drops_always(name: &str) -> bool {
    is_raw_text(name) || matches!(name, "template" | "noscript")
}

/// Where the hidden element that opened just before `from` ends: the end of its content
/// and the index after its close tag. Same-name nesting is counted. An element that never
/// closes runs to the end of the input, as a browser would hide it.
fn hidden_end(c: &[char], from: usize, name: &str, budget: &mut Budget) -> (usize, usize) {
    if is_raw_text(name) {
        let close = format!("</{name}");
        return match find_from(c, from, &close, budget) {
            Some(at) => (
                at,
                c[at..]
                    .iter()
                    .position(|&ch| ch == '>')
                    .map_or(c.len(), |p| at + p + 1),
            ),
            None => (c.len(), c.len()),
        };
    }
    let mut depth = 1;
    let mut i = from;
    while i < c.len() {
        budget.spend(1);
        if budget.spent() {
            return (c.len(), c.len());
        }
        if !tag_start(c, i) {
            i += 1;
            continue;
        }
        if starts_with_at(c, i, "<!--") {
            i = find_from(c, i + 4, "-->", budget).map_or(c.len(), |end| end + 3);
            continue;
        }
        let Some((tag, next)) = read_tag(c, i, budget) else {
            i += 1;
            continue;
        };
        if tag.name == name && !tag.self_closing && !is_void(name) {
            if tag.closing {
                depth -= 1;
                if depth == 0 {
                    return (i, next);
                }
            } else {
                depth += 1;
            }
        }
        i = next;
    }
    (c.len(), c.len())
}

/// Text with every tag cut out, for an excerpt of what was hidden. It does not recurse and
/// stops once it has more than an excerpt can use.
fn flat_text(c: &[char], budget: &mut Budget) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() && out.len() < 2000 && !budget.spent() {
        match read_if_tag(c, i, budget) {
            Some(next) => i = next,
            None => {
                out.push(c[i]);
                i += 1;
            }
        }
    }
    out
}

fn read_if_tag(c: &[char], i: usize, budget: &mut Budget) -> Option<usize> {
    if !tag_start(c, i) {
        return None;
    }
    read_tag(c, i, budget).map(|(_, next)| next)
}

/// Returns the text and whether any tag was seen.
fn html_to_text(input: &str, findings: &mut Vec<Finding>) -> (String, bool) {
    let c: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut saw_tag = false;
    let mut budget = Budget::for_input(c.len());
    let mut i = 0;
    while i < c.len() && !budget.spent() {
        if !tag_start(&c, i) {
            out.push(c[i]);
            i += 1;
            continue;
        }
        if starts_with_at(&c, i, "<!--") {
            saw_tag = true;
            let end = find_from(&c, i + 4, "-->", &mut budget);
            let inside = &c[i + 4..end.unwrap_or(c.len())];
            let said = excerpt(&flat_text(inside, &mut budget), EXCERPT_CHARS);
            if !said.is_empty() {
                findings.push(finding("hidden_text", said));
            }
            i = end.map_or(c.len(), |end| end + 3);
            continue;
        }
        if c[i + 1] == '!' {
            // A doctype or the like; with no `>` near, the `<` is text.
            let reach = &c[i..(i + TAG_SCAN).min(c.len())];
            budget.spend(reach.len());
            let end = reach.iter().position(|&ch| ch == '>');
            saw_tag |= end.is_some();
            if end.is_none() {
                out.push('<');
            }
            i += end.map_or(1, |p| p + 1);
            continue;
        }
        let Some((tag, next)) = read_tag(&c, i, &mut budget) else {
            out.push(c[i]);
            i += 1;
            continue;
        };
        saw_tag = true;
        i = next;
        let boundary = is_block(&tag.name) || tag.name == "br" || tag.name == "hr";
        if tag.closing {
            if boundary || tag.name == "li" {
                out.push('\n');
            }
            continue;
        }
        if boundary {
            out.push('\n');
        }
        if is_void(&tag.name) || tag.self_closing {
            continue;
        }
        if drops_always(&tag.name) || is_hidden(&tag.attrs) {
            let (content_end, after) = hidden_end(&c, i, &tag.name, &mut budget);
            let said = excerpt(&flat_text(&c[i..content_end], &mut budget), EXCERPT_CHARS);
            if !said.is_empty() {
                findings.push(finding("hidden_text", said));
            }
            i = after;
        }
    }
    if budget.spent() {
        findings.push(finding(
            "truncated",
            "the markup was too tangled to read; the rest was dropped".into(),
        ));
    }
    (out, saw_tag)
}

fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| entity(&rest[1..1 + end]).map(|ch| (ch, end + 2)));
        match decoded {
            Some((ch, used)) => {
                out.push(ch);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code).filter(|&ch| ch != '\0')
        }
    }
}

// ---- job text storage ---------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct JobRow {
    id: String,
    title: String,
    normalized: String,
    findings: String,
}

impl JobRow {
    fn into_job_text(self) -> Result<JobText> {
        let findings = serde_json::from_str(&self.findings)
            .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))?;
        Ok(JobText {
            id: self.id,
            title: self.title,
            normalized: self.normalized,
            findings,
        })
    }
}

/// Stores the text as pasted and its normalized form. Only `normalized` is for a model,
/// and only as data.
pub async fn import_job_text(db: &Db, title: &str, raw: &str) -> Result<JobText> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_LIMIT {
        return Err(bad("a title is 1 to 300 characters"));
    }
    if raw.len() > RAW_LIMIT {
        return Err(Error::TooLarge {
            limit: RAW_LIMIT as u64,
        });
    }
    let (normalized, findings) = normalize(raw);
    if normalized.trim().is_empty() {
        return Err(bad("nothing readable is left in this text"));
    }
    let findings_text =
        serde_json::to_string(&findings).map_err(|error| Error::Validation(error.to_string()))?;
    let row: JobRow = sqlx::query_as(concat!(
        "INSERT INTO job_texts (id, title, raw, normalized, findings, created_at) \
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ",
        now!(),
        ") RETURNING id, title, normalized, findings"
    ))
    .bind(title)
    .bind(raw)
    .bind(&normalized)
    .bind(findings_text)
    .fetch_one(&db.pool)
    .await?;
    row.into_job_text()
}

pub async fn get_job_text(db: &Db, id: &str) -> Result<JobText> {
    let row: JobRow =
        sqlx::query_as("SELECT id, title, normalized, findings FROM job_texts WHERE id = ?")
            .bind(id)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(Error::NotFound)?;
    row.into_job_text()
}

/// Newest first.
pub async fn list_job_texts(db: &Db) -> Result<Vec<JobText>> {
    let rows: Vec<JobRow> =
        sqlx::query_as("SELECT id, title, normalized, findings FROM job_texts ORDER BY seq DESC")
            .fetch_all(&db.pool)
            .await?;
    rows.into_iter().map(JobRow::into_job_text).collect()
}
