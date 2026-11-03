//! Apply runs in Claude Code mode: the checks before a run, the run and its item rows, and
//! the pauses a person resolves. Sealed values are released only here, at start, by a person.
//! No row written here holds an answer's text.

pub mod brief;
pub mod guard;
pub mod proxy;
pub mod session;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use self::guard::PauseKind;
use crate::ai::claude_code::{self, PROVIDER};
use crate::ai::consent;
use crate::ai::{CancellationToken, DataCategory};
use crate::applications::{self, Application};
use crate::browser::runtime::{self, RuntimeState};
use crate::db::Db;
use crate::drafts::{self, ExportFormat};
use crate::error::{Error, Result};
use crate::memory::Class;
use crate::memory::usage::{self, ApprovedRow};
use crate::permissions::{self, Action, Decision, Grant, Mode, Site};
use crate::render::Template;
use crate::{agents, audit, pause, runner};

pub const MAX_RUN_ITEMS: usize = 25;
pub const PAUSE_HOLD: Duration = Duration::from_secs(30 * 60);
/// Recorded on every item: this mode rests on the owner's decision, not a register row.
pub const POLICY_NOTE: &str = "owner decision 2026-11-01";
/// What a run sends to the session, besides `Sensitive` when sealed values are released.
pub(crate) const CATEGORIES: [DataCategory; 3] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::CareerMemory,
];

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocState {
    Ready,
    Missing,
    Blocked(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationState {
    Stated,
    NotStated,
    NoCountry,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemCheck {
    pub application_id: String,
    pub company: String,
    pub role: String,
    pub host: String,
    pub country: String,
    pub resume: DocState,
    pub letter: DocState,
    /// Approved fresh normal answers the brief will hold.
    pub answers: i64,
    pub stale_left_out: i64,
    /// Sealed answers and the authorization row a release would open for this application.
    pub sealed: i64,
    pub authorization: AuthorizationState,
    pub blocks: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preflight {
    pub items: Vec<ItemCheck>,
    pub runtime: &'static str,
    pub claude: Option<String>,
    pub consent_missing: Vec<DataCategory>,
    pub sealed_total: i64,
    /// Whether consent covers `Sensitive`, without which nothing sealed can be released.
    pub sensitive_consent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: String,
    pub run_id: String,
    pub application_id: Option<String>,
    pub position: i64,
    pub host: String,
    pub state: String,
    pub submit_clicks: i64,
    pub auth_released: bool,
    pub resume_path: Option<PathBuf>,
    pub letter_path: Option<PathBuf>,
    pub policy_note: Option<String>,
    pub confirmation_text: Option<String>,
    pub confirmation_url: Option<String>,
    pub status_written: bool,
    pub reason: Option<String>,
    /// The `Sensitive` answers a person released for this item at start.
    pub released_answers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PauseView {
    pub id: String,
    pub item_id: String,
    pub kind: String,
    pub origin: String,
    pub excerpt: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunView {
    pub id: String,
    pub mode: String,
    pub state: String,
    pub items: Vec<Item>,
    pub active_pause: Option<PauseView>,
    pub created_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Continue,
    TakeOver,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Continue => "continue",
            Resolution::TakeOver => "take_over",
        }
    }
}

/// One application as checked, with what `start` stores for it.
struct Checked {
    check: ItemCheck,
    resume_path: Option<PathBuf>,
    letter_path: Option<PathBuf>,
    /// Approved, fresh `Sensitive` answers that serve this application's country.
    sealed_ids: Vec<String>,
}

/// Reads and exports only: it releases nothing and writes no run.
pub async fn preflight(db: &Db, application_ids: &[String]) -> Result<Preflight> {
    let approved = usage::approved_for(db).await?;
    let mut items = Vec::with_capacity(application_ids.len());
    for id in application_ids {
        items.push(check_item(db, "system", id, &approved).await?.check);
    }
    let runtime = match runtime::state(db).await? {
        RuntimeState::NoNode => "no_node",
        RuntimeState::NodeTooOld(_) => "node_too_old",
        RuntimeState::NotInstalled => "not_installed",
        RuntimeState::WrongVersion(_) => "wrong_version",
        RuntimeState::Ready(_) => "ready",
    };
    let claude = claude_code::find_binary(Some(db))
        .await
        .ok()
        .map(|path| path.display().to_string());
    Ok(Preflight {
        sealed_total: items.iter().map(|item| item.sealed).sum(),
        consent_missing: consent_missing(db, &CATEGORIES).await?,
        sensitive_consent: consent_missing(db, &[DataCategory::Sensitive])
            .await?
            .is_empty(),
        items,
        runtime,
        claude,
    })
}

async fn consent_missing(db: &Db, needed: &[DataCategory]) -> Result<Vec<DataCategory>> {
    let given = consent::active(db, PROVIDER).await?.unwrap_or_default();
    Ok(needed
        .iter()
        .copied()
        .filter(|category| !given.contains(category))
        .collect())
}

async fn check_item(
    db: &Db,
    actor: &str,
    application_id: &str,
    approved: &[ApprovedRow],
) -> Result<Checked> {
    let app = applications::get(db, application_id).await?;
    if !app.id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::Validation(
            "this application id cannot name a folder".into(),
        ));
    }
    let mut blocks = Vec::new();
    // A submission is never retried, whichever mode made it or whether a click was seen.
    if submitted_before(db, &app.id).await? {
        blocks.push("already_submitted");
    }
    if app.status != "saved" {
        blocks.push("not_saved");
    }
    if !permissions::link_allowed(&app.link) {
        blocks.push("link_not_https");
    } else {
        let grant = Grant {
            owner_started: true,
            authorized: false,
        };
        let site = permissions::site_of(&app.link)?;
        if let Decision::Refuse(reason) =
            permissions::decide(Mode::ClaudeCode, site, Action::Submit, &grant)
        {
            blocks.push(reason);
        }
    }
    if in_a_run(db, &app.id).await? {
        blocks.push("already_in_a_run");
    }
    let (resume, resume_path) =
        export_doc(db, actor, &app, app.resume_draft_id.as_deref(), "resume").await?;
    if matches!(resume, DocState::Blocked(_)) {
        blocks.push("resume_blocked");
    }
    let (letter, letter_path) = export_doc(
        db,
        actor,
        &app,
        app.letter_draft_id.as_deref(),
        "cover_letter",
    )
    .await?;
    if matches!(letter, DocState::Blocked(_)) {
        blocks.push("letter_blocked");
    }
    let serving: Vec<&ApprovedRow> = approved
        .iter()
        .filter(|row| row.serves(&app.country))
        .collect();
    let count =
        |keep: &dyn Fn(&ApprovedRow) -> bool| serving.iter().filter(|r| keep(r)).count() as i64;
    let sealed_ids: Vec<String> = serving
        .iter()
        .filter(|row| row.class == Class::Sensitive && !row.stale)
        .map(|row| row.answer_id.clone())
        .collect();
    let authorization = if app.country.is_empty() {
        AuthorizationState::NoCountry
    } else if usage::authorization_for(db, &app.country).await?.stated {
        AuthorizationState::Stated
    } else {
        AuthorizationState::NotStated
    };
    let sealed = sealed_ids.len() as i64 + i64::from(authorization == AuthorizationState::Stated);
    Ok(Checked {
        check: ItemCheck {
            host: host_of(&app.link),
            answers: count(&|row| row.class == Class::Normal && !row.stale),
            stale_left_out: count(&|row| {
                matches!(row.class, Class::Normal | Class::Sensitive) && row.stale
            }),
            sealed,
            authorization,
            resume,
            letter,
            blocks,
            application_id: app.id,
            company: app.company,
            role: app.role,
            country: app.country,
        },
        resume_path,
        letter_path,
        sealed_ids,
    })
}

fn host_of(link: &str) -> String {
    reqwest::Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Any sign this application was sent before, in any mode: a submission audited, an item a
/// submit was clicked for, or an item recorded `submitted` or `uncertain`. An uncertain one
/// stays until the person records the outcome, which moves the status off `saved`.
pub(crate) async fn submitted_before(db: &Db, application_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM audit_log \
                        WHERE action = 'application_submitted' AND target = ?1) \
             OR EXISTS (SELECT 1 FROM apply_items WHERE application_id = ?1 \
                        AND (submit_clicks > 0 OR state IN ('submitted', 'uncertain')))",
    )
    .bind(application_id)
    .fetch_one(&db.pool)
    .await?)
}

