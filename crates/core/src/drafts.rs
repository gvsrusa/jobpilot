//! Draft versions, their claims, and the guards in front of an export.
//!
//! FR-7.12 and FR-4.10 live here. A version is never rewritten; every change is a new row.
//! From the first generated version of a lineage on, every line of [`lines`] is a claim,
//! and a claim that is not `ok` blocks the export. The stored status is the worst it can
//! be: [`claims`], [`compare`], and [`export`] check again, because an item can stop being
//! evidence after the draft was stored, and they never turn a stored flag back into `ok`.
//!
//! What a document shows outside [`lines`] (name, contact, section headings) is not a
//! claim, so a model must not be able to write it: [`create_version`] refuses any value
//! that is not the user's, and any value that states what a claim may not. [`save_manual`],
//! the user's own hand, sets them freely, except for such a statement in a generated lineage.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::knowledge::{self, KnowledgeItem, is_invisible};
use crate::parse::{KNOWN_HEADINGS, SUMMARY_HEADINGS};
use crate::profile;
use crate::render::{Entry, ResumeDoc, Template, compact_order, lines, to_docx, to_pdf};

const TITLE_LIMIT: usize = 300;
const SLUG_CHARS: usize = 60;
/// What a one-word note says about nothing; matching it would flag ordinary sentences.
const FILLER: [&str; 5] = ["no", "yes", "none", "na", "n a"];

macro_rules! columns {
    () => {
        "id, kind, lineage_id, version, origin, title, job_text_id, provider, body, created_at"
    };
}

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub id: String,
    pub kind: String,
    pub lineage_id: String,
    pub version: i64,
    pub origin: String,
    pub title: String,
    pub job_text_id: Option<String>,
    /// The current text of this draft's job, or `job_text_id` when no job holds it.
    pub current_text_id: Option<String>,
    pub provider: Option<String>,
    pub doc: ResumeDoc,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Claim {
    /// Empty until the claim is stored.
    pub id: String,
    pub position: i64,
    pub text: String,
    pub evidence_ids: Vec<String>,
    /// `"ok"`, `"no_evidence"`, or `"authorization_statement"`. The schema also allows
    /// `"removed"`; nothing writes it, because a removed line has no row in the new version
    /// and the old version is not changed.
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Compare {
    pub original: Vec<String>,
    pub generated: Vec<Claim>,
    pub evidence: Vec<KnowledgeItem>,
    pub blocked: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimInput {
    pub text: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Docx,
    Pdf,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            ExportFormat::Docx => "docx",
            ExportFormat::Pdf => "pdf",
        }
    }
}

/// Matched at the start of a word of the folded text, so `citizenship` counts and
/// `noncitizen` needs its own entry; [`WHOLE_WORD_PATTERNS`] must match a word exactly.
// ponytail: this matches words, not meaning. A paraphrase passes ("my papers are in order"),
// a look-alike letter from another script passes, and an honest "Visa Inc." or "opt in" is
// flagged until it is reworded or entered by the user. The compare screen is the next gate.
pub const AUTHORIZATION_PATTERNS: &[&str] = &[
    "authorized to work",
    "authorised to work",
    "unauthorized to work",
    "unauthorised to work",
    "authorization to work",
    "authorisation to work",
    "work authorization",
    "work authorisation",
    "work authorized",
    "work authorised",
    "work permit",
    "residence permit",
    "visa",
    "visas",
    "sponsor",
    "citizen",
    "noncitizen",
    "permanent resident",
    "permanent residence",
    "permanent residency",
    "green card",
    "blue card",
    "passport",
    "right to work",
    "eligible to work",
    "allowed to work",
    "permitted to work",
    "entitled to work",
    "immigration",
    "legally authorized",
    "legally authorised",
    "legally allowed",
    "legally eligible",
    "legally entitled",
    "legally permitted",
    "legally able to work",
    "legally work",
    "h1b",
    "h 1b",
    "h1 b",
    "h 1 b",
    "ead",
    "opt",
];

/// After "able to work in", in lower case. A name not here still counts when capitalized.
// ponytail: the countries job applications name most, not all of them. A lower-case or
// all-caps "able to work in chile" passes; add a name when one is missed.
const COUNTRIES: &[&str] = &[
    "argentina",
    "australia",
    "austria",
    "belgium",
    "brazil",
    "bulgaria",
    "canada",
    "china",
    "croatia",
    "cyprus",
    "czechia",
    "czech republic",
    "denmark",
    "egypt",
    "england",
    "estonia",
    "finland",
    "france",
    "germany",
    "greece",
    "hong kong",
    "hungary",
    "iceland",
    "india",
    "indonesia",
    "ireland",
    "israel",
    "italy",
    "japan",
    "latvia",
    "lithuania",
    "luxembourg",
    "malaysia",
    "malta",
    "mexico",
    "netherlands",
    "new zealand",
    "nigeria",
    "norway",
    "pakistan",
    "philippines",
    "poland",
    "portugal",
    "romania",
    "scotland",
    "singapore",
    "slovakia",
    "slovenia",
    "south africa",
    "south korea",
    "spain",
    "sweden",
    "switzerland",
    "taiwan",
    "thailand",
    "turkey",
    "ukraine",
    "united arab emirates",
    "united kingdom",
    "united states",
    "uk",
    "usa",
    "vietnam",
    "wales",
];
/// Capitalized at the start of a heading or a title, and no place.
const NOT_PLACES: &[&str] = &[
    "a", "an", "fast", "remote", "remotely", "shift", "shifts", "team", "teams", "weekend",
    "weekends",
];

/// In [`AUTHORIZATION_PATTERNS`]: short enough to begin other words (`visage`, `option`).
const WHOLE_WORD_PATTERNS: [&str; 4] = ["visa", "visas", "ead", "opt"];

fn bad(message: &str) -> Error {
    Error::Validation(message.into())
}

fn decode(error: serde_json::Error) -> Error {
    Error::Db(sqlx::Error::Decode(Box::new(error)))
}

