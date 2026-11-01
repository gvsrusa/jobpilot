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
//! that is not the user's. Only [`save_manual`], the user's own hand, skips that check.

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
use crate::knowledge::{self, KnowledgeItem};
use crate::parse::{KNOWN_HEADINGS, SUMMARY_HEADINGS};
use crate::profile;
use crate::render::{Entry, ResumeDoc, Template, lines, to_docx, to_pdf};

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

/// Matched as substrings of the folded claim, so `citizenship` counts.
// ponytail: this matches words, not meaning. A paraphrase passes ("my papers are in order"),
// a look-alike letter from another script passes, and an honest "advisable" or "Visa Inc."
// is flagged until it is reworded or entered by the user. The compare screen is the next gate.
pub const AUTHORIZATION_PATTERNS: &[&str] = &[
    "authorized to work",
    "authorised to work",
    "work authorization",
    "work authorisation",
    "work permit",
    "visa",
    "sponsorship",
    "citizen",
    "permanent resident",
    "green card",
    "right to work",
    "eligible to work",
];

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
/// split a word the patterns look for.
fn hides(c: char) -> bool {
    (c.is_control() && !c.is_whitespace())
        || matches!(c,
            '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}'
            | '\u{feff}' | '\u{e0000}'..='\u{e007f}')
}