async fn in_a_run(db: &Db, application_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM apply_items i JOIN apply_runs r ON r.id = i.apply_run_id \
         WHERE i.application_id = ? AND r.state IN ('ready', 'running'))",
    )
    .bind(application_id)
    .fetch_one(&db.pool)
    .await?)
}

/// Through `drafts::export`, so every claim guard holds. Each application and kind has its
/// own folder, as two drafts with one title and version share a file name.
async fn export_doc(
    db: &Db,
    actor: &str,
    app: &Application,
    draft_id: Option<&str>,
    kind: &str,
) -> Result<(DocState, Option<PathBuf>)> {
    let Some(draft_id) = draft_id else {
        return Ok((DocState::Missing, None));
    };
    let dir = doc_dir(db, app, kind);
    let exported = drafts::export(
        db,
        draft_id,
        Template::Classic,
        ExportFormat::Pdf,
        &dir,
        actor,
    )
    .await;
    match exported {
        Ok(path) => {
            // Names the draft this file came from, for `material`'s reuse.
            std::fs::write(dir.join(DRAFT_MARKER), draft_id)?;
            Ok((DocState::Ready, Some(path)))
        }
        Err(error) => Ok((blocked(error)?, None)),
    }
}

const DRAFT_MARKER: &str = "draft-id";

fn doc_dir(db: &Db, app: &Application, kind: &str) -> PathBuf {
    db.paths().exports().join("apply").join(&app.id).join(kind)
}

/// A refusal of the claim guard or of a locked key is a blocked document, not an error.
fn blocked(error: Error) -> Result<DocState> {
    match error {
        Error::Refused(reason) if reason == "claims_blocked" => {
            Ok(DocState::Blocked("claims_blocked"))
        }
        error @ (Error::Locked
        | Error::KeyMissing { .. }
        | Error::KeyMismatch { .. }
        | Error::KeyUnavailable(_)
        | Error::NotFound) => Ok(DocState::Blocked(error.code())),
        error => Err(error),
    }
}

async fn run_active(db: &Db) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM apply_runs WHERE state IN ('ready', 'running'))",
    )
    .fetch_one(&db.pool)
    .await?)
}

/// A person's act. With `release_sealed`, each item records the fresh `Sensitive` answers that
/// serve its country, and is released for authorization when its exact country has a stated
/// row. Every row it writes lands in one transaction, so a failed start releases nothing.
pub async fn start(
    db: &Db,
    actor: &str,
    application_ids: &[String],
    release_sealed: bool,
) -> Result<String> {
    start_with(db, actor, application_ids, release_sealed, None).await
}

/// `start` after a person confirmed a release of `confirmed` sealed values in a dialog. The
/// count is taken again here, and a release whose count moved while the dialog was open is
/// refused with `release_changed`: what is released is what the person was told.
pub async fn start_confirmed(
    db: &Db,
    actor: &str,
    application_ids: &[String],
    release_sealed: bool,
    confirmed: i64,
) -> Result<String> {
    start_with(db, actor, application_ids, release_sealed, Some(confirmed)).await
}