#[derive(sqlx::FromRow)]
struct DraftRow {
    id: String,
    kind: String,
    lineage_id: String,
    version: i64,
    origin: String,
    title: String,
    job_text_id: Option<String>,
    /// Only [`list`] selects it.
    #[sqlx(default)]
    current_text_id: Option<String>,
    provider: Option<String>,
    body: String,
    created_at: String,
}

impl DraftRow {
    fn into_draft(self) -> Result<Draft> {
        Ok(Draft {
            doc: serde_json::from_str(&self.body).map_err(decode)?,
            id: self.id,
            kind: self.kind,
            lineage_id: self.lineage_id,
            version: self.version,
            origin: self.origin,
            title: self.title,
            current_text_id: self.current_text_id.or_else(|| self.job_text_id.clone()),
            job_text_id: self.job_text_id,
            provider: self.provider,
            created_at: self.created_at,
        })
    }
}

fn into_drafts(rows: Vec<DraftRow>) -> Result<Vec<Draft>> {
    rows.into_iter().map(DraftRow::into_draft).collect()
}

// ---- the deterministic check ------------------------------------------------------------

/// Case and whitespace only: the comparison the exemption and the owned fields use.
fn plain(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Takes no room on the page, or is dropped by the renderer, so a model cannot use it to
/// split a word the patterns look for. Only tab and the line breaks separate words: the
/// renderer's `clean` drops the other controls and U+FFFE, U+FFFF. ZWNJ and ZWJ are only
/// spared in stored text.
fn hides(c: char) -> bool {
    (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
        || is_invisible(c)
        || matches!(c, '\u{200c}' | '\u{200d}' | '\u{fffe}' | '\u{ffff}')
}

/// Lower-case words split at anything that is not a letter or digit, after compatibility
/// decomposition with marks dropped: what a reader sees, not how it was typed.
pub(crate) fn fold(text: &str) -> String {
    let words: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c) && !hides(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn states_authorization(text: &str) -> bool {
    let folded = format!(" {} ", fold(text));
    let listed = AUTHORIZATION_PATTERNS.iter().any(|pattern| {
        if WHOLE_WORD_PATTERNS.contains(pattern) {
            folded.contains(&format!(" {pattern} "))
        } else {
            folded.contains(&format!(" {pattern}"))
        }
    });
    listed || able_to_work_in_a_place(text)
}

/// "Able to work in a team" is an honest line on a resume, so this phrase needs a place
/// after it. The text keeps its case here, as [`fold`] would lose a capitalized place.
// ponytail: a capital is all that marks an unlisted place, so "Able to work in Python" is
// flagged until it is reworded, and all-caps text counts only [`COUNTRIES`].
fn able_to_work_in_a_place(text: &str) -> bool {
    let spaced: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c) && !hides(*c))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = spaced.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
    let cased = text.chars().any(char::is_lowercase);
    lower.windows(4).enumerate().any(|(at, phrase)| {
        phrase == ["able", "to", "work", "in"]
            && names_a_place(&words[at + 4..], &lower[at + 4..], cased)
    })
}

fn names_a_place(words: &[&str], lower: &[String], cased: bool) -> bool {
    let Some(first) = lower.first() else {
        return false;
    };
    let the = first == "the";
    let name = if the { &lower[1..] } else { lower };
    if the
        && name
            .first()
            .is_some_and(|word| matches!(word.as_str(), "us" | "eu"))
    {
        return true;
    }
    if COUNTRIES.iter().any(|country| {
        let parts = country.split(' ').count();
        name.len() >= parts
            && country
                .split(' ')
                .eq(name[..parts].iter().map(String::as_str))
    }) {
        return true;
    }
    let capitalized = words[0].chars().next().is_some_and(char::is_uppercase);
    !the && cased && capitalized && !NOT_PLACES.contains(&first.as_str())
}

/// Line indexes in each order a reader meets them: as stored, and as Compact prints them when
/// these are the document's lines. A list that is not is refused at export anyway.
fn print_orders(doc: &ResumeDoc, claims: &[ClaimInput]) -> Vec<Vec<usize>> {
    let mut orders = vec![(0..claims.len()).collect()];
    if claims.iter().map(|c| &c.text).eq(lines(doc).iter()) {
        orders.push(compact_order(doc));
    }
    orders
}

/// For each line, whether `hit` holds for the fewest neighbouring lines that make it, in any of
/// `orders`. Each order is read whole first, so a clean document costs one read per order. The
/// window then slides, which marks those lines and no others: a line that makes a statement
/// itself does not taint the line beside it.
fn hits_by_line(texts: &[&str], orders: &[Vec<usize>], hit: impl Fn(&str) -> bool) -> Vec<bool> {
    let mut found = vec![false; texts.len()];
    for order in orders {
        let read = |span: &[usize]| {
            let span: Vec<&str> = span.iter().map(|&line| texts[line]).collect();
            hit(&span.join(" "))
        };
        if !read(order) {
            continue;
        }
        let (mut start, mut end) = (0, 0);
        while end < order.len() {
            if !read(&order[start..=end]) {
                end += 1;
                continue;
            }
            if start == end || !read(&order[start + 1..=end]) {
                for &line in &order[start..=end] {
                    found[line] = true;
                }
            }
            start += 1;
            end = end.max(start);
        }
    }
    found
}

pub(crate) fn usable(item: &KnowledgeItem) -> bool {
    item.approved && !item.marked_sensitive && !item.sealed
}

/// The claim is, whole, the body of something the user typed in.
fn user_entered(text: &str, evidence: &[KnowledgeItem]) -> bool {
    let text = plain(text);
    evidence
        .iter()
        .any(|e| usable(e) && e.source == "user" && plain(&e.body) == text)
}

/// Deterministic (M-17, M-18). `evidence_ids` must be non-empty and every id must be in
/// `evidence` (approved, unmarked, user or approved-parsed). Any claim text matching the
/// authorization patterns gets status `authorization_statement` unless the same sentence
/// (case-folded, whitespace-collapsed) is the body of an evidence item the user entered
/// (`source = 'user'`). A statement split across neighbouring claims flags each claim it
/// spans. That status comes first, so an uncited statement is reported as what it is. Items in
/// `evidence` that are not usable count for nothing. Each `id` is empty.
// ponytail: proves only that cited ids are real evidence, not that the evidence supports the
// wording (threat path 11); the compare screen is the second gate.
pub fn check_claims(claims: &[ClaimInput], evidence: &[KnowledgeItem]) -> Vec<Claim> {
    check_read(claims, evidence, &[(0..claims.len()).collect()])
}

/// [`check_claims`], with a split statement looked for in each of `orders`.
fn check_read(
    claims: &[ClaimInput],
    evidence: &[KnowledgeItem],
    orders: &[Vec<usize>],
) -> Vec<Claim> {
    let known: HashSet<&str> = evidence
        .iter()
        .filter(|e| usable(e))
        .map(|e| e.id.as_str())
        .collect();
    let texts: Vec<&str> = claims.iter().map(|claim| claim.text.as_str()).collect();
    let stated = hits_by_line(&texts, orders, states_authorization);
    let status = |claim: &ClaimInput, stated: bool| {
        if stated && !user_entered(&claim.text, evidence) {
            "authorization_statement"
        } else if claim.evidence_ids.is_empty()
            || !claim
                .evidence_ids
                .iter()
                .all(|id| known.contains(id.as_str()))
        {
            "no_evidence"
        } else {
            "ok"
        }
    };
    claims
        .iter()
        .zip(stated)
        .enumerate()
        .map(|(position, (claim, stated))| Claim {
            id: String::new(),
            position: position as i64,
            text: claim.text.clone(),
            evidence_ids: claim.evidence_ids.clone(),
            status: status(claim, stated).into(),
        })
        .collect()
}

/// Text only the user may put in a document: authorization notes (the `Stated` rows' free
/// text) and marked knowledge, folded, one entry per line.
#[derive(Default)]
struct Sealed {
    notes: Vec<String>,
    marked: Vec<String>,
}

fn fragments<'a>(texts: impl Iterator<Item = &'a str>) -> Vec<String> {
    texts
        .flat_map(str::lines)
        .map(fold)
        .filter(|line| !line.is_empty() && !FILLER.contains(&line.as_str()))
        .collect()
}