/// Lower-case words split at anything that is not a letter or digit, after compatibility
/// decomposition with marks dropped: what a reader sees, not how it was typed.
fn fold(text: &str) -> String {
    let words: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c) && !hides(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn usable(item: &KnowledgeItem) -> bool {
    item.approved && !item.marked_sensitive && !item.locked
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
/// (`source = 'user'`). That status comes first, so an uncited statement is reported as what
/// it is. Items in `evidence` that are not usable count for nothing. Each `id` is empty.
// ponytail: proves only that cited ids are real evidence, not that the evidence supports the
// wording (threat path 11); the compare screen is the second gate.
pub fn check_claims(claims: &[ClaimInput], evidence: &[KnowledgeItem]) -> Vec<Claim> {
    let known: HashSet<&str> = evidence
        .iter()
        .filter(|e| usable(e))
        .map(|e| e.id.as_str())
        .collect();
    let status = |claim: &ClaimInput| {
        let folded = fold(&claim.text);
        let stated = AUTHORIZATION_PATTERNS.iter().any(|p| folded.contains(p));
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
        .enumerate()
        .map(|(position, claim)| Claim {
            id: String::new(),
            position: position as i64,
            text: claim.text.clone(),
            evidence_ids: claim.evidence_ids.clone(),
            status: status(claim).into(),
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
    let items = knowledge::list(db).await?;
    if items.iter().any(|item| item.locked) {
        return Err(Error::Locked);
    }
    let notes = profile::authorization_notes(db).await?;
    Ok(Sealed {
        notes: fragments(notes.iter().map(String::as_str)),
        marked: fragments(
            items
                .iter()
                .filter(|item| item.marked_sensitive)
                .map(|item| item.body.as_str()),
        ),
    })
}

/// Whole words only, so a sealed "D7" is not found in "ID70".
fn holds(text: &str, sealed: &[String]) -> bool {
    let text = format!(" {} ", fold(text));
    sealed.iter().any(|s| text.contains(&format!(" {s} ")))
}

/// A note is a statement about authorization; a marked text is at least not evidence. The
/// exemption is the one [`check_claims`] gives: the user typed this very sentence as evidence.
fn flag_sealed(claims: &mut [Claim], evidence: &[KnowledgeItem], sealed: &Sealed) {
    for claim in claims.iter_mut() {
        if user_entered(&claim.text, evidence) {
            continue;
        }
        if holds(&claim.text, &sealed.notes) {
            claim.status = "authorization_statement".into();
        } else if claim.status == "ok" && holds(&claim.text, &sealed.marked) {
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

async fn review(db: &Db, inputs: &[ClaimInput], when_locked: WhenLocked) -> Result<Vec<Claim>> {
    let evidence = knowledge::evidence(db).await?;
    let mut claims = check_claims(inputs, &evidence);
    match sealed_text(db).await {
        Ok(sealed) => flag_sealed(&mut claims, &evidence, &sealed),
        Err(error) if matches!(when_locked, WhenLocked::Proceed) && is_key_error(&error) => {}
        Err(error) => return Err(error),
    }
    Ok(claims)
}

/// The name, contact, and headings must be the user's: from the profile, a stock heading, or
/// any stored version. Those were the user's own, or passed this check, so a draft stays
/// editable after the profile changes.
// ponytail: a heading outside these fails the whole draft with `Err(Validation)`, like a claim
// mismatch; the Task 15 prompt has to name the stock headings.
async fn check_user_owned(db: &Db, doc: &ResumeDoc) -> Result<()> {
    let basic = profile::basic(db).await?;
    let mut names = HashSet::from([plain(&basic.name)]);
    let mut contacts: HashSet<String> = std::iter::once(&basic.email)
        .chain(&basic.countries)
        .chain(&basic.regions)
        .chain(&basic.cities)
        .map(|value| plain(value))
        .collect();
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
    Ok(())
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
}

/// One statement picks the version and inserts it, so two writers cannot pick the same one.
async fn store(db: &Db, new: NewDraft<'_>, doc: &ResumeDoc, claims: &[Claim]) -> Result<Draft> {
    let body = serde_json::to_string(doc).map_err(|e| Error::Failed(e.to_string()))?;
    let mut tx = db.pool.begin().await?;
    let row: DraftRow = sqlx::query_as(concat!(
        "INSERT INTO drafts \
         (id, kind, lineage_id, version, origin, title, job_text_id, provider, body, created_at) \
         SELECT lower(hex(randomblob(16))), ?1, coalesce(?2, lower(hex(randomblob(16)))), \
         coalesce(max(version), 0) + 1, ?3, ?4, ?5, ?6, ?7, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now') FROM drafts WHERE lineage_id = ?2 RETURNING ",
        columns!()
    ))
    .bind(new.kind)
    .bind(new.lineage_id)
    .bind(new.origin)
    .bind(new.title)
    .bind(new.job_text_id)
    .bind(new.provider)
    .bind(&body)
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
            if lineage_id.is_some() || !claims.is_empty() {
                return Err(bad("an upload starts a new lineage and has no claims"));
            }
            Vec::new()
        }
        "generated" | "edited" => {
            if origin == "edited" && !generated {
                return Err(bad("the user's own edit is saved with save_manual"));
            }
            let units = lines(&doc);
            if !claims.iter().map(|c| &c.text).eq(units.iter()) {
                return Err(bad("the claims must be the lines of the document"));
            }
            check_user_owned(db, &doc).await?;
            review(db, &claims, WhenLocked::Proceed).await?
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
        " FROM drafts WHERE ?1 IS NULL OR kind = ?1 ORDER BY created_at DESC, version DESC, id DESC"
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

/// Stored claims with a stored `ok` re-judged against the evidence as it is now.
async fn current(db: &Db, draft_id: &str, when_locked: WhenLocked) -> Result<Vec<Claim>> {
    let mut claims = stored_claims(db, draft_id).await?;
    if claims.is_empty() {
        return Ok(claims);
    }
    let live = review(db, &inputs_of(&claims), when_locked).await?;
    for (claim, live) in claims.iter_mut().zip(live) {
        if claim.status == "ok" {
            claim.status = live.status;
        }
    }
    Ok(claims)
}

/// The claims as an export would judge them now; see the module notes.
pub async fn claims(db: &Db, draft_id: &str) -> Result<Vec<Claim>> {
    current(db, draft_id, WhenLocked::Proceed).await
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

/// A new `edited` version of the claim's own version, with that line changed or gone. Later
/// versions stay in the lineage, but their changes are not in this one.
async fn revise(db: &Db, claim_id: &str, replacement: Option<ClaimInput>) -> Result<Draft> {
    let (draft_id, position): (String, i64) =
        sqlx::query_as("SELECT draft_id, position FROM claims WHERE id = ?")
            .bind(claim_id)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(Error::NotFound)?;
    let draft = get(db, &draft_id).await?;
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
/// Any other lineage stores no claims. The name, contact, and headings are the user's to set.
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
            review(db, &inputs, WhenLocked::Proceed).await?
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
    let blocked = generated.iter().filter(|c| c.status != "ok").count();
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

/// `Err(Refused("claims_blocked"))` when a claim is not `ok` now, or the claims are not the
/// document's lines. `Err(Locked)` or a key error while a sealed text cannot be read, since
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
    let draft = get(db, draft_id).await?;
    if holds_generated_text(db, &draft).await? {
        let claims = current(db, draft_id, WhenLocked::Refuse).await?;
        let aligned = claims.iter().map(|c| &c.text).eq(lines(&draft.doc).iter());
        if !aligned || claims.iter().any(|claim| claim.status != "ok") {
            return Err(Error::Refused("claims_blocked".into()));
        }
    }
    let bytes = match format {
        ExportFormat::Docx => to_docx(&draft.doc, template)?,
        ExportFormat::Pdf => to_pdf(&draft.doc, template)?,
    };
    let name = format!(
        "{}-v{}.{}",
        slug(&draft.title),
        draft.version,
        format.extension()
    );
    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(name);
    write_file(&path, &bytes)?;
    let detail = json!({ "version": draft.version, "format": format, "template": template });
    audit::record(db, actor, "export", Some(&draft.id), detail).await?;
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
            locked: false,
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
        flag_sealed(&mut claims, evidence, s);
        claims[0].status.clone()
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