async fn start_with(
    db: &Db,
    actor: &str,
    application_ids: &[String],
    release_sealed: bool,
    confirmed: Option<i64>,
) -> Result<String> {
    runner::person(actor)?;
    pause::ensure_off(db).await?;
    if application_ids.is_empty() || application_ids.len() > MAX_RUN_ITEMS {
        return Err(Error::Validation(format!(
            "a run takes 1 to {MAX_RUN_ITEMS} applications"
        )));
    }
    if application_ids.iter().collect::<HashSet<_>>().len() != application_ids.len() {
        return Err(Error::Validation("an application is listed twice".into()));
    }
    // A dead host's run must not block this one.
    session::sweep_orphans(db).await?;
    if run_active(db).await? {
        return Err(Error::Refused("apply_run_active".into()));
    }
    let mut categories = CATEGORIES.to_vec();
    if release_sealed {
        categories.push(DataCategory::Sensitive);
    }
    if !consent_missing(db, &categories).await?.is_empty() {
        return Err(Error::Refused("consent_required".into()));
    }
    let approved = usage::approved_for(db).await?;
    let mut checked = Vec::with_capacity(application_ids.len());
    for id in application_ids {
        let item = check_item(db, actor, id, &approved).await?;
        if let Some(block) = item.check.blocks.first() {
            return Err(Error::Refused((*block).into()));
        }
        checked.push(item);
    }
    let fresh: i64 = checked.iter().map(|item| item.check.sealed).sum();
    if release_sealed && confirmed.is_some_and(|confirmed| confirmed != fresh) {
        return Err(Error::Refused("release_changed".into()));
    }

    // Legal answers are never released: each one stops for a person.
    let released_answers = |item: &Checked| -> Vec<String> {
        match release_sealed {
            true => item.sealed_ids.clone(),
            false => Vec::new(),
        }
    };
    let released =
        |item: &Checked| release_sealed && item.check.authorization == AuthorizationState::Stated;

    let mut tx = db.pool.begin().await?;
    let id: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
        .fetch_one(&mut *tx)
        .await?;
    let inserted = sqlx::query(concat!(
        "INSERT INTO apply_runs (id, mode, actor, state, created_at) \
         SELECT ?, 'claude_code', ?, 'ready', ",
        now!(),
        " WHERE NOT EXISTS (SELECT 1 FROM apply_runs WHERE state IN ('ready', 'running'))"
    ))
    .bind(&id)
    .bind(actor)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(Error::Refused("apply_run_active".into()));
    }
    for (position, item) in checked.iter().enumerate() {
        sqlx::query(
            "INSERT INTO apply_items (id, apply_run_id, application_id, position, host, state, \
             resume_path, letter_path, auth_released, policy_note, released_answers) \
             VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, 'queued', ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&item.check.application_id)
        .bind(position as i64)
        .bind(&item.check.host)
        .bind(path_text(item.resume_path.as_deref())?)
        .bind(path_text(item.letter_path.as_deref())?)
        .bind(released(item))
        .bind(POLICY_NOTE)
        .bind(json!(released_answers(item)).to_string())
        .execute(&mut *tx)
        .await?;
    }
    let hosts: Vec<&str> = checked
        .iter()
        .map(|item| item.check.host.as_str())
        .collect();
    let detail = json!({
        "items": checked.len(),
        "hosts": hosts,
        "release_sealed": release_sealed,
    });
    audit::record_to(&mut *tx, actor, "apply_run_created", Some(&id), detail).await?;
    if release_sealed {
        let applications: Vec<&str> = checked
            .iter()
            .map(|item| item.check.application_id.as_str())
            .collect();
        let detail = json!({
            "applications": applications,
            "answers": checked.iter().map(|item| item.sealed_ids.len()).sum::<usize>(),
            "authorization": checked.iter().filter(|item| released(item)).count(),
        });
        audit::record_to(&mut *tx, actor, "sensitive_released", Some(&id), detail).await?;
    }
    let detail = json!({ "provider": PROVIDER, "categories": categories });
    audit::record_to(&mut *tx, actor, "ai_external_call", Some(&id), detail).await?;
    tx.commit().await?;
    Ok(id)
}

fn path_text(path: Option<&Path>) -> Result<Option<&str>> {
    path.map(|path| {
        path.to_str()
            .ok_or_else(|| Error::Validation("a file path is not valid text".into()))
    })
    .transpose()
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: String,
    apply_run_id: String,
    application_id: Option<String>,
    position: i64,
    host: String,
    state: String,
    submit_clicks: i64,
    auth_released: bool,
    resume_path: Option<String>,
    letter_path: Option<String>,
    policy_note: Option<String>,
    confirmation_text: Option<String>,
    confirmation_url: Option<String>,
    status_written: bool,
    reason: Option<String>,
    released_answers: String,
}

impl TryFrom<ItemRow> for Item {
    type Error = Error;

    fn try_from(row: ItemRow) -> Result<Item> {
        let released_answers = serde_json::from_str(&row.released_answers)
            .map_err(|error| Error::Db(sqlx::Error::Decode(Box::new(error))))?;
        Ok(Item {
            id: row.id,
            run_id: row.apply_run_id,
            application_id: row.application_id,
            position: row.position,
            host: row.host,
            state: row.state,
            submit_clicks: row.submit_clicks,
            auth_released: row.auth_released,
            resume_path: row.resume_path.map(PathBuf::from),
            letter_path: row.letter_path.map(PathBuf::from),
            policy_note: row.policy_note,
            confirmation_text: row.confirmation_text,
            confirmation_url: row.confirmation_url,
            status_written: row.status_written,
            reason: row.reason,
            released_answers,
        })
    }
}