/// `Err` with a key error, or `Err(Locked)`, when a sealed text exists and cannot be opened.
/// With nothing sealed, the keychain is not touched.
async fn sealed_text(db: &Db) -> Result<Sealed> {
    let marked = knowledge::marked_bodies(db).await?;
    let notes = profile::authorization_notes(db).await?;
    Ok(Sealed {
        notes: fragments(notes.iter().map(String::as_str)),
        marked: fragments(marked.iter().map(String::as_str)),
    })
}

/// Whole words only, so a sealed "D7" is not found in "ID70".
fn holds(text: &str, sealed: &[String]) -> bool {
    let text = format!(" {} ", fold(text));
    sealed.iter().any(|s| text.contains(&format!(" {s} ")))
}

/// A note is a statement about authorization; a marked text is at least not evidence. The
/// exemption is the one [`check_claims`] gives: the user typed this very sentence as evidence.
fn flag_sealed(
    claims: &mut [Claim],
    evidence: &[KnowledgeItem],
    sealed: &Sealed,
    orders: &[Vec<usize>],
) {
    let texts: Vec<String> = claims.iter().map(|claim| claim.text.clone()).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    let notes = hits_by_line(&texts, orders, |text| holds(text, &sealed.notes));
    let marked = hits_by_line(&texts, orders, |text| holds(text, &sealed.marked));
    for ((claim, note), mark) in claims.iter_mut().zip(notes).zip(marked) {
        if user_entered(&claim.text, evidence) {
            continue;
        }
        if note {
            claim.status = "authorization_statement".into();
        } else if claim.status == "ok" && mark {
            claim.status = "no_evidence".into();
        }
    }
}

#[derive(Clone, Copy)]
enum WhenLocked {
    /// Check what can be read. Export refuses later, so nothing leaves on this answer.
    Proceed,
    Refuse,
}

fn is_key_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Locked
            | Error::KeyMissing { .. }
            | Error::KeyMismatch { .. }
            | Error::KeyUnavailable(_)
    )
}

async fn readable_sealed_text(db: &Db, when_locked: WhenLocked) -> Result<Sealed> {
    match sealed_text(db).await {
        Err(error) if matches!(when_locked, WhenLocked::Proceed) && is_key_error(&error) => {
            Ok(Sealed::default())
        }
        other => other,
    }
}

async fn review(db: &Db, doc: &ResumeDoc, inputs: &[ClaimInput]) -> Result<Vec<Claim>> {
    let evidence = knowledge::evidence(db).await?;
    let orders = print_orders(doc, inputs);
    let mut claims = check_read(inputs, &evidence, &orders);
    let sealed = readable_sealed_text(db, WhenLocked::Proceed).await?;
    flag_sealed(&mut claims, &evidence, &sealed, &orders);
    Ok(claims)
}

/// What the profile says the user's contact details are.
fn profile_contacts(basic: &profile::ProfileBasic) -> HashSet<String> {
    std::iter::once(&basic.email)
        .chain(&basic.countries)
        .chain(&basic.regions)
        .chain(&basic.cities)
        .map(|value| plain(value))
        .collect()
}

/// Whether the name, contact, or a heading says what a claim could not: an authorization
/// pattern or sealed text. These lines have no claim to carry a flag, and a stored draft does
/// not vouch for them, because an upload makes any capitalised line a heading. Text typed
/// into the profile, or entered verbatim as the user's evidence, is the user's statement, and
/// a stock heading says nothing, even when a marked item happens to hold the same word.
async fn owned_field_states(db: &Db, doc: &ResumeDoc, when_locked: WhenLocked) -> Result<bool> {
    let basic = profile::basic(db).await?;
    let evidence = knowledge::evidence(db).await?;
    let sealed = readable_sealed_text(db, when_locked).await?;
    let mut own = profile_contacts(&basic);
    own.insert(plain(&basic.name));
    let spoken = |text: &str| !own.contains(&plain(text)) && !user_entered(text, &evidence);
    let mut fields: Vec<String> = std::iter::once(&doc.name)
        .chain(
            doc.sections
                .iter()
                .map(|section| &section.heading)
                .filter(|heading| !is_stock_heading(heading)),
        )
        .filter(|field| spoken(field))
        .cloned()
        .collect();
    // Contact entries print on one line, so they are read as one.
    let contact: Vec<&str> = doc
        .contact
        .iter()
        .map(String::as_str)
        .filter(|entry| spoken(entry))
        .collect();
    fields.push(contact.join(" "));
    Ok(fields.iter().any(|field| {
        states_authorization(field) || holds(field, &sealed.notes) || holds(field, &sealed.marked)
    }))
}

/// The name, contact, and headings must be the user's: from the profile, a stock heading, or
/// any stored version, so a draft stays editable after the profile changes. Being the user's
/// says only who wrote the value; [`owned_field_states`] judges what it says.
async fn check_user_owned(db: &Db, doc: &ResumeDoc) -> Result<()> {
    let basic = profile::basic(db).await?;
    let mut names = HashSet::from([plain(&basic.name)]);
    let mut contacts = profile_contacts(&basic);
    let mut headings: HashSet<String> = KNOWN_HEADINGS
        .iter()
        .chain(&SUMMARY_HEADINGS)
        .map(|heading| plain(heading))
        .collect();
    let bodies: Vec<String> = sqlx::query_scalar("SELECT body FROM drafts")
        .fetch_all(&db.pool)
        .await?;
    for body in bodies {
        let stored: ResumeDoc = serde_json::from_str(&body).map_err(decode)?;
        names.insert(plain(&stored.name));
        contacts.extend(stored.contact.iter().map(|value| plain(value)));
        headings.extend(stored.sections.iter().map(|s| heading_key(&s.heading)));
    }
    let theirs = |set: &HashSet<String>, key: String| key.is_empty() || set.contains(&key);
    if !theirs(&names, plain(&doc.name)) {
        return Err(bad("a generated draft cannot set the name"));
    }
    if !doc.contact.iter().all(|c| theirs(&contacts, plain(c))) {
        return Err(bad("a generated draft cannot set contact details"));
    }
    if !doc
        .sections
        .iter()
        .all(|s| theirs(&headings, heading_key(&s.heading)))
    {
        return Err(bad("a generated draft cannot invent a section heading"));
    }
    refuse_owned_statement(db, doc).await
}

async fn refuse_owned_statement(db: &Db, doc: &ResumeDoc) -> Result<()> {
    if owned_field_states(db, doc, WhenLocked::Proceed).await? {
        return Err(bad(
            "the name, contact, or a heading states work authorization or sealed text; \
             enter the sentence as your own knowledge item to keep it",
        ));
    }
    Ok(())
}

fn is_stock_heading(heading: &str) -> bool {
    let key = heading_key(heading);
    KNOWN_HEADINGS
        .iter()
        .chain(&SUMMARY_HEADINGS)
        .any(|stock| *stock == key)
}

fn heading_key(heading: &str) -> String {
    plain(heading.trim_end_matches(':'))
}

// ---- storing ----------------------------------------------------------------------------

struct NewDraft<'a> {
    kind: &'a str,
    lineage_id: Option<&'a str>,
    origin: &'a str,
    title: &'a str,
    job_text_id: Option<&'a str>,
    provider: Option<&'a str>,
    /// The agent run that made a generated version, written with the row.
    run_id: Option<&'a str>,
}

/// One statement picks the version and inserts it, so two writers cannot pick the same one.
async fn store(db: &Db, new: NewDraft<'_>, doc: &ResumeDoc, claims: &[Claim]) -> Result<Draft> {
    let body = serde_json::to_string(doc).map_err(|e| Error::Failed(e.to_string()))?;
    let mut tx = db.pool.begin().await?;
    let row: DraftRow = sqlx::query_as(concat!(
        "INSERT INTO drafts \
         (id, kind, lineage_id, version, origin, title, job_text_id, provider, body, created_at, \
         run_id) \
         SELECT lower(hex(randomblob(16))), ?1, coalesce(?2, lower(hex(randomblob(16)))), \
         coalesce(max(version), 0) + 1, ?3, ?4, ?5, ?6, ?7, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?8 FROM drafts WHERE lineage_id = ?2 RETURNING ",
        columns!()
    ))
    .bind(new.kind)
    .bind(new.lineage_id)
    .bind(new.origin)
    .bind(new.title)
    .bind(new.job_text_id)
    .bind(new.provider)
    .bind(&body)
    .bind(new.run_id)
    .fetch_one(&mut *tx)
    .await?;
    for claim in claims {
        let ids =
            serde_json::to_string(&claim.evidence_ids).map_err(|e| Error::Failed(e.to_string()))?;
        sqlx::query(
            "INSERT INTO claims (id, draft_id, position, text, evidence_ids, status) \
             VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(claim.position)
        .bind(&claim.text)
        .bind(ids)
        .bind(&claim.status)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    row.into_draft()
}

/// The version a run already made, if any.
pub(crate) async fn of_run(db: &Db, run_id: &str) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM drafts WHERE run_id = ? ORDER BY version DESC")
            .bind(run_id)
            .fetch_optional(&db.pool)
            .await?,
    )
}

/// Whether a lineage has a generated version, after checking it exists and is of `kind`.
async fn lineage_is_generated(db: &Db, lineage_id: &str, kind: &str) -> Result<bool> {
    let (found, generated): (Option<String>, Option<bool>) = sqlx::query_as(
        "SELECT max(kind), max(origin = 'generated') FROM drafts WHERE lineage_id = ?",
    )
    .bind(lineage_id)
    .fetch_one(&db.pool)
    .await?;
    match found {
        None => Err(Error::NotFound),
        Some(found) if found != kind => Err(bad("this lineage holds another kind of draft")),
        Some(_) => Ok(generated.unwrap_or(false)),
    }
}