pub async fn view(db: &Db, id: &str) -> Result<RunView> {
    let (mode, state, created_at, ended_at): (String, String, String, Option<String>) =
        sqlx::query_as("SELECT mode, state, created_at, ended_at FROM apply_runs WHERE id = ?")
            .bind(id)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(Error::NotFound)?;
    let items: Vec<ItemRow> = sqlx::query_as(
        "SELECT id, apply_run_id, application_id, position, host, state, submit_clicks, \
         auth_released, resume_path, letter_path, policy_note, confirmation_text, \
         confirmation_url, status_written, reason, released_answers \
         FROM apply_items WHERE apply_run_id = ? ORDER BY position",
    )
    .bind(id)
    .fetch_all(&db.pool)
    .await?;
    let active_pause = sqlx::query_as(
        "SELECT p.id, p.item_id, p.kind, p.origin, p.excerpt, p.created_at \
         FROM apply_pauses p JOIN apply_items i ON i.id = p.item_id \
         WHERE i.apply_run_id = ? AND p.resolved_at IS NULL ORDER BY p.seq DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&db.pool)
    .await?;
    Ok(RunView {
        id: id.to_owned(),
        mode,
        state,
        items: items
            .into_iter()
            .map(Item::try_from)
            .collect::<Result<_>>()?,
        active_pause,
        created_at,
        ended_at,
    })
}

/// The newest run, whatever its state.
pub async fn latest(db: &Db) -> Result<Option<RunView>> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM apply_runs ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&db.pool)
            .await?;
    match id {
        Some(id) => Ok(Some(view(db, &id).await?)),
        None => Ok(None),
    }
}