fn check_kind(kind: &str) -> Result<()> {
    if !matches!(kind, "resume" | "cover_letter") {
        return Err(bad("a draft is a resume or a cover letter"));
    }
    Ok(())
}

/// `version` is `max(version) + 1` within the lineage, or 1 in a new lineage.
///
/// `generated`, and `edited` in a lineage with a generated version, need one claim per entry
/// of [`lines`] with identical text (`Err(Validation)`, nothing stored) and the user's own
/// name, contact, and headings; claims are checked and stored with their status. `uploaded`
/// starts a new lineage and stores no claims. An `edited` version of anything else goes
/// through [`save_manual`], so this function never stores unclaimed text under a lineage
/// that a model has written to.
#[allow(clippy::too_many_arguments)]
pub async fn create_version(
    db: &Db,
    kind: &str,
    lineage_id: Option<&str>,
    origin: &str,
    title: &str,
    job_text_id: Option<&str>,
    provider: Option<&str>,
    doc: ResumeDoc,
    claims: Vec<ClaimInput>,
) -> Result<Draft> {
    let made = Made {
        kind,
        lineage_id,
        origin,
        title,
        job_text_id,
        provider,
        run_id: None,
    };
    create_version_for(db, made, doc, claims).await
}

/// What [`create_version`] takes besides the document and its claims, and the run that asked.
pub(crate) struct Made<'a> {
    pub kind: &'a str,
    pub lineage_id: Option<&'a str>,
    pub origin: &'a str,
    pub title: &'a str,
    pub job_text_id: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub run_id: Option<&'a str>,
}

/// What refuses a generated or edited version whole. A failure is `Err(Validation)`.
pub(crate) async fn check_generated(db: &Db, doc: &ResumeDoc, claims: &[ClaimInput]) -> Result<()> {
    let units = lines(doc);
    if !claims.iter().map(|c| &c.text).eq(units.iter()) {
        return Err(bad("the claims must be the lines of the document"));
    }
    check_user_owned(db, doc).await
}

/// As [`create_version`], with the run's id written in the same transaction as the version.
pub(crate) async fn create_version_for(
    db: &Db,
    made: Made<'_>,
    doc: ResumeDoc,
    claims: Vec<ClaimInput>,
) -> Result<Draft> {
    let Made {
        kind,
        lineage_id,
        origin,
        title,
        job_text_id,
        provider,
        run_id,
    } = made;
    check_kind(kind)?;
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_LIMIT {
        return Err(bad("a title is 1 to 300 characters"));
    }
    let generated = match lineage_id {
        Some(id) => lineage_is_generated(db, id, kind).await?,
        None => false,
    };
    let checked = match origin {
        "uploaded" => {
            if lineage_id.is_some()
                || !claims.is_empty()
                || provider.is_some()
                || job_text_id.is_some()
            {
                return Err(bad(
                    "an upload starts a new lineage with no claims, provider, or job text",
                ));
            }
            Vec::new()
        }
        "generated" | "edited" => {
            if origin == "edited" && !generated {
                return Err(bad("the user's own edit is saved with save_manual"));
            }
            check_generated(db, &doc, &claims).await?;
            review(db, &doc, &claims).await?
        }
        _ => return Err(bad("an origin is uploaded, generated, or edited")),
    };
    let new = NewDraft {
        kind,
        lineage_id,
        origin,
        title,
        job_text_id,
        provider,
        run_id,
    };
    store(db, new, &doc, &checked).await
}

/// Every version, newest first.
// ponytail: `drafts` has no seq, so two rows in one millisecond of different lineages have
// no meaningful order.
pub async fn list(db: &Db, kind: Option<&str>) -> Result<Vec<Draft>> {
    let rows: Vec<DraftRow> = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        ", COALESCE((SELECT j.job_text_id FROM job_texts t JOIN jobs j ON j.id = t.version_of \
                     WHERE t.id = d.job_text_id), d.job_text_id) AS current_text_id \
         FROM drafts d WHERE ?1 IS NULL OR kind = ?1 ORDER BY created_at DESC, version DESC, id DESC"
    ))
    .bind(kind)
    .fetch_all(&db.pool)
    .await?;
    into_drafts(rows)
}

/// Oldest first.
pub async fn versions(db: &Db, lineage_id: &str) -> Result<Vec<Draft>> {
    let rows: Vec<DraftRow> = sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM drafts WHERE lineage_id = ? ORDER BY version"
    ))
    .bind(lineage_id)
    .fetch_all(&db.pool)
    .await?;
    into_drafts(rows)
}