/// Only a person ends a pause: page text and the session cannot.
pub async fn resolve_pause(
    db: &Db,
    actor: &str,
    pause_id: &str,
    resolution: Resolution,
) -> Result<()> {
    runner::person(actor)?;
    let mut tx = db.pool.begin().await?;
    let done = sqlx::query(concat!(
        "UPDATE apply_pauses SET resolved_at = ",
        now!(),
        ", resolved_by = ?, resolution = ? WHERE id = ? AND resolved_at IS NULL"
    ))
    .bind(actor)
    .bind(resolution.as_str())
    .bind(pause_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if done == 0 {
        let found: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM apply_pauses WHERE id = ?)")
                .bind(pause_id)
                .fetch_one(&mut *tx)
                .await?;
        return Err(if found {
            Error::Validation("this pause is already resolved".into())
        } else {
            Error::NotFound
        });
    }
    let detail = json!({ "resolution": resolution.as_str() });
    audit::record_to(
        &mut *tx,
        actor,
        "apply_pause_resolved",
        Some(pause_id),
        detail,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A person's act. A `ready` run with no live host ends here, through `finish_items`. Any
/// other goes to `agents::cancel_run`, and its host ends it. The sweep then ends it here when
/// its host is gone.
pub async fn cancel(db: &Db, actor: &str, id: &str) -> Result<()> {
    runner::person(actor)?;
    let (state, agent_run): (String, Option<String>) =
        sqlx::query_as("SELECT state, agent_run_id FROM apply_runs WHERE id = ?")
            .bind(id)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(Error::NotFound)?;
    if state == "ended" {
        return Err(ended());
    }
    // A host that died in `prepare` left its claim on a `ready` run: that run ends here too.
    let host_alive = match &agent_run {
        Some(agent_run) => {
            runner::reap_stale(db).await?;
            let alive: Option<bool> = sqlx::query_scalar(
                "SELECT state NOT IN ('completed', 'manual_required', 'refused', 'failed', \
                 'cancelled', 'halted') FROM agent_runs WHERE id = ?",
            )
            .bind(agent_run)
            .fetch_optional(&db.pool)
            .await?;
            alive.unwrap_or(false)
        }
        None => false,
    };
    if let Some(agent_run) = agent_run.filter(|_| state != "ready" || host_alive) {
        let cancelled = agents::cancel_run(db, actor, &agent_run).await;
        // The cancel stands whatever the sweep meets; the next sweep finishes what this one
        // could not.
        let _ = session::sweep_orphans(db).await;
        return cancelled;
    }
    let skipped: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM apply_items \
         WHERE apply_run_id = ? AND state IN ('queued', 'active', 'filled')",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await?;
    if !finish(db, id, None).await? {
        return Err(ended());
    }
    let detail = json!({ "skipped": skipped });
    audit::record(db, actor, "apply_run_cancelled", Some(id), detail).await?;
    Ok(())
}

/// Where a host that claimed a run writes who it is: its pid and its agent run.
const HOST_FILE: &str = "host.json";

/// Written by the host once it holds the claim, so whoever kills it by pid can tell its run
/// from one another host holds. Ids and a pid only.
pub fn record_host(dir: &Path, pid: u32, agent_run: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let record = json!({ "pid": pid, "agent_run": agent_run });
    std::fs::write(dir.join(HOST_FILE), record.to_string())?;
    Ok(())
}

/// The pid and agent run a host recorded in the run folder `dir`, if any.
pub fn recorded_host(dir: &Path) -> Option<(u32, String)> {
    let text = std::fs::read_to_string(dir.join(HOST_FILE)).ok()?;
    let record: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pid = u32::try_from(record["pid"].as_u64()?).ok()?;
    Some((pid, record["agent_run"].as_str()?.to_owned()))
}

/// What `host_gone` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostGone {
    /// The run has ended.
    Ended,
    /// No host ever claimed it: a person's close may cancel it.
    Unclaimed,
    /// Another host holds the claim: the run is left to it.
    Elsewhere,
    /// Halted, and the sweep has not ended it yet: the next sweep will.
    Pending,
}

/// The process hosting `apply_id` is known to be gone: the app closed its terminal, or the
/// emergency pause killed it. Its agent run, if still active, is halted with `reason`, so no
/// one waits for a heartbeat to go stale, and the sweep then winds the run up. With
/// `host_pid`, the halt applies only when the claim belongs to the host that recorded that
/// pid: a run another host won stays its own.
pub async fn host_gone(
    db: &Db,
    apply_id: &str,
    reason: &str,
    host_pid: Option<u32>,
) -> Result<HostGone> {
    let agent_run: Option<String> =
        sqlx::query_scalar("SELECT agent_run_id FROM apply_runs WHERE id = ?")
            .bind(apply_id)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(Error::NotFound)?;
    let claimed = agent_run.is_some();
    if let Some(agent_run) = agent_run {
        let ours = match host_pid {
            None => true,
            Some(pid) => recorded_host(&run_dir(db, apply_id)) == Some((pid, agent_run.clone())),
        };
        if !ours {
            return Ok(HostGone::Elsewhere);
        }
        runner::store::halt(db, &agent_run, reason).await?;
    }
    session::sweep_orphans(db).await?;
    let state: String = sqlx::query_scalar("SELECT state FROM apply_runs WHERE id = ?")
        .bind(apply_id)
        .fetch_one(&db.pool)
        .await?;
    Ok(match (state == "ended", claimed) {
        (true, _) => HostGone::Ended,
        (false, false) => HostGone::Unclaimed,
        (false, true) => HostGone::Pending,
    })
}

/// Every end of a run comes through here, so `apply_run_active` never outlives one. An item
/// left mid-way is `uncertain` if a submit was clicked, else skipped; a queued one is skipped;
/// a pause still open is stopped. Running it again changes nothing.
pub async fn finish_items(db: &Db, apply_id: &str) -> Result<()> {
    finish(db, apply_id, None).await.map(|_| ())
}

/// `finish_items`, with `because` as the reason on every item it ends, in place of
/// `session_ended` and `run_ended`.
pub(crate) async fn finish_items_because(
    db: &Db,
    apply_id: &str,
    because: Option<&str>,
) -> Result<()> {
    finish(db, apply_id, because).await.map(|_| ())
}

/// True when this call moved the run row to `ended`.
async fn finish(db: &Db, apply_id: &str, because: Option<&str>) -> Result<bool> {
    let mut tx = db.pool.begin().await?;
    let found: Option<String> = sqlx::query_scalar("SELECT state FROM apply_runs WHERE id = ?")
        .bind(apply_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(state) = found else {
        return Err(Error::NotFound);
    };
    sqlx::query(
        "UPDATE apply_items SET reason = coalesce(?, 'session_ended'), \
         state = CASE WHEN submit_clicks > 0 THEN 'uncertain' ELSE 'skipped' END \
         WHERE apply_run_id = ? AND state IN ('active', 'filled')",
    )
    .bind(because)
    .bind(apply_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE apply_items SET state = 'skipped', reason = coalesce(?, 'run_ended') \
         WHERE apply_run_id = ? AND state = 'queued'",
    )
    .bind(because)
    .bind(apply_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(concat!(
        "UPDATE apply_pauses SET resolved_by = 'system', resolution = 'stopped', resolved_at = ",
        now!(),
        " WHERE resolved_at IS NULL AND item_id IN \
         (SELECT id FROM apply_items WHERE apply_run_id = ?)"
    ))
    .bind(apply_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(concat!(
        "UPDATE apply_runs SET state = 'ended', ended_at = coalesce(ended_at, ",
        now!(),
        ") WHERE id = ? AND state <> 'ended'"
    ))
    .bind(apply_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(state != "ended")
}

/// An item an earlier session left `active` ends as `finish_items` would end it, and a pause
/// still open on it is stopped.
pub(crate) async fn end_left(db: &Db, item_id: &str) -> Result<()> {
    let mut tx = db.pool.begin().await?;
    sqlx::query(
        "UPDATE apply_items SET reason = 'session_ended', \
         state = CASE WHEN submit_clicks > 0 THEN 'uncertain' ELSE 'skipped' END \
         WHERE id = ? AND state IN ('active', 'filled')",
    )
    .bind(item_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(concat!(
        "UPDATE apply_pauses SET resolved_by = 'system', resolution = 'stopped', resolved_at = ",
        now!(),
        " WHERE resolved_at IS NULL AND item_id = ?"
    ))
    .bind(item_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

const UNCERTAIN_NOTE: &str = "Check the site. JobPilot will not try again.";

/// Writes what the run's results mean for each application, once: a `submitted` item moves
/// the application to `applied` through the tracking agent, and an `uncertain` one gets a
/// note. `status_written` marks either as done. Returns how many it wrote. It stops only on
/// the emergency pause (`Err(Paused)`) or a database error; an item that fails otherwise stays
/// unwritten and the rest go on. A later call finishes what is left.
pub async fn settle(db: &Db, actor: &str, apply_id: &str) -> Result<usize> {
    let run = view(db, apply_id).await?;
    let mut settled = 0;
    for item in run.items {
        let Some(application) = item.application_id.as_deref() else {
            continue;
        };
        if item.status_written || !matches!(item.state.as_str(), "submitted" | "uncertain") {
            continue;
        }
        // Claimed first, so two settles at once write each item once.
        let claimed = sqlx::query(
            "UPDATE apply_items SET status_written = 1 WHERE id = ? AND status_written = 0",
        )
        .bind(&item.id)
        .execute(&db.pool)
        .await?
        .rows_affected();
        if claimed == 0 {
            continue;
        }
        let written = if item.state == "submitted" {
            let said: String = item
                .confirmation_text
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(EXCERPT)
                .collect();
            let by = match run.mode.as_str() {
                "copilot" => "Copilot",
                "autonomous" => "Autonomous",
                _ => "Claude Code",
            };
            let note = format!("Applied by {by} run {apply_id}. The page said: {said}");
            let cancel = CancellationToken::new();
            agents::set_status(db, actor, application, "applied", None, &note, cancel)
                .await
                .map(|_| ())
        } else {
            match pause::ensure_off(db).await {
                Ok(()) => applications::add_note(db, actor, application, UNCERTAIN_NOTE).await,
                Err(error) => Err(error),
            }
        };
        if let Err(error) = written {
            // Not written: the claim goes back, for a later settle.
            sqlx::query("UPDATE apply_items SET status_written = 0 WHERE id = ?")
                .bind(&item.id)
                .execute(&db.pool)
                .await?;
            match error {
                Error::Paused | Error::Db(_) => return Err(error),
                // This one stays unwritten; the rest go on.
                _ => continue,
            }
        }
        settled += 1;
    }
    Ok(settled)
}

const EXCERPT: usize = 300;
const HOLD_POLL: Duration = Duration::from_millis(250);
/// The most of a confirmation page an item keeps: its end, where the result is.
const CONFIRMATION: usize = 2000;

/// The `Page URL` line Playwright puts on a snapshot, when it holds a web address.
pub(crate) fn page_url(snapshot: &str) -> Option<String> {
    snapshot.lines().find_map(|line| {
        let (_, url) = line.split_once("Page URL:")?;
        let url = url.trim();
        let parsed = reqwest::Url::parse(url).ok()?;
        matches!(parsed.scheme(), "http" | "https").then(|| url.to_owned())
    })
}

/// Who reported a submission, for its audit row.
pub(crate) struct Submitter<'a> {
    pub agent: &'a str,
    pub mode: &'a str,
}

/// An item a submit was made for: the page is kept as the page's claim (M-39), and the
/// submission is audited. `capture` is the snapshot taken after it, if one could be. The
/// status is written later, by `settle`.
pub(crate) async fn record_submitted(
    db: &Db,
    actor: &str,
    run_id: &str,
    item: &Item,
    capture: Option<&str>,
    by: Submitter<'_>,
) -> Result<()> {
    let page = capture.unwrap_or("");
    let (text, _) = crate::knowledge::normalize(page);
    let count = text.chars().count();
    let tail: String = text
        .chars()
        .skip(count.saturating_sub(CONFIRMATION))
        .collect();
    sqlx::query(concat!(
        "UPDATE apply_items SET state = 'submitted', reason = NULL, confirmation_text = ?, \
         confirmation_url = ?, captured_at = CASE WHEN ? THEN ",
        now!(),
        " END WHERE id = ? AND state IN ('active', 'filled')"
    ))
    .bind(capture.is_some().then_some(&tail))
    .bind(page_url(page))
    .bind(capture.is_some())
    .bind(&item.id)
    .execute(&db.pool)
    .await?;
    let mut detail = json!({
        "agent": by.agent,
        "mode": by.mode,
        "run": run_id,
        "application": item.application_id,
        "host": item.host,
    });
    if by.mode == "copilot" {
        detail["policy_note"] = json!(item.policy_note);
    }
    audit::record(
        db,
        actor,
        "application_submitted",
        item.application_id.as_deref(),
        detail,
    )
    .await
}

/// A Copilot or Autonomous run of one application, held by `agent_run` from the start, with
/// the files the preflight renders. Refused with the first block the preflight finds, or when
/// another run is active. The audit row carries `policy_note`: what the register said about this mode.
pub(crate) async fn open_assisted(
    db: &Db,
    actor: &str,
    apply_id: &str,
    application_id: &str,
    agent_run: &str,
    mode: Mode,
    policy_note: Option<&str>,
) -> Result<Item> {
    runner::person(actor)?;
    let mode = match mode {
        Mode::Copilot => "copilot",
        Mode::Autonomous => "autonomous",
        _ => return Err(Error::Refused("mode_does_not_allow".into())),
    };
    let approved = usage::approved_for(db).await?;
    let item = check_item(db, actor, application_id, &approved).await?;
    if let Some(block) = item.check.blocks.first() {
        return Err(Error::Refused((*block).into()));
    }
    let mut tx = db.pool.begin().await?;
    let inserted = sqlx::query(concat!(
        "INSERT INTO apply_runs (id, agent_run_id, mode, actor, state, created_at) \
         SELECT ?, ?, ?, ?, 'running', ",
        now!(),
        " WHERE NOT EXISTS (SELECT 1 FROM apply_runs WHERE state IN ('ready', 'running'))"
    ))
    .bind(apply_id)
    .bind(agent_run)
    .bind(mode)
    .bind(actor)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(Error::Refused("apply_run_active".into()));
    }
    sqlx::query(
        "INSERT INTO apply_items (id, apply_run_id, application_id, position, host, state, \
         resume_path, letter_path, policy_note) \
         VALUES (lower(hex(randomblob(16))), ?, ?, 0, ?, 'active', ?, ?, ?)",
    )
    .bind(apply_id)
    .bind(&item.check.application_id)
    .bind(&item.check.host)
    .bind(path_text(item.resume_path.as_deref())?)
    .bind(path_text(item.letter_path.as_deref())?)
    .bind(policy_note)
    .execute(&mut *tx)
    .await?;
    let detail = json!({
        "mode": mode,
        "items": 1,
        "hosts": [item.check.host],
        "policy_note": policy_note,
    });
    audit::record_to(&mut *tx, actor, "apply_run_created", Some(apply_id), detail).await?;
    tx.commit().await?;
    view(db, apply_id)
        .await?
        .items
        .into_iter()
        .next()
        .ok_or(Error::NotFound)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    Continue,
    TakenOver,
    TimedOut,
}

/// Opens a pause on the item and waits for a person, up to `limit` (`PAUSE_HOLD` outside
/// tests). `Err(Paused)` or `Err(Cancelled)` when the flag or a cancel ends the hold; the pause
/// is then `stopped`. The host's agent run reads `waiting_for_human` meanwhile.
pub(crate) async fn hold(
    db: &Db,
    item: &Item,
    kind: PauseKind,
    origin: &str,
    excerpt: &str,
    cancel: &CancellationToken,
    limit: Duration,
) -> Result<Held> {
    let excerpt: String = excerpt.chars().take(EXCERPT).collect();
    let kind = json!(kind);
    let id: String = sqlx::query_scalar(concat!(
        "INSERT INTO apply_pauses (id, item_id, kind, origin, excerpt, created_at) \
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, ",
        now!(),
        ") RETURNING id"
    ))
    .bind(&item.id)
    .bind(kind.as_str().unwrap_or("other"))
    .bind(origin)
    .bind(&excerpt)
    .fetch_one(&db.pool)
    .await?;
    let agent_run: Option<String> =
        sqlx::query_scalar("SELECT agent_run_id FROM apply_runs WHERE id = ?")
            .bind(&item.run_id)
            .fetch_optional(&db.pool)
            .await?
            .flatten();
    if let Some(run) = &agent_run {
        runner::store::set_waiting(db, run, true).await?;
    }
    let held = wait(db, &id, &item.run_id, cancel, limit).await;
    let back = match &agent_run {
        Some(run) => runner::store::set_waiting(db, run, false).await,
        None => Ok(()),
    };
    let held = held?;
    back?;
    Ok(held)
}

async fn wait(
    db: &Db,
    pause_id: &str,
    run_id: &str,
    cancel: &CancellationToken,
    limit: Duration,
) -> Result<Held> {
    let begun = tokio::time::Instant::now();
    loop {
        let resolution: Option<Option<String>> = sqlx::query_scalar(
            "SELECT resolution FROM apply_pauses WHERE id = ? AND resolved_at IS NOT NULL",
        )
        .bind(pause_id)
        .fetch_optional(&db.pool)
        .await?;
        match resolution.flatten().as_deref() {
            Some("continue") => return Ok(Held::Continue),
            Some("take_over") => return Ok(Held::TakenOver),
            Some("timeout") => return Ok(Held::TimedOut),
            // Stopped by `finish_items` from another process.
            Some(_) => return Err(Error::Cancelled),
            None => {}
        }
        if pause::is_set(db).await? {
            stop(db, pause_id).await?;
            return Err(Error::Paused);
        }
        if cancel.is_cancelled() || run_cancelled(db, run_id).await? {
            stop(db, pause_id).await?;
            return Err(Error::Cancelled);
        }
        if begun.elapsed() >= limit {
            let timed_out = sqlx::query(concat!(
                "UPDATE apply_pauses SET resolved_by = 'system', resolution = 'timeout', \
                 resolved_at = ",
                now!(),
                " WHERE id = ? AND resolved_at IS NULL"
            ))
            .bind(pause_id)
            .execute(&db.pool)
            .await?
            .rows_affected();
            if timed_out == 1 {
                return Ok(Held::TimedOut);
            }
            // A person got there first: read what they chose.
            continue;
        }
        tokio::time::sleep(HOLD_POLL).await;
    }
}

async fn stop(db: &Db, pause_id: &str) -> Result<()> {
    sqlx::query(concat!(
        "UPDATE apply_pauses SET resolved_by = 'system', resolution = 'stopped', resolved_at = ",
        now!(),
        " WHERE id = ? AND resolved_at IS NULL"
    ))
    .bind(pause_id)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// The run has ended, a person asked its agent run to stop, or its host is gone: the agent
/// run is no longer active (halted or ended), or its heartbeat is older than the runner's
/// `STALE`. A session that outlives its host so refuses its next call and ends.
pub(crate) async fn run_cancelled(db: &Db, run_id: &str) -> Result<bool> {
    let stale = format!("-{:.3} seconds", runner::STALE.as_secs_f64());
    let stopped: Option<bool> = sqlx::query_scalar(
        "SELECT r.state = 'ended' OR coalesce(a.cancel_requested, 0) = 1 \
           OR (a.id IS NOT NULL AND (a.state NOT IN \
             ('queued','checking','running','checkpointed','retrying','waiting_for_human') \
             OR a.heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))) \
         FROM apply_runs r LEFT JOIN agent_runs a ON a.id = r.agent_run_id WHERE r.id = ?",
    )
    .bind(&stale)
    .bind(run_id)
    .fetch_optional(&db.pool)
    .await?;
    Ok(stopped.unwrap_or(true))
}

fn ended() -> Error {
    Error::Validation("this run has ended".into())
}

/// `<root>/apply/<id>`: the run's browser profile, staged files, and config.
pub fn run_dir(db: &Db, id: &str) -> PathBuf {
    db.paths().apply().join(id)
}

// ---- Manual mode: the review screen's material and the outcome a person records ----

/// Wireframe 11's fixed text, by decision.
const MANUAL_NOTE: &str = "You fill and submit in your own browser. JobPilot opens the link, \
                           gives you files and answers, and records what happened.";
const BLOCKED_NOTE: &str =
    "The site's terms forbid this. JobPilot will not do it, even if you ask.";
const UNKNOWN_NOTE: &str =
    "The site has not said whether tools may do this. Treated as blocked until it answers.";
const NO_CLAUSE_NOTE: &str = "No clause found for this site. You review and submit.";
const ALLOWED_NOTE: &str = "The site's own rule allows this. You review and submit.";
const NO_AUTONOMOUS_NOTE: &str = "No site or connector qualifies today. Needs a site rule of \
                                  allowed, verified permission, and your explicit authorization.";
const AUTONOMOUS_NOTE: &str = "Fills and submits on this site, and stops at every mandatory pause.";
pub const CLAUDE_CODE_NOTE: &str = "Fills and submits with Claude Code in a visible browser. Add \
                                    it to a run from Applications.";
pub const CLAUDE_CODE_SETUP_NOTE: &str = "Needs the browser runtime and Claude Code set up first. \
                                          Add it to a run from Applications to see what is missing.";

#[derive(Debug, Clone, Serialize)]
pub struct ModeChoice {
    pub mode: &'static str,
    pub available: bool,
    pub note: String,
}

/// An approved answer this application can use, with no text.
#[derive(Debug, Clone, Serialize)]
pub struct MaterialRow {
    pub answer_id: String,
    pub question: String,
    pub class: String,
    pub stale: bool,
    /// A sealed class with no current confirmation for this application.
    pub needs_confirmation: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Material {
    pub modes: Vec<ModeChoice>,
    pub resume: Option<PathBuf>,
    pub letter: Option<PathBuf>,
    pub answers: Vec<MaterialRow>,
    pub authorization: usage::AuthorizationAnswer,
}

/// What Manual mode hands a person for one application, and how each mode stands on its
/// site. Reads and exports only: no answer text, no request to the site, no run.
pub async fn material(db: &Db, application_id: &str) -> Result<Material> {
    let app = applications::get(db, application_id).await?;
    // A link that is not https, or names no site, reads as a page with no register row.
    let site = match permissions::link_allowed(&app.link) {
        true => permissions::site_of(&app.link).unwrap_or(Site::CompanyPage),
        false => Site::CompanyPage,
    };
    let grant = Grant {
        owner_started: true,
        authorized: permissions::granted(db, site).await?,
    };
    let decide = |mode, action| permissions::decide(mode, site, action, &grant);
    let manual = matches!(
        decide(Mode::Manual, Action::OpenLink),
        Decision::Allow { .. }
    );
    let copilot = match decide(Mode::Copilot, Action::Fill) {
        Decision::Allow { note: Some(_) } => (true, NO_CLAUSE_NOTE),
        Decision::Allow { note: None } => (true, ALLOWED_NOTE),
        Decision::Refuse("policy_blocked") => (false, BLOCKED_NOTE),
        Decision::Refuse(_) => (false, UNKNOWN_NOTE),
    };
    let autonomous = match decide(Mode::Autonomous, Action::Submit) {
        Decision::Allow { .. } => (true, AUTONOMOUS_NOTE),
        Decision::Refuse("policy_blocked") => (false, BLOCKED_NOTE),
        Decision::Refuse(_) => (false, NO_AUTONOMOUS_NOTE),
    };
    // Files and settings only: this screen starts no process. The preflight checks Node.
    let set_up = runtime::installed(db)
        && matches!(
            claude_code::find_binary_quietly(Some(db)).await,
            Ok(Some(_))
        );
    let claude = match set_up {
        true => (true, CLAUDE_CODE_NOTE),
        false => (false, CLAUDE_CODE_SETUP_NOTE),
    };
    let choice = |mode, (available, note): (bool, &str)| ModeChoice {
        mode,
        available,
        note: note.to_owned(),
    };
    let modes = vec![
        choice("manual", (manual, MANUAL_NOTE)),
        choice("copilot", copilot),
        choice("autonomous", autonomous),
        choice("claude_code", claude),
    ];

    let resume = rendered(db, &app, app.resume_draft_id.as_deref(), "resume").await?;
    let letter = rendered(db, &app, app.letter_draft_id.as_deref(), "cover_letter").await?;

    let confirmed: HashSet<String> = usage::application_answers(db, &app.id)
        .await?
        .into_iter()
        .filter(|used| used.current)
        .map(|used| used.answer_id)
        .collect();
    let answers = usage::approved_for(db)
        .await?
        .into_iter()
        .filter(|row| row.class != Class::WorkAuthorization && row.serves(&app.country))
        .map(|row| MaterialRow {
            needs_confirmation: row.class.sealed() && !confirmed.contains(&row.answer_id),
            class: row.class.as_str().to_owned(),
            answer_id: row.answer_id,
            question: row.question,
            stale: row.stale,
        })
        .collect();

    // Only the exact country's row. A blank country reads "not stated", as no row does.
    let authorization = match app.country.trim().is_empty() {
        true => usage::AuthorizationAnswer {
            country: String::new(),
            authorized: crate::profile::Stated::NotStated,
            needs_sponsorship: crate::profile::Stated::NotStated,
            note: String::new(),
            stated: false,
        },
        false => usage::authorization_for(db, &app.country).await?,
    };
    Ok(Material {
        modes,
        resume,
        letter,
        answers,
        authorization,
    })
}

/// A person records what happened after applying by hand. The status goes through the
/// tracking agent, and `applied` also audits the submission as Manual mode's.
pub async fn record_outcome(
    db: &Db,
    actor: &str,
    application_id: &str,
    outcome: &str,
    on: Option<&str>,
    note: &str,
    cancel: CancellationToken,
) -> Result<Application> {
    runner::person(actor)?;
    let app = applications::get(db, application_id).await?;
    refuse_in_a_run(db, &app.id).await?;
    agents::set_status_with(db, actor, &app.id, outcome, on, note, true, cancel).await
}

/// A run's own proxy writes this application's status: a second writer would race it. A run
/// whose host died is swept first, as `start` does, so it does not block a record for ever.
pub(crate) async fn refuse_in_a_run(db: &Db, application_id: &str) -> Result<()> {
    session::sweep_orphans(db).await?;
    match in_a_run(db, application_id).await? {
        true => Err(Error::Refused("in_active_run".into())),
        false => Ok(()),
    }
}

/// The `application_submitted` detail of a Manual record: ids and the host, no text.
pub fn manual_submission(app: &Application) -> serde_json::Value {
    json!({
        "agent": "manual",
        "mode": "manual",
        "application": app.id,
        "host": host_of(&app.link),
    })
}

/// The file Rust rendered for this application's draft, rendered again only when it is gone
/// or the application now names another draft. A draft never changes once stored, and the
/// marker beside the file names the draft it came from.
async fn rendered(
    db: &Db,
    app: &Application,
    draft_id: Option<&str>,
    kind: &str,
) -> Result<Option<PathBuf>> {
    let Some(draft_id) = draft_id else {
        return Ok(None);
    };
    let draft = match drafts::get(db, draft_id).await {
        Ok(draft) => draft,
        Err(Error::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    let dir = doc_dir(db, app, kind);
    let path = dir.join(drafts::export_file_name(&draft, ExportFormat::Pdf));
    let marker = std::fs::read_to_string(dir.join(DRAFT_MARKER)).ok();
    if path.is_file() && marker.as_deref() == Some(&draft.id) {
        // The file stands, but the claim guard is asked again: its answer is live.
        return match drafts::export_check(db, draft_id).await {
            Ok(()) => Ok(Some(path)),
            Err(error) => blocked(error).map(|_| None),
        };
    }
    // As the preflight renders them, so every claim guard holds. A blocked document is none.
    let (_, path) = export_doc(db, "system", app, Some(draft_id), kind).await?;
    Ok(path)
}