pub async fn get(db: &Db, id: &str) -> Result<Draft> {
    let row: DraftRow = sqlx::query_as(concat!("SELECT ", columns!(), " FROM drafts WHERE id = ?"))
        .bind(id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or(Error::NotFound)?;
    row.into_draft()
}

async fn stored_claims(db: &Db, draft_id: &str) -> Result<Vec<Claim>> {
    let rows: Vec<(String, i64, String, String, String)> = sqlx::query_as(
        "SELECT id, position, text, evidence_ids, status FROM claims \
         WHERE draft_id = ? ORDER BY position",
    )
    .bind(draft_id)
    .fetch_all(&db.pool)
    .await?;
    rows.into_iter()
        .map(|(id, position, text, ids, status)| {
            Ok(Claim {
                id,
                position,
                text,
                evidence_ids: serde_json::from_str(&ids).map_err(decode)?,
                status,
            })
        })
        .collect()
}

fn inputs_of(claims: &[Claim]) -> Vec<ClaimInput> {
    claims
        .iter()
        .map(|claim| ClaimInput {
            text: claim.text.clone(),
            evidence_ids: claim.evidence_ids.clone(),
        })
        .collect()
}

/// The claims as an export would judge them now: a stored `ok` is judged again, a stored
/// flag stands. See the module notes.
pub async fn claims(db: &Db, draft_id: &str) -> Result<Vec<Claim>> {
    let mut claims = stored_claims(db, draft_id).await?;
    if claims.is_empty() {
        return Ok(claims);
    }
    let doc = get(db, draft_id).await?.doc;
    let live = review(db, &doc, &inputs_of(&claims)).await?;
    for (claim, live) in claims.iter_mut().zip(live) {
        if claim.status == "ok" {
            claim.status = live.status;
        }
    }
    Ok(claims)
}

/// The fields of [`lines`], in its order. Clearing one takes it out of the document.
fn line_slots(doc: &mut ResumeDoc) -> Vec<&mut String> {
    let entries = doc.sections.iter_mut().flat_map(|s| &mut s.entries);
    let fields = entries.flat_map(|entry| {
        let Entry {
            title,
            subtitle,
            dates,
            bullets,
        } = entry;
        [title, subtitle, dates].into_iter().chain(bullets)
    });
    doc.summary
        .iter_mut()
        .chain(fields)
        .filter(|field| !field.is_empty())
        .collect()
}

/// A new `edited` version with that line changed or gone. The claim must be from the
/// lineage's latest version: forking an older one would bring back what was removed since.
async fn revise(db: &Db, claim_id: &str, replacement: Option<ClaimInput>) -> Result<Draft> {
    let (draft_id, position): (String, i64) =
        sqlx::query_as("SELECT draft_id, position FROM claims WHERE id = ?")
            .bind(claim_id)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(Error::NotFound)?;
    let draft = get(db, &draft_id).await?;
    let latest: i64 = sqlx::query_scalar("SELECT max(version) FROM drafts WHERE lineage_id = ?")
        .bind(&draft.lineage_id)
        .fetch_one(&db.pool)
        .await?;
    if draft.version != latest {
        return Err(Error::Refused("not_latest_version".into()));
    }
    let stored = stored_claims(db, &draft_id).await?;
    let mut doc = draft.doc;
    let slot = usize::try_from(position)
        .ok()
        .and_then(|at| line_slots(&mut doc).into_iter().nth(at))
        .ok_or(Error::NotFound)?;
    match &replacement {
        Some(input) => slot.clone_from(&input.text),
        None => slot.clear(),
    }
    let inputs = stored
        .iter()
        .zip(inputs_of(&stored))
        .filter_map(|(claim, input)| {
            if claim.id == claim_id {
                replacement.clone()
            } else {
                Some(input)
            }
        })
        .collect();
    create_version(
        db,
        &draft.kind,
        Some(&draft.lineage_id),
        "edited",
        &draft.title,
        draft.job_text_id.as_deref(),
        draft.provider.as_deref(),
        doc,
        inputs,
    )
    .await
}

/// Checks the new text again; the line is replaced in a new `edited` version, returned.
pub async fn edit_claim(db: &Db, claim_id: &str, input: ClaimInput) -> Result<Draft> {
    if input.text.trim().is_empty() {
        return Err(bad("a claim needs some text; remove it instead"));
    }
    revise(db, claim_id, Some(input)).await
}

/// A new `edited` version without the line.
pub async fn remove_claim(db: &Db, claim_id: &str) -> Result<Draft> {
    revise(db, claim_id, None).await
}

/// The user's own edit (FR-3.23), `edited`. The title comes from the lineage's latest
/// version, else `doc.name`. In a lineage with a generated version each line keeps the
/// evidence of the claim with the same text and other lines get none, so they block export.
/// Any other lineage stores no claims. The name, contact, and headings are the user's to set,
/// but in a lineage with a generated version they may not state authorization or sealed text.
pub async fn save_manual(
    db: &Db,
    kind: &str,
    lineage_id: Option<&str>,
    doc: ResumeDoc,
) -> Result<Draft> {
    check_kind(kind)?;
    let (generated, latest) = match lineage_id {
        Some(id) => {
            let generated = lineage_is_generated(db, id, kind).await?;
            let row: DraftRow = sqlx::query_as(concat!(
                "SELECT ",
                columns!(),
                " FROM drafts WHERE lineage_id = ? ORDER BY version DESC LIMIT 1"
            ))
            .bind(id)
            .fetch_one(&db.pool)
            .await?;
            (generated, Some(row))
        }
        None => (false, None),
    };
    let claims = match (&latest, generated) {
        (Some(latest), true) => {
            refuse_owned_statement(db, &doc).await?;
            let prior = stored_claims(db, &latest.id).await?;
            let inputs: Vec<ClaimInput> = lines(&doc)
                .into_iter()
                .map(|line| ClaimInput {
                    evidence_ids: prior
                        .iter()
                        .find(|claim| claim.text == line)
                        .map(|claim| claim.evidence_ids.clone())
                        .unwrap_or_default(),
                    text: line,
                })
                .collect();
            review(db, &doc, &inputs).await?
        }
        _ => Vec::new(),
    };
    let name: String = doc.name.trim().chars().take(TITLE_LIMIT).collect();
    let title = match &latest {
        Some(latest) => latest.title.as_str(),
        None if name.is_empty() => "Untitled",
        None => &name,
    };
    let new = NewDraft {
        kind,
        lineage_id,
        origin: "edited",
        title,
        job_text_id: latest.as_ref().and_then(|l| l.job_text_id.as_deref()),
        provider: latest.as_ref().and_then(|l| l.provider.as_deref()),
        run_id: None,
    };
    store(db, new, &doc, &claims).await
}

/// `original` defaults to the newest uploaded resume, and to nothing for a cover letter.
/// `evidence` holds the cited items that are evidence now, so never a marked one.
pub async fn compare(db: &Db, generated_id: &str, original_id: Option<&str>) -> Result<Compare> {
    let draft = get(db, generated_id).await?;
    let generated = claims(db, generated_id).await?;
    let original = match original_id {
        Some(id) => lines(&get(db, id).await?.doc),
        None if draft.kind == "resume" => {
            let newest: Option<String> = sqlx::query_scalar(
                "SELECT body FROM drafts WHERE kind = 'resume' AND origin = 'uploaded' \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .fetch_optional(&db.pool)
            .await?;
            match newest {
                Some(body) => lines(&serde_json::from_str(&body).map_err(decode)?),
                None => Vec::new(),
            }
        }
        None => Vec::new(),
    };
    let cited: HashSet<&String> = generated.iter().flat_map(|c| &c.evidence_ids).collect();
    let evidence = knowledge::evidence(db)
        .await?
        .into_iter()
        .filter(|item| cited.contains(&item.id))
        .collect();
    let flagged = generated.iter().filter(|c| c.status != "ok").count();
    // Export refuses for this too, for instance sealed text marked since the draft was stored.
    let stated = holds_generated_text(db, &draft).await?
        && owned_field_states(db, &draft.doc, WhenLocked::Proceed).await?;
    let blocked = flagged + usize::from(stated);
    Ok(Compare {
        original,
        generated,
        evidence,
        blocked,
    })
}

// ---- export -----------------------------------------------------------------------------

/// Letters and digits joined by `-`, so a title can never name a path.
fn slug(title: &str) -> String {
    let words: String = title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let slug = words.split_whitespace().collect::<Vec<_>>().join("-");
    let slug: String = slug.chars().take(SLUG_CHARS).collect();
    match slug.trim_end_matches('-') {
        "" => "draft".to_string(),
        kept => kept.to_string(),
    }
}

/// `draft` is a version at or after the lineage's first generated one.
async fn holds_generated_text(db: &Db, draft: &Draft) -> Result<bool> {
    let found: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM drafts \
         WHERE lineage_id = ? AND origin = 'generated' AND version <= ?)",
    )
    .bind(&draft.lineage_id)
    .bind(draft.version)
    .fetch_one(&db.pool)
    .await?;
    Ok(found)
}

/// Replaces the file only once it is whole, and never writes through a link at `path`.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_added_extension("tmp");
    let _ = std::fs::remove_file(&temp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    Ok(written?)
}

/// `Err(Refused("claims_blocked"))` when a claim is not `ok` now, the claims are not the
/// document's lines, or the name, contact, or a heading now states what a claim may not. `Err(Locked)` or a key error while a sealed text cannot be read, since
/// the draft cannot then be shown free of it. Neither writes a file. The file is
/// `<title-slug>-v<version>.<docx|pdf>` under `out_dir`, replacing one of that name.
// ponytail: two lineages with one title share a file name, and the later export wins.
pub async fn export(
    db: &Db,
    draft_id: &str,
    template: Template,
    format: ExportFormat,
    out_dir: &Path,
    actor: &str,
) -> Result<PathBuf> {
    export_signalling(db, draft_id, template, format, out_dir, actor, None).await
}

/// `export`, sending on `started` once the render is under way, so a test can drop the
/// future at that exact point. `export` is the only production caller.
#[doc(hidden)]
pub async fn export_signalling(
    db: &Db,
    draft_id: &str,
    template: Template,
    format: ExportFormat,
    out_dir: &Path,
    actor: &str,
    started: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<PathBuf> {
    let draft = get(db, draft_id).await?;
    if holds_generated_text(db, &draft).await? {
        // First, so sealed text that cannot be read refuses before `claims` looks past it.
        let stated = owned_field_states(db, &draft.doc, WhenLocked::Refuse).await?;
        let claims = claims(db, draft_id).await?;
        let aligned = claims.iter().map(|c| &c.text).eq(lines(&draft.doc).iter());
        let flagged = claims.iter().any(|claim| claim.status != "ok");
        if !aligned || flagged || stated {
            return Err(Error::Refused("claims_blocked".into()));
        }
    }
    let name = format!(
        "{}-v{}.{}",
        slug(&draft.title),
        draft.version,
        format.extension()
    );
    let path = out_dir.join(name);
    // Rendering and the file write run off the async threads: a runner step shares its
    // overseer's task, and a stretch that never yields would stall its timeout and heartbeat.
    // The audit row is written by a task of its own, so a cancel that drops this future past
    // its grace cannot leave a file with no row: the thread finishes, and the row follows.
    let (doc, out_dir, target) = (draft.doc.clone(), out_dir.to_owned(), path.clone());
    let work = tokio::task::spawn_blocking(move || -> Result<()> {
        let bytes = match format {
            ExportFormat::Docx => to_docx(&doc, template)?,
            ExportFormat::Pdf => to_pdf(&doc, template)?,
        };
        std::fs::create_dir_all(&out_dir)?;
        write_file(&target, &bytes)
    });
    if let Some(started) = started {
        started.send(()).ok();
    }
    let detail = json!({ "version": draft.version, "format": format, "template": template });
    let (pool, who, id) = (db.pool.clone(), actor.to_owned(), draft.id.clone());
    let landed = tokio::spawn(async move {
        work.await
            .map_err(|_| Error::Failed("export_interrupted".into()))??;
        let mut conn = pool.acquire().await?;
        let row = audit::record_to(&mut *conn, &who, "export", Some(&id), detail).await;
        if let Err(error) = &row {
            // Nobody may be waiting on this any more, so say it here: the file is on disk.
            eprintln!("export: the file was written but its audit row was not: {error}");
        }
        row
    });
    landed
        .await
        .map_err(|_| Error::Failed("export_interrupted".into()))??;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Section;

    fn claim(text: &str, status: &str) -> Claim {
        Claim {
            id: String::new(),
            position: 0,
            text: text.into(),
            evidence_ids: Vec::new(),
            status: status.into(),
        }
    }

    fn item(body: &str, source: &str) -> KnowledgeItem {
        KnowledgeItem {
            id: "e".into(),
            kind: "note".into(),
            body: body.into(),
            source: source.into(),
            source_ref: None,
            approved: true,
            marked_sensitive: false,
            sealed: false,
        }
    }

    fn sealed(notes: &[&str], marked: &[&str]) -> Sealed {
        Sealed {
            notes: fragments(notes.iter().copied()),
            marked: fragments(marked.iter().copied()),
        }
    }

    fn flagged(text: &str, status: &str, evidence: &[KnowledgeItem], s: &Sealed) -> String {
        let mut claims = [claim(text, status)];
        flag_sealed(&mut claims, evidence, s, &[vec![0]]);
        claims[0].status.clone()
    }

    #[test]
    fn able_to_work_in_counts_only_before_a_place() {
        for text in [
            "Are you able to work in Germany?",
            "able to work in germany",
            "ABLE TO WORK IN GERMANY",
            "able to work in new zealand",
            "Able to work in the US",
            "able to work in the uk",
            "Able to work in the EU",
            "Able to work in Lisbon",
            "able to w\u{200b}ork in Germany",
            "Legally able to work",
        ] {
            assert!(states_authorization(text), "{text}");
        }
        for text in [
            "Able to work in a team",
            "Able to work in teams",
            "Able To Work In Teams",
            "Able to work in remote teams",
            "Able to work in Remote settings",
            "Are you able to work weekends?",
            "Able to work in shifts",
            "Able to work in fast-paced settings",
            "Able to work in Fast-paced settings",
            "Able to work in the office",
            "ABLE TO WORK IN AGILE TEAMS",
            "Able to work in",
        ] {
            assert!(!states_authorization(text), "{text}");
        }
    }

    #[test]
    fn a_note_in_a_claim_is_an_authorization_statement() {
        let s = sealed(&["Residence card D7\nvalid to 2031"], &[]);
        for (text, status) in [
            ("Holds a Residence-Card d7.", "authorization_statement"),
            ("VALID TO 2031", "authorization_statement"),
            ("Shipped card sorting", "ok"),
            ("Residence card D70", "ok"),
        ] {
            assert_eq!(flagged(text, "ok", &[], &s), status, "{text}");
        }
        // It outranks the weaker flag.
        assert_eq!(
            flagged("valid to 2031", "no_evidence", &[], &s),
            "authorization_statement"
        );
    }

    #[test]
    fn marked_text_in_a_claim_is_not_evidence_and_stays_flagged() {
        let s = sealed(&[], &["Treated for a rare condition"]);
        assert_eq!(
            flagged("I was treated for a RARE condition.", "ok", &[], &s),
            "no_evidence"
        );
        assert_eq!(flagged("Rust", "ok", &[], &s), "ok");
        assert_eq!(
            flagged("Rust", "authorization_statement", &[], &s),
            "authorization_statement"
        );
    }

    #[test]
    fn the_users_own_verbatim_sentence_is_exempt_from_sealed_text() {
        let s = sealed(&["Residence card D7"], &["Residence card D7"]);
        let own = [item("residence  CARD d7", "user")];
        assert_eq!(flagged("Residence card D7", "ok", &own, &s), "ok");
        let parsed = [item("Residence card D7", "parsed")];
        assert_eq!(
            flagged("Residence card D7", "ok", &parsed, &s),
            "authorization_statement"
        );
    }

    #[test]
    fn filler_notes_match_nothing() {
        let s = sealed(&["no", "Yes", "N/A", "none", "  "], &[]);
        assert!(s.notes.is_empty());
        assert_eq!(flagged("No downtime, none lost", "ok", &[], &s), "ok");
    }

    #[test]
    fn a_sealed_text_split_across_two_claims_flags_both() {
        let s = sealed(&["Residence card D7"], &["Treated for a rare condition"]);
        let flag = |texts: [&str; 3]| {
            let mut claims = texts.map(|text| claim(text, "ok"));
            flag_sealed(&mut claims, &[], &s, &[vec![0, 1, 2]]);
            claims.map(|c| c.status)
        };
        assert_eq!(
            flag(["Rust", "Residence", "card D7"]),
            ["ok", "authorization_statement", "authorization_statement"]
        );
        assert_eq!(
            flag(["Treated for", "a rare condition", "Rust"]),
            ["no_evidence", "no_evidence", "ok"]
        );
    }

    #[test]
    fn one_line_stating_something_does_not_taint_its_neighbour() {
        let hit = |text: &str| text.contains("visa");
        let flat = |n: usize| vec![(0..n).collect::<Vec<usize>>()];
        assert_eq!(hits_by_line(&["a visa", "b"], &flat(2), hit), [true, false]);
        let split = |t: &str| t.contains("vi sa");
        assert_eq!(hits_by_line(&["vi", "sa"], &flat(2), split), [true, true]);
        assert_eq!(hits_by_line(&["a"], &flat(1), hit), [false]);
        assert!(hits_by_line(&[], &flat(0), hit).is_empty());
    }

    #[test]
    fn a_statement_is_marked_in_the_order_that_makes_it() {
        let hit = |text: &str| text.contains("a b");
        let orders = [vec![0, 1, 2], vec![0, 2, 1]];
        assert_eq!(
            hits_by_line(&["a", "x", "b"], &orders, hit),
            [true, false, true]
        );
    }

    #[test]
    fn line_slots_follow_lines() {
        let mut doc = ResumeDoc {
            name: "N".into(),
            contact: vec!["C".into()],
            summary: vec!["s1".into(), String::new(), "s2".into()],
            sections: vec![
                Section {
                    heading: "H".into(),
                    entries: vec![
                        Entry {
                            title: "t".into(),
                            subtitle: String::new(),
                            dates: "d".into(),
                            bullets: vec![String::new(), "b1".into()],
                        },
                        Entry {
                            title: String::new(),
                            subtitle: "u".into(),
                            dates: String::new(),
                            bullets: vec!["b2".into()],
                        },
                    ],
                },
                Section::default(),
            ],
        };
        let want = lines(&doc);
        assert_eq!(want.len(), 7);
        let got: Vec<String> = line_slots(&mut doc).iter().map(|s| s.to_string()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(slug("Senior Engineer: Acme"), "senior-engineer-acme");
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("..\\C:\\x"), "c-x");
        assert_eq!(slug("!!!"), "draft");
        assert_eq!(slug(&"a ".repeat(100)).chars().count(), 59);
    }
}
