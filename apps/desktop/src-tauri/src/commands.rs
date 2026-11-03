//! The IPC surface: a thin adapter over `jobpilot-core`. A quick command takes the `db`
//! lock, calls core, and returns. Anything that can wait on a model, a keychain prompt, or
//! a dialog does not hold that lock: see [`AppState::work`], and no lock at all goes across
//! a dialog. Only `backup_restore`, `data_delete`, and `retry_open` change the slot, under
//! the write lock, so no command sees a half-swapped handle.
//!
//! There is deliberately no `secret_get`: secret values arrive here and never leave. What
//! decides what runs is set by a dialog Rust opens, so script in the webview cannot set it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jobpilot_core::agents::{
    AiPlan, CompanyResearch, DiscoveryReport, MatchReport, MatchRequest, MemoryRefreshed,
};
use jobpilot_core::ai::ollama::Ollama;
use jobpilot_core::ai::register::{self, ProviderInfo};
use jobpilot_core::ai::router::{Router, ollama_from_settings};
use jobpilot_core::ai::{
    self, AiStatus, CancellationToken, DataCategory, Sharing, TaskType, consent,
};
use jobpilot_core::ai::{api, child, claude_code};
use jobpilot_core::analytics::{self, Report};
use jobpilot_core::applications::{self, Application, ApplicationEvent, ApplicationInput};
use jobpilot_core::apply::{self, Preflight, Resolution, RunView};
use jobpilot_core::audit::{self, AuditEntry};
use jobpilot_core::backup::{self, BackupInfo};
use jobpilot_core::browser::runtime::{self as browser_runtime, PLAYWRIGHT_MCP, RuntimeState};
use jobpilot_core::connectors::{self, SourceStatus};
use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::documents::{self, Document, SearchHit};
use jobpilot_core::drafts::{self, ClaimInput, Compare, Draft, ExportFormat};
use jobpilot_core::embed::{self, Hit};
use jobpilot_core::generate::{self, Generated, PromptPreview};
use jobpilot_core::jobs::company::{self, Company, CompanyNote};
use jobpilot_core::jobs::prefs::{self as match_prefs, MatchPrefs};
use jobpilot_core::jobs::{self as jobs_core, Job, JobDetail, JobFilter, PastedJob, SourceRow};
use jobpilot_core::knowledge::{self, JobText, KnowledgeItem};
use jobpilot_core::memory::{
    self, Answer, AppliedAnswer, AuthorizationAnswer, Class, MemoryHit, Question, QuestionRow,
    RefreshPlan, UsedAnswer,
};
use jobpilot_core::parse::{self, Imported};
use jobpilot_core::pause::{self, PauseState};
use jobpilot_core::profile::{self, Profile, ProfileBasic, WorkAuthorization};
use jobpilot_core::recovery::{self, RecoveryReceipt};
use jobpilot_core::render::{ResumeDoc, Template};
use jobpilot_core::runner::{self, ExportChoice, RunDetail, RunSummary};
use jobpilot_core::secrets::Zeroizing;
use jobpilot_core::{Db, Error, Result, agents, secrets, settings, transfer};
use serde_json::{Value, json};
use tauri::ipc::Invoke;
use tauri::{AppHandle, Runtime, State};

use crate::native::{self, FileDialog};
use crate::pty::PtyState;
use crate::state::{AppState, StartupError, Status};

const ACTOR: &str = "gui";

/// `withheld` counts sealed answers left out, because no key could open them.
#[derive(serde::Serialize)]
pub struct Exported {
    path: String,
    withheld: usize,
}

/// Settings core keeps for itself: what the router runs, and the recovery reminder's mark.
/// The first has a command for each key, and the second is only ever core's to write.
const OWNED_SETTINGS: &[&str] = &["ai.", "jobs.", "recovery_key_exported_"];

pub fn handler<R: Runtime>() -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        status,
        retry_open,
        documents_list,
        documents_create,
        documents_update,
        documents_delete,
        documents_search,
        settings_get,
        settings_set,
        backup_create,
        backup_list,
        backup_restore,
        data_export,
        data_delete,
        swap_warning_clear,
        secret_set,
        secret_delete,
        secret_names,
        audit_recent,
        profile_get,
        profile_set_basic,
        profile_set_authorization,
        profile_remove_authorization,
        kb_list,
        kb_add,
        kb_approve,
        kb_mark_sensitive,
        kb_remove,
        kb_reveal,
        kb_search,
        job_paste,
        job_list,
        job_get,
        jobs_list,
        job_detail,
        job_import,
        job_remove,
        job_split,
        job_open,
        job_to_app,
        job_sources_status,
        job_sources_list,
        job_src_add,
        job_src_remove,
        jobs_fetch,
        jobs_analyze,
        jobs_analyze_plan,
        job_research,
        job_research_plan,
        companies_list,
        company_note_add,
        match_prefs_get,
        match_prefs_set,
        app_list,
        app_get,
        app_add,
        app_update,
        app_set_status,
        app_note,
        app_history,
        agents_list,
        agent_get,
        agent_cancel,
        agent_resume,
        pause_status,
        pause_set,
        pause_clear,
        analytics_run,
        analytics_latest,
        app_remove,
        memory_list,
        memory_get,
        memory_add_question,
        memory_add_answer,
        memory_edit_answer,
        memory_approve,
        memory_mark_reviewed,
        memory_set_class,
        memory_reveal,
        memory_confirm_use,
        memory_use,
        memory_application_answers,
        memory_authorization,
        memory_remove_question,
        memory_remove_answer,
        memory_search,
        memory_refresh,
        memory_refresh_plan,
        resume_import,
        resume_list,
        resume_versions,
        resume_generate,
        letter_generate,
        resume_compare,
        claim_edit,
        claim_remove,
        draft_save,
        resume_export,
        ai_status,
        ai_pull,
        ai_cancel_pull,
        ai_set_models,
        ai_set_order,
        ai_enable,
        ai_set_claude_path,
        ai_clear_claude_path,
        ai_set_cli_path,
        ai_clear_cli_path,
        ai_set_ollama_url,
        ai_set_api_model,
        ai_set_endpoint,
        ai_consent_give,
        ai_consent_revoke,
        ai_sharing,
        ai_preview,
        ai_key_from_clipboard,
        ai_key_remove,
        ai_index,
        ai_cancel,
        key_state,
        key_retry,
        recovery_key_export,
        recovery_key_import,
        discard_locked_fields,
        browser_status,
        browser_setup,
        apply_preflight,
        apply_start,
        apply_view,
        apply_latest,
        apply_resolve_pause,
        apply_settle,
        apply_cancel,
        crate::pty::pty_open,
        crate::pty::pty_open_apply,
        crate::pty::pty_write,
        crate::pty::pty_resize,
        crate::pty::pty_close,
    ]
}

fn ready(slot: &Option<Arc<Db>>) -> Result<&Db> {
    slot.as_deref().ok_or(Error::Unavailable)
}

/// The handle, to give to core. Call with the gate held: no `Work` can share it then, and
/// should one ever, the slot keeps it and the caller is refused with nothing changed.
fn take_db(slot: &mut Option<Arc<Db>>) -> Result<Option<Db>> {
    match slot.take().map(Arc::try_unwrap).transpose() {
        Ok(db) => Ok(db),
        Err(shared) => {
            *slot = Some(shared);
            Err(Error::InUse)
        }
    }
}

/// For an operation that consumed a live handle. Core closed it on every path, so a
/// new one goes in the slot whatever happened; `failure` is what the operation returned.
/// If the app cannot reopen it is in recovery, and that is the answer the UI must act
/// on, so it comes back as a `Status` carrying `failure` in its message.
async fn reopen(
    state: &AppState,
    slot: &mut Option<Arc<Db>>,
    failure: Option<Error>,
) -> Result<Status> {
    match Db::open(&state.paths).await {
        Ok(db) => {
            *slot = Some(Arc::new(db));
            state.set_failure(None);
            match failure {
                Some(error) => Err(error),
                None => Ok(state.status(slot.as_deref()).await),
            }
        }
        Err(error) => {
            let mut stored = StartupError::from(&error);
            if let Some(failure) = failure {
                stored.message = format!("{} (after: {failure})", stored.message);
            }
            state.set_failure(Some(stored));
            Ok(state.status(None).await)
        }
    }
}

#[tauri::command]
pub async fn status(state: State<'_, AppState>) -> Result<Status> {
    let slot = state.db.read().await;
    Ok(state.status(slot.as_deref()).await)
}

/// User-initiated only: a failed migration adds a snapshot per open, so nothing else
/// may call `Db::open` while the app is in recovery.
#[tauri::command]
pub async fn retry_open(state: State<'_, AppState>) -> Result<Status> {
    let mut slot = state.db.write().await;
    if slot.is_none()
        && let Ok(paths) = state.known_root()
    {
        match Db::open(paths).await {
            Ok(db) => {
                *slot = Some(Arc::new(db));
                state.set_failure(None);
            }
            Err(error) => state.set_failure(Some((&error).into())),
        }
    }
    Ok(state.status(slot.as_deref()).await)
}

#[tauri::command]
pub async fn documents_list(state: State<'_, AppState>) -> Result<Vec<Document>> {
    let slot = state.db.read().await;
    documents::list(ready(&slot)?).await
}

#[tauri::command]
pub async fn documents_create(
    state: State<'_, AppState>,
    title: String,
    body: String,
) -> Result<Document> {
    let slot = state.db.read().await;
    documents::create(ready(&slot)?, &title, &body).await
}

#[tauri::command]
pub async fn documents_update(
    state: State<'_, AppState>,
    id: String,
    title: String,
    body: String,
) -> Result<Document> {
    let slot = state.db.read().await;
    documents::update(ready(&slot)?, &id, &title, &body).await
}

#[tauri::command]
pub async fn documents_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    let slot = state.db.read().await;
    documents::delete(ready(&slot)?, &id).await
}

#[tauri::command]
pub async fn documents_search(state: State<'_, AppState>, query: String) -> Result<Vec<SearchHit>> {
    let slot = state.db.read().await;
    documents::search(ready(&slot)?, &query).await
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>, key: String) -> Result<Option<Value>> {
    let slot = state.db.read().await;
    settings::get(ready(&slot)?, &key).await
}

#[tauri::command]
pub async fn settings_set(state: State<'_, AppState>, key: String, value: Value) -> Result<()> {
    if OWNED_SETTINGS.iter().any(|owned| key.starts_with(owned)) {
        return Err(Error::Refused("this setting has its own command".into()));
    }
    let slot = state.db.read().await;
    settings::set(ready(&slot)?, &key, &value).await
}

#[tauri::command]
pub async fn backup_create(state: State<'_, AppState>) -> Result<BackupInfo> {
    let slot = state.db.read().await;
    backup::create(ready(&slot)?, ACTOR).await
}

/// Needs no handle: the recovery screen's Restore lists snapshots from here. The read
/// lock keeps it clear of a restore or delete changing `backups/`.
#[tauri::command]
pub async fn backup_list(state: State<'_, AppState>) -> Result<Vec<BackupInfo>> {
    let _slot = state.db.read().await;
    backup::list(state.known_root()?).await
}

#[tauri::command]
pub async fn backup_restore<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    source: String,
) -> Result<Status> {
    state.known_root()?;
    if state.db.read().await.is_none() {
        state.check_offline_restore()?;
    }
    let message = format!(
        "Replace all data in JobPilot with this backup?\n\n{source}\n\n\
         Running AI work stops. A copy of the current data is kept in the backups folder."
    );
    native::confirmed(&app, "Restore backup", message, "Restore").await?;

    let (_gate, mut slot) = state.exclusive().await;
    let source = PathBuf::from(source);
    let restored = match take_db(&mut slot)? {
        Some(db) => match backup::restore(db, &source, ACTOR).await {
            Err(error) if !matches!(error, Error::RestoredButNotOpened(_)) => {
                return reopen(&state, &mut slot, Some(error)).await;
            }
            restored => restored,
        },
        None => {
            state.check_offline_restore()?;
            match backup::restore_offline(&state.paths, &source, ACTOR).await {
                // Live data is unchanged, so the stored failure still stands. No
                // reopen here: `retry_open` is the user's call.
                Err(error) if !matches!(error, Error::RestoredButNotOpened(_)) => {
                    return Err(error);
                }
                restored => restored,
            }
        }
    };
    match restored {
        Ok(report) => {
            *slot = Some(Arc::new(report.db));
            state.set_restored_key_state(report.backup_key_state);
            state.set_failure(None);
        }
        // Live data is now the backup. This must not read as "nothing changed".
        Err(error) => {
            state.set_restored_key_state(None);
            state.set_failure(Some((&error).into()));
        }
    }
    Ok(state.status(slot.as_deref()).await)
}

#[tauri::command]
pub async fn data_export(state: State<'_, AppState>) -> Result<Exported> {
    let report = transfer::export_report(&*state.work().await?, ACTOR).await?;
    Ok(Exported {
        path: report.path.to_string_lossy().into_owned(),
        withheld: report.withheld,
    })
}

#[tauri::command]
pub async fn data_delete<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    confirm: String,
    include_backups: bool,
) -> Result<Status> {
    if state.db.read().await.is_none() {
        return Err(Error::Unavailable);
    }
    let root = state.paths.root().display();
    let backups = match include_backups {
        true => "and all backups",
        false => "and keep the backups",
    };
    let message = format!(
        "Delete all JobPilot data {backups}?\n\n{root}\n\n\
         Running AI work stops. This cannot be undone."
    );
    native::confirmed(&app, "Delete all data", message, "Delete").await?;

    let (_gate, mut slot) = state.exclusive().await;
    let db = take_db(&mut slot)?.ok_or(Error::Unavailable)?;
    let failure = transfer::delete_all(db, &confirm, include_backups)
        .await
        .err();
    if failure.is_none() {
        state.set_restored_key_state(None);
    }
    reopen(&state, &mut slot, failure).await
}

/// The user has seen the warning.
#[tauri::command]
pub async fn swap_warning_clear(state: State<'_, AppState>) -> Result<()> {
    state.work().await?.clear_swap_warning().await
}

#[tauri::command]
pub async fn secret_set(state: State<'_, AppState>, name: String, value: String) -> Result<()> {
    secrets::set(&*state.work().await?, &name, &value, ACTOR).await
}

#[tauri::command]
pub async fn secret_delete(state: State<'_, AppState>, name: String) -> Result<()> {
    secrets::delete(&*state.work().await?, &name, ACTOR).await
}

#[tauri::command]
pub async fn secret_names(state: State<'_, AppState>) -> Result<Vec<String>> {
    let slot = state.db.read().await;
    secrets::names(ready(&slot)?).await
}

#[tauri::command]
pub async fn audit_recent(state: State<'_, AppState>, limit: i64) -> Result<Vec<AuditEntry>> {
    let slot = state.db.read().await;
    audit::recent(ready(&slot)?, limit).await
}

/// `show`, not `get`: looking at the profile must not make a key.
#[tauri::command]
pub async fn profile_get(state: State<'_, AppState>) -> Result<Profile> {
    profile::show(&*state.work().await?).await
}

#[tauri::command]
pub async fn profile_set_basic(
    state: State<'_, AppState>,
    basic: ProfileBasic,
) -> Result<ProfileBasic> {
    profile::set_basic(&*state.work().await?, basic).await
}

#[tauri::command]
pub async fn profile_set_authorization(
    state: State<'_, AppState>,
    row: WorkAuthorization,
) -> Result<()> {
    profile::set_authorization(&*state.work().await?, row).await
}

#[tauri::command]
pub async fn profile_remove_authorization(
    state: State<'_, AppState>,
    country: String,
) -> Result<()> {
    profile::remove_authorization(&*state.work().await?, &country).await
}

#[tauri::command]
pub async fn kb_list(state: State<'_, AppState>) -> Result<Vec<KnowledgeItem>> {
    knowledge::list(&*state.work().await?).await
}

#[tauri::command]
pub async fn kb_add(
    state: State<'_, AppState>,
    kind: String,
    body: String,
) -> Result<KnowledgeItem> {
    knowledge::add(&*state.work().await?, &kind, &body, "user", None).await
}

#[tauri::command]
pub async fn kb_approve(state: State<'_, AppState>, id: String, approved: bool) -> Result<()> {
    knowledge::approve(&*state.work().await?, &id, approved).await
}

#[tauri::command]
pub async fn kb_mark_sensitive(state: State<'_, AppState>, id: String, marked: bool) -> Result<()> {
    knowledge::mark_sensitive(&*state.work().await?, &id, marked).await
}

#[tauri::command]
pub async fn kb_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    knowledge::remove(&*state.work().await?, &id).await
}

/// The text of one item. A sealed body is shown only after a native confirmation, every
/// time, so script in the webview cannot read marked text on its own: the dialog is Rust's
/// and does not name the text. An unknown id answers `not_found` and an unsealed item
/// answers at once, with no dialog, because neither holds anything to protect.
#[tauri::command]
pub async fn kb_reveal<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: String,
) -> Result<String> {
    let db = state.work().await?;
    let items = knowledge::list(&db).await?;
    let item = items
        .iter()
        .find(|item| item.id == id)
        .ok_or(Error::NotFound)?;
    if item.sealed {
        let message = format!(
            "Show the sealed {} item {}?\n\nIts text will appear on screen until you hide it \
             or leave this screen.",
            item.kind, item.id
        );
        native::confirmed(&app, "Show sealed text", message, "Show").await?;
    }
    knowledge::reveal(&db, &id).await
}

/// Embeddings when a local model is set up, else a plain text match.
#[tauri::command]
pub async fn kb_search(
    state: State<'_, AppState>,
    query: String,
    limit: usize,
) -> Result<Vec<Hit>> {
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    match embed::search(&db, &router, &query, limit).await {
        Err(Error::ManualRequired(_)) => knowledge::search_text(&db, &query, limit).await,
        found => found,
    }
}

#[tauri::command]
pub async fn job_paste(state: State<'_, AppState>, title: String, raw: String) -> Result<JobText> {
    let db = state.work().await?;
    let input = PastedJob {
        title,
        company: String::new(),
        url: None,
        text: raw,
    };
    let job = jobs_core::import_pasted(&db, input).await?;
    knowledge::get_job_text(&db, &job.job_text_id).await
}

#[tauri::command]
pub async fn jobs_list(
    state: State<'_, AppState>,
    filter: JobFilter,
    limit: i64,
) -> Result<Vec<Job>> {
    jobs_core::list(&*state.work().await?, &filter, limit).await
}

#[tauri::command]
pub async fn job_detail(state: State<'_, AppState>, id: String) -> Result<JobDetail> {
    jobs_core::detail(&*state.work().await?, &id).await
}

/// Stores the text and keeps the link. Nothing here requests it.
#[tauri::command]
pub async fn job_import(state: State<'_, AppState>, input: PastedJob) -> Result<Job> {
    jobs_core::import_pasted(&*state.work().await?, input).await
}

#[tauri::command]
pub async fn job_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    jobs_core::remove(&*state.work().await?, ACTOR, &id).await
}

#[tauri::command]
pub async fn job_split(state: State<'_, AppState>, id: String) -> Result<()> {
    jobs_core::split(&*state.work().await?, ACTOR, &id).await
}

/// The webview names a job, never a link: Rust reads the stored one and opens it only when
/// it is `https`.
#[tauri::command]
pub async fn job_open<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: String,
) -> Result<()> {
    let job = jobs_core::get(&*state.work().await?, &id).await?;
    let url = job.url.as_deref().map(str::trim).unwrap_or_default();
    // The rule a fetched link passes: https, no user name or password, no port.
    if !connectors::link_ok(url, &[]) {
        return Err(Error::Refused("this job has no https link to open".into()));
    }
    native::open_url(&app, url).await
}

/// The tracker entry links the stored text and carries no note.
#[tauri::command]
pub async fn job_to_app(state: State<'_, AppState>, id: String) -> Result<Application> {
    let db = state.work().await?;
    let job = jobs_core::get(&db, &id).await?;
    let input = ApplicationInput {
        // A tracker entry needs a company: say plainly when the posting named none.
        company: job.company.unwrap_or_else(|| "Company not stated".into()),
        role: job.title,
        job_text_id: Some(job.job_text_id),
        link: job.url.unwrap_or_default(),
        country: job.country.unwrap_or_default(),
        ..ApplicationInput::default()
    };
    applications::add(&db, ACTOR, input).await
}

#[tauri::command]
pub async fn job_sources_status() -> Vec<SourceStatus> {
    connectors::statuses()
}

#[tauri::command]
pub async fn job_sources_list(state: State<'_, AppState>) -> Result<Vec<SourceRow>> {
    jobs_core::list_sources(&*state.work().await?).await
}

#[tauri::command]
pub async fn job_src_add(
    state: State<'_, AppState>,
    connector: String,
    target: String,
    query: String,
) -> Result<SourceRow> {
    jobs_core::add_source(&*state.work().await?, &connector, &target, &query).await
}

#[tauri::command]
pub async fn job_src_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    jobs_core::remove_source(&*state.work().await?, &id).await
}

/// Reads the sources named, or all of them. The run's token is the pause watcher's.
#[tauri::command]
pub async fn jobs_fetch(
    state: State<'_, AppState>,
    src_ids: Vec<String>,
) -> Result<DiscoveryReport> {
    let running = state.ai.begin()?;
    let db = state.work().await?;
    agents::discover(&db, ACTOR, src_ids, running.token.clone()).await
}

#[tauri::command]
pub async fn jobs_analyze_plan(state: State<'_, AppState>, ids: Vec<String>) -> Result<AiPlan> {
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    agents::match_plan(&db, &router, &ids).await
}

/// Like `memory_refresh`: with outside calls ahead, the count is named in a native dialog
/// first, and a count that moved after the yes is refused.
#[tauri::command]
pub async fn jobs_analyze<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    ids: Vec<String>,
    ai: bool,
) -> Result<MatchReport> {
    let mut asked = 0;
    if ai {
        let plan = {
            let db = state.work().await?;
            // A pause refuses the run, so no one is asked to confirm a count first.
            pause::ensure_off(&db).await?;
            let router = Router::from_settings(&db).await?;
            agents::match_plan(&db, &router, &ids).await?
        };
        asked = plan.external_calls;
        if asked > 0 {
            let message = format!(
                "This will make at most {asked} paid or subscription calls for {} jobs.",
                plan.jobs
            );
            native::confirmed(&app, "Analyze jobs with a model", message, "Analyze").await?;
        }
    }
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    if ai && agents::match_plan(&db, &router, &ids).await?.external_calls != asked {
        return Err(Error::Validation(
            "the number of outside calls changed since you were asked; press Analyze again".into(),
        ));
    }
    let req = MatchRequest { job_ids: ids, ai };
    agents::match_jobs(&db, &router, ACTOR, req, running.token.clone()).await
}

#[tauri::command]
pub async fn job_research_plan(state: State<'_, AppState>, company_id: String) -> Result<AiPlan> {
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    agents::research_plan(&db, &router, &company_id).await
}

#[tauri::command]
pub async fn job_research<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    company_id: String,
) -> Result<CompanyResearch> {
    let plan = {
        let db = state.work().await?;
        pause::ensure_off(&db).await?;
        let router = Router::from_settings(&db).await?;
        agents::research_plan(&db, &router, &company_id).await?
    };
    let asked = plan.external_calls;
    if asked > 0 {
        let message = format!(
            "This will make at most {asked} paid or subscription calls for {} stored texts.",
            plan.jobs
        );
        native::confirmed(&app, "Research a company", message, "Research").await?;
    }
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    if agents::research_plan(&db, &router, &company_id)
        .await?
        .external_calls
        != asked
    {
        return Err(Error::Validation(
            "the number of outside calls changed since you were asked; press Research again".into(),
        ));
    }
    agents::research_company(&db, &router, ACTOR, &company_id, running.token.clone()).await
}

#[tauri::command]
pub async fn companies_list(state: State<'_, AppState>) -> Result<Vec<Company>> {
    company::list(&*state.work().await?).await
}

#[tauri::command]
pub async fn company_note_add(
    state: State<'_, AppState>,
    company_id: String,
    text: String,
) -> Result<CompanyNote> {
    company::add_note(&*state.work().await?, ACTOR, &company_id, &text).await
}

#[tauri::command]
pub async fn match_prefs_get(state: State<'_, AppState>) -> Result<MatchPrefs> {
    match_prefs::get(&*state.work().await?).await
}

#[tauri::command]
pub async fn match_prefs_set(state: State<'_, AppState>, prefs: MatchPrefs) -> Result<()> {
    match_prefs::set(&*state.work().await?, &prefs).await
}

#[tauri::command]
pub async fn job_list(state: State<'_, AppState>) -> Result<Vec<JobText>> {
    knowledge::list_job_texts(&*state.work().await?).await
}

#[tauri::command]
pub async fn job_get(state: State<'_, AppState>, id: String) -> Result<JobText> {
    knowledge::get_job_text(&*state.work().await?, &id).await
}

#[tauri::command]
pub async fn app_list(
    state: State<'_, AppState>,
    status: Option<String>,
) -> Result<Vec<Application>> {
    applications::list(&*state.work().await?, status.as_deref()).await
}

#[tauri::command]
pub async fn app_get(state: State<'_, AppState>, id: String) -> Result<Application> {
    applications::get(&*state.work().await?, &id).await
}

#[tauri::command]
pub async fn app_add(state: State<'_, AppState>, input: ApplicationInput) -> Result<Application> {
    applications::add(&*state.work().await?, ACTOR, input).await
}

#[tauri::command]
pub async fn app_update(
    state: State<'_, AppState>,
    id: String,
    input: ApplicationInput,
) -> Result<Application> {
    applications::update(&*state.work().await?, ACTOR, &id, input).await
}

#[tauri::command]
pub async fn app_set_status(
    state: State<'_, AppState>,
    id: String,
    status: String,
    on: Option<String>,
    note: String,
) -> Result<Application> {
    agents::set_status(
        &*state.work().await?,
        ACTOR,
        &id,
        &status,
        on.as_deref(),
        &note,
        CancellationToken::new(),
    )
    .await
}

#[tauri::command]
pub async fn app_note(state: State<'_, AppState>, id: String, note: String) -> Result<()> {
    applications::add_note(&*state.work().await?, ACTOR, &id, &note).await
}

#[tauri::command]
pub async fn app_history(state: State<'_, AppState>, id: String) -> Result<Vec<ApplicationEvent>> {
    applications::history(&*state.work().await?, &id).await
}

#[tauri::command]
pub async fn app_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    applications::remove(&*state.work().await?, ACTOR, &id).await
}

#[tauri::command]
pub async fn memory_list(
    state: State<'_, AppState>,
    class: Option<Class>,
) -> Result<Vec<QuestionRow>> {
    memory::list(&*state.work().await?, class).await
}

#[tauri::command]
pub async fn memory_get(state: State<'_, AppState>, question_id: String) -> Result<QuestionRow> {
    memory::get(&*state.work().await?, &question_id).await
}

#[tauri::command]
pub async fn memory_add_question(
    state: State<'_, AppState>,
    text: String,
    country: Option<String>,
) -> Result<Question> {
    memory::add_question(&*state.work().await?, &text, country.as_deref()).await
}

#[tauri::command]
pub async fn memory_add_answer(
    state: State<'_, AppState>,
    question_id: String,
    text: String,
    source_ref: Option<String>,
) -> Result<Answer> {
    let db = state.work().await?;
    memory::add_answer(&db, &question_id, &text, source_ref.as_deref()).await
}

#[tauri::command]
pub async fn memory_edit_answer(
    state: State<'_, AppState>,
    answer_id: String,
    text: String,
) -> Result<Answer> {
    memory::edit_answer(&*state.work().await?, &answer_id, &text).await
}

// ponytail: the class is read before the dialog and not again after it, so a raise that lands
// while the dialog is open is approved on the answer given. Core still refuses what the
// class forbids then, and the audit row names who approved.
/// Approving under a sealed class asks a native yes, so script in the webview cannot make a
/// sensitive answer usable. Each `state.work()` here ends before any dialog.
#[tauri::command]
pub async fn memory_approve<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    answer_id: String,
    approved: bool,
) -> Result<()> {
    if approved {
        let (class, _) = memory::find_answer(&*state.work().await?, &answer_id).await?;
        memory::refuse_authorization(class)?;
        if class.sealed() {
            let message = format!(
                "Approve the {} answer {answer_id}?\n\nIt can then be offered for applications, \
                 each use confirmed by you.",
                class.as_str()
            );
            native::confirmed(&app, "Approve sealed answer", message, "Approve").await?;
        }
    }
    memory::approve(&*state.work().await?, ACTOR, &answer_id, approved).await
}

/// See [`memory_approve`]: marking a sealed-class answer as still true is a person's word.
#[tauri::command]
pub async fn memory_mark_reviewed<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    answer_id: String,
) -> Result<()> {
    let (class, _) = memory::find_answer(&*state.work().await?, &answer_id).await?;
    memory::refuse_authorization(class)?;
    if class.sealed() {
        let message = format!(
            "Mark the {} answer {answer_id} as reviewed?\n\nThis says it is still true.",
            class.as_str()
        );
        native::confirmed(&app, "Review sealed answer", message, "Mark reviewed").await?;
    }
    memory::mark_reviewed(&*state.work().await?, ACTOR, &answer_id).await
}

/// Lowering a class can open sealed bodies, so it asks a native yes. Raising only protects.
#[tauri::command]
pub async fn memory_set_class<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    question_id: String,
    class: Class,
) -> Result<Question> {
    let question = memory::get(&*state.work().await?, &question_id)
        .await?
        .question;
    memory::check_lowering(&question, class)?;
    let from = question.class;
    if class < from {
        let takes_answers = match from {
            Class::WorkAuthorization => "It can then take an answer here. ",
            _ => "",
        };
        let message = format!(
            "Lower question {question_id} from {} to {}?\n\n{takes_answers}A body in the \
             normal class is stored as plain text.",
            from.as_str(),
            class.as_str()
        );
        native::confirmed(&app, "Lower class", message, "Lower").await?;
    }
    memory::set_class(&*state.work().await?, ACTOR, &question_id, class).await
}

/// The text of one answer. A sealed body is shown only after a native confirmation, every
/// time, as in [`kb_reveal`]: the dialog is Rust's and does not name the text. An unknown id
/// answers `not_found` and an unsealed answer answers at once, with no dialog.
#[tauri::command]
pub async fn memory_reveal<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    answer_id: String,
) -> Result<String> {
    let (class, answer) = memory::find_answer(&*state.work().await?, &answer_id).await?;
    if answer.sealed {
        let message = format!(
            "Show the sealed {} answer {answer_id}?\n\nIts text will appear on screen until \
             you hide it or leave this screen.",
            class.as_str()
        );
        native::confirmed(&app, "Show sealed text", message, "Show").await?;
    }
    memory::reveal(&*state.work().await?, ACTOR, &answer_id).await
}

/// Confirming a use always asks a native yes, whatever the class, so script in the webview
/// cannot make a sealed answer usable. The dialog names the class, the application, and the
/// question, never the answer's value. Each `state.work()` here ends before the dialog.
// ponytail: the version read before the dialog is not bound across it, so an edit that lands
// while it is open is confirmed as the new version. Core still refuses an unapproved answer
// then, and an edit unapproves, so the user must approve the new text before this counts.
#[tauri::command]
pub async fn memory_confirm_use<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    answer_id: String,
    application_id: String,
) -> Result<()> {
    let message = {
        let db = state.work().await?;
        let (class, answer) = memory::find_answer(&db, &answer_id).await?;
        let question = memory::get(&db, &answer.question_id).await?.question;
        let application = applications::get(&db, &application_id).await?;
        // The gates core would refuse on run first, so no one is asked for a doomed yes.
        if class == Class::WorkAuthorization {
            return Err(Error::Refused("use_profile_by_country".into()));
        }
        if !answer.approved {
            return Err(Error::Refused("answer_not_approved".into()));
        }
        let stale = match answer.stale {
            true => "\n\nThis answer has not been reviewed for over 180 days.",
            false => "",
        };
        format!(
            "Use the {} answer to \"{}\" for {} / {}?\n\nYou confirm this is true today.{stale}",
            class.as_str(),
            question.text.chars().take(300).collect::<String>(),
            application.company,
            application.role
        )
    };
    native::confirmed(&app, "Confirm this answer", message, "Confirm").await?;
    memory::confirm_use(&*state.work().await?, ACTOR, &answer_id, &application_id).await
}

/// The text of one answer for one application. Core refuses a sealed class with no current
/// confirmation, so this opens no dialog of its own.
#[tauri::command]
pub async fn memory_use(
    state: State<'_, AppState>,
    answer_id: String,
    application_id: String,
) -> Result<UsedAnswer> {
    memory::use_answer(&*state.work().await?, ACTOR, &answer_id, &application_id).await
}

#[tauri::command]
pub async fn memory_application_answers(
    state: State<'_, AppState>,
    application_id: String,
) -> Result<Vec<AppliedAnswer>> {
    memory::application_answers(&*state.work().await?, &application_id).await
}

#[tauri::command]
pub async fn memory_authorization(
    state: State<'_, AppState>,
    country: String,
) -> Result<AuthorizationAnswer> {
    memory::authorization_for(&*state.work().await?, &country).await
}

#[tauri::command]
pub async fn memory_remove_question(state: State<'_, AppState>, id: String) -> Result<()> {
    memory::remove_question(&*state.work().await?, &id).await
}

#[tauri::command]
pub async fn memory_remove_answer(state: State<'_, AppState>, id: String) -> Result<()> {
    memory::remove_answer(&*state.work().await?, &id).await
}

/// By meaning when a local model is set up, else by words. A sealed answer's text is never
/// in a hit.
#[tauri::command]
pub async fn memory_search(
    state: State<'_, AppState>,
    query: String,
    limit: usize,
) -> Result<Vec<MemoryHit>> {
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    memory::search(&db, &router, &query, limit).await
}

/// What a press of Refresh would spend. Reads only: it starts no provider and asks no key.
#[tauri::command]
pub async fn memory_refresh_plan(state: State<'_, AppState>) -> Result<RefreshPlan> {
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    memory::refresh_plan(&db, &router).await
}

/// Only a person's press starts it. With outside calls ahead, the count is named in a native
/// dialog first, with no `work` held, and the plan is read again after the answer: a count
/// that moved in between is refused, so the yes covers what ran.
#[tauri::command]
pub async fn memory_refresh<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<MemoryRefreshed> {
    let plan = {
        let db = state.work().await?;
        let router = Router::from_settings(&db).await?;
        memory::refresh_plan(&db, &router).await?
    };
    let asked = plan.external_calls;
    if asked > 0 {
        let message = format!(
            "This will make at most {asked} paid or subscription calls for {} questions \
             (a press checks {} questions at most).",
            plan.pending,
            memory::CLASSIFY_BATCH
        );
        native::confirmed(&app, "Refresh career memory", message, "Refresh").await?;
    }
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    if memory::refresh_plan(&db, &router).await?.external_calls != asked {
        return Err(Error::Validation(
            "the number of outside calls changed since you were asked; press Refresh again".into(),
        ));
    }
    agents::memory_refresh(&db, &router, ACTOR, running.token.clone()).await
}

/// The path comes from the picker: the webview never names a file to read. Closing the
/// picker is `cancelled`.
#[tauri::command]
pub async fn resume_import<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<Imported> {
    drop(state.work().await?);
    let dialog = FileDialog {
        title: "Import resume",
        filter: "Resume",
        extensions: &["pdf", "docx"],
        file_name: None,
    };
    let path = native::open_file(&app, dialog).await?;
    parse::import_resume(&*state.work().await?, &path, &title_of(&path)).await
}

fn title_of(path: &Path) -> String {
    path.file_stem().map_or_else(
        || "Resume".into(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

#[tauri::command]
pub async fn resume_list(state: State<'_, AppState>, kind: Option<String>) -> Result<Vec<Draft>> {
    drafts::list(&*state.work().await?, kind.as_deref()).await
}

#[tauri::command]
pub async fn resume_versions(state: State<'_, AppState>, lineage_id: String) -> Result<Vec<Draft>> {
    drafts::versions(&*state.work().await?, &lineage_id).await
}

#[tauri::command]
pub async fn resume_generate(
    state: State<'_, AppState>,
    job_text_id: String,
    export: Option<ExportChoice>,
) -> Result<Generated> {
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    let token = running.token.clone();
    agents::draft_resume(&db, &router, ACTOR, &job_text_id, export, token).await
}

#[tauri::command]
pub async fn letter_generate(
    state: State<'_, AppState>,
    job_text_id: String,
    export: Option<ExportChoice>,
) -> Result<Generated> {
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    let token = running.token.clone();
    agents::draft_cover_letter(&db, &router, ACTOR, &job_text_id, export, token).await
}

#[tauri::command]
pub async fn resume_compare(
    state: State<'_, AppState>,
    generated_id: String,
    original_id: Option<String>,
) -> Result<Compare> {
    drafts::compare(&*state.work().await?, &generated_id, original_id.as_deref()).await
}

#[tauri::command]
pub async fn claim_edit(
    state: State<'_, AppState>,
    claim_id: String,
    input: ClaimInput,
) -> Result<Draft> {
    drafts::edit_claim(&*state.work().await?, &claim_id, input).await
}

#[tauri::command]
pub async fn claim_remove(state: State<'_, AppState>, claim_id: String) -> Result<Draft> {
    drafts::remove_claim(&*state.work().await?, &claim_id).await
}

/// The user's own edit (FR-3.23), which needs no AI.
#[tauri::command]
pub async fn draft_save(
    state: State<'_, AppState>,
    kind: String,
    lineage_id: Option<String>,
    doc: ResumeDoc,
) -> Result<Draft> {
    drafts::save_manual(&*state.work().await?, &kind, lineage_id.as_deref(), doc).await
}

/// Always under `exports/`: the webview chooses no folder.
#[tauri::command]
pub async fn resume_export(
    state: State<'_, AppState>,
    draft_id: String,
    template: Template,
    format: ExportFormat,
) -> Result<PathBuf> {
    let db = state.work().await?;
    let exports = db.paths().exports();
    drafts::export(&db, &draft_id, template, format, &exports, ACTOR).await
}

#[tauri::command]
pub async fn ai_status(state: State<'_, AppState>) -> Result<AiStatus> {
    ai::status(&*state.work().await?).await
}

#[tauri::command]
pub async fn ai_pull(state: State<'_, AppState>, model: String) -> Result<()> {
    let running = state.pull.begin()?;
    let db = state.work().await?;
    let ollama = ollama_from_settings(&db)
        .await?
        .ok_or_else(|| Error::Refused("not_loopback".into()))?;
    let cancel = running.token.clone();
    ollama.pull(&db, ACTOR, &model, |_, _| {}, cancel).await
}

#[tauri::command]
pub async fn ai_cancel_pull(state: State<'_, AppState>) -> Result<()> {
    state.pull.cancel();
    Ok(())
}

/// No confirmation here: the consent record is the gate, and the router checks it on every call.
#[tauri::command]
pub async fn ai_set_models(
    state: State<'_, AppState>,
    drafting: Option<String>,
    embedding: Option<String>,
) -> Result<()> {
    if drafting.is_none() && embedding.is_none() {
        return Err(Error::Validation(
            "give a drafting or embedding model".into(),
        ));
    }
    let db = state.work().await?;
    if let Some(model) = drafting {
        ai::set_model(&db, "drafting", &model).await?;
    }
    if let Some(model) = embedding {
        ai::set_model(&db, "embedding", &model).await?;
    }
    Ok(())
}

/// No confirmation here: the consent record is the gate, and the router checks it on every call.
#[tauri::command]
pub async fn ai_set_order(state: State<'_, AppState>, order: Vec<String>) -> Result<()> {
    ai::set_order(&*state.work().await?, &order).await
}

/// No confirmation here: the consent record is the gate, and the router checks it on every call.
#[tauri::command]
pub async fn ai_enable(state: State<'_, AppState>, provider: String, enabled: bool) -> Result<()> {
    ai::set_enabled(&*state.work().await?, &provider, enabled).await
}

/// The program that runs as Claude Code. The picker is the confirmation: the webview cannot
/// name a path, as it could to a confirm dialog that only asks yes or no.
#[tauri::command]
pub async fn ai_set_claude_path<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<()> {
    drop(state.work().await?);
    let dialog = FileDialog {
        title: "Choose the claude program",
        filter: "Program",
        extensions: &[],
        file_name: None,
    };
    let path = native::open_file(&app, dialog).await?;
    if !(path.is_absolute() && claude_code::has_claude_name(&path)) {
        return Err(Error::Validation("choose a program named claude".into()));
    }
    let path = path
        .to_str()
        .ok_or_else(|| Error::Validation("this path cannot be stored".into()))?;
    let db = state.work().await?;
    settings::set(&db, claude_code::PATH_SETTING, &json!(path)).await
}

/// Back to finding `claude` by itself, which changes what runs, so it is confirmed too.
#[tauri::command]
pub async fn ai_clear_claude_path<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<()> {
    let stored = settings::get(&*state.work().await?, claude_code::PATH_SETTING).await?;
    let stored = stored.as_ref().and_then(Value::as_str).unwrap_or_default();
    let message = format!(
        "Stop using this program as Claude Code, and find claude automatically?\n\n{stored}"
    );
    native::confirmed(&app, "Claude Code program", message, "Clear").await?;
    let db = state.work().await?;
    settings::set(&db, claude_code::PATH_SETTING, &json!("")).await
}

/// The program that runs as the Gemini or Codex CLI. The picker is the confirmation, as for
/// Claude: the webview names the provider and never a path.
#[tauri::command]
pub async fn ai_set_cli_path<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider: String,
) -> Result<()> {
    drop(state.work().await?);
    let spec = child::spec_for(&provider)
        .ok_or_else(|| Error::Validation("choose a program for gemini_cli or codex_cli".into()))?;
    let dialog = FileDialog {
        title: &format!("Choose the {} program", spec.program),
        filter: "Program",
        extensions: &[],
        file_name: None,
    };
    let path = native::open_file(&app, dialog).await?;
    if !(path.is_absolute() && child::accepts_name(&path, spec, cfg!(windows))) {
        return Err(Error::Validation(format!(
            "choose a program named {}",
            child::bin_names(spec.program, cfg!(windows)).join(" or ")
        )));
    }
    let path = path
        .to_str()
        .ok_or_else(|| Error::Validation("this path cannot be stored".into()))?;
    let db = state.work().await?;
    child::set_cli_path(&db, &provider, path).await
}

/// Back to finding the program by itself, which changes what runs, so it is confirmed too.
#[tauri::command]
pub async fn ai_clear_cli_path<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider: String,
) -> Result<()> {
    let spec = child::spec_for(&provider)
        .ok_or_else(|| Error::Validation("choose a program for gemini_cli or codex_cli".into()))?;
    let stored = settings::get(&*state.work().await?, spec.setting).await?;
    let stored = stored.as_ref().and_then(Value::as_str).unwrap_or_default();
    if stored.trim().is_empty() {
        // Already automatic: nothing changes, so there is nothing to confirm.
        return Ok(());
    }
    let message = format!(
        "Stop using this program as {}, and find {} automatically?\n\n{stored}",
        spec.program, spec.program
    );
    native::confirmed(&app, "Provider program", message, "Clear").await?;
    let db = state.work().await?;
    child::set_cli_path(&db, &provider, "").await
}

/// Where Ollama listens. Core refuses anything off this machine (M-21) before anyone is
/// asked; the dialog is for another program that listens here.
#[tauri::command]
pub async fn ai_set_ollama_url<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    url: String,
) -> Result<()> {
    drop(state.work().await?);
    Ollama::new(&url, None, None)?;
    let message = format!("Send local AI requests to this address?\n\n{url}");
    native::confirmed(&app, "Ollama address", message, "Use this address").await?;
    let db = state.work().await?;
    settings::set(&db, "ai.ollama_url", &json!(url)).await
}

/// The model an API provider or the endpoint runs. No dialog: the consent record is the gate,
/// and `check_model` holds the origin rules.
#[tauri::command]
pub async fn ai_set_api_model(
    state: State<'_, AppState>,
    provider: String,
    model: String,
) -> Result<()> {
    api::set_model(&*state.work().await?, &provider, &model).await
}

/// Where the compatible endpoint listens. Core refuses anything off this machine before anyone
/// is asked; the dialog is for another program that listens here. An empty address clears it.
#[tauri::command]
pub async fn ai_set_endpoint<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    url: String,
) -> Result<()> {
    drop(state.work().await?);
    let (message, button) = if url.trim().is_empty() {
        let message = "Stop using the compatible endpoint, and forget its address, model, and \
                       any stored key?";
        (message.to_string(), "Clear")
    } else {
        let stored = api::check_endpoint(&url)?;
        let message = format!(
            "Send AI requests to this address?\n\n{stored}\n\nA different address ends the \
             consent given for the old one, and removes any key stored for it."
        );
        (message, "Use this address")
    };
    native::confirmed(&app, "Compatible endpoint", message, button).await?;
    api::set_endpoint(&*state.work().await?, &url).await
}

/// A provider in the table. Ollama is not one: it runs here and needs no record, and a name
/// the owner decisions leave out has no row.
fn consent_provider(provider: &str) -> Result<&'static ProviderInfo> {
    register::info(provider)
        .ok_or_else(|| Error::Validation("consent does not apply to this provider".into()))
}

/// What the native dialog says. It states the data use and the attestation itself, as a yes
/// to this text is the consent: script in the webview cannot change what it reads.
fn consent_message(sharing: &Sharing, categories: &[DataCategory]) -> String {
    let (label, destination) = (&sharing.label, &sharing.destination);
    let names = json!(categories);
    let word = if sharing.name == "endpoint" {
        "Address"
    } else {
        "Program"
    };
    let program = sharing.path.as_ref();
    let program = program.map_or_else(String::new, |path| format!("\n\n{word}: {path}"));
    let attestation = sharing.attestation.as_ref();
    let attestation = attestation.map_or_else(String::new, |line| format!("\n\n{line}"));
    format!(
        "Allow JobPilot to send this data to {label}?\n\n{names}{program}\n\n\
         Where it goes: {destination}\n\n\
         How {label} uses it: {}{attestation}",
        sharing.data_use
    )
}

/// The record that lets data leave this machine (FR-3.30), so it is confirmed outside the
/// webview. The dialog alone would not stop injected script: it can type into the terminal
/// panel too, which is why the CLI's `ai consent give` is refused there (M-14).
#[tauri::command]
pub async fn ai_consent_give<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider: String,
    categories: Vec<DataCategory>,
) -> Result<()> {
    consent_provider(&provider)?;
    let sharing = ai::sharing(&*state.work().await?, &provider).await?;
    let title = format!("{} consent", sharing.label);
    let message = consent_message(&sharing, &categories);
    native::confirmed(&app, &title, message, "Allow").await?;
    let db = state.work().await?;
    // Pressing Allow on that text attests, when the provider asks for it.
    consent::give_for(&db, &provider, &categories, true, ACTOR).await
}

/// Revoked first, so a run that has not yet reached the consent check cannot pass it, and the
/// token fires whatever the write did: a run the user wants stopped stops.
#[tauri::command]
pub async fn ai_consent_revoke(state: State<'_, AppState>, provider: String) -> Result<()> {
    let id = consent_provider(&provider)?.id.to_string();
    let revoked = match state.work().await {
        Ok(db) => consent::revoke(&db, &id, ACTOR).await,
        Err(error) => Err(error),
    };
    state.ai.cancel();
    revoked
}

/// What the consent dialog shows.
#[tauri::command]
pub async fn ai_sharing(state: State<'_, AppState>, provider: String) -> Result<Sharing> {
    consent_provider(&provider)?;
    ai::sharing(&*state.work().await?, &provider).await
}

/// The exact resume prompt for a pasted posting. Builds it and sends nothing.
#[tauri::command]
pub async fn ai_preview(
    state: State<'_, AppState>,
    job_text_id: String,
    provider: String,
) -> Result<PromptPreview> {
    let db = state.work().await?;
    generate::preview(&db, TaskType::DraftResume, &job_text_id, &provider).await
}

/// A key comes from the system clipboard, so it never enters the webview, which sends only a
/// provider name. Script there cannot answer the dialog, and what it shows is what is stored:
/// the clipboard is read once, before the dialog. No lock is held while it is open.
#[tauri::command]
pub async fn ai_key_from_clipboard<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider: String,
) -> Result<()> {
    let label = secrets::key_provider(&provider)?.label;
    drop(state.work().await?);
    let copied = native::clipboard_text(&app).await?.map(Zeroizing::new);
    // Any other text is someone's posting or password, so it is not repeated back.
    let not_a_key = || Error::Validation("the clipboard does not hold a key".into());
    let copied = copied.ok_or_else(not_a_key)?;
    let key = secrets::check_api_key(&copied).map_err(|_| not_a_key())?;
    let last_four = &key[key.len() - 4..];
    let message = format!(
        "Store the key ending in {last_four} as your {label} key? It goes to the system \
         keychain. The clipboard is cleared after."
    );
    native::confirmed(&app, &format!("{label} key"), message, "Store key").await?;
    secrets::set_api_key(&*state.work().await?, &provider, &key, ACTOR).await?;
    // The text as copied: a key with a line break after it is still that text.
    native::clear_clipboard(&app, &copied).await
}

/// Confirmed outside the webview, as script there must not be able to end the user's access.
#[tauri::command]
pub async fn ai_key_remove<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    provider: String,
) -> Result<()> {
    let label = secrets::key_provider(&provider)?.label;
    let stored = secrets::names(&*state.work().await?).await?;
    if !stored.contains(&secrets::api_key_name(&provider)) {
        return Err(Error::NotFound);
    }
    let message = format!("Remove your {label} key from the system keychain?");
    native::confirmed(&app, &format!("{label} key"), message, "Remove key").await?;
    secrets::delete_api_key(&*state.work().await?, &provider, ACTOR).await
}

#[tauri::command]
pub async fn ai_index(state: State<'_, AppState>) -> Result<usize> {
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    embed::index_missing(&db, &router, running.token.clone()).await
}

#[tauri::command]
pub async fn ai_cancel(state: State<'_, AppState>) -> Result<()> {
    state.ai.cancel();
    Ok(())
}

/// What the command and the menu item both run. The tokens fire first, whatever comes
/// after, so a running task stops even with no data open. The write goes through the `db`
/// read lock and not [`AppState::work`]: it must not queue behind a task holding the gate.
/// An apply session in the panel is closed, with its browser, before the row: the row can
/// fail, and the session must not outlive the press. Once the row is in, the run the session
/// hosted is ended `halted`, off this path.
pub async fn pause_now(state: &AppState, pty: &PtyState) -> Result<PauseState> {
    // Before the tokens, so a run that ends on its token knows why while the row is pending.
    pause::announce(&state.paths);
    state.ai.cancel();
    let closed = pty.close_apply().await;
    state.pull.cancel();
    let db = state.db.read().await.clone().ok_or(Error::Unavailable)?;
    let set = pause::set(&db, ACTOR).await?;
    if let Some(apply_id) = closed {
        halt_hosted(db, apply_id);
    }
    Ok(set)
}

/// The host the pause killed cannot write its own end: its agent run is halted with reason
/// `paused`, and the run is wound up. Spawned, as a purge can wait.
fn halt_hosted(db: Arc<Db>, apply_id: String) {
    tauri::async_runtime::spawn(async move {
        let _ = apply::host_gone(&db, &apply_id, "paused").await;
    });
}

/// For a pause set by another process, such as `jobpilot pause set`: its running tasks are
/// not this app's to find, but a download or an index started here is, and so is an apply
/// session in the panel. True when it is on.
pub async fn stop_if_paused(state: &AppState, pty: &PtyState) -> bool {
    let db = state.db.read().await.clone();
    let on = match &db {
        Some(db) => pause::is_set(db).await.unwrap_or(false),
        None => false,
    };
    if on {
        state.ai.cancel();
        let closed = pty.close_apply().await;
        state.pull.cancel();
        if let (Some(db), Some(apply_id)) = (db, closed) {
            halt_hosted(db, apply_id);
        }
    }
    on
}

/// Once at startup: what a dead apply host left, its processes, folders, and a run that
/// would block the next one. A failure here only waits for the next sweep.
pub async fn sweep_at_start(state: &AppState) {
    if let Ok(db) = state.work().await {
        let _ = apply::session::sweep_orphans(&db).await;
    }
}

#[tauri::command]
pub async fn pause_status(state: State<'_, AppState>) -> Result<PauseState> {
    let slot = state.db.read().await;
    pause::state(ready(&slot)?).await
}

#[tauri::command]
pub async fn pause_set(state: State<'_, AppState>, pty: State<'_, PtyState>) -> Result<PauseState> {
    pause_now(&state, &pty).await
}

/// Only a dialog Rust opens clears it, with no lock held while it waits. The shortcut and
/// the menu only ever set.
#[tauri::command]
pub async fn pause_clear<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<PauseState> {
    ready(&*state.db.read().await)?;
    native::confirmed(
        &app,
        "Clear emergency pause",
        "Runs stay halted until you resume each one.".into(),
        "Clear",
    )
    .await?;
    let slot = state.db.read().await;
    pause::clear(ready(&slot)?, ACTOR).await
}

#[tauri::command]
pub async fn agents_list(state: State<'_, AppState>, limit: i64) -> Result<Vec<RunSummary>> {
    runner::list(&*state.work().await?, limit).await
}

#[tauri::command]
pub async fn agent_get(state: State<'_, AppState>, id: String) -> Result<RunDetail> {
    runner::get(&*state.work().await?, &id).await
}

#[tauri::command]
pub async fn agent_cancel(state: State<'_, AppState>, id: String) -> Result<()> {
    agents::cancel_run(&*state.work().await?, ACTOR, &id).await
}

/// A run stopped short is the command's error, so the view shows its code.
#[tauri::command]
pub async fn agent_resume(state: State<'_, AppState>, id: String) -> Result<RunDetail> {
    let running = state.ai.begin()?;
    let db = state.work().await?;
    let router = Router::from_settings(&db).await?;
    agents::resume_run(&db, &router, ACTOR, &id, running.token.clone())
        .await?
        .into_result()?;
    runner::get(&db, &id).await
}

/// Builds a report from the application store. No model, so no AI slot is taken.
#[tauri::command]
pub async fn analytics_run(state: State<'_, AppState>) -> Result<Report> {
    agents::analytics_report(&*state.work().await?, ACTOR, CancellationToken::new()).await
}

#[tauri::command]
pub async fn analytics_latest(state: State<'_, AppState>) -> Result<Option<Report>> {
    analytics::latest(&*state.work().await?).await
}

/// `peek`: asks the store and never makes a key.
#[tauri::command]
pub async fn key_state(state: State<'_, AppState>) -> Result<KeyState> {
    crypto::peek(&*state.work().await?).await
}

/// The Retry button. `crypto::state`, so a first key that could not be stored is tried again.
#[tauri::command]
pub async fn key_retry(state: State<'_, AppState>) -> Result<KeyState> {
    crypto::state(&*state.work().await?).await
}

/// The file holds the data key, so the path comes from Rust's own dialog, and core refuses
/// one inside the data root before it writes. The reply names the file and holds no key.
#[tauri::command]
pub async fn recovery_key_export<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<RecoveryReceipt> {
    let key_id = match crypto::peek(&*state.work().await?).await? {
        KeyState::Unlocked { key_id } => key_id,
        KeyState::KeyUnavailable { reason, .. } if reason == crypto::NO_KEY_YET => {
            return Err(Error::Refused(
                "there is no key yet, so there is nothing to export".into(),
            ));
        }
        _ => return Err(Error::Locked),
    };
    let dialog = FileDialog {
        title: "Save recovery key",
        filter: "Recovery key",
        extensions: &["json"],
        file_name: Some(format!("jobpilot-recovery-{key_id}.json")),
    };
    let path = native::save_file(&app, dialog).await?;
    recovery::export(&*state.work().await?, &path, ACTOR).await
}

/// `peek` first: core imports only from a state a handle has seen, and a new one has seen none.
/// From a state with no key to import, core's own refusal comes before it reads any file, so
/// that answers and no picker opens.
#[tauri::command]
pub async fn recovery_key_import<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<KeyState> {
    {
        let db = state.work().await?;
        let held = crypto::peek(&db).await?;
        if !matches!(
            held,
            KeyState::KeyMissing { .. } | KeyState::KeyMismatch { .. }
        ) {
            return recovery::import(&db, Path::new(""), ACTOR).await;
        }
    }
    let dialog = FileDialog {
        title: "Import recovery key",
        filter: "Recovery key",
        extensions: &["json"],
        file_name: None,
    };
    let path = native::open_file(&app, dialog).await?;
    let db = state.work().await?;
    crypto::peek(&db).await?;
    let imported = recovery::import(&db, &path, ACTOR).await?;
    state.set_restored_key_state(None);
    Ok(imported)
}

/// Core re-checks the id against the store after the dialog, so what the user read is what
/// goes. From a state with nothing to discard, core's own refusal answers and no dialog opens.
#[tauri::command]
pub async fn discard_locked_fields<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<KeyState> {
    let held = crypto::peek(&*state.work().await?).await?;
    let (KeyState::KeyMissing { key_id } | KeyState::KeyMismatch { key_id }) = held else {
        return recovery::discard_locked(&*state.work().await?, "", ACTOR).await;
    };
    let message = format!(
        "Discard every field locked under key {key_id}?\n\n\
         They cannot be read again without that key's recovery file. A copy of the current \
         data is kept in the backups folder."
    );
    native::confirmed(&app, "Discard locked fields", message, "Discard").await?;
    let discarded = recovery::discard_locked(&*state.work().await?, &key_id, ACTOR).await?;
    state.set_restored_key_state(None);
    Ok(discarded)
}

/// The browser runtime, as `jobpilot browser status` reports it.
#[derive(serde::Serialize)]
pub struct BrowserStatus {
    /// `no_node`, `node_too_old`, `not_installed`, `wrong_version`, or `ready`.
    state: &'static str,
    /// The Node or server version found, when it is the wrong one.
    found: Option<String>,
    playwright_mcp: &'static str,
}

async fn browser_state(db: &Db) -> Result<BrowserStatus> {
    let (state, found) = match browser_runtime::state(db).await? {
        RuntimeState::NoNode => ("no_node", None),
        RuntimeState::NodeTooOld(found) => ("node_too_old", Some(found)),
        RuntimeState::NotInstalled => ("not_installed", None),
        RuntimeState::WrongVersion(found) => ("wrong_version", Some(found)),
        RuntimeState::Ready(_) => ("ready", None),
    };
    Ok(BrowserStatus {
        state,
        found,
        playwright_mcp: PLAYWRIGHT_MCP,
    })
}

#[tauri::command]
pub async fn browser_status(state: State<'_, AppState>) -> Result<BrowserStatus> {
    browser_state(&*state.work().await?).await
}

/// A download the owner starts with the button, in the download slot: emergency pause and
/// app exit cancel it as they cancel a model pull.
#[tauri::command]
pub async fn browser_setup(state: State<'_, AppState>) -> Result<BrowserStatus> {
    let running = state.pull.begin()?;
    let db = state.work().await?;
    browser_runtime::setup(&db, ACTOR, running.token.clone()).await?;
    browser_state(&db).await
}

/// Reads and exports only: nothing is released and no run is written.
#[tauri::command]
pub async fn apply_preflight(
    state: State<'_, AppState>,
    application_ids: Vec<String>,
) -> Result<Preflight> {
    apply_preflight_of(&*state.work().await?, &application_ids).await
}

/// Whether sealed values go to this run is asked here, in Rust's own dialog, and nowhere
/// else: the webview has no argument that releases them. `Leave them out` starts the run with
/// nothing released; closing the dialog starts nothing. With no consent for `Sensitive`
/// nothing sealed can go, so nothing is asked. Core counts again, and refuses a release whose
/// count moved while the dialog was open.
#[tauri::command]
pub async fn apply_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    application_ids: Vec<String>,
) -> Result<String> {
    let checked = apply_preflight_of(&*state.work().await?, &application_ids).await?;
    let sealed = checked.sealed_total;
    let release_sealed = if sealed == 0 || !checked.sensitive_consent {
        false
    } else {
        let applications = checked.items.iter().filter(|item| item.sealed > 0).count();
        let what = match sealed {
            1 => "1 sensitive answer or work-authorization row".to_owned(),
            n => format!("{n} sensitive answers and work-authorization rows"),
        };
        let whom = match applications {
            1 => "1 application".to_owned(),
            n => format!("{n} applications"),
        };
        let message = format!("Release {what} to this run for {whom}?");
        let release = "Release to this run";
        let leave = "Leave them out";
        native::choose(&app, "Sealed answers", message, release, leave)
            .await?
            .ok_or(Error::Cancelled)?
    };
    apply::start_confirmed(
        &*state.work().await?,
        ACTOR,
        &application_ids,
        release_sealed,
        sealed,
    )
    .await
}

/// At most a run's worth: each application renders two documents.
async fn apply_preflight_of(db: &Db, application_ids: &[String]) -> Result<Preflight> {
    if application_ids.len() > apply::MAX_RUN_ITEMS {
        return Err(Error::Validation(format!(
            "a run takes at most {} applications",
            apply::MAX_RUN_ITEMS
        )));
    }
    apply::preflight(db, application_ids).await
}

#[tauri::command]
pub async fn apply_view(state: State<'_, AppState>, id: String) -> Result<RunView> {
    apply::view(&*state.work().await?, &id).await
}

#[tauri::command]
pub async fn apply_latest(state: State<'_, AppState>) -> Result<Option<RunView>> {
    apply::latest(&*state.work().await?).await
}

/// A person's act, from the pause panel's buttons. Page text never reaches this.
#[tauri::command]
pub async fn apply_resolve_pause(
    state: State<'_, AppState>,
    pause_id: String,
    resolution: String,
) -> Result<()> {
    let resolution = match resolution.as_str() {
        "continue" => Resolution::Continue,
        "take_over" => Resolution::TakeOver,
        _ => {
            return Err(Error::Validation(
                "a pause is resolved with continue or take_over".into(),
            ));
        }
    };
    apply::resolve_pause(&*state.work().await?, ACTOR, &pause_id, resolution).await
}

#[tauri::command]
pub async fn apply_settle(state: State<'_, AppState>, id: String) -> Result<usize> {
    apply::settle(&*state.work().await?, ACTOR, &id).await
}

#[tauri::command]
pub async fn apply_cancel(state: State<'_, AppState>, id: String) -> Result<()> {
    apply::cancel(&*state.work().await?, ACTOR, &id).await
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use jobpilot_core::recovery::RecoveryReceipt;
    use jobpilot_core::{AppPaths, Db, Error, backup, crypto, secrets};
    use serde_json::{Value, json};
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;
    use tauri::{App, Listener, Manager, WebviewWindow, WebviewWindowBuilder, WindowEvent};
    use tempfile::TempDir;

    use super::handler;
    use crate::native::{self, NativeHook};
    use crate::pty::{self, MAX_WRITE_BYTES, PtyState};
    use crate::state::{AppState, StartupError};

    const OPENS: &str = "schema_too_new";

    struct Harness {
        _tmp: TempDir,
        paths: AppPaths,
        app: App<MockRuntime>,
        window: WebviewWindow<MockRuntime>,
    }

    fn temp_root() -> (TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(tmp.path().join("data"));
        (tmp, paths)
    }

    fn block_on<F: Future>(work: F) -> F::Output {
        tauri::async_runtime::block_on(work)
    }

    fn harness(tmp: TempDir, paths: AppPaths, state: AppState) -> Harness {
        let app = mock_builder()
            .manage(state)
            .manage(PtyState::default())
            .manage(NativeHook::default())
            .on_window_event(pty::on_window_event)
            .invoke_handler(handler())
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        Harness {
            _tmp: tmp,
            paths,
            app,
            window,
        }
    }

    /// The app as it starts with healthy data.
    fn ready() -> Harness {
        let (tmp, paths) = temp_root();
        let state = block_on(AppState::open(paths.clone()));
        harness(tmp, paths, state)
    }

    /// Healthy data on disk and no handle: a startup failure of `code`. The data is
    /// healthy so a test can tell an attempt to open from no attempt.
    fn recovery(code: &str) -> Harness {
        let (tmp, paths) = temp_root();
        block_on(async { Db::open(&paths).await.unwrap().close().await });
        recovery_at(tmp, paths, code)
    }

    fn recovery_at(tmp: TempDir, paths: AppPaths, code: &str) -> Harness {
        let state = AppState::failed(
            paths.clone(),
            StartupError {
                code: code.into(),
                message: format!("file error: stored {code}"),
            },
        );
        harness(tmp, paths, state)
    }

    /// A real backup of a database holding one document titled `title`, kept outside
    /// the data root so a test with a recovery state has it too.
    fn backup_holding(tmp: &TempDir, title: &str) -> PathBuf {
        let paths = AppPaths::new(tmp.path().join(format!("source-{title}")));
        block_on(async {
            let db = Db::open(&paths).await.unwrap();
            jobpilot_core::documents::create(&db, title, "body")
                .await
                .unwrap();
            let info = backup::create(&db, "test").await.unwrap();
            db.close().await;
            info.path
        })
    }

    impl Harness {
        fn call(&self, cmd: &str, args: Value) -> Result<Value, Value> {
            invoke(&self.window, cmd, args)
        }

        fn ok(&self, cmd: &str, args: Value) -> Value {
            self.call(cmd, args).unwrap()
        }

        fn code(&self, cmd: &str, args: Value) -> String {
            let error = self.call(cmd, args).unwrap_err();
            error["code"].as_str().unwrap().to_owned()
        }

        fn status(&self) -> Value {
            self.ok("status", json!({}))
        }

        fn has_handle(&self) -> bool {
            block_on(self.app.state::<AppState>().db.read()).is_some()
        }

        fn titles(&self) -> Vec<String> {
            let docs = self.ok("documents_list", json!({}));
            let docs = docs.as_array().unwrap();
            docs.iter()
                .map(|doc| doc["title"].as_str().unwrap().to_owned())
                .collect()
        }

        fn assert_ready(&self) {
            let status = self.status();
            assert_eq!(status["state"], "ready", "{status}");
            assert_eq!(status["error"], Value::Null);
            assert!(self.has_handle());
        }

        fn assert_recovery(&self, code: &str) {
            let status = self.status();
            assert_eq!(status["state"], "recovery", "{status}");
            assert_eq!(status["error"]["code"], code, "{status}");
            assert!(!self.has_handle());
        }

        /// What the user answers in the next confirmation dialogs.
        fn answer(&self, yes: bool) {
            let hook = self.app.state::<NativeHook>();
            hook.confirm.store(yes, Ordering::SeqCst);
        }

        /// What the system clipboard holds from here on.
        fn copy(&self, text: &str) {
            let hook = self.app.state::<NativeHook>();
            *hook.clipboard.lock().unwrap() = Some(text.to_owned());
        }

        fn clipboard(&self) -> Option<String> {
            let hook = self.app.state::<NativeHook>();
            hook.clipboard.lock().unwrap().clone()
        }

        /// A `secret_index` row, as a stored key leaves, with no keychain entry behind it.
        fn list_secret(&self, name: &str) {
            let state = self.app.state::<AppState>();
            block_on(async {
                let db = state.work().await.unwrap();
                sqlx::query("INSERT INTO secret_index (name, created_at) VALUES (?, 't')")
                    .bind(name)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            });
        }

        /// What the user picks in the next file dialog.
        fn pick(&self, path: &Path) {
            let hook = self.app.state::<NativeHook>();
            *hook.picked.lock().unwrap() = Some(path.to_path_buf());
        }

        fn asked(&self) -> Vec<String> {
            let hook = self.app.state::<NativeHook>();
            hook.asked.lock().unwrap().clone()
        }

        fn snapshots(&self) -> usize {
            match fs::read_dir(self.paths.backups()) {
                Ok(entries) => entries.count(),
                Err(_) => 0,
            }
        }
    }

    fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
        let request = InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            // The app's own origin: Tauri rejects other origins before any command.
            url: if cfg!(windows) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        get_ipc_response(window, request).map(|body| body.deserialize().unwrap())
    }

    fn take_exclusive(paths: &AppPaths) -> File {
        let lock = File::options().write(true).open(paths.lock()).unwrap();
        lock.try_lock().unwrap();
        lock
    }

    fn restore(source: &Path) -> Value {
        json!({ "source": source })
    }

    fn delete(confirm: &str, include_backups: bool) -> Value {
        json!({ "confirm": confirm, "includeBackups": include_backups })
    }

    #[test]
    fn status_is_recovery_when_db_failed_to_open() {
        let h = recovery(OPENS);

        let status = h.status();

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], OPENS);
        assert_eq!(
            status["error"]["message"],
            "file error: stored schema_too_new"
        );
        assert_eq!(status["schema_version"], Value::Null);
        assert_eq!(status["app_version"], jobpilot_core::VERSION);
        assert_eq!(status["data_root"], h.paths.root().to_str().unwrap());
    }

    #[test]
    fn status_is_ready_with_a_handle() {
        let h = ready();

        let status = h.status();

        assert_eq!(status["state"], "ready");
        assert_eq!(
            status["schema_version"],
            jobpilot_core::app_schema_version()
        );
        assert_eq!(status["error"], Value::Null);
    }

    #[test]
    fn startup_failure_is_stored_with_its_code() {
        let (tmp, paths) = temp_root();
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(paths.root().join("stranger.txt"), "not ours").unwrap();

        let state = block_on(AppState::open(paths.clone()));
        let h = harness(tmp, paths, state);

        h.assert_recovery("root_not_owned");
    }

    #[test]
    fn data_commands_return_unavailable_in_recovery() {
        let h = recovery(OPENS);
        let commands = [
            ("documents_list", json!({})),
            ("documents_create", json!({ "title": "t", "body": "b" })),
            (
                "documents_update",
                json!({ "id": "i", "title": "t", "body": "b" }),
            ),
            ("documents_delete", json!({ "id": "i" })),
            ("documents_search", json!({ "query": "q" })),
            ("settings_get", json!({ "key": "k" })),
            ("settings_set", json!({ "key": "k", "value": 1 })),
            ("backup_create", json!({})),
            ("data_export", json!({})),
            ("data_delete", delete("DELETE", true)),
            ("secret_set", json!({ "name": "n", "value": "v" })),
            ("secret_delete", json!({ "name": "n" })),
            ("secret_names", json!({})),
            ("audit_recent", json!({ "limit": 1 })),
        ];

        for (command, args) in commands {
            assert_eq!(h.code(command, args), "unavailable", "{command}");
        }
        h.assert_recovery(OPENS);
    }

    #[test]
    fn no_secret_read_command_is_registered() {
        let h = ready();

        let error = h.call("secret_get", json!({ "name": "n" })).unwrap_err();

        assert!(error.as_str().unwrap().contains("not found"), "{error}");
    }

    #[test]
    fn a_rejected_secret_never_echoes_its_value() {
        let h = ready();

        let error = h
            .call("secret_set", json!({ "name": "", "value": "hunter2" }))
            .unwrap_err();

        assert_eq!(error["code"], "secret");
        assert!(!error.to_string().contains("hunter2"), "{error}");
        assert_eq!(h.ok("secret_names", json!({})), json!([]));
    }

    #[test]
    fn data_commands_reach_core_as_gui() {
        let h = ready();

        let doc = h.ok(
            "documents_create",
            json!({ "title": "Resume", "body": "rust" }),
        );
        let id = doc["id"].as_str().unwrap();
        h.ok(
            "documents_update",
            json!({ "id": id, "title": "CV", "body": "rust" }),
        );
        assert_eq!(h.titles(), ["CV"]);
        let hits = h.ok("documents_search", json!({ "query": "rust" }));
        assert_eq!(hits[0]["id"], id);
        h.ok("documents_delete", json!({ "id": id }));
        assert!(h.titles().is_empty());
        assert_eq!(h.code("documents_delete", json!({ "id": id })), "not_found");

        assert_eq!(h.ok("settings_get", json!({ "key": "k" })), Value::Null);
        h.ok("settings_set", json!({ "key": "k", "value": { "a": 1 } }));
        assert_eq!(
            h.ok("settings_get", json!({ "key": "k" })),
            json!({ "a": 1 })
        );

        let info = h.ok("backup_create", json!({}));
        assert_eq!(h.ok("backup_list", json!({}))[0]["path"], info["path"]);
        let export = h.ok("data_export", json!({}));
        assert!(
            Path::new(export["path"].as_str().unwrap())
                .join("data.json")
                .is_file()
        );
        assert_eq!(export["withheld"], 0);
        assert_eq!(h.code("secret_delete", json!({ "name": "n" })), "not_found");

        let audit = h.ok("audit_recent", json!({ "limit": 2 }));
        assert_eq!(audit[0]["action"], "export");
        assert_eq!(audit[0]["actor"], "gui");
        assert_eq!(audit[1]["action"], "backup");
        assert_eq!(audit[1]["actor"], "gui");
    }

    #[test]
    fn restore_swaps_the_live_handle() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "kept", "body": "" }));
        let info = h.ok("backup_create", json!({}));
        h.ok("documents_create", json!({ "title": "later", "body": "" }));
        let source = Path::new(info["path"].as_str().unwrap());

        let status = h.ok("backup_restore", restore(source));

        assert_eq!(status["state"], "ready");
        h.assert_ready();
        assert_eq!(h.titles(), ["kept"]);
        let audit = h.ok("audit_recent", json!({ "limit": 1 }));
        assert_eq!(audit[0]["action"], "restore");
        assert_eq!(audit[0]["actor"], "gui");
    }

    #[test]
    fn failed_restore_reopens_and_stays_ready() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let missing = h.paths.root().join("no-such-backup");

        assert_eq!(
            h.code("backup_restore", restore(&missing)),
            "invalid_backup"
        );

        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
    }

    #[test]
    fn restore_refused_while_another_handle_is_open_stays_ready() {
        let h = ready();
        let info = h.ok("backup_create", json!({}));
        let source = Path::new(info["path"].as_str().unwrap());
        let other = block_on(Db::open(&h.paths)).unwrap();

        assert_eq!(h.code("backup_restore", restore(source)), "in_use");

        h.assert_ready();
        block_on(other.close());
    }

    #[test]
    fn restored_but_not_reopened_is_a_recovery_status_not_an_error() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let source = backup_holding(&h._tmp, "restored");
        // The online path does not look at the marker, so only the reopen fails.
        fs::remove_file(h.paths.marker()).unwrap();

        let status = h.ok("backup_restore", restore(&source));

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], "restored_not_opened");
        let message = status["error"]["message"].as_str().unwrap();
        assert!(message.contains("restore finished"), "{message}");
        h.assert_recovery("restored_not_opened");

        fs::write(h.paths.marker(), "io.github.gvsrusa.jobpilot").unwrap();
        assert_eq!(h.ok("retry_open", json!({}))["state"], "ready");
        assert_eq!(h.titles(), ["restored"]);
    }

    #[test]
    fn failed_restore_with_a_failed_reopen_is_a_recovery_status() {
        let h = ready();
        fs::remove_file(h.paths.marker()).unwrap();
        let missing = h.paths.root().join("no-such-backup");

        let status = h.ok("backup_restore", restore(&missing));

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], "root_not_owned");
        let message = status["error"]["message"].as_str().unwrap();
        assert!(message.contains("invalid backup"), "{message}");
        h.assert_recovery("root_not_owned");
    }

    #[test]
    fn restore_works_from_recovery_state() {
        for code in [
            "schema_too_new",
            "migration_failed",
            "db",
            "restored_not_opened",
        ] {
            let h = recovery(code);
            let source = backup_holding(&h._tmp, "restored");

            let status = h.ok("backup_restore", restore(&source));

            assert_eq!(status["state"], "ready", "{code}");
            h.assert_ready();
            assert_eq!(h.titles(), ["restored"]);
        }
    }

    #[test]
    fn offline_restore_is_refused_unless_the_database_cannot_open() {
        for (code, expected) in [
            ("in_use", "in_use"),
            ("root_not_owned", "root_not_owned"),
            ("io", "io"),
            ("unavailable", "unavailable"),
        ] {
            let h = recovery(code);
            let source = backup_holding(&h._tmp, "restored");

            assert_eq!(h.code("backup_restore", restore(&source)), expected);

            h.assert_recovery(code);
            assert_eq!(h.snapshots(), 0, "nothing was written for {code}");
        }
    }

    #[test]
    fn a_refused_io_restore_keeps_a_single_message_prefix() {
        let h = recovery("io");
        let source = backup_holding(&h._tmp, "restored");

        let error = h.call("backup_restore", restore(&source)).unwrap_err();

        assert_eq!(error["message"], "file error: stored io");
    }

    #[test]
    fn failed_offline_restore_stays_in_recovery_without_opening() {
        // The data would open, so a handle here could only come from a retry nobody
        // asked for. Each one adds a snapshot when the failure is a migration.
        let h = recovery("migration_failed");
        let missing = h.paths.root().join("no-such-backup");

        assert_eq!(
            h.code("backup_restore", restore(&missing)),
            "invalid_backup"
        );

        h.assert_recovery("migration_failed");
    }

    #[test]
    fn offline_restore_blocked_by_a_lock_keeps_the_startup_failure() {
        let h = recovery(OPENS);
        let source = backup_holding(&h._tmp, "restored");
        let held = take_exclusive(&h.paths);

        assert_eq!(h.code("backup_restore", restore(&source)), "in_use");

        h.assert_recovery(OPENS);
        drop(held);
    }

    #[test]
    fn no_command_sees_a_half_swapped_handle() {
        let h = ready();
        let info = h.ok("backup_create", json!({}));
        let source = PathBuf::from(info["path"].as_str().unwrap());

        let reader = {
            let window = h.window.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    invoke(&window, "documents_list", json!({})).unwrap();
                    let status = invoke(&window, "status", json!({})).unwrap();
                    assert_eq!(status["state"], "ready");
                }
            })
        };
        for _ in 0..5 {
            h.ok("backup_restore", restore(&source));
        }

        reader.join().unwrap();
    }

    #[test]
    fn data_delete_with_no_handle_is_unavailable_and_changes_nothing() {
        let h = recovery(OPENS);

        assert_eq!(h.code("data_delete", delete("DELETE", true)), "unavailable");

        h.assert_recovery(OPENS);
        assert!(h.paths.db().is_file());
    }

    #[test]
    fn wrong_confirmation_leaves_the_app_ready() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));

        let code = h.code("data_delete", delete("delete", true));

        assert_eq!(code, "confirmation_required");
        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
    }

    #[test]
    fn data_delete_refused_while_another_handle_is_open_stays_ready() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let other = block_on(Db::open(&h.paths)).unwrap();

        assert_eq!(h.code("data_delete", delete("DELETE", true)), "in_use");

        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
        block_on(other.close());
    }

    #[test]
    fn data_delete_reopens_on_empty_data() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "gone", "body": "" }));
        h.ok("backup_create", json!({}));

        let status = h.ok("data_delete", delete("DELETE", false));

        assert_eq!(status["state"], "ready");
        h.assert_ready();
        assert!(h.titles().is_empty());
        assert_eq!(h.ok("backup_list", json!({})).as_array().unwrap().len(), 1);
    }

    #[test]
    fn data_delete_with_a_failed_reopen_is_a_recovery_status() {
        let h = ready();
        fs::remove_file(h.paths.marker()).unwrap();

        let status = h.ok("data_delete", delete("delete", false));

        assert_eq!(status["state"], "recovery");
        assert_eq!(status["error"]["code"], "root_not_owned");
        h.assert_recovery("root_not_owned");
    }

    #[test]
    fn retry_open_recovers_when_the_cause_has_gone() {
        let h = recovery("in_use");

        let status = h.ok("retry_open", json!({}));

        assert_eq!(status["state"], "ready");
        h.assert_ready();
    }

    #[test]
    fn failed_retry_replaces_the_stored_failure() {
        let h = recovery(OPENS);
        let held = take_exclusive(&h.paths);

        let status = h.ok("retry_open", json!({}));

        assert_eq!(status["error"]["code"], "in_use");
        h.assert_recovery("in_use");
        drop(held);
    }

    #[test]
    fn retry_open_with_a_handle_does_not_open_again() {
        let h = ready();
        // An open would fail on this, so a stored failure would prove it ran.
        fs::remove_file(h.paths.marker()).unwrap();

        assert_eq!(h.ok("retry_open", json!({}))["state"], "ready");

        h.assert_ready();
    }

    #[test]
    fn backups_are_listed_in_recovery_for_the_picker() {
        let (tmp, paths) = temp_root();
        let made = block_on(async {
            let db = Db::open(&paths).await.unwrap();
            let info = backup::create(&db, "test").await.unwrap();
            db.close().await;
            info
        });
        let h = recovery_at(tmp, paths, "migration_failed");

        let list = h.ok("backup_list", json!({}));

        assert_eq!(list[0]["path"], made.path.to_str().unwrap());
    }

    #[test]
    fn with_no_data_root_only_status_answers() {
        let (tmp, paths) = temp_root();
        let state = AppState::no_root(&Error::Unavailable);
        let h = harness(tmp, paths, state);
        let source = backup_holding(&h._tmp, "restored");

        h.assert_recovery("unavailable");
        assert_eq!(h.status()["data_root"], "");
        assert_eq!(
            h.ok("retry_open", json!({}))["error"]["code"],
            "unavailable"
        );
        assert_eq!(h.code("backup_list", json!({})), "unavailable");
        assert_eq!(h.code("backup_restore", restore(&source)), "unavailable");
        assert_eq!(h.code("data_delete", delete("DELETE", true)), "unavailable");
    }

    // The PTY tests drive the real debug CLI: `cargo build -p jobpilot-cli` first (a
    // workspace test run builds it). Wide, so ConPTY does not wrap a long temp path.
    const SIZE: (u16, u16) = (400, 40);
    const PTY_WAIT: Duration = Duration::from_secs(20);

    /// What the webview would see of one terminal session.
    struct Panel<'a> {
        harness: &'a Harness,
        data: Arc<Mutex<Vec<u8>>>,
        exits: Arc<Mutex<Vec<Value>>>,
        answered: Cell<usize>,
    }

    impl<'a> Panel<'a> {
        fn listening(harness: &'a Harness) -> Self {
            let data = Arc::new(Mutex::new(Vec::new()));
            let exits = Arc::new(Mutex::new(Vec::new()));
            let sink = data.clone();
            harness.window.listen("pty:data", move |event| {
                let bytes: Vec<u8> = serde_json::from_str(event.payload()).unwrap();
                sink.lock().unwrap().extend(bytes);
            });
            let sink = exits.clone();
            harness.window.listen("pty:exit", move |event| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::from_str(event.payload()).unwrap());
            });
            Panel {
                harness,
                data,
                exits,
                answered: Cell::new(0),
            }
        }

        fn open(harness: &'a Harness) -> Self {
            let panel = Panel::listening(harness);
            let (cols, rows) = SIZE;
            invoke(
                &harness.window,
                "pty_open",
                json!({ "cols": cols, "rows": rows }),
            )
            .expect("build the CLI first: cargo build -p jobpilot-cli");
            panel.wait_for("jobpilot> ");
            panel
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.data.lock().unwrap()).into_owned()
        }

        fn mark(&self) -> usize {
            self.data.lock().unwrap().len()
        }

        fn send(&self, bytes: &[u8]) -> Result<Value, Value> {
            invoke(&self.harness.window, "pty_write", json!({ "data": bytes }))
        }

        /// Enter, as xterm.js sends it.
        fn line(&self, line: &str) {
            self.send(format!("{line}\r").as_bytes()).unwrap();
        }

        fn wait_for(&self, needle: &str) -> String {
            self.wait_after(0, needle)
        }

        fn wait_after(&self, mark: usize, needle: &str) -> String {
            let until = Instant::now() + PTY_WAIT;
            loop {
                let seen = self.data.lock().unwrap()[mark..].to_vec();
                let text = String::from_utf8_lossy(&seen).into_owned();
                if text.contains(needle) {
                    return text;
                }
                // ConPTY asks where the cursor is before it paints; xterm.js answers this.
                let asked = text.matches("\x1b[6n").count();
                if mark == 0 && asked > self.answered.get() {
                    self.answered.set(asked);
                    self.send(b"\x1b[1;1R").unwrap();
                }
                assert!(Instant::now() < until, "no {needle:?} in {text:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn close(&self) {
            invoke(&self.harness.window, "pty_close", json!({})).unwrap();
        }
    }

    impl Drop for Panel<'_> {
        fn drop(&mut self) {
            self.harness.app.state::<PtyState>().close();
        }
    }

    #[cfg(unix)]
    fn running(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(windows)]
    fn running(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        let listed = std::process::Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&listed.stdout).contains(&pid.to_string())
    }

    fn wait_gone(pid: u32) {
        let until = Instant::now() + PTY_WAIT;
        while running(pid) {
            assert!(Instant::now() < until, "process {pid} still runs");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A stand-in for `claude` at a path the app was told in `ai.claude_path`.
    #[cfg(unix)]
    fn fake_claude(harness: &Harness, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = harness._tmp.path().join("fake");
        fs::create_dir(&dir).unwrap();
        let path = dir.join("claude");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        harness.pick(&path);
        invoke(&harness.window, "ai_set_claude_path", json!({})).unwrap();
        path
    }

    #[test]
    fn pty_write_without_session_is_unavailable() {
        let h = ready();
        let written = invoke(&h.window, "pty_write", json!({ "data": [97] }));
        assert_eq!(code_of(written), "unavailable");
        let resized = invoke(&h.window, "pty_resize", json!({ "cols": 80, "rows": 24 }));
        assert_eq!(code_of(resized), "unavailable");
        // Closing nothing is fine: the panel does it before every open.
        invoke(&h.window, "pty_close", json!({})).unwrap();
    }

    #[test]
    fn pty_write_over_the_limit_is_refused_before_anything_else() {
        let h = ready();
        let data = vec![b'x'; MAX_WRITE_BYTES + 1];
        let refused = invoke(&h.window, "pty_write", json!({ "data": data }));
        assert_eq!(code_of(refused), "too_large");
        let panel = Panel::open(&h);
        let refused = panel.send(&vec![b'x'; MAX_WRITE_BYTES + 1]);
        assert_eq!(code_of(refused), "too_large");
        panel.send(&vec![b'x'; MAX_WRITE_BYTES]).unwrap();
    }

    #[test]
    fn pty_open_spawns_shell_and_echoes_prompt() {
        let h = ready();
        let panel = Panel::open(&h);
        assert!(panel.text().contains("jobpilot> "));
        assert!(h.app.state::<PtyState>().pid().is_some());
    }

    #[test]
    fn pty_open_refuses_a_zero_size() {
        let h = ready();
        let refused = invoke(&h.window, "pty_open", json!({ "cols": 0, "rows": 24 }));
        assert_eq!(code_of(refused), "validation");
        assert!(h.app.state::<PtyState>().pid().is_none());
    }

    /// The webview names a size and nothing else: extra arguments do not exist for it.
    #[test]
    fn webview_cannot_choose_what_the_pty_runs() {
        let h = ready();
        let marker = h._tmp.path().join("ran");
        let (cols, rows) = SIZE;
        let script = format!("touch {}", marker.display());
        let panel = Panel::listening(&h);
        let args = json!({
            "cols": cols,
            "rows": rows,
            "command": "/bin/sh",
            "program": "/bin/sh",
            "args": ["-c", script],
            "argv": ["/bin/sh", "-c", script],
            "shell": "/bin/sh",
            "cwd": "/",
            "env": { "JOBPILOT_PANEL": "" },
        });
        invoke(&h.window, "pty_open", args).unwrap();
        panel.wait_for("jobpilot> ");
        panel.line("not-a-command");
        panel.wait_for("not-a-command");
        assert!(!marker.exists());
    }

    #[test]
    fn a_line_never_reaches_a_system_shell() {
        let h = ready();
        let panel = Panel::open(&h);
        let marker = h._tmp.path().join("ran");
        let mark = panel.mark();
        panel.line(&format!("sh -c \"echo hi > {}\"", marker.display()));
        panel.wait_after(mark, "error");
        assert!(!marker.exists());
    }

    #[test]
    fn sessions_are_capped_at_one() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid();
        let again = invoke(&h.window, "pty_open", json!({ "cols": 80, "rows": 24 }));
        assert_eq!(code_of(again), "refused");
        assert_eq!(h.app.state::<PtyState>().pid(), pid);
        panel.close();
    }

    #[test]
    fn pty_close_kills_child() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        assert!(running(pid));
        panel.close();
        assert_eq!(h.app.state::<PtyState>().pid(), None);
        wait_gone(pid);
        assert_eq!(code_of(panel.send(b"x")), "unavailable");
    }

    #[test]
    fn window_destroy_kills_the_session() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        // The mock runtime dispatches no window events, so this calls the handler `run` installs.
        let window = h.window.as_ref().window();
        pty::on_window_event(&window, &WindowEvent::Focused(false));
        assert_eq!(h.app.state::<PtyState>().pid(), Some(pid));
        pty::on_window_event(&window, &WindowEvent::Destroyed);
        wait_gone(pid);
        assert_eq!(h.app.state::<PtyState>().pid(), None);
        drop(panel);
    }

    /// Output belongs to the window that opened the session.
    #[test]
    fn output_goes_only_to_the_window_that_opened_it() {
        let h = ready();
        let other = WebviewWindowBuilder::new(&h.app, "other", Default::default())
            .build()
            .unwrap();
        let leaked = Arc::new(Mutex::new(0));
        for event in ["pty:data", "pty:exit"] {
            let sink = leaked.clone();
            other.listen(event, move |_| *sink.lock().unwrap() += 1);
        }
        let panel = Panel::open(&h);
        panel.line("exit");
        wait_until(|| !panel.exits.lock().unwrap().is_empty());
        assert_eq!(*leaked.lock().unwrap(), 0);
    }

    #[test]
    fn exit_is_reported_and_the_next_open_works() {
        let h = ready();
        let panel = Panel::open(&h);
        panel.line("exit");
        wait_until(|| !panel.exits.lock().unwrap().is_empty());
        assert_eq!(*panel.exits.lock().unwrap(), [json!(0)]);
        assert_eq!(h.app.state::<PtyState>().pid(), None);
        assert_eq!(code_of(panel.send(b"x")), "unavailable");
        let next = Panel::open(&h);
        next.close();
    }

    /// A closed session says nothing, or its exit would end the session after it.
    #[test]
    fn a_closed_session_stays_silent() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.close();
        wait_gone(pid);
        let mark = panel.mark();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(panel.mark(), mark);
        assert!(panel.exits.lock().unwrap().is_empty());
    }

    #[test]
    fn pty_child_env_has_panel_marker_and_home() {
        let h = ready();
        let panel = Panel::open(&h);
        let root = serde_json::to_string(h.paths.root()).unwrap();
        panel.line("jobpilot status --json");
        panel.wait_for(&root);

        let mark = panel.mark();
        panel.line("data delete --confirm DELETE");
        panel.wait_after(mark, "panel_refused");
        assert!(h.paths.root().exists());
    }

    /// With the terminal's foreground job gone, the prompt must still answer.
    #[test]
    fn ctrl_c_at_the_prompt_leaves_the_shell_running() {
        let h = ready();
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.send(b"\x03").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(running(pid));
        let mark = panel.mark();
        panel.line("jobpilot status --json");
        panel.wait_after(mark, "schema");
        assert_eq!(h.app.state::<PtyState>().pid(), Some(pid));
    }

    /// macOS CI lost the interrupt in one run of eight and no run here repeats it. The mock's
    /// trap is set before it says it is ready, so a failure prints who was running and in which
    /// state. Dropped before the panel, which ends them.
    #[cfg(unix)]
    struct GroupTableOnPanic(u32);

    #[cfg(unix)]
    impl Drop for GroupTableOnPanic {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                return;
            }
            let table = std::process::Command::new("ps")
                .args(["-axo", "pid=,pgid=,tpgid=,stat=,command="])
                .output();
            let text = table.map_or(String::new(), |table| {
                String::from_utf8_lossy(&table.stdout).into_owned()
            });
            let group = self.0.to_string();
            for row in text.lines() {
                if row.split_whitespace().nth(1) == Some(group.as_str()) {
                    eprintln!("{row}");
                }
            }
        }
    }

    /// Ctrl-C is a byte until the terminal driver makes it a signal, so this needs the PTY.
    #[cfg(unix)]
    #[test]
    fn ctrl_c_reaches_the_foreground_child_and_the_shell_survives() {
        let h = ready();
        fake_claude(
            &h,
            "trap 'echo got-interrupt; exit 7' INT\necho claude-ready\nwhile :; do sleep 0.05; done",
        );
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        let _table = GroupTableOnPanic(pid);
        panel.line("claude");
        panel.wait_for("claude-ready");
        panel.send(b"\x03").unwrap();
        panel.wait_for("got-interrupt");
        panel.wait_for("exited with code 7");
        let mark = panel.mark();
        panel.line("jobpilot status --json");
        panel.wait_after(mark, "schema");
        assert_eq!(h.app.state::<PtyState>().pid(), Some(pid));
    }

    /// The owner's session: found from the setting, with the terminal and its size.
    #[cfg(unix)]
    #[test]
    fn claude_starts_inside_the_panel_on_its_own_terminal() {
        let h = ready();
        fake_claude(
            &h,
            "test -t 0 && test -t 1 && echo on-a-tty\nstty size\necho \"term=$TERM\"\necho \"leak=[$CARGO_PKG_NAME]\"",
        );
        let panel = Panel::open(&h);
        panel.line("claude");
        panel.wait_for("on-a-tty");
        let (cols, rows) = SIZE;
        panel.wait_for(&format!("{rows} {cols}"));
        panel.wait_for("term=xterm-256color");
        // Cargo set this for the test process, so it stands for all the app's own variables.
        panel.wait_for("leak=[]");
    }

    #[cfg(unix)]
    #[test]
    fn pty_resize_reaches_the_terminal() {
        let h = ready();
        fake_claude(&h, "stty size\nread line\nstty size\n");
        let panel = Panel::open(&h);
        panel.line("claude");
        let (cols, rows) = SIZE;
        panel.wait_for(&format!("{rows} {cols}"));
        invoke(&h.window, "pty_resize", json!({ "cols": 91, "rows": 33 })).unwrap();
        panel.line("");
        panel.wait_for("33 91");
    }

    #[cfg(unix)]
    #[test]
    fn pty_close_takes_a_running_claude_with_it() {
        let h = ready();
        let pid_file = h._tmp.path().join("claude.pid");
        fake_claude(
            &h,
            &format!(
                "echo $$ > {}\necho claude-ready\nexec sleep 300",
                pid_file.display()
            ),
        );
        let panel = Panel::open(&h);
        panel.line("claude");
        panel.wait_for("claude-ready");
        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(running(pid));
        panel.close();
        wait_gone(pid);
    }

    /// `pty_write` runs on the main thread, so a child that stops reading must not stall
    /// the window; and Close must still work with input stuck behind it.
    #[cfg(unix)]
    #[test]
    fn a_child_that_stops_reading_never_blocks_the_caller() {
        let h = ready();
        // Raw, so the driver holds the input: a line-mode terminal drops what overflows.
        fake_claude(&h, "stty raw -echo\necho claude-ready\nexec sleep 300");
        let panel = Panel::open(&h);
        let pid = h.app.state::<PtyState>().pid().unwrap();
        panel.line("claude");
        panel.wait_for("claude-ready");
        let started = Instant::now();
        let refused = (0..1000)
            .map(|_| panel.send(&vec![b'x'; MAX_WRITE_BYTES]))
            .find(Result::is_err);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(code_of(refused.expect("the queue must fill")), "refused");
        panel.close();
        wait_gone(pid);
    }

    /// Every name the webview can call. A new command is added here on purpose: none may
    /// hand back a data key or the bytes of a recovery file.
    const ALLOWED_COMMANDS: &[&str] = &[
        "status",
        "retry_open",
        "documents_list",
        "documents_create",
        "documents_update",
        "documents_delete",
        "documents_search",
        "settings_get",
        "settings_set",
        "backup_create",
        "backup_list",
        "backup_restore",
        "data_export",
        "data_delete",
        "swap_warning_clear",
        "secret_set",
        "secret_delete",
        "secret_names",
        "audit_recent",
        "profile_get",
        "profile_set_basic",
        "profile_set_authorization",
        "profile_remove_authorization",
        "kb_list",
        "kb_add",
        "kb_approve",
        "kb_mark_sensitive",
        "kb_remove",
        "kb_reveal",
        "kb_search",
        "job_paste",
        "job_list",
        "job_get",
        "jobs_list",
        "job_detail",
        "job_import",
        "job_remove",
        "job_split",
        "job_open",
        "job_to_app",
        "job_sources_status",
        "job_sources_list",
        "job_src_add",
        "job_src_remove",
        "jobs_fetch",
        "jobs_analyze",
        "jobs_analyze_plan",
        "job_research",
        "job_research_plan",
        "companies_list",
        "company_note_add",
        "match_prefs_get",
        "match_prefs_set",
        "app_list",
        "app_get",
        "app_add",
        "app_update",
        "app_set_status",
        "app_note",
        "app_history",
        "agents_list",
        "agent_get",
        "agent_cancel",
        "agent_resume",
        "pause_status",
        "pause_set",
        "pause_clear",
        "analytics_run",
        "analytics_latest",
        "app_remove",
        "memory_list",
        "memory_get",
        "memory_add_question",
        "memory_add_answer",
        "memory_edit_answer",
        "memory_approve",
        "memory_mark_reviewed",
        "memory_set_class",
        "memory_reveal",
        "memory_confirm_use",
        "memory_use",
        "memory_application_answers",
        "memory_authorization",
        "memory_remove_question",
        "memory_remove_answer",
        "memory_search",
        "memory_refresh",
        "memory_refresh_plan",
        "resume_import",
        "resume_list",
        "resume_versions",
        "resume_generate",
        "letter_generate",
        "resume_compare",
        "claim_edit",
        "claim_remove",
        "draft_save",
        "resume_export",
        "ai_status",
        "ai_pull",
        "ai_cancel_pull",
        "ai_set_models",
        "ai_set_order",
        "ai_enable",
        "ai_set_claude_path",
        "ai_clear_claude_path",
        "ai_set_cli_path",
        "ai_clear_cli_path",
        "ai_set_ollama_url",
        "ai_set_api_model",
        "ai_set_endpoint",
        "ai_consent_give",
        "ai_consent_revoke",
        "ai_sharing",
        "ai_preview",
        "ai_key_from_clipboard",
        "ai_key_remove",
        "ai_index",
        "ai_cancel",
        "key_state",
        "key_retry",
        "recovery_key_export",
        "recovery_key_import",
        "discard_locked_fields",
        "browser_status",
        "browser_setup",
        "apply_preflight",
        "apply_start",
        "apply_view",
        "apply_latest",
        "apply_resolve_pause",
        "apply_settle",
        "apply_cancel",
        "pty_open",
        "pty_open_apply",
        "pty_write",
        "pty_resize",
        "pty_close",
    ];

    fn registered_commands() -> Vec<String> {
        let source = include_str!("commands.rs");
        let (_, rest) = source.split_once("tauri::generate_handler![").unwrap();
        let (block, _) = rest.split_once(']').unwrap();
        block
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| name.rsplit("::").next().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn no_command_returns_a_key_or_recovery_bytes() {
        let mut names = registered_commands();
        names.sort();
        let mut allowed: Vec<String> = ALLOWED_COMMANDS.iter().map(|n| n.to_string()).collect();
        allowed.sort();
        assert_eq!(names, allowed);

        let receipt = RecoveryReceipt {
            key_id: "id".into(),
            path: "file.json".into(),
        };
        let receipt = serde_json::to_value(receipt).unwrap();
        let mut fields: Vec<&String> = receipt.as_object().unwrap().keys().collect();
        fields.sort();
        assert_eq!(fields, ["key_id", "path"]);
    }

    #[test]
    fn the_webview_gains_no_dialog_capability() {
        let capability: Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let dialogs: Vec<&str> = capability["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| name.starts_with("dialog:"))
            .collect();
        assert_eq!(dialogs, ["dialog:allow-open"]);
    }

    /// Every capability file, as Tauri loads them all, and the config, which can hold one inline.
    #[test]
    fn the_webview_gains_no_clipboard_capability() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = vec![root.join("tauri.conf.json")];
        for entry in fs::read_dir(root.join("capabilities")).unwrap() {
            files.push(entry.unwrap().path());
        }
        assert!(files.len() > 1, "no capability file was read");
        for file in files {
            let text = fs::read_to_string(&file).unwrap().to_lowercase();
            assert!(!text.contains("clipboard"), "{}", file.display());
        }
    }

    /// Fake, and the last four are what a dialog may show of it.
    const SECRET_KEY: &str = "sk-ZXQ-sentinel-WXYZ";
    const SECRET_LAST4: &str = "WXYZ";

    #[test]
    fn clipboard_that_is_not_a_key_stores_nothing_and_echoes_nothing() {
        let h = ready();
        let posting = "Senior Rust engineer wanted, remote, Zebra Labs, apply today";
        let args = json!({ "provider": "openai" });
        h.copy(posting);

        let error = h.call("ai_key_from_clipboard", args.clone()).unwrap_err();

        assert_eq!(error["code"], "validation");
        let text = error.to_string();
        assert!(text.contains("the clipboard does not hold a key"), "{text}");
        for word in posting.split_whitespace() {
            assert!(!text.contains(word), "{word}: {text}");
        }
        assert!(h.asked().is_empty());
        assert_eq!(h.ok("secret_names", json!({})), json!([]));
        assert_eq!(h.clipboard().as_deref(), Some(posting), "left alone");

        *h.app.state::<NativeHook>().clipboard.lock().unwrap() = None;
        assert_eq!(h.code("ai_key_from_clipboard", args), "validation");
        assert!(h.asked().is_empty());
    }

    #[test]
    fn the_dialog_shows_only_the_last_four() {
        let h = ready();
        // A key copied from a web page often brings its line break along.
        let copied = format!("{SECRET_KEY}\n");
        h.copy(&copied);
        h.answer(false);

        let code = h.code("ai_key_from_clipboard", json!({ "provider": "openai" }));

        assert_eq!(code, "confirmation_required");
        let asked = h.asked();
        assert_eq!(
            asked,
            [format!(
                "OpenAI key: Store the key ending in {SECRET_LAST4} as your OpenAI key? It goes \
                 to the system keychain. The clipboard is cleared after."
            )]
        );
        assert!(!asked[0].contains(&SECRET_KEY[..SECRET_KEY.len() - 4]));
        assert_eq!(h.ok("secret_names", json!({})), json!([]));
        assert_eq!(h.clipboard().as_deref(), Some(copied.as_str()));
        let hook = h.app.state::<NativeHook>();
        assert_eq!(*hook.work_at_dialog.lock().unwrap(), [false]);
    }

    #[test]
    fn key_commands_take_no_value_argument() {
        let source = include_str!("commands.rs");
        for command in ["ai_key_from_clipboard", "ai_key_remove"] {
            let (_, rest) = source
                .split_once(&format!("pub async fn {command}<R: Runtime>("))
                .unwrap();
            let (arguments, _) = rest.split_once(") -> ").unwrap();
            let names: Vec<&str> = arguments
                .lines()
                .filter_map(|line| line.split_once(':'))
                .map(|(name, _)| name.trim())
                .collect();
            assert_eq!(names, ["app", "state", "provider"], "{command}");
        }
    }

    #[test]
    fn secret_set_refuses_a_provider_key_name() {
        let h = ready();
        for name in ["ai-key-openai", "AI-KEY-openai"] {
            let set = json!({ "name": name, "value": "sk-from-script" });
            assert_eq!(h.code("secret_set", set), "refused", "{name}");
            assert_eq!(
                h.code("secret_delete", json!({ "name": name })),
                "refused",
                "{name}"
            );
        }
        assert_eq!(h.ok("secret_names", json!({})), json!([]));
    }

    #[test]
    fn a_provider_with_no_key_asks_and_reads_nothing() {
        let h = ready();
        h.copy(SECRET_KEY);
        for provider in ["ollama", "claude_code", "gemini_cli", "codex_cli", "nope"] {
            let args = json!({ "provider": provider });
            assert_eq!(
                h.code("ai_key_from_clipboard", args.clone()),
                "refused",
                "{provider}"
            );
            assert_eq!(h.code("ai_key_remove", args), "refused", "{provider}");
        }
        assert!(h.asked().is_empty());
    }

    #[test]
    fn key_commands_are_unavailable_in_recovery() {
        let h = recovery(OPENS);
        h.copy(SECRET_KEY);
        for command in ["ai_key_from_clipboard", "ai_key_remove"] {
            let args = json!({ "provider": "openai" });
            assert_eq!(h.code(command, args), "unavailable", "{command}");
        }
        assert!(h.asked().is_empty());
    }

    #[test]
    fn key_remove_confirms_natively_and_a_no_keeps_the_key() {
        let h = ready();
        let args = json!({ "provider": "anthropic" });
        assert_eq!(h.code("ai_key_remove", args.clone()), "not_found");
        assert!(h.asked().is_empty(), "nothing to ask about");

        h.list_secret("ai-key-anthropic");
        h.answer(false);
        assert_eq!(h.code("ai_key_remove", args), "confirmation_required");
        assert!(h.asked()[0].contains("Anthropic"), "{:?}", h.asked());
        assert_eq!(h.ok("secret_names", json!({})), json!(["ai-key-anthropic"]));
        let hook = h.app.state::<NativeHook>();
        assert_eq!(*hook.work_at_dialog.lock().unwrap(), [false]);
    }

    #[test]
    fn clear_clipboard_clears_only_what_is_still_there() {
        let h = ready();
        h.copy("copied since");
        let app = h.app.handle();
        block_on(native::clear_clipboard(app, "the key")).unwrap();
        assert_eq!(h.clipboard().as_deref(), Some("copied since"));

        block_on(native::clear_clipboard(app, "copied since")).unwrap();
        assert_eq!(h.clipboard(), None);

        block_on(native::clear_clipboard(app, "copied since")).unwrap();
    }

    #[test]
    fn a_clipboard_that_cannot_be_read_says_so_and_asks_nothing() {
        let h = ready();
        h.copy(SECRET_KEY);
        let hook = h.app.state::<NativeHook>();
        hook.clipboard_fails.store(true, Ordering::SeqCst);

        let args = json!({ "provider": "openai" });
        assert_eq!(h.code("ai_key_from_clipboard", args), "failed");
        assert!(h.asked().is_empty());
    }

    /// The dialog promised a clear, so a key that may still be there must not read as done.
    #[test]
    fn a_failed_read_after_storing_tells_the_person_to_clear_it() {
        let h = ready();
        h.copy(SECRET_KEY);
        let hook = h.app.state::<NativeHook>();
        hook.clipboard_fails.store(true, Ordering::SeqCst);

        let error = block_on(native::clear_clipboard(h.app.handle(), SECRET_KEY)).unwrap_err();

        assert_eq!(error.code(), "failed");
        let text = error.to_string();
        assert!(text.contains("the key is stored"), "{text}");
        assert!(text.contains("Clear it yourself"), "{text}");
        assert!(!text.contains(SECRET_LAST4), "{text}");
    }

    #[test]
    fn the_endpoint_takes_a_key_from_the_clipboard_too() {
        let h = ready();
        h.copy(SECRET_KEY);
        h.answer(false);
        let args = json!({ "provider": "endpoint" });
        assert_eq!(
            h.code("ai_key_from_clipboard", args),
            "confirmation_required"
        );
        assert!(h.asked()[0].contains(SECRET_LAST4), "{:?}", h.asked());
    }

    /// The whole path, so the keychain. `.cargo/config.toml` and CI name throwaway services,
    /// and nothing else runs here, the real one least of all.
    #[test]
    #[ignore = "touches the OS keychain"]
    fn a_pasted_key_is_stored_and_the_clipboard_cleared() {
        let service = std::env::var("JOBPILOT_SECRET_SERVICE").unwrap();
        assert!(
            matches!(service.as_str(), "jobpilot-test" | "jobpilot-ci"),
            "{service}"
        );
        let h = ready();
        let args = json!({ "provider": "openai" });

        h.copy(&format!("{SECRET_KEY}\n"));
        h.ok("ai_key_from_clipboard", args.clone());
        assert_eq!(h.ok("secret_names", json!({})), json!(["ai-key-openai"]));
        assert_eq!(h.clipboard(), None, "cleared, line break and all");
        assert_eq!(
            secrets::get("ai-key-openai").unwrap().as_deref(),
            Some(SECRET_KEY)
        );

        // Copied over while the dialog was open: stored as shown, and the new text stays.
        h.copy(SECRET_KEY);
        let hook = h.app.state::<NativeHook>();
        let app = h.app.handle().clone();
        *hook.on_ask.lock().unwrap() = Some(Box::new(move || {
            let app = app.clone();
            Box::pin(async move {
                let hook = app.state::<NativeHook>();
                *hook.clipboard.lock().unwrap() = Some("copied since".into());
            })
        }));
        h.ok("ai_key_from_clipboard", args.clone());
        assert_eq!(h.clipboard().as_deref(), Some("copied since"));

        *hook.on_ask.lock().unwrap() = None;
        h.ok("ai_key_remove", args);
        assert_eq!(h.ok("secret_names", json!({})), json!([]));
        assert_eq!(secrets::get("ai-key-openai").unwrap(), None);
    }

    #[test]
    fn new_data_commands_return_unavailable_in_recovery() {
        let h = recovery(OPENS);
        let commands = [
            "profile_get",
            "kb_list",
            "job_list",
            "resume_import",
            "ai_status",
            "ai_index",
            "key_state",
            "key_retry",
            "recovery_key_export",
            "recovery_key_import",
            "discard_locked_fields",
            "swap_warning_clear",
        ];
        for command in commands {
            assert_eq!(h.code(command, json!({})), "unavailable", "{command}");
        }
        assert!(h.asked().is_empty(), "no dialog for what cannot run");
    }

    #[test]
    fn profile_commands_reach_core() {
        let h = ready();
        let basic = json!({ "name": "Ada", "email": "ada@example.com", "countries": ["pt"] });

        let saved = h.ok("profile_set_basic", json!({ "basic": basic }));

        assert_eq!(saved["countries"], json!(["PT"]));
        let profile = h.ok("profile_get", json!({}));
        assert_eq!(profile["basic"]["name"], "Ada");
        assert_eq!(profile["basic"]["countries"], json!(["PT"]));
        assert_eq!(profile["authorization"], json!([]));
        assert_eq!(profile["locked"], false);
    }

    #[test]
    fn reading_never_makes_a_key() {
        let h = ready();

        h.ok("profile_get", json!({}));
        h.ok("kb_list", json!({}));
        let state = h.ok("key_state", json!({}));

        assert_eq!(state["state"], "key_unavailable");
        assert_eq!(state["reason"], crypto::NO_KEY_YET);
        assert_eq!(h.status()["key_state"], state);
    }

    #[test]
    fn kb_search_falls_back_to_text_without_embeddings() {
        let h = ready();
        let item = h.ok(
            "kb_add",
            json!({ "kind": "skill", "body": "Rust and SQLite" }),
        );
        h.ok("kb_add", json!({ "kind": "skill", "body": "Gardening" }));

        let hits = h.ok("kb_search", json!({ "query": "sqlite", "limit": 5 }));

        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["item_id"], item["id"]);
    }

    #[test]
    fn kb_commands_reach_core() {
        let h = ready();
        let item = h.ok("kb_add", json!({ "kind": "note", "body": "Shipped it" }));
        let id = item["id"].as_str().unwrap();

        h.ok("kb_approve", json!({ "id": id, "approved": false }));
        assert_eq!(h.ok("kb_list", json!({}))[0]["approved"], false);
        h.ok("kb_remove", json!({ "id": id }));

        assert_eq!(h.ok("kb_list", json!({})), json!([]));
    }

    /// A marked row naming a key this keychain never held, so nothing here asks a keychain.
    fn seal_without_a_key(h: &Harness, id: &str) {
        block_on(async {
            let slot = h.app.state::<AppState>();
            let slot = slot.db.read().await;
            let mut connection = slot.as_ref().unwrap().pool.acquire().await.unwrap();
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO knowledge_items (id, kind, body, body_enc, key_id, source, \
                 approved, marked_sensitive, created_at, updated_at) \
                 VALUES (?, 'note', '', x'00', 'gone', 'user', 0, 1, 't', 't')",
            )
            .bind(id)
            .execute(&mut *connection)
            .await
            .unwrap();
        });
    }

    #[test]
    fn kb_reveal_of_a_sealed_item_needs_a_native_yes_every_time() {
        let h = ready();
        seal_without_a_key(&h, "s1");
        assert_eq!(h.ok("kb_list", json!({}))[0]["sealed"], true);

        h.answer(false);
        assert_eq!(
            h.code("kb_reveal", json!({ "id": "s1" })),
            "confirmation_required"
        );
        assert_eq!(h.asked().len(), 1);
        assert!(h.asked()[0].contains("s1"), "{:?}", h.asked());

        // A yes opens the dialog again and reaches the key, which this keychain lacks.
        h.answer(true);
        assert_eq!(h.code("kb_reveal", json!({ "id": "s1" })), "key_missing");
        assert_eq!(h.asked().len(), 2, "one dialog per reveal");
    }

    #[test]
    fn kb_reveal_of_a_plain_item_or_an_unknown_id_asks_nothing() {
        let h = ready();
        let item = h.ok("kb_add", json!({ "kind": "note", "body": "Shipped it" }));

        let body = h.ok("kb_reveal", json!({ "id": item["id"] }));

        assert_eq!(body, "Shipped it");
        assert_eq!(h.code("kb_reveal", json!({ "id": "nope" })), "not_found");
        assert!(h.asked().is_empty());
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn kb_reveal_shows_a_sealed_body_after_a_yes() {
        let h = ready();
        let item = h.ok("kb_add", json!({ "kind": "note", "body": "ZXQ-SEALED" }));
        let id = item["id"].as_str().unwrap();
        h.ok("kb_mark_sensitive", json!({ "id": id, "marked": true }));
        assert_eq!(h.ok("kb_list", json!({}))[0]["body"], "");

        h.answer(false);
        assert_eq!(
            h.code("kb_reveal", json!({ "id": id })),
            "confirmation_required"
        );
        h.answer(true);
        assert_eq!(h.ok("kb_reveal", json!({ "id": id })), "ZXQ-SEALED");
        assert!(!h.asked().iter().any(|m| m.contains("ZXQ-SEALED")));
        h.ok("data_delete", delete("DELETE", true));
    }

    #[test]
    fn job_commands_reach_core() {
        let h = ready();
        let job = h.ok(
            "job_paste",
            json!({ "title": "Role", "raw": "<b>Build</b>" }),
        );
        let id = job["id"].as_str().unwrap();

        assert_eq!(h.ok("job_get", json!({ "id": id }))["normalized"], "Build");
        assert_eq!(h.ok("job_list", json!({}))[0]["id"], id);
    }

    #[test]
    fn resume_generate_with_no_ai_is_manual_required() {
        let h = ready();
        h.ok(
            "ai_enable",
            json!({ "provider": "ollama", "enabled": false }),
        );
        h.ok("kb_add", json!({ "kind": "skill", "body": "Rust" }));
        let job = h.ok("job_paste", json!({ "title": "Role", "raw": "Build" }));
        let args = json!({ "jobTextId": job["id"] });

        assert_eq!(h.code("resume_generate", args.clone()), "manual_required");
        assert_eq!(h.code("letter_generate", args), "manual_required");

        // The slot is free again once a task has ended.
        let args = json!({ "jobTextId": job["id"] });
        assert_eq!(h.code("resume_generate", args), "manual_required");
    }

    #[test]
    fn draft_save_stores_an_edited_version_with_no_ai() {
        let h = ready();
        let doc = json!({ "name": "Ada", "summary": ["Builds things"] });

        let draft = h.ok(
            "draft_save",
            json!({ "kind": "resume", "lineageId": null, "doc": doc }),
        );

        assert_eq!(draft["origin"], "edited");
        let list = h.ok("resume_list", json!({ "kind": "resume" }));
        assert_eq!(list[0]["id"], draft["id"]);
        let versions = h.ok(
            "resume_versions",
            json!({ "lineageId": draft["lineage_id"] }),
        );
        assert_eq!(versions.as_array().unwrap().len(), 1);
        let path = h.ok(
            "resume_export",
            json!({ "draftId": draft["id"], "template": "classic", "format": "pdf" }),
        );
        let path = Path::new(path.as_str().unwrap());
        assert!(path.is_file());
        assert!(path.starts_with(h.paths.exports()));
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../crates/core/tests/fixtures")
            .join(name)
    }

    #[test]
    fn resume_import_through_the_picker_hook_reaches_compare() {
        let h = ready();
        h.pick(&fixture("resume.docx"));

        let imported = h.ok("resume_import", json!({}));

        let args = json!({ "generatedId": imported["draft_id"], "originalId": null });
        let compared = h.ok("resume_compare", args);
        assert!(!compared["original"].as_array().unwrap().is_empty());
        let drafts = h.ok("resume_list", json!({ "kind": null }));
        assert_eq!(drafts[0]["title"], "resume");
    }

    #[test]
    fn resume_import_takes_no_path_from_the_webview() {
        let h = ready();
        let args = json!({ "path": fixture("resume.docx") });

        assert_eq!(h.code("resume_import", args), "cancelled");

        assert_eq!(h.ok("resume_list", json!({ "kind": null })), json!([]));
    }

    #[test]
    fn ai_status_reports_not_running_and_no_ai_keeps_working() {
        let h = ready();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        h.ok("ai_set_ollama_url", json!({ "url": url }));

        let status = h.ok("ai_status", json!({}));

        assert_eq!(status["ollama"]["state"], "not_running");
        h.assert_ready();
        h.ok(
            "draft_save",
            json!({ "kind": "resume", "lineageId": null, "doc": {} }),
        );
    }

    #[test]
    fn ai_settings_commands_reach_core() {
        let h = ready();
        let models = json!({ "drafting": "llama3", "embedding": null });
        h.ok("ai_set_models", models);
        h.ok(
            "ai_set_order",
            json!({ "order": ["claude_code", "ollama"] }),
        );
        h.ok(
            "ai_enable",
            json!({ "provider": "claude_code", "enabled": true }),
        );

        assert_eq!(
            h.ok("settings_get", json!({ "key": "ai.drafting_model" })),
            "llama3"
        );
        assert_eq!(
            h.ok("settings_get", json!({ "key": "ai.order" })),
            json!(["claude_code", "ollama"])
        );
        let none = json!({ "drafting": null, "embedding": null });
        assert_eq!(h.code("ai_set_models", none), "validation");
        let unknown = json!({ "provider": "gemini", "enabled": true });
        assert_eq!(h.code("ai_enable", unknown), "validation");
    }

    #[test]
    fn settings_set_refuses_what_decides_what_runs() {
        let h = ready();
        for key in [
            "ai.claude_path",
            "ai.gemini_path",
            "ai.codex_path",
            "ai.ollama_url",
            "ai.enabled",
            "ai.order",
            "ai.drafting_model",
            "ai.embedding_model",
            "ai.model.openai",
            "ai.endpoint.url",
            "ai.anything_later",
            "recovery_key_exported_0123456789abcdef",
        ] {
            let args = json!({ "key": key, "value": "/tmp/evil/claude" });
            assert_eq!(h.code("settings_set", args), "refused", "{key}");
            assert_eq!(h.ok("settings_get", json!({ "key": key })), Value::Null);
        }
        h.ok("settings_set", json!({ "key": "theme", "value": "dark" }));
    }

    #[test]
    fn the_claude_path_comes_from_the_native_picker_only() {
        let h = ready();
        let key = json!({ "key": "ai.claude_path" });

        // The picker is closed: nothing the webview sends can stand in for it.
        let args = json!({ "path": "/tmp/evil/claude" });
        assert_eq!(h.code("ai_set_claude_path", args), "cancelled");
        assert_eq!(h.ok("settings_get", key.clone()), Value::Null);

        h.pick(&h._tmp.path().join("not-it"));
        assert_eq!(h.code("ai_set_claude_path", json!({})), "validation");
        assert_eq!(h.ok("settings_get", key.clone()), Value::Null);

        let claude = h._tmp.path().join("claude");
        h.pick(&claude);
        h.ok("ai_set_claude_path", json!({}));
        assert_eq!(h.ok("settings_get", key), json!(claude));
    }

    #[test]
    fn clearing_the_claude_path_is_natively_confirmed() {
        let h = ready();
        let key = json!({ "key": "ai.claude_path" });
        let claude = h._tmp.path().join("claude");
        h.pick(&claude);
        h.ok("ai_set_claude_path", json!({}));

        h.answer(false);
        let code = h.code("ai_clear_claude_path", json!({}));
        assert_eq!(code, "confirmation_required");
        assert!(h.asked().last().unwrap().contains(claude.to_str().unwrap()));
        assert_eq!(h.ok("settings_get", key.clone()), json!(claude));

        h.answer(true);
        h.ok("ai_clear_claude_path", json!({}));
        assert_eq!(h.ok("settings_get", key), "");
    }

    #[test]
    fn cli_picker_refuses_another_program() {
        let h = ready();
        let key = |provider: &str| {
            let setting = if provider == "gemini_cli" {
                "ai.gemini_path"
            } else {
                "ai.codex_path"
            };
            json!({ "key": setting })
        };
        // The picker is closed: no path from the webview stands in for it.
        let args = json!({ "provider": "gemini_cli", "path": "/tmp/evil/gemini" });
        assert_eq!(h.code("ai_set_cli_path", args), "cancelled");

        for (provider, name) in [
            ("gemini_cli", "codex"),
            ("gemini_cli", "sh"),
            ("gemini_cli", "gemini-evil"),
            ("codex_cli", "gemini"),
        ] {
            h.pick(&h._tmp.path().join(name));
            assert_eq!(
                h.code("ai_set_cli_path", json!({ "provider": provider })),
                "validation",
                "{provider} {name}"
            );
            assert_eq!(h.ok("settings_get", key(provider)), Value::Null);
        }
        // Not a program provider at all: refused before any picker opens.
        h.pick(&h._tmp.path().join("claude"));
        assert_eq!(
            h.code("ai_set_cli_path", json!({ "provider": "claude_code" })),
            "validation"
        );
        assert_eq!(
            h.code("ai_clear_cli_path", json!({ "provider": "openai" })),
            "validation"
        );
    }

    /// Every provider off is a state the app runs in, not an error: the screen loads, a task
    /// ends on the manual path, and nothing is sent.
    #[test]
    fn no_provider_screen_is_a_normal_state() {
        let h = ready();
        // A setting that names another program stops discovery: no installed `claude` runs.
        with_db(&h, |db| async move {
            let claude = json!("/bin/sh");
            jobpilot_core::settings::set(&db, "ai.claude_path", &claude)
                .await
                .unwrap();
        });
        h.ok(
            "ai_enable",
            json!({ "provider": "ollama", "enabled": false }),
        );

        let status = h.ok("ai_status", json!({}));
        let enabled = status["enabled"].as_object().unwrap();
        assert!(enabled.values().all(|on| on == false), "{status}");
        h.assert_ready();

        let job = h.ok(
            "job_paste",
            json!({ "title": "Rust role", "raw": "Write Rust" }),
        );
        h.ok(
            "kb_add",
            json!({ "kind": "experience", "body": "Shipped the Rust compiler" }),
        );
        let error = h
            .call(
                "resume_generate",
                json!({ "jobTextId": job["id"], "export": null }),
            )
            .unwrap_err();
        assert_eq!(error["code"], "manual_required", "{error}");
        assert!(
            error["message"].as_str().unwrap().contains("no_ai"),
            "{error}"
        );
        h.assert_ready();
        h.ok(
            "draft_save",
            json!({ "kind": "resume", "lineageId": null, "doc": {} }),
        );

        let calls: i64 = with_db(&h, |db| async move {
            sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'ai_external_call'")
                .fetch_one(&db.pool)
                .await
                .unwrap()
        });
        assert_eq!(calls, 0);
    }

    /// Off, so found and not started, and a posting can go to it: it declares no tools.
    #[test]
    fn ai_status_lists_gemini_with_its_path() {
        let h = ready();
        {
            // A setting that names another program stops discovery: no installed `claude` runs.
            let state = h.app.state::<AppState>();
            let db = block_on(state.db.read());
            let claude = json!("/bin/sh");
            let set = jobpilot_core::settings::set(db.as_ref().unwrap(), "ai.claude_path", &claude);
            block_on(set).unwrap();
        }
        let mock = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../crates/core/tests/mock-gemini")
            .join(if cfg!(windows) {
                "gemini.cmd"
            } else {
                "gemini"
            });
        h.pick(&mock);
        h.ok("ai_set_cli_path", json!({ "provider": "gemini_cli" }));

        let status = h.ok("ai_status", json!({}));
        let rows = status["external"].as_array().unwrap();
        let row = rows.iter().find(|row| row["name"] == "gemini_cli").unwrap();
        assert_eq!(row["path"], json!(mock));
        assert_eq!(row["available"], true);
        assert_eq!(row["reason"], Value::Null);
        assert_eq!(row["runs_job_text"], true);
    }

    /// Found and not started, and the row says it runs no job text: no tools are proven.
    #[test]
    fn ai_status_says_codex_runs_no_job_text() {
        let h = ready();
        {
            // A setting that names another program stops discovery: no installed `claude` runs.
            let state = h.app.state::<AppState>();
            let db = block_on(state.db.read());
            let claude = json!("/bin/sh");
            let set = jobpilot_core::settings::set(db.as_ref().unwrap(), "ai.claude_path", &claude);
            block_on(set).unwrap();
        }
        let mock = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../crates/core/tests/mock-codex")
            .join(if cfg!(windows) { "codex.cmd" } else { "codex" });
        h.pick(&mock);
        h.ok("ai_set_cli_path", json!({ "provider": "codex_cli" }));

        let status = h.ok("ai_status", json!({}));
        let rows = status["external"].as_array().unwrap();
        let row = rows.iter().find(|row| row["name"] == "codex_cli").unwrap();
        assert_eq!(row["path"], json!(mock));
        assert_eq!(row["available"], true);
        assert_eq!(row["runs_job_text"], false);
        let gemini = rows.iter().find(|row| row["name"] == "gemini_cli").unwrap();
        assert_eq!(gemini["runs_job_text"], true);
    }

    #[test]
    fn clearing_a_cli_path_that_is_not_set_asks_nothing() {
        let h = ready();
        // A refusing dialog would fail the call; there is nothing to confirm.
        h.answer(false);
        h.ok("ai_clear_cli_path", json!({ "provider": "codex_cli" }));
        assert!(h.asked().is_empty());
    }

    /// What the macOS panel returns for npm's `gemini` link. Windows resolves no link: its npm
    /// folder holds `gemini.cmd`, a `.ps1`, and an sh script, and only the `.cmd` can start.
    #[test]
    fn cli_picker_accepts_a_resolved_npm_entry() {
        let h = ready();
        let args = json!({ "provider": "gemini_cli" });
        let key = json!({ "key": "ai.gemini_path" });
        let entry = h
            ._tmp
            .path()
            .join("lib/node_modules/@google/gemini-cli/bundle/gemini.js");
        let plain = if cfg!(windows) {
            for name in ["gemini", "gemini.ps1"] {
                h.pick(&h._tmp.path().join(name));
                let error = h.call("ai_set_cli_path", args.clone()).unwrap_err();
                assert_eq!(error["code"], "validation", "{name}");
                let message = error["message"].as_str().unwrap();
                assert!(message.contains("gemini.cmd"), "{message}");
            }
            h.pick(&entry);
            assert_eq!(h.code("ai_set_cli_path", args.clone()), "validation");
            assert_eq!(h.ok("settings_get", key.clone()), Value::Null);
            h._tmp.path().join("gemini.cmd")
        } else {
            h.pick(&entry);
            h.ok("ai_set_cli_path", args.clone());
            assert_eq!(h.ok("settings_get", key.clone()), json!(entry));
            h._tmp.path().join("gemini")
        };

        h.pick(&plain);
        h.ok("ai_set_cli_path", json!({ "provider": "gemini_cli" }));
        assert_eq!(h.ok("settings_get", key.clone()), json!(plain));

        // Clearing changes what runs, so a native dialog confirms it.
        h.answer(false);
        let code = h.code("ai_clear_cli_path", json!({ "provider": "gemini_cli" }));
        assert_eq!(code, "confirmation_required");
        assert!(h.asked().last().unwrap().contains(plain.to_str().unwrap()));
        assert_eq!(h.ok("settings_get", key.clone()), json!(plain));
        h.answer(true);
        h.ok("ai_clear_cli_path", json!({ "provider": "gemini_cli" }));
        assert_eq!(h.ok("settings_get", key), "");
    }

    /// What the macOS panel returns for the installer's `claude` link.
    #[test]
    fn a_picked_release_of_claude_is_stored() {
        let h = ready();
        let release = h._tmp.path().join("claude/versions/2.1.316");
        h.pick(&release);

        h.ok("ai_set_claude_path", json!({}));

        let key = json!({ "key": "ai.claude_path" });
        assert_eq!(h.ok("settings_get", key), json!(release));
    }

    #[cfg(unix)]
    #[test]
    fn a_picked_path_that_is_not_text_is_refused_without_a_panic() {
        use std::os::unix::ffi::OsStrExt;
        let h = ready();
        let dir = std::ffi::OsStr::from_bytes(b"/tmp/\xff");

        h.pick(&Path::new(dir).join("claude"));

        assert_eq!(h.code("ai_set_claude_path", json!({})), "validation");
    }

    #[test]
    fn recovery_import_with_nothing_to_import_opens_no_picker() {
        let h = ready();
        let junk = h._tmp.path().join("junk.json");
        fs::write(&junk, "{}").unwrap();
        h.pick(&junk);

        assert_eq!(h.code("recovery_key_import", json!({})), "refused");

        assert!(h.asked().is_empty());
    }

    #[test]
    fn the_ollama_address_is_loopback_and_natively_confirmed() {
        let h = ready();
        let key = json!({ "key": "ai.ollama_url" });
        let args = json!({ "url": "http://example.com:11434" });

        assert_eq!(h.code("ai_set_ollama_url", args), "refused");
        assert!(h.asked().is_empty(), "a refused address asks nothing");

        h.answer(false);
        let args = json!({ "url": "http://127.0.0.1:11500" });
        assert_eq!(h.code("ai_set_ollama_url", args), "confirmation_required");
        assert!(h.asked()[0].contains("http://127.0.0.1:11500"));
        assert_eq!(h.ok("settings_get", key), Value::Null);
    }

    #[test]
    fn ai_set_endpoint_refuses_before_any_dialog() {
        let h = ready();
        let key = json!({ "key": "ai.endpoint.url" });
        for url in [
            "https://api.example.com/v1",
            "http://192.168.1.2:1234/v1",
            "https://127.0.0.1:1234/v1",
        ] {
            let args = json!({ "url": url });
            assert_eq!(h.code("ai_set_endpoint", args), "refused", "{url}");
        }
        for url in [
            "http://user:secret@127.0.0.1:1234/v1",
            "http://127.0.0.1:1234/v1?key=x",
            "http://127.0.0.1:1234/sk-secret/v1",
        ] {
            let args = json!({ "url": url });
            assert_eq!(h.code("ai_set_endpoint", args), "validation", "{url}");
        }
        assert!(h.asked().is_empty(), "a refused address asks nothing");
        assert_eq!(h.ok("settings_get", key), Value::Null);
    }

    #[test]
    fn the_endpoint_address_is_natively_confirmed_and_reaches_status() {
        let h = ready();
        let key = json!({ "key": "ai.endpoint.url" });
        let args = json!({ "url": "http://127.0.0.1:1234/v1/" });

        h.answer(false);
        assert_eq!(
            h.code("ai_set_endpoint", args.clone()),
            "confirmation_required"
        );
        // The stored form, and what a change costs.
        assert!(
            h.asked()[0].contains("http://127.0.0.1:1234/v1\n"),
            "{:?}",
            h.asked()
        );
        assert!(!h.asked()[0].contains("/v1/"), "{:?}", h.asked());
        assert!(h.asked()[0].contains("consent"), "{:?}", h.asked());
        assert_eq!(h.ok("settings_get", key.clone()), Value::Null);

        h.answer(true);
        h.ok("ai_set_endpoint", args);
        assert_eq!(
            h.ok("settings_get", key.clone()),
            "http://127.0.0.1:1234/v1"
        );
        let row = |status: &Value| {
            let rows = status["external"].as_array().unwrap();
            let row = rows.iter().find(|row| row["name"] == "endpoint");
            row.unwrap().clone()
        };
        let status = h.ok("ai_status", json!({}));
        assert_eq!(row(&status)["path"], "http://127.0.0.1:1234/v1");
        assert_eq!(row(&status)["reason"], "no_api_model");

        let shown = h.asked().len();
        let give = json!({ "provider": "endpoint", "categories": ["job_text"] });
        h.ok("ai_consent_give", give);
        let asked = h.asked();
        assert!(
            asked[shown].contains("Address: http://127.0.0.1:1234/v1"),
            "{asked:?}"
        );
        assert!(!asked[shown].contains("Program:"), "{asked:?}");

        h.answer(false);
        let clear = json!({ "url": "" });
        assert_eq!(
            h.code("ai_set_endpoint", clear.clone()),
            "confirmation_required"
        );
        h.answer(true);
        h.ok("ai_set_endpoint", clear);
        let status = h.ok("ai_status", json!({}));
        assert_eq!(row(&status)["reason"], "no_endpoint");
    }

    #[test]
    fn api_model_reaches_core() {
        let h = ready();
        let key = json!({ "key": "ai.model.openrouter" });
        let args = json!({ "provider": "openrouter", "model": "anthropic/claude-x" });

        h.ok("ai_set_api_model", args);

        assert_eq!(h.ok("settings_get", key.clone()), "anthropic/claude-x");
        assert!(h.asked().is_empty(), "the consent record is the gate");
        let bad = json!({ "provider": "openrouter", "model": "meta-llama/llama-x" });
        let error = h.call("ai_set_api_model", bad).unwrap_err();
        assert_eq!(error["code"], "refused");
        assert_eq!(error["message"], "refused: origin_refused");
        assert_eq!(h.ok("settings_get", key), "anthropic/claude-x");
        let cli = json!({ "provider": "gemini_cli", "model": "m" });
        assert_eq!(h.code("ai_set_api_model", cli), "validation");
        for provider in ["anthropic", "google"] {
            h.ok(
                "ai_set_api_model",
                json!({ "provider": provider, "model": "m-1" }),
            );
            let key = json!({ "key": format!("ai.model.{provider}") });
            assert_eq!(h.ok("settings_get", key), "m-1");
        }
        let path = json!({ "provider": "google", "model": "models/x" });
        assert_eq!(h.code("ai_set_api_model", path), "validation");
    }

    #[test]
    fn consent_is_natively_confirmed_and_names_its_categories() {
        let h = ready();
        let args = json!({ "provider": "claude_code", "categories": ["job_text"] });

        h.answer(false);
        assert_eq!(
            h.code("ai_consent_give", args.clone()),
            "confirmation_required"
        );
        assert!(h.asked()[0].contains("job_text"));
        assert_eq!(
            h.ok("ai_status", json!({}))["claude_code"]["consent"],
            Value::Null
        );

        h.answer(true);
        h.ok("ai_consent_give", args);
        assert_eq!(
            h.ok("ai_status", json!({}))["claude_code"]["consent"],
            json!(["job_text"])
        );

        h.ok("ai_consent_revoke", json!({ "provider": "claude_code" }));
        assert_eq!(
            h.ok("ai_status", json!({}))["claude_code"]["consent"],
            Value::Null
        );
        let audit = h.ok("audit_recent", json!({ "limit": 2 }));
        assert_eq!(audit[0]["action"], "consent_revoked");
        assert_eq!(audit[1]["action"], "consent_given");
        assert_eq!(audit[1]["actor"], "gui");

        let other = json!({ "provider": "ollama", "categories": ["job_text"] });
        assert_eq!(h.code("ai_consent_give", other), "validation");
        let other = json!({ "provider": "ollama" });
        assert_eq!(h.code("ai_consent_revoke", other), "validation");
    }

    #[test]
    fn consent_dialog_names_data_use_and_attestation() {
        let h = ready();
        let args = json!({ "provider": "google", "categories": ["job_text"] });

        h.answer(false);
        assert_eq!(
            h.code("ai_consent_give", args.clone()),
            "confirmation_required"
        );
        let asked = h.asked().remove(0);
        assert!(asked.contains("Google Gemini"), "{asked}");
        assert!(asked.contains("job_text"), "{asked}");
        assert!(
            asked.contains("generativelanguage.googleapis.com"),
            "{asked}"
        );
        assert!(asked.contains("logged for a limited period"), "{asked}");
        assert!(asked.contains("billing on"), "{asked}");

        h.answer(true);
        h.ok("ai_consent_give", args);
        let sharing = h.ok("ai_sharing", json!({ "provider": "google" }));
        assert_eq!(sharing["given"], json!(["job_text"]));
        assert_eq!(sharing["suspended"], false);
        assert_eq!(sharing["terms_date"], "2026-11-01");
        h.ok("ai_consent_revoke", json!({ "provider": "google" }));
        let sharing = h.ok("ai_sharing", json!({ "provider": "google" }));
        assert_eq!(sharing["given"], Value::Null);
    }

    #[test]
    fn a_left_out_provider_is_refused_at_ipc() {
        let h = ready();
        h.answer(true);
        for name in ["vertex_ai", "antigravity", "ollama", "gemini_free"] {
            let give = json!({ "provider": name, "categories": ["job_text"] });
            assert_eq!(h.code("ai_consent_give", give), "validation", "{name}");
            let one = json!({ "provider": name });
            assert_eq!(
                h.code("ai_consent_revoke", one.clone()),
                "validation",
                "{name}"
            );
            assert_eq!(h.code("ai_sharing", one), "validation", "{name}");
        }
        for name in ["vertex_ai", "antigravity"] {
            let args = json!({ "provider": name, "enabled": true });
            assert_eq!(h.code("ai_enable", args), "validation", "{name}");
        }
        assert!(h.asked().is_empty(), "nothing to ask about");
    }

    #[test]
    fn ai_preview_shows_the_prompt_and_sends_nothing() {
        let h = ready();
        let job = h.ok(
            "job_paste",
            json!({ "title": "Role", "raw": "ZXQ-POSTING <script>x</script>" }),
        );
        h.ok(
            "kb_add",
            json!({ "kind": "note", "body": "Shipped a compiler" }),
        );
        let args = json!({ "jobTextId": job["id"], "provider": "claude_code" });
        let preview = h.ok("ai_preview", args);
        assert_eq!(preview["task"], "draft_resume");
        assert!(preview["user"].as_str().unwrap().contains("ZXQ-POSTING"));
        assert!(
            preview["system"]
                .as_str()
                .unwrap()
                .contains("Never fabricate")
        );
    }

    #[test]
    fn ai_cancels_with_nothing_running_do_nothing() {
        let h = ready();
        h.ok("ai_cancel", json!({}));
        h.ok("ai_cancel_pull", json!({}));
    }

    #[test]
    fn data_delete_declined_in_the_native_dialog_changes_nothing() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        h.answer(false);

        assert_eq!(
            h.code("data_delete", delete("DELETE", true)),
            "confirmation_required"
        );

        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
        assert!(h.asked()[0].contains(h.paths.root().to_str().unwrap()));
    }

    #[test]
    fn restore_declined_in_the_native_dialog_changes_nothing() {
        let h = ready();
        h.ok("documents_create", json!({ "title": "live", "body": "" }));
        let source = backup_holding(&h._tmp, "restored");
        h.answer(false);

        assert_eq!(
            h.code("backup_restore", restore(&source)),
            "confirmation_required"
        );

        h.assert_ready();
        assert_eq!(h.titles(), ["live"]);
        assert_eq!(h.snapshots(), 0, "no pre-restore snapshot was written");
        assert!(h.asked()[0].contains(source.to_str().unwrap()));
    }

    #[test]
    fn a_refused_offline_restore_opens_no_dialog() {
        let h = recovery("in_use");
        let source = backup_holding(&h._tmp, "restored");

        assert_eq!(h.code("backup_restore", restore(&source)), "in_use");

        assert!(h.asked().is_empty());
    }

    #[test]
    fn status_carries_key_state_and_swap_flag() {
        let h = ready();

        let status = h.status();
        assert_eq!(status["key_state"]["reason"], crypto::NO_KEY_YET);
        assert_eq!(status["swap_interrupted"], false);
        assert_eq!(status["recovery_key_exported"], true, "nothing to export");
        assert_eq!(status["restored_key_state"], Value::Null);

        fs::write(h.paths.swap_marker(), "").unwrap();
        assert_eq!(h.status()["swap_interrupted"], true);
        h.ok("swap_warning_clear", json!({}));
        assert_eq!(h.status()["swap_interrupted"], false);

        // A backup with no key row reports nothing about a key.
        let source = backup_holding(&h._tmp, "restored");
        let status = h.ok("backup_restore", restore(&source));
        assert_eq!(status["state"], "ready");
        assert_eq!(status["restored_key_state"], Value::Null);

        let status = recovery(OPENS).status();
        assert_eq!(status["key_state"], Value::Null);
    }

    #[test]
    fn recovery_key_export_with_no_key_is_refused_before_any_dialog() {
        let h = ready();
        h.pick(&h._tmp.path().join("key.json"));

        assert_eq!(h.code("recovery_key_export", json!({})), "refused");

        assert!(h.asked().is_empty());
        assert!(!h._tmp.path().join("key.json").exists());
    }

    /// A key this keychain has never held, with a row that names it.
    const LOST_KEY: &str = "0123456789abcdef";

    fn lose_a_key(h: &Harness) {
        block_on(async {
            let slot = h.app.state::<AppState>();
            let slot = slot.db.read().await;
            sqlx::query(
                "INSERT INTO data_keys (key_id, created_at, key_check, active) \
                 VALUES (?, '2026-11-01T00:00:00.000Z', x'00', 1)",
            )
            .bind(LOST_KEY)
            .execute(&slot.as_ref().unwrap().pool)
            .await
            .unwrap();
        });
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn discard_requires_native_confirmation() {
        let h = ready();
        lose_a_key(&h);
        h.answer(false);

        let code = h.code("discard_locked_fields", json!({}));

        assert_eq!(code, "confirmation_required");
        assert!(h.asked()[0].contains(LOST_KEY), "{:?}", h.asked());
        assert_eq!(h.snapshots(), 0, "nothing changed");
        assert_eq!(h.ok("key_state", json!({}))["key_id"], LOST_KEY);
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn discard_after_confirmation_replaces_the_lost_key() {
        let h = ready();
        lose_a_key(&h);
        let info = h.ok("backup_create", json!({}));
        let source = PathBuf::from(info["path"].as_str().unwrap());
        let restored = h.ok("backup_restore", restore(&source));
        assert_eq!(restored["restored_key_state"]["state"], "key_missing");

        let state = h.ok("discard_locked_fields", json!({}));

        assert_eq!(state["state"], "unlocked");
        assert_ne!(state["key_id"], LOST_KEY);
        assert_eq!(h.status()["key_state"], state);
        assert_eq!(h.status()["restored_key_state"], Value::Null);
        h.ok("data_delete", delete("DELETE", true));
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn discard_of_a_readable_key_is_refused_before_any_dialog() {
        let h = ready();
        h.ok("key_retry", json!({}));

        assert_eq!(h.code("discard_locked_fields", json!({})), "refused");

        assert!(h.asked().is_empty());
        h.ok("data_delete", delete("DELETE", true));
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn recovery_import_asks_the_store_before_reading_the_file() {
        let h = ready();
        lose_a_key(&h);
        let junk = h._tmp.path().join("junk.json");
        fs::write(&junk, "{}").unwrap();
        h.pick(&junk);

        // Past the "not been checked yet" refusal, so the file itself was read.
        assert_eq!(h.code("recovery_key_import", json!({})), "validation");
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn recovery_key_export_stays_out_of_the_root_and_returns_no_key() {
        let h = ready();
        let state = h.ok("key_retry", json!({}));
        assert_eq!(state["state"], "unlocked");
        let key_id = state["key_id"].as_str().unwrap();

        let inside = h.paths.root().join("key.json");
        h.pick(&inside);
        assert_eq!(h.code("recovery_key_export", json!({})), "refused");
        assert!(!inside.exists());
        let named = format!("jobpilot-recovery-{key_id}.json");
        assert!(h.asked()[0].contains(&named), "{:?}", h.asked());

        let outside = h._tmp.path().join("key.json");
        h.pick(&outside);
        let receipt = h.ok("recovery_key_export", json!({}));
        assert_eq!(receipt, json!({ "key_id": key_id, "path": outside }));
        assert!(outside.is_file());
        assert_eq!(h.status()["recovery_key_exported"], true);
        h.ok("data_delete", delete("DELETE", true));
    }

    #[test]
    #[ignore = "uses the test keychain"]
    fn status_carries_restored_key_state_until_the_next_change() {
        let h = ready();
        lose_a_key(&h);
        let info = h.ok("backup_create", json!({}));
        let source = PathBuf::from(info["path"].as_str().unwrap());

        let status = h.ok("backup_restore", restore(&source));

        assert_eq!(status["restored_key_state"]["state"], "key_missing");
        assert_eq!(status["restored_key_state"]["key_id"], LOST_KEY);
        assert_eq!(
            h.status()["restored_key_state"],
            status["restored_key_state"]
        );
        let deleted = h.ok("data_delete", delete("DELETE", true));
        assert_eq!(deleted["restored_key_state"], Value::Null);
    }

    /// Everything a drafting task needs, on a stand-in `claude`. Returns the posting's id.
    #[cfg(unix)]
    fn claude_is_set_up(h: &Harness, run: &str) -> Value {
        fake_claude(h, &format!("[ \"$1\" = --version ] && exit 0\n{run}"));
        h.ok(
            "ai_enable",
            json!({ "provider": "claude_code", "enabled": true }),
        );
        let categories = ["job_text", "profile_basic", "resume_text"];
        h.ok(
            "ai_consent_give",
            json!({ "provider": "claude_code", "categories": categories }),
        );
        h.ok("kb_add", json!({ "kind": "skill", "body": "Rust" }));
        h.ok("job_paste", json!({ "title": "Role", "raw": "Build" }))["id"].clone()
    }

    /// What the consent dialog calls, and nothing else: `ai.order` is never written, so this is
    /// a default install. A reply with no citation is still stored, flagged.
    #[cfg(unix)]
    #[test]
    fn enable_and_consent_alone_reach_claude() {
        let h = ready();
        let summary = json!([{ "text": "Builds", "evidence_ids": [] }]);
        let reply = json!({ "type": "result", "subtype": "success", "is_error": false,
            "permission_denials": [], "structured_output": { "summary": summary, "sections": [] } });
        let job = claude_is_set_up(&h, &format!("cat > /dev/null\nprintf '%s' '{reply}'"));

        assert_eq!(
            h.ok("ai_status", json!({}))["order"],
            json!(["ollama", "claude_code"])
        );
        let made = h.ok("resume_generate", json!({ "jobTextId": job }));
        assert_eq!(made["provider"], "claude_code");
    }

    #[cfg(unix)]
    #[test]
    fn partial_consent_reaches_the_ui_as_a_reason() {
        let h = ready();
        let ran = h._tmp.path().join("ran");
        let job = claude_is_set_up(&h, &format!("touch {}", ran.display()));
        h.ok(
            "ai_consent_give",
            json!({ "provider": "claude_code", "categories": ["job_text"] }),
        );

        let error = h
            .call("resume_generate", json!({ "jobTextId": job }))
            .unwrap_err();

        assert_eq!(error["code"], "manual_required");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("consent_needed"), "{message}");
        assert!(!ran.exists());
    }

    /// A paid run in progress: its pid, and the thread waiting on `resume_generate`.
    #[cfg(unix)]
    fn running_claude(h: &Harness) -> (u32, std::thread::JoinHandle<Result<Value, Value>>) {
        let pid_file = h._tmp.path().join("claude.pid");
        let run = format!("echo $$ > {}\nexec sleep 300", pid_file.display());
        let job = claude_is_set_up(h, &run);
        let window = h.window.clone();
        let task = std::thread::spawn(move || {
            invoke(&window, "resume_generate", json!({ "jobTextId": job }))
        });
        wait_until(|| fs::read_to_string(&pid_file).is_ok_and(|text| text.ends_with('\n')));
        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        (pid, task)
    }

    #[cfg(unix)]
    #[test]
    fn a_model_call_holds_no_state_lock() {
        let h = ready();
        let (pid, task) = running_claude(&h);

        assert!(h.app.state::<AppState>().db.try_write().is_ok());
        assert_eq!(h.status()["state"], "ready");
        assert_eq!(h.titles(), Vec::<String>::new());

        h.ok("ai_cancel", json!({}));
        assert_eq!(code_of(task.join().unwrap()), "cancelled");
        wait_gone(pid);
    }

    #[cfg(unix)]
    #[test]
    fn one_ai_task_runs_at_a_time() {
        let h = ready();
        let (pid, task) = running_claude(&h);
        let job = h.ok("job_list", json!({}))[0]["id"].clone();

        assert_eq!(
            h.code("letter_generate", json!({ "jobTextId": job })),
            "refused"
        );

        h.ok("ai_cancel", json!({}));
        assert_eq!(code_of(task.join().unwrap()), "cancelled");
        wait_gone(pid);
    }

    #[cfg(unix)]
    #[test]
    fn consent_revoke_fires_the_in_flight_token() {
        let h = ready();
        let (pid, task) = running_claude(&h);

        h.ok("ai_consent_revoke", json!({ "provider": "claude_code" }));

        assert_eq!(code_of(task.join().unwrap()), "cancelled");
        wait_gone(pid);
    }

    #[cfg(unix)]
    #[test]
    fn restore_ends_a_running_task_and_waits_for_it() {
        let h = ready();
        let source = backup_holding(&h._tmp, "restored");
        let (pid, task) = running_claude(&h);

        let status = h.ok("backup_restore", restore(&source));

        assert_eq!(status["state"], "ready");
        assert_eq!(h.titles(), ["restored"]);
        assert_eq!(code_of(task.join().unwrap()), "cancelled");
        wait_gone(pid);
    }

    #[cfg(unix)]
    #[test]
    fn delete_ends_a_running_task_and_waits_for_it() {
        let h = ready();
        let (pid, task) = running_claude(&h);

        let status = h.ok("data_delete", delete("DELETE", true));

        assert_eq!(status["state"], "ready");
        assert_eq!(code_of(task.join().unwrap()), "cancelled");
        wait_gone(pid);
    }

    #[cfg(unix)]
    #[test]
    fn app_exit_cancels_running_ai_and_ends_the_child() {
        let h = ready();
        let (pid, task) = running_claude(&h);

        block_on(h.app.state::<AppState>().shutdown());

        assert_eq!(code_of(task.join().unwrap()), "cancelled");
        wait_gone(pid);
    }

    /// `shutdown` returning is what lets the process end, and a task dropped by that end
    /// never kills its child. So it must outlast a task that is still letting go.
    #[test]
    fn app_exit_waits_for_a_running_task_to_let_go() {
        let h = ready();
        let state = h.app.state::<AppState>();
        let running = state.ai.begin().unwrap();
        let work = block_on(state.work()).unwrap();
        let handle = h.app.handle().clone();
        let exit = std::thread::spawn(move || {
            block_on(handle.state::<AppState>().shutdown());
        });

        wait_until(|| running.token.is_cancelled());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!exit.is_finished());

        drop(work);
        exit.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn not_logged_in_reaches_the_ui_as_a_reason() {
        let h = ready();
        let job = claude_is_set_up(&h, "echo 'Not logged in'\nexit 1");

        let error = h
            .call("resume_generate", json!({ "jobTextId": job }))
            .unwrap_err();

        assert_eq!(error["code"], "manual_required");
        assert!(error["message"].as_str().unwrap().contains("not_logged_in"));
    }

    #[cfg(unix)]
    #[test]
    fn prompt_too_large_reaches_the_ui_as_a_reason() {
        let h = ready();
        let ran = h._tmp.path().join("ran");
        claude_is_set_up(&h, &format!("touch {}", ran.display()));
        let raw = "語".repeat(190_000);
        let job = h.ok("job_paste", json!({ "title": "Long", "raw": raw }));

        let error = h
            .call("resume_generate", json!({ "jobTextId": job["id"] }))
            .unwrap_err();

        assert_eq!(error["code"], "manual_required");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("prompt_too_large"), "{message}");
        assert!(!ran.exists());
    }

    fn wait_until(done: impl Fn() -> bool) {
        let until = Instant::now() + PTY_WAIT;
        while !done() {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn app_commands_reach_core() {
        let h = ready();
        let added = h.ok(
            "app_add",
            json!({ "input": { "company": "Acme", "role": "Engineer", "country": "pt" } }),
        );
        let id = added["id"].as_str().unwrap().to_owned();
        assert_eq!(added["status"], "saved");
        assert_eq!(added["country"], "PT");
        let moved = h.ok(
            "app_set_status",
            json!({ "id": id, "status": "applied", "on": "2026-10-02", "note": "sent" }),
        );
        assert_eq!(moved["applied_on"], "2026-10-02");
        let history = h.ok("app_history", json!({ "id": id }));
        assert_eq!(history.as_array().unwrap().len(), 2);
        h.ok("app_note", json!({ "id": id, "note": "called" }));
        assert_eq!(
            h.ok("app_history", json!({ "id": id }))
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let edited = h.ok(
            "app_update",
            json!({ "id": id, "input": { "company": "Acme", "role": "Staff" } }),
        );
        assert_eq!(edited["role"], "Staff");
        assert_eq!(h.ok("app_get", json!({ "id": id }))["status"], "applied");
        assert_eq!(
            h.ok("app_list", json!({ "status": "applied" }))
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            h.ok("app_list", json!({ "status": null }))
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let refused = h.call(
            "app_set_status",
            json!({ "id": id, "status": "ghosted", "on": null, "note": "" }),
        );
        assert_eq!(code_of(refused), "validation");
        h.ok("app_remove", json!({ "id": id }));
        assert_eq!(code_of(h.call("app_get", json!({ "id": id }))), "not_found");
    }

    #[test]
    fn agents_commands_reach_core() {
        let h = ready();
        assert_eq!(
            h.ok("agents_list", json!({ "limit": 10 })),
            json!([]),
            "no run yet"
        );
        let added = h.ok(
            "app_add",
            json!({ "input": { "company": "Acme", "role": "Engineer" } }),
        );
        let id = added["id"].as_str().unwrap().to_owned();
        h.ok(
            "app_set_status",
            json!({ "id": id, "status": "applied", "on": null, "note": "sent" }),
        );

        let runs = h.ok("agents_list", json!({ "limit": 10 }));
        assert_eq!(runs[0]["agent"], "tracking");
        assert_eq!(runs[0]["state"], "completed");
        assert_eq!(runs[0]["actor"], "gui");
        let run = runs[0]["id"].as_str().unwrap().to_owned();
        let detail = h.ok("agent_get", json!({ "id": run }));
        assert_eq!(detail["run"]["id"], run.as_str());
        assert_eq!(detail["log"].as_array().unwrap().len(), 5);

        for command in ["agent_cancel", "agent_resume"] {
            let refused = h.call(command, json!({ "id": run }));
            assert_eq!(code_of(refused), "validation", "{command}");
        }
        assert_eq!(
            code_of(h.call("agent_get", json!({ "id": "x" }))),
            "not_found"
        );

        with_db(&h, |db| {
            let run = run.clone();
            async move {
                sqlx::query("UPDATE agent_runs SET state = 'halted', step = 0 WHERE id = ?")
                    .bind(run)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
        });
        let resumed = h.ok("agent_resume", json!({ "id": run }));
        assert_eq!(resumed["run"]["state"], "completed");

        with_db(&h, |db| {
            let run = run.clone();
            async move {
                sqlx::query("UPDATE agent_runs SET state = 'halted' WHERE id = ?")
                    .bind(run)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
        });
        h.ok("agent_cancel", json!({ "id": run }));
        let detail = h.ok("agent_get", json!({ "id": run }));
        assert_eq!(detail["run"]["state"], "cancelled");
    }

    #[test]
    fn analytics_commands_reach_core() {
        let h = ready();
        assert_eq!(h.ok("analytics_latest", json!({})), Value::Null);
        let added = h.ok(
            "app_add",
            json!({ "input": { "company": "Acme", "role": "Engineer" } }),
        );
        let id = added["id"].as_str().unwrap().to_owned();
        h.ok(
            "app_set_status",
            json!({ "id": id, "status": "applied", "on": null, "note": "" }),
        );
        let report = h.ok("analytics_run", json!({}));
        assert_eq!(report["volume"]["total"], 1);
        assert_eq!(report["funnel"]["applied"], 1);
        assert_eq!(report["funnel"]["response_rate"], 0.0);
        let latest = h.ok("analytics_latest", json!({}));
        assert_eq!(latest["id"], report["id"]);
        let runs = h.ok("agents_list", json!({ "limit": 10 }));
        assert_eq!(runs[0]["agent"], "analytics");
        assert_eq!(runs[0]["actor"], "gui");
    }

    /// A sealed row naming a key this keychain never held, under a question of `class`.
    fn seal_answer_without_a_key(h: &Harness, class: &str, id: &str) -> String {
        let text = format!("Question for {id}");
        let question = h.ok(
            "memory_add_question",
            json!({ "text": text, "country": null }),
        );
        let question = question["id"].as_str().unwrap().to_owned();
        with_db(h, |db| async move {
            let mut connection = db.pool.acquire().await.unwrap();
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query("UPDATE memory_questions SET class = ? WHERE id = ?")
                .bind(class)
                .bind(&question)
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO memory_answers (id, question_id, body, body_enc, key_id, source, \
                 reviewed_at, created_at, updated_at) \
                 VALUES (?, ?, '', x'00', 'gone', 'user', 't', 't', 't')",
            )
            .bind(id)
            .bind(&question)
            .execute(&mut *connection)
            .await
            .unwrap();
            sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&mut *connection)
                .await
                .unwrap();
            question
        })
    }

    fn with_db<T, F: Future<Output = T>>(h: &Harness, work: impl FnOnce(Arc<Db>) -> F) -> T {
        block_on(async {
            let slot = h.app.state::<AppState>();
            let db = slot.db.read().await.as_ref().unwrap().clone();
            work(db).await
        })
    }

    fn approved(h: &Harness, question: &str) -> Value {
        h.ok("memory_get", json!({ "questionId": question }))["answers"][0]["approved"].clone()
    }

    #[test]
    fn memory_commands_reach_core() {
        let h = ready();
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "Why this job?", "country": "pt" }),
        );
        let question = added["id"].as_str().unwrap().to_owned();
        assert_eq!(added["class"], "normal");
        assert_eq!(added["country"], "PT");
        let item = h.ok("kb_add", json!({ "kind": "note", "body": "A note" }));

        let answer = h.ok(
            "memory_add_answer",
            json!({ "questionId": question, "text": "Because", "sourceRef": item["id"] }),
        );
        let id = answer["id"].as_str().unwrap().to_owned();
        assert_eq!(answer["source"], "knowledge");
        assert_eq!(answer["approved"], false);

        let edited = h.ok(
            "memory_edit_answer",
            json!({ "answerId": id, "text": "Since then" }),
        );
        assert_eq!(edited["version"], 2);
        h.ok(
            "memory_approve",
            json!({ "answerId": id, "approved": true }),
        );
        h.ok("memory_mark_reviewed", json!({ "answerId": id }));
        assert!(h.asked().is_empty(), "a normal answer asks nothing");

        let row = h.ok("memory_get", json!({ "questionId": question }));
        assert_eq!(row["answers"][0]["approved"], true);
        assert_eq!(row["answers"][0]["body"], "Since then");
        assert_eq!(
            h.ok("memory_reveal", json!({ "answerId": id })),
            "Since then"
        );
        h.ok(
            "memory_approve",
            json!({ "answerId": id, "approved": false }),
        );

        let raised = h.ok(
            "memory_add_question",
            json!({ "text": "A question with no answers", "country": null }),
        );
        let raised = raised["id"].as_str().unwrap().to_owned();
        let saved = h.ok(
            "memory_set_class",
            json!({ "questionId": raised, "class": "legal" }),
        );
        assert_eq!(saved["class"], "legal");
        assert_eq!(saved["class_source"], "user");
        let legal = h.ok("memory_list", json!({ "class": "legal" }));
        assert_eq!(legal.as_array().unwrap().len(), 1);
        let all = h.ok("memory_list", json!({ "class": null }));
        assert_eq!(all.as_array().unwrap().len(), 2);

        h.ok("memory_remove_answer", json!({ "id": id }));
        h.ok("memory_remove_question", json!({ "id": question }));
        let gone = h.call("memory_get", json!({ "questionId": question }));
        assert_eq!(code_of(gone), "not_found");
    }

    #[test]
    fn memory_search_reaches_core() {
        let h = ready();
        let found = h.ok(
            "memory_add_question",
            json!({ "text": "Describe your leadership style", "country": null }),
        );
        let found = found["id"].as_str().unwrap().to_owned();
        let answer = h.ok(
            "memory_add_answer",
            json!({ "questionId": found, "text": "I mentor", "sourceRef": null }),
        );
        h.ok(
            "memory_approve",
            json!({ "answerId": answer["id"], "approved": true }),
        );
        let authorization = h.ok(
            "memory_add_question",
            json!({ "text": "Are you authorized to work in the US?", "country": "US" }),
        );
        assert_eq!(authorization["class"], "work_authorization");

        let hits = h.ok(
            "memory_search",
            json!({ "query": "leadership", "limit": 5 }),
        );
        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["question_id"], found.as_str());
        assert_eq!(hits[0]["via"], "text");
        assert_eq!(hits[0]["answer"], "I mentor");
        let none = h.ok(
            "memory_search",
            json!({ "query": "authorized", "limit": 5 }),
        );
        assert!(none.as_array().unwrap().is_empty());

        // With no embedding model the refresh embeds nothing and is not an error.
        let refreshed = h.ok("memory_refresh", json!({}));
        assert_eq!(refreshed["embedded"], 0);
        assert_eq!(refreshed["classified"]["skipped"], true);
    }

    #[test]
    fn approving_a_sealed_class_answer_asks_natively() {
        let h = ready();
        let question = seal_answer_without_a_key(&h, "sensitive", "s1");

        h.answer(false);
        let refused = h.call(
            "memory_approve",
            json!({ "answerId": "s1", "approved": true }),
        );
        assert_eq!(code_of(refused), "confirmation_required");
        assert_eq!(approved(&h, &question), false);
        assert_eq!(h.asked().len(), 1);
        assert!(h.asked()[0].contains("s1"), "{:?}", h.asked());

        h.answer(true);
        h.ok(
            "memory_approve",
            json!({ "answerId": "s1", "approved": true }),
        );
        assert_eq!(approved(&h, &question), true);
        assert_eq!(h.asked().len(), 2);

        // Taking it back asks no one.
        h.answer(false);
        h.ok(
            "memory_approve",
            json!({ "answerId": "s1", "approved": false }),
        );
        assert_eq!(approved(&h, &question), false);
        assert_eq!(h.asked().len(), 2);
    }

    #[test]
    fn reviewing_a_sealed_class_answer_asks_natively() {
        let h = ready();
        let question = seal_answer_without_a_key(&h, "legal", "s1");

        h.answer(false);
        let refused = h.call("memory_mark_reviewed", json!({ "answerId": "s1" }));
        assert_eq!(code_of(refused), "confirmation_required");
        let row = h.ok("memory_get", json!({ "questionId": question }));
        assert_eq!(row["answers"][0]["reviewed_at"], "t");

        h.answer(true);
        h.ok("memory_mark_reviewed", json!({ "answerId": "s1" }));
        let row = h.ok("memory_get", json!({ "questionId": question }));
        assert_ne!(row["answers"][0]["reviewed_at"], "t");
    }

    #[test]
    fn lowering_a_class_asks_natively_and_raising_does_not() {
        let h = ready();
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "What is your salary?", "country": null }),
        );
        let question = added["id"].as_str().unwrap().to_owned();
        assert_eq!(added["class"], "sensitive");

        h.answer(false);
        let saved = h.ok(
            "memory_set_class",
            json!({ "questionId": question, "class": "legal" }),
        );
        assert_eq!(saved["class"], "legal");
        assert!(h.asked().is_empty());

        let refused = h.call(
            "memory_set_class",
            json!({ "questionId": question, "class": "normal" }),
        );
        assert_eq!(code_of(refused), "confirmation_required");
        let row = h.ok("memory_get", json!({ "questionId": question }));
        assert_eq!(row["question"]["class"], "legal");
        assert_eq!(h.asked().len(), 1);

        h.answer(true);
        let saved = h.ok(
            "memory_set_class",
            json!({ "questionId": question, "class": "normal" }),
        );
        assert_eq!(saved["class"], "normal");
    }

    #[test]
    fn memory_reveal_of_a_sealed_answer_needs_a_native_yes_every_time() {
        let h = ready();
        // A body raised and not yet sealed needs no key, so a yes reaches the text here.
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "Why this job?", "country": null }),
        );
        let question = added["id"].as_str().unwrap().to_owned();
        let answer = h.ok(
            "memory_add_answer",
            json!({ "questionId": question, "text": "ZXQ-SEALED", "sourceRef": null }),
        );
        let id = answer["id"].as_str().unwrap().to_owned();
        with_db(&h, |db| async move {
            sqlx::query("UPDATE memory_questions SET class = 'sensitive'")
                .execute(&db.pool)
                .await
                .unwrap();
        });
        let row = h.ok("memory_get", json!({ "questionId": question }));
        assert_eq!(row["answers"][0]["sealed"], true);
        assert_eq!(row["answers"][0]["body"], "");

        h.answer(false);
        let refused = h.call("memory_reveal", json!({ "answerId": id }));
        assert_eq!(code_of(refused), "confirmation_required");
        assert_eq!(h.asked().len(), 1);
        assert!(h.asked()[0].contains(&id), "{:?}", h.asked());
        let log = h.ok("audit_recent", json!({ "limit": 10 }));
        assert!(!log.to_string().contains("answer_revealed"), "{log}");

        h.answer(true);
        assert_eq!(
            h.ok("memory_reveal", json!({ "answerId": id })),
            "ZXQ-SEALED"
        );
        assert_eq!(
            h.ok("memory_reveal", json!({ "answerId": id })),
            "ZXQ-SEALED"
        );
        assert_eq!(h.asked().len(), 3, "one dialog per reveal");
        assert!(!h.asked().iter().any(|m| m.contains("ZXQ-SEALED")));
        let log = h.ok("audit_recent", json!({ "limit": 10 }));
        assert!(log.to_string().contains("answer_revealed"), "{log}");
        assert!(!log.to_string().contains("ZXQ-SEALED"), "{log}");
    }

    /// A Sensitive-class answer whose approved body waits to be sealed, so no key is needed,
    /// and an application to use it for.
    fn waiting_sensitive_answer(h: &Harness) -> (String, String) {
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "Why this job?", "country": null }),
        );
        let answer = h.ok(
            "memory_add_answer",
            json!({ "questionId": added["id"], "text": "ZXQ-CANARY", "sourceRef": null }),
        );
        let id = answer["id"].as_str().unwrap().to_owned();
        with_db(h, |db| async move {
            sqlx::query("UPDATE memory_questions SET class = 'sensitive'")
                .execute(&db.pool)
                .await
                .unwrap();
            sqlx::query("UPDATE memory_answers SET approved = 1")
                .execute(&db.pool)
                .await
                .unwrap();
        });
        let application = h.ok(
            "app_add",
            json!({ "input": {
                "company": "Acme", "role": "Engineer", "job_text_id": null,
                "resume_draft_id": null, "letter_draft_id": null, "link": "",
                "country": "", "notes": ""
            } }),
        );
        (id, application["id"].as_str().unwrap().to_owned())
    }

    #[test]
    fn confirm_use_opens_a_native_dialog_without_the_value() {
        let h = ready();
        let (id, app) = waiting_sensitive_answer(&h);
        let args = json!({ "answerId": id, "applicationId": app });

        let refused = h.call("memory_use", args.clone());
        assert_eq!(code_of(refused), "confirmation_required");
        assert!(h.asked().is_empty(), "use opens no dialog of its own");

        h.answer(true);
        h.ok("memory_confirm_use", args.clone());
        let asked = h.asked();
        assert_eq!(asked.len(), 1);
        assert!(!asked[0].contains("ZXQ-CANARY"), "{asked:?}");
        for part in [
            "Confirm this answer",
            "sensitive",
            "Why this job?",
            "Acme / Engineer",
            "You confirm this is true today",
        ] {
            assert!(asked[0].contains(part), "{part}: {asked:?}");
        }
        assert!(!asked[0].contains("180 days"), "{asked:?}");

        let used = h.ok("memory_use", args);
        assert_eq!(used["text"], "ZXQ-CANARY");
        let listed = h.ok(
            "memory_application_answers",
            json!({ "applicationId": app }),
        );
        assert_eq!(listed[0]["current"], true);
        assert!(!listed.to_string().contains("ZXQ-CANARY"));
        let log = h.ok("audit_recent", json!({ "limit": 20 }));
        assert!(log.to_string().contains("answer_confirmed"), "{log}");
        assert!(log.to_string().contains("answer_used"), "{log}");
        assert!(!log.to_string().contains("ZXQ-CANARY"), "{log}");
    }

    #[test]
    fn a_refused_dialog_records_no_confirmation() {
        let h = ready();
        let (id, app) = waiting_sensitive_answer(&h);
        let args = json!({ "answerId": id, "applicationId": app });

        h.answer(false);
        let refused = h.call("memory_confirm_use", args.clone());

        assert_eq!(code_of(refused), "confirmation_required");
        assert_eq!(h.asked().len(), 1);
        let listed = h.ok(
            "memory_application_answers",
            json!({ "applicationId": app }),
        );
        assert_eq!(listed.as_array().unwrap().len(), 0);
        assert_eq!(code_of(h.call("memory_use", args)), "confirmation_required");
        let log = h.ok("audit_recent", json!({ "limit": 20 }));
        assert!(!log.to_string().contains("answer_confirmed"), "{log}");
    }

    #[test]
    fn confirm_use_refuses_an_unapproved_answer_before_any_dialog() {
        let h = ready();
        let (id, app) = waiting_sensitive_answer(&h);
        with_db(&h, |db| async move {
            sqlx::query("UPDATE memory_answers SET approved = 0")
                .execute(&db.pool)
                .await
                .unwrap();
        });

        let refused = h.call(
            "memory_confirm_use",
            json!({ "answerId": id, "applicationId": app }),
        );

        assert_eq!(code_of(refused), "refused");
        assert!(h.asked().is_empty());
    }

    #[test]
    fn memory_authorization_reads_the_profile_by_country() {
        let h = ready();
        let none = h.ok("memory_authorization", json!({ "country": "ca" }));
        assert_eq!(none["stated"], false);
        assert_eq!(none["country"], "CA");
        assert_eq!(none["authorized"], "not_stated");
        let bad = h.call("memory_authorization", json!({ "country": "canada" }));
        assert_eq!(code_of(bad), "validation");
    }

    #[test]
    fn memory_reveal_of_a_plain_answer_or_an_unknown_id_asks_nothing() {
        let h = ready();
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "Why this job?", "country": null }),
        );
        let answer = h.ok(
            "memory_add_answer",
            json!({ "questionId": added["id"], "text": "Because", "sourceRef": null }),
        );

        assert_eq!(
            h.ok("memory_reveal", json!({ "answerId": answer["id"] })),
            "Because"
        );
        let unknown = h.call("memory_reveal", json!({ "answerId": "nope" }));
        assert_eq!(code_of(unknown), "not_found");
        assert!(h.asked().is_empty());
    }

    // A dialog that can only end in a refusal asks the user for nothing.
    #[test]
    fn a_work_authorization_refusal_opens_no_dialog() {
        let h = ready();
        seal_answer_without_a_key(&h, "work_authorization", "s1");
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "Are you authorized to work in Canada?", "country": null }),
        );

        for result in [
            h.call(
                "memory_approve",
                json!({ "answerId": "s1", "approved": true }),
            ),
            h.call("memory_mark_reviewed", json!({ "answerId": "s1" })),
            h.call(
                "memory_set_class",
                json!({ "questionId": added["id"], "class": "normal" }),
            ),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error["code"], "refused");
            assert!(
                error["message"]
                    .as_str()
                    .unwrap()
                    .contains("use_profile_by_country")
            );
        }
        assert!(h.asked().is_empty(), "{:?}", h.asked());
    }

    #[test]
    fn lowering_from_work_authorization_says_what_it_allows() {
        let h = ready();
        let question = seal_answer_without_a_key(&h, "work_authorization", "s1");
        h.answer(false);
        let refused = h.call(
            "memory_set_class",
            json!({ "questionId": question, "class": "sensitive" }),
        );
        assert_eq!(code_of(refused), "confirmation_required");
        assert!(
            h.asked()[0].contains("can then take an answer"),
            "{:?}",
            h.asked()
        );
    }

    #[test]
    fn data_export_says_how_many_answers_it_left_out() {
        let h = ready();
        let added = h.ok(
            "memory_add_question",
            json!({ "text": "Why this job?", "country": null }),
        );
        h.ok(
            "memory_add_answer",
            json!({ "questionId": added["id"], "text": "Because", "sourceRef": null }),
        );
        with_db(&h, |db| async move {
            sqlx::query("UPDATE memory_questions SET class = 'sensitive'")
                .execute(&db.pool)
                .await
                .unwrap();
        });

        assert_eq!(h.ok("data_export", json!({}))["withheld"], 1);
    }

    #[test]
    fn no_memory_dialog_holds_the_handle() {
        let h = ready();
        let question = seal_answer_without_a_key(&h, "sensitive", "s1");
        with_db(&h, |db| async move {
            sqlx::query("UPDATE memory_answers SET approved = 1")
                .execute(&db.pool)
                .await
                .unwrap();
        });
        let app = h.ok(
            "app_add",
            json!({ "input": {
                "company": "Acme", "role": "Engineer", "job_text_id": null,
                "resume_draft_id": null, "letter_draft_id": null, "link": "",
                "country": "", "notes": ""
            } }),
        );
        h.answer(false);
        for result in [
            h.call(
                "memory_confirm_use",
                json!({ "answerId": "s1", "applicationId": app["id"] }),
            ),
            h.call(
                "memory_approve",
                json!({ "answerId": "s1", "approved": true }),
            ),
            h.call("memory_mark_reviewed", json!({ "answerId": "s1" })),
            h.call("memory_reveal", json!({ "answerId": "s1" })),
            h.call(
                "memory_set_class",
                json!({ "questionId": question, "class": "normal" }),
            ),
        ] {
            assert_eq!(code_of(result), "confirmation_required");
        }
        let hook = h.app.state::<NativeHook>();
        assert_eq!(*hook.work_at_dialog.lock().unwrap(), [false; 5]);
    }

    /// A posting, evidence it can cite, and a `claude` that cites it.
    #[cfg(unix)]
    fn drafting_with_claude(h: &Harness) -> Value {
        let item = h.ok("kb_add", json!({ "kind": "skill", "body": "Rust" }));
        let summary = json!([{ "text": "Rust", "evidence_ids": [item["id"]] }]);
        let reply = json!({ "type": "result", "subtype": "success", "is_error": false,
            "permission_denials": [], "structured_output": { "summary": summary, "sections": [] } });
        fake_claude(
            h,
            &format!("[ \"$1\" = --version ] && exit 0\ncat > /dev/null\nprintf '%s' '{reply}'"),
        );
        h.ok(
            "ai_enable",
            json!({ "provider": "claude_code", "enabled": true }),
        );
        let categories = ["job_text", "profile_basic", "resume_text"];
        h.ok(
            "ai_consent_give",
            json!({ "provider": "claude_code", "categories": categories }),
        );
        h.ok("job_paste", json!({ "title": "Role", "raw": "Build" }))["id"].clone()
    }

    #[cfg(unix)]
    #[test]
    fn resume_generate_runs_through_the_agent() {
        let h = ready();
        let job = drafting_with_claude(&h);
        let export = json!({ "format": "pdf", "template": "compact" });
        let made = h.ok(
            "resume_generate",
            json!({ "jobTextId": job, "export": export }),
        );
        assert_eq!(made["provider"], "claude_code");
        let path = PathBuf::from(made["exported"].as_str().expect("a path"));
        assert!(path.starts_with(h.paths.exports()), "{path:?}");
        assert!(path.is_file());

        let runs = h.ok("agents_list", json!({ "limit": 10 }));
        assert_eq!(runs[0]["agent"], "resume");
        assert_eq!(runs[0]["state"], "completed");
        assert_eq!(runs[0]["actor"], "gui");

        let made = h.ok(
            "resume_generate",
            json!({ "jobTextId": job, "export": null }),
        );
        assert_eq!(made["exported"], Value::Null);
    }

    /// Two questions waiting, and Claude on with consent: a press names the count first.
    fn claude_on_with_questions(h: &Harness) {
        h.ok(
            "ai_enable",
            json!({ "provider": "claude_code", "enabled": true }),
        );
        h.ok(
            "ai_consent_give",
            json!({ "provider": "claude_code", "categories": ["job_text"] }),
        );
        for text in ["Describe project one", "Describe project two"] {
            h.ok(
                "memory_add_question",
                json!({ "text": text, "country": null }),
            );
        }
    }

    #[test]
    fn memory_refresh_names_the_count_natively() {
        let h = ready();
        claude_on_with_questions(&h);
        let plan = h.ok("memory_refresh_plan", json!({}));
        assert_eq!(
            (plan["pending"].clone(), plan["external_calls"].clone()),
            (json!(2), json!(2))
        );

        h.answer(false);
        assert_eq!(h.code("memory_refresh", json!({})), "confirmation_required");
        let asked = h.asked();
        let dialog = asked.last().unwrap();
        assert!(
            dialog.contains("Refresh career memory")
                && dialog.contains(
                    "This will make at most 2 paid or subscription calls for 2 questions \
                     (a press checks 20 questions at most)."
                ),
            "{asked:?}"
        );
        assert_eq!(
            h.ok("agents_list", json!({ "limit": 10 })),
            json!([]),
            "no run row"
        );
        let hook = h.app.state::<NativeHook>();
        assert_eq!(hook.work_at_dialog.lock().unwrap().last(), Some(&false));
    }

    /// The yes covers the count it was asked with: a question that arrives while the dialog
    /// is open changes the plan, and the press is refused before anything runs.
    #[test]
    fn a_count_that_moves_after_the_dialog_is_refused() {
        let h = ready();
        claude_on_with_questions(&h);
        let db = with_db(&h, |db| async move { db });
        let hook = h.app.state::<NativeHook>();
        *hook.on_ask.lock().unwrap() = Some(Box::new(move || {
            let db = db.clone();
            Box::pin(async move {
                jobpilot_core::memory::add_question(&db, "Describe project three", None)
                    .await
                    .unwrap();
            })
        }));
        h.answer(true);
        let refused = h.call("memory_refresh", json!({}));
        assert_eq!(code_of(refused), "validation");
        let asked = h.asked();
        assert!(
            asked
                .last()
                .unwrap()
                .contains("at most 2 paid or subscription calls")
        );
        assert_eq!(h.ok("agents_list", json!({ "limit": 10 })), json!([]));
        let plan = h.ok("memory_refresh_plan", json!({}));
        assert_eq!(plan["external_calls"], 3, "the new question is counted now");
    }

    #[test]
    fn memory_refresh_asks_nothing_when_no_external_call_is_ahead() {
        let h = ready();
        h.ok(
            "memory_add_question",
            json!({ "text": "Describe project one", "country": null }),
        );
        let plan = h.ok("memory_refresh_plan", json!({}));
        assert_eq!(plan["external_calls"], 0);
        h.answer(false);
        let done = h.ok("memory_refresh", json!({}));
        assert_eq!(done["classified"]["skipped"], true);
        assert!(h.asked().is_empty());
        let runs = h.ok("agents_list", json!({ "limit": 10 }));
        assert_eq!(runs[0]["agent"], "career_memory");
    }

    fn audited(h: &Harness, action: &str) -> Vec<String> {
        let rows = h.ok("audit_recent", json!({ "limit": 50 }));
        let rows = rows.as_array().unwrap();
        rows.iter()
            .filter(|row| row["action"] == action)
            .map(|row| row["actor"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn pause_set_fires_the_running_tokens_and_is_audited_as_gui() {
        let h = ready();
        let state = h.app.state::<AppState>();
        let ai = state.ai.begin().unwrap();
        let pull = state.pull.begin().unwrap();
        assert_eq!(h.ok("pause_status", json!({}))["paused"], false);

        let before = jobpilot_core::pause::epoch(&h.paths);
        let on = h.ok("pause_set", json!({}));

        assert_eq!(
            jobpilot_core::pause::epoch(&h.paths),
            before + 1,
            "announced first"
        );
        assert_eq!(on["paused"], true);
        assert_eq!(on["set_by"], "gui");
        assert!(ai.token.is_cancelled() && pull.token.is_cancelled());
        assert_eq!(audited(&h, "emergency_pause_set"), ["gui"]);
        assert_eq!(h.ok("pause_status", json!({}))["paused"], true);
        // Again: nothing more is written.
        h.ok("pause_set", json!({}));
        assert_eq!(audited(&h, "emergency_pause_set"), ["gui"]);
    }

    #[test]
    fn pause_set_in_recovery_fires_tokens_and_reports_unavailable() {
        let h = recovery(OPENS);
        let state = h.app.state::<AppState>();
        let ai = state.ai.begin().unwrap();
        let pull = state.pull.begin().unwrap();

        assert_eq!(h.code("pause_set", json!({})), "unavailable");

        assert!(ai.token.is_cancelled() && pull.token.is_cancelled());
        assert_eq!(h.code("pause_status", json!({})), "unavailable");
    }

    #[test]
    fn pause_clear_asks_natively_and_stays_set_when_refused() {
        let h = ready();
        h.ok("pause_set", json!({}));

        h.answer(false);
        assert_eq!(h.code("pause_clear", json!({})), "confirmation_required");
        assert_eq!(h.ok("pause_status", json!({}))["paused"], true);
        let asked = h.asked();
        assert!(
            asked.last().unwrap().contains("Clear emergency pause"),
            "{asked:?}"
        );
        let hook = h.app.state::<NativeHook>();
        assert_eq!(hook.work_at_dialog.lock().unwrap().last(), Some(&false));
        assert!(audited(&h, "emergency_pause_cleared").is_empty());

        h.answer(true);
        let off = h.ok("pause_clear", json!({}));
        assert_eq!(off["paused"], false);
        assert_eq!(audited(&h, "emergency_pause_cleared"), ["gui"]);
    }

    #[test]
    fn pause_clear_in_recovery_asks_nothing() {
        let h = recovery(OPENS);
        assert_eq!(h.code("pause_clear", json!({})), "unavailable");
        assert!(h.asked().is_empty());
    }

    #[test]
    fn the_menu_item_uses_the_shared_pause_path() {
        use crate::menu::{self, PAUSE_ID};
        let h = ready();
        let state = h.app.state::<AppState>();
        let ai = state.ai.begin().unwrap();
        let event = |id: &str| tauri::menu::MenuEvent { id: id.into() };

        menu::on_event(h.app.handle(), event("something_else"));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(h.ok("pause_status", json!({}))["paused"], false);

        menu::on_event(h.app.handle(), event(PAUSE_ID));
        wait_until(|| h.ok("pause_status", json!({}))["paused"] == true);
        assert!(ai.token.is_cancelled());
        assert_eq!(audited(&h, "emergency_pause_set"), ["gui"]);
    }

    /// macOS makes a submenu only on the main thread, and a test never runs there, so the
    /// whole menu is built where it can be: the item is checked on every system.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn menu_has_the_pause_item_and_accelerator() {
        use crate::menu::{self, PAUSE_ACCELERATOR, PAUSE_ID, SAFETY};
        let h = ready();

        let menu = menu::build(h.app.handle()).unwrap();

        let items = menu.items().unwrap();
        let names: Vec<String> = items
            .iter()
            .filter_map(|item| item.as_submenu()?.text().ok())
            .collect();
        assert!(
            names.contains(&"Edit".to_owned()),
            "copy and paste stay: {names:?}"
        );
        let safety = items
            .iter()
            .filter_map(|item| item.as_submenu())
            .find(|submenu| submenu.text().is_ok_and(|text| text == SAFETY))
            .expect("a Safety submenu");
        assert!(safety.get(PAUSE_ID).is_some());
        assert_eq!(PAUSE_ACCELERATOR, "CmdOrCtrl+Shift+H");
    }

    #[test]
    fn the_pause_item_has_its_id_label_and_a_valid_accelerator() {
        use crate::menu::{self, PAUSE_ACCELERATOR, PAUSE_ID};
        let h = ready();

        let item = menu::pause_item(h.app.handle()).unwrap();

        assert_eq!(item.id().0, PAUSE_ID);
        assert_eq!(item.text().unwrap(), "Emergency pause");
        assert_eq!(PAUSE_ACCELERATOR, "CmdOrCtrl+Shift+H");
        assert!(item.is_enabled().unwrap());
    }

    #[test]
    fn a_pause_set_by_another_process_fires_the_tokens() {
        let h = ready();
        let state = h.app.state::<AppState>();
        let (ai, pull) = (state.ai.begin().unwrap(), state.pull.begin().unwrap());
        let pty = h.app.state::<PtyState>();
        assert!(!block_on(super::stop_if_paused(&state, &pty)));
        assert!(!ai.token.is_cancelled());

        // A second handle stands for the CLI.
        block_on(async {
            let other = Db::open(&h.paths).await.unwrap();
            jobpilot_core::pause::set(&other, "cli").await.unwrap();
            other.close().await;
        });

        assert!(block_on(super::stop_if_paused(&state, &pty)));
        assert!(ai.token.is_cancelled() && pull.token.is_cancelled());
    }

    // ---- Claude Code apply runs ----

    /// A saved application with a resume and a letter, and the consent a run needs. Returns
    /// the application's id.
    fn apply_ready(h: &Harness) -> String {
        apply_ready_with(h, true)
    }

    /// As [`apply_ready`], with consent for `Sensitive` only when `sensitive`.
    fn apply_ready_with(h: &Harness, sensitive: bool) -> String {
        use jobpilot_core::ai::claude_code::PROVIDER;
        use jobpilot_core::ai::{DataCategory, consent};
        use jobpilot_core::applications::{self, ApplicationInput};
        use jobpilot_core::drafts;
        use jobpilot_core::render::{Entry, ResumeDoc, Section};
        with_db(h, |db| async move {
            let doc = |title: &str| ResumeDoc {
                name: "Ada Lovelace".into(),
                contact: vec!["ada@example.com".into()],
                summary: Vec::new(),
                sections: vec![Section {
                    heading: "Experience".into(),
                    entries: vec![Entry {
                        bullets: vec![title.into()],
                        ..Entry::default()
                    }],
                }],
            };
            let mut made = Vec::new();
            for kind in ["resume", "cover_letter"] {
                let title = format!("Acme {kind}");
                let draft = drafts::create_version(
                    &db,
                    kind,
                    None,
                    "uploaded",
                    &title,
                    None,
                    None,
                    doc(&title),
                    Vec::new(),
                )
                .await
                .unwrap();
                made.push(draft.id);
            }
            let input = ApplicationInput {
                company: "Acme".into(),
                role: "Engineer".into(),
                link: "https://boards.greenhouse.io/acme/jobs/1".into(),
                resume_draft_id: Some(made[0].clone()),
                letter_draft_id: Some(made[1].clone()),
                ..ApplicationInput::default()
            };
            let app = applications::add(&db, "gui", input).await.unwrap().id;
            let mut categories = vec![
                DataCategory::JobText,
                DataCategory::ProfileBasic,
                DataCategory::CareerMemory,
            ];
            if sensitive {
                categories.push(DataCategory::Sensitive);
            }
            consent::give(&db, PROVIDER, &categories, "2026-11-01", "gui")
                .await
                .unwrap();
            app
        })
    }

    /// A `ready` run over one fresh application, started through the command.
    fn ready_run(h: &Harness) -> String {
        let app = apply_ready(h);
        let run = h.ok("apply_start", json!({ "applicationIds": [app] }));
        run.as_str().unwrap().to_owned()
    }

    #[test]
    fn pty_open_apply_takes_only_a_size_and_an_id() {
        let source = include_str!("pty.rs");
        let (_, rest) = source
            .split_once("pub async fn pty_open_apply<R: Runtime>(")
            .unwrap();
        let (arguments, _) = rest.split_once(") -> ").unwrap();
        let names: Vec<&str> = arguments
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim())
            .collect();
        assert_eq!(
            names,
            ["window", "app", "state", "cols", "rows", "apply_id"]
        );
    }

    #[test]
    fn pty_open_apply_refuses_a_run_that_is_not_ready() {
        let h = ready();
        let (cols, rows) = SIZE;
        let open = |id: &str| {
            invoke(
                &h.window,
                "pty_open_apply",
                json!({ "cols": cols, "rows": rows, "applyId": id }),
            )
        };
        assert_eq!(code_of(open("0123abcd")), "not_found");
        let run = ready_run(&h);
        h.ok("apply_cancel", json!({ "id": run }));
        let refused = open(&run).unwrap_err();
        assert_eq!(refused["code"], "refused");
        assert!(
            refused["message"]
                .as_str()
                .unwrap()
                .contains("apply_run_not_ready"),
            "{refused}"
        );
        assert!(h.app.state::<PtyState>().pid().is_none());
        let refused = invoke(
            &h.window,
            "pty_open_apply",
            json!({ "cols": 0, "rows": rows, "applyId": run }),
        );
        assert_eq!(code_of(refused), "validation");
    }

    /// The host runs with no panel marker, or the CLI would refuse `apply session`. With no
    /// browser runtime in a fresh data folder it stops at `prepare`, before any `claude`.
    #[test]
    fn the_apply_child_has_no_panel_marker_and_runs_apply_session() {
        let h = ready();
        let run = ready_run(&h);
        let panel = Panel::listening(&h);
        let (cols, rows) = SIZE;
        invoke(
            &h.window,
            "pty_open_apply",
            json!({ "cols": cols, "rows": rows, "applyId": run }),
        )
        .expect("build the CLI first: cargo build -p jobpilot-cli");
        let text = panel.wait_for(&format!("run {run}"));
        assert!(!text.contains("panel_refused"), "{text}");
        wait_until(|| !panel.exits.lock().unwrap().is_empty());
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert_eq!(shown["state"], "ended", "{shown}");
    }

    /// In recovery the row cannot be written, and the session still goes: it is closed first.
    #[test]
    fn pause_set_closes_the_apply_session_before_the_row() {
        use crate::pty::test_support::{gone_within_two_seconds, marked_browser};
        let h = recovery(OPENS);
        let panel = Panel::open(&h);
        let pty = h.app.state::<PtyState>();
        let pid = pty.pid().unwrap();
        pty.mark_apply(&h.paths, "0123abcd");
        let mut browser = marked_browser(&h.paths.apply().join("0123abcd"));

        assert_eq!(h.code("pause_set", json!({})), "unavailable");

        assert!(pty.pid().is_none());
        wait_gone(pid);
        gone_within_two_seconds(&mut browser);
        drop(panel);

        // With data open: closed, and the pause is on.
        let h = ready();
        let panel = Panel::open(&h);
        let pty = h.app.state::<PtyState>();
        let pid = pty.pid().unwrap();
        pty.mark_apply(&h.paths, "0123abcd");
        assert_eq!(h.ok("pause_set", json!({}))["paused"], true);
        assert!(pty.pid().is_none());
        wait_gone(pid);
        drop(panel);
    }

    #[test]
    fn a_pause_set_by_another_process_closes_the_apply_session() {
        use crate::pty::test_support::{gone_within_two_seconds, marked_browser};
        let h = ready();
        let panel = Panel::open(&h);
        let state = h.app.state::<AppState>();
        let pty = h.app.state::<PtyState>();
        let pid = pty.pid().unwrap();
        pty.mark_apply(&h.paths, "0123abcd");
        let mut browser = marked_browser(&h.paths.apply().join("0123abcd"));
        assert!(!block_on(super::stop_if_paused(&state, &pty)));
        assert_eq!(pty.pid(), Some(pid));

        block_on(async {
            let other = Db::open(&h.paths).await.unwrap();
            jobpilot_core::pause::set(&other, "cli").await.unwrap();
            other.close().await;
        });

        assert!(block_on(super::stop_if_paused(&state, &pty)));
        assert!(pty.pid().is_none());
        wait_gone(pid);
        gone_within_two_seconds(&mut browser);
        drop(panel);
    }

    /// One sealed answer serves every country: a normal question raised to sensitive.
    fn sealed_answer(h: &Harness) {
        let question = h.ok(
            "memory_add_question",
            json!({ "text": "Your current salary?", "country": null }),
        );
        let question = question["id"].as_str().unwrap().to_owned();
        let answer = h.ok(
            "memory_add_answer",
            json!({ "questionId": question, "text": "Ninety", "sourceRef": null }),
        );
        with_db(h, |db| async move {
            sqlx::query(
                "UPDATE memory_answers SET approved = 1, \
                 reviewed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
            )
            .bind(answer["id"].as_str().unwrap())
            .execute(&db.pool)
            .await
            .unwrap();
            sqlx::query("UPDATE memory_questions SET class = 'sensitive' WHERE id = ?")
                .bind(&question)
                .execute(&db.pool)
                .await
                .unwrap();
        });
    }

    #[test]
    fn apply_start_asks_natively_and_the_webview_cannot_release() {
        let h = ready();
        let app = apply_ready(&h);
        sealed_answer(&h);
        let preflight = h.ok("apply_preflight", json!({ "applicationIds": [app] }));
        assert_eq!(preflight["sealed_total"], 1, "{preflight}");

        // The webview's own word for a release is not an argument at all.
        h.answer(false);
        let args =
            json!({ "applicationIds": [app], "releaseSealed": true, "release_sealed": true });
        let run = h.ok("apply_start", args);
        let asked = h.asked();
        assert_eq!(
            asked.last().unwrap(),
            "Sealed answers: Release 1 sensitive answer or work-authorization row to this run \
             for 1 application?"
        );
        let hook = h.app.state::<NativeHook>();
        assert_eq!(
            hook.buttons.lock().unwrap().last().unwrap(),
            &(
                "Release to this run".to_owned(),
                "Leave them out".to_owned()
            )
        );
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert_eq!(shown["items"][0]["released_answers"], json!([]), "{shown}");
        assert!(audited(&h, "sensitive_released").is_empty());
        h.ok("apply_cancel", json!({ "id": run }));

        h.answer(true);
        let run = h.ok("apply_start", json!({ "applicationIds": [app] }));
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert_eq!(
            shown["items"][0]["released_answers"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(audited(&h, "sensitive_released"), ["gui"]);
        assert_eq!(h.asked().len(), 2);
    }

    #[test]
    fn apply_start_with_nothing_sealed_asks_nothing() {
        let h = ready();
        let run = ready_run(&h);
        assert!(h.asked().is_empty());
        let latest = h.ok("apply_latest", json!({}));
        assert_eq!(latest["id"], run.as_str());
        assert_eq!(latest["state"], "ready");
        assert_eq!(audited(&h, "apply_run_created"), ["gui"]);
        let status = h.ok("browser_status", json!({}));
        assert_ne!(status["state"], "ready", "{status}");
        assert_eq!(h.ok("apply_settle", json!({ "id": run })), 0);
    }

    #[test]
    fn apply_resolve_pause_audits_as_gui() {
        let h = ready();
        let run = ready_run(&h);
        let shown = h.ok("apply_view", json!({ "id": run }));
        let item = shown["items"][0]["id"].as_str().unwrap().to_owned();
        let pause: String = with_db(&h, |db| async move {
            sqlx::query_scalar(
                "INSERT INTO apply_pauses (id, item_id, kind, origin, excerpt, created_at) \
                 VALUES (lower(hex(randomblob(16))), ?, 'login', 'page', 'Sign in', \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) RETURNING id",
            )
            .bind(&item)
            .fetch_one(&db.pool)
            .await
            .unwrap()
        });
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert_eq!(shown["active_pause"]["id"], pause.as_str());

        let bad = json!({ "pauseId": pause, "resolution": "ended_by_page" });
        assert_eq!(h.code("apply_resolve_pause", bad), "validation");
        h.ok(
            "apply_resolve_pause",
            json!({ "pauseId": pause, "resolution": "continue" }),
        );
        assert_eq!(audited(&h, "apply_pause_resolved"), ["gui"]);
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert!(shown["active_pause"].is_null(), "{shown}");
    }

    /// Esc or the close button is not an answer: nothing starts, and the screen stays put.
    #[test]
    fn dismissing_the_release_dialog_starts_nothing() {
        let h = ready();
        let app = apply_ready(&h);
        sealed_answer(&h);
        let hook = h.app.state::<NativeHook>();
        hook.dismiss.store(true, Ordering::SeqCst);
        assert_eq!(
            h.code("apply_start", json!({ "applicationIds": [app] })),
            "cancelled"
        );
        assert_eq!(h.ok("apply_latest", json!({})), Value::Null);
    }

    /// With no consent for `Sensitive` nothing sealed can go, so nothing is asked.
    #[test]
    fn with_no_sensitive_consent_the_release_is_not_asked() {
        let h = ready();
        let app = apply_ready_with(&h, false);
        sealed_answer(&h);
        let preflight = h.ok("apply_preflight", json!({ "applicationIds": [app] }));
        assert_eq!(preflight["sensitive_consent"], false, "{preflight}");
        let run = h.ok("apply_start", json!({ "applicationIds": [app] }));
        assert!(h.asked().is_empty(), "{:?}", h.asked());
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert_eq!(shown["items"][0]["released_answers"], json!([]));
    }

    /// An answer approved while the dialog is open is not released uncounted.
    #[test]
    fn a_release_whose_count_moved_under_the_dialog_is_refused() {
        let h = ready();
        let app = apply_ready(&h);
        sealed_answer(&h);
        let paths = h.paths.clone();
        let hook = h.app.state::<NativeHook>();
        *hook.on_ask.lock().unwrap() = Some(Box::new(move || {
            let paths = paths.clone();
            Box::pin(async move {
                let db = Db::open(&paths).await.unwrap();
                use jobpilot_core::memory;
                let question = memory::add_question(&db, "Notice period?", None)
                    .await
                    .unwrap();
                let answer = memory::add_answer(&db, &question.id, "Thirty", None)
                    .await
                    .unwrap();
                memory::approve(&db, "cli", &answer.id, true).await.unwrap();
                sqlx::query("UPDATE memory_questions SET class = 'sensitive' WHERE id = ?")
                    .bind(&question.id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
                db.close().await;
            })
        }));
        h.answer(true);
        let refused = h.call("apply_start", json!({ "applicationIds": [app] }));
        let refused = refused.unwrap_err();
        assert_eq!(refused["code"], "refused");
        assert!(
            refused["message"]
                .as_str()
                .unwrap()
                .contains("release_changed")
        );
        assert_eq!(h.ok("apply_latest", json!({})), Value::Null);
    }

    #[test]
    fn apply_preflight_takes_at_most_a_run() {
        let h = ready();
        let ids: Vec<String> = (0..26).map(|n| format!("a{n}")).collect();
        assert_eq!(
            h.code("apply_preflight", json!({ "applicationIds": ids })),
            "validation"
        );
    }

    /// On every system, ConPTY included: the pause leaves the owner's own shell running.
    #[test]
    fn close_apply_leaves_a_plain_shell_session_alone_in_the_panel() {
        let h = ready();
        let panel = Panel::open(&h);
        let pty = h.app.state::<PtyState>();
        let pid = pty.pid().unwrap();
        assert!(block_on(pty.close_apply()).is_none());
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(pty.pid(), Some(pid));
        assert!(running(pid));
        drop(panel);
    }

    /// Closing the panel is a person's act: the browser goes at once and the run ends.
    #[test]
    fn closing_the_panel_ends_the_apply_run_and_its_browser() {
        use crate::pty::test_support::{gone_within_two_seconds, marked_browser};
        let h = ready();
        let run = ready_run(&h);
        let panel = Panel::open(&h);
        let pty = h.app.state::<PtyState>();
        pty.mark_apply(&h.paths, &run);
        let mut browser = marked_browser(&h.paths.apply().join(&run));

        panel.close();

        gone_within_two_seconds(&mut browser);
        wait_until(|| h.ok("apply_view", json!({ "id": run }))["state"] == "ended");
    }

    /// The real host under the panel, with the fake `claude` and a fake browser runtime: the
    /// pause kills it, and the run reads halted and ended without waiting for a stale heartbeat.
    #[cfg(unix)]
    #[test]
    fn an_emergency_pause_ends_a_hosted_run_halted() {
        let h = ready();
        let run = ready_run(&h);
        hosted_runtime(&h);
        let panel = Panel::listening(&h);
        let (cols, rows) = SIZE;
        invoke(
            &h.window,
            "pty_open_apply",
            json!({ "cols": cols, "rows": rows, "applyId": run }),
        )
        .expect("build the CLI first: cargo build -p jobpilot-cli");
        let active = || {
            let shown = h.ok("apply_view", json!({ "id": run }));
            shown["state"] == "running" && shown["items"][0]["state"] == "active"
        };
        let until = Instant::now() + PTY_WAIT;
        while !active() {
            assert!(Instant::now() < until, "never active: {}", panel.text());
            std::thread::sleep(Duration::from_millis(50));
        }

        h.ok("pause_set", json!({}));

        let agent = || {
            let id = run.clone();
            with_db(&h, |db| async move {
                sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT a.state, a.reason FROM agent_runs a \
                     JOIN apply_runs r ON r.agent_run_id = a.id WHERE r.id = ?",
                )
                .bind(&id)
                .fetch_one(&db.pool)
                .await
                .unwrap()
            })
        };
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let ended = h.ok("apply_view", json!({ "id": run }))["state"] == "ended";
            if ended && agent().0 == "halted" {
                break;
            }
            assert!(Instant::now() < until, "{:?}", agent());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(agent(), ("halted".to_owned(), Some("paused".to_owned())));
        drop(panel);
    }

    /// An installed browser runtime as `runtime::state` reads one, whose server is the mock,
    /// and `ai.claude_path` set to the scripted fake.
    #[cfg(unix)]
    fn hosted_runtime(h: &Harness) {
        use jobpilot_core::browser::runtime::PLAYWRIGHT_MCP;
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/core/tests");
        let core = core.canonicalize().unwrap();
        let runtime = h.paths.root().join("browser-runtime");
        let mcp = runtime.join("node_modules/@playwright/mcp");
        fs::create_dir_all(&mcp).unwrap();
        fs::create_dir_all(runtime.join("browsers")).unwrap();
        let snapshot = h._tmp.path().join("snapshot.yml");
        fs::copy(core.join("fixtures/forms/snapshots/form.yml"), &snapshot).unwrap();
        let manifest =
            json!({ "name": "@playwright/mcp", "version": PLAYWRIGHT_MCP, "type": "module" });
        fs::write(mcp.join("package.json"), manifest.to_string()).unwrap();
        let server = format!("file://{}", core.join("mock-mcp/server.mjs").display());
        let script = format!(
            "process.env.MOCK_SNAPSHOT = {};\nawait import({});\n",
            json!(snapshot.display().to_string()),
            json!(server),
        );
        fs::write(mcp.join("cli.js"), script).unwrap();
        let scenario = h._tmp.path().join("scenario.json");
        fs::write(
            &scenario,
            r#"[{"tool": "apply_next"}, {"tool": "browser_snapshot"}]"#,
        )
        .unwrap();
        let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("node"))
            .find(|node| node.is_file())
            .expect("node is needed for this test");
        fake_claude(
            h,
            &format!(
                "export JOBPILOT_FAKE_SCENARIO='{}'\nexec '{}' '{}' \"$@\"",
                scenario.display(),
                node.display(),
                core.join("mock-claude-apply/claude").display()
            ),
        );
    }

    /// Startup ends a `ready` run nobody opened a session for, so the next can start.
    #[test]
    fn the_startup_sweep_ends_a_stale_ready_run() {
        let h = ready();
        let run = ready_run(&h);
        let id = run.clone();
        with_db(&h, |db| async move {
            sqlx::query(
                "UPDATE apply_runs SET created_at = '2026-01-01T00:00:00.000Z' WHERE id = ?",
            )
            .bind(&id)
            .execute(&db.pool)
            .await
            .unwrap();
        });
        block_on(super::sweep_at_start(&h.app.state::<AppState>()));
        let shown = h.ok("apply_view", json!({ "id": run }));
        assert_eq!(shown["state"], "ended");
        // With no data open it does nothing and does not fail.
        let h = recovery(OPENS);
        block_on(super::sweep_at_start(&h.app.state::<AppState>()));
    }

    fn import_job(h: &Harness, url: Option<&str>) -> Value {
        let input = json!({
            "title": "Backend Engineer", "company": "Acme", "url": url,
            "text": "Build services in Rust. Remote.",
        });
        h.ok("job_import", json!({ "input": input }))
    }

    fn all_jobs(h: &Harness) -> Value {
        let filter = json!({ "include_duplicates": true });
        h.ok("jobs_list", json!({ "filter": filter, "limit": 50 }))
    }

    #[test]
    fn every_job_cmd_is_listed_and_reaches_core() {
        let h = ready();
        let job = import_job(&h, Some("https://example.com/jobs/1"));
        let id = job["id"].as_str().unwrap().to_owned();

        assert_eq!(all_jobs(&h)[0]["id"], id);
        let detail = h.ok("job_detail", json!({ "id": id }));
        assert_eq!(detail["job"]["title"], "Backend Engineer");
        assert!(detail["text"].as_str().unwrap().contains("Rust"));

        let status = h.ok("job_sources_status", json!({}));
        let names: Vec<&str> = status
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"greenhouse") && names.contains(&"lever"),
            "{names:?}"
        );

        let src = h.ok(
            "job_src_add",
            json!({ "connector": "lever", "target": "acme", "query": "" }),
        );
        assert_eq!(h.ok("job_sources_list", json!({}))[0]["id"], src["id"]);
        h.ok("job_src_remove", json!({ "id": src["id"] }));
        assert_eq!(h.ok("job_sources_list", json!({})), json!([]));

        let companies = h.ok("companies_list", json!({}));
        assert_eq!(companies[0]["name"], "Acme");
        let note = h.ok(
            "company_note_add",
            json!({ "companyId": companies[0]["id"], "text": "Small team" }),
        );
        assert_eq!(note["origin"], "pasted");

        h.ok("job_split", json!({ "id": id }));
        let old = h.ok(
            "job_paste",
            json!({ "title": "Role", "raw": "<b>Build</b>" }),
        );
        assert_eq!(old["normalized"], "Build", "job_paste keeps its shape");

        h.ok("job_remove", json!({ "id": id }));
        assert_eq!(h.code("job_detail", json!({ "id": id })), "not_found");
    }

    #[test]
    fn job_open_takes_only_an_id_and_refuses_a_non_https_url() {
        let source = include_str!("commands.rs");
        let (_, rest) = source
            .split_once("pub async fn job_open<R: Runtime>(")
            .unwrap();
        let (arguments, _) = rest.split_once(") -> ").unwrap();
        let names: Vec<&str> = arguments
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim())
            .collect();
        assert_eq!(names, ["app", "state", "id"]);

        let h = ready();
        let id = import_job(&h, Some("https://example.com/jobs/1"))["id"].clone();
        with_db(&h, |db| async move {
            sqlx::query("UPDATE jobs SET url = 'http://example.com/jobs/1'")
                .execute(&db.pool)
                .await
                .unwrap();
        });
        assert_eq!(h.code("job_open", json!({ "id": id })), "refused");
        let hook = h.app.state::<NativeHook>();
        assert!(hook.opened.lock().unwrap().is_empty());

        with_db(&h, |db| async move {
            sqlx::query("UPDATE jobs SET url = 'https://example.com/jobs/1'")
                .execute(&db.pool)
                .await
                .unwrap();
        });
        h.ok("job_open", json!({ "id": id }));
        assert_eq!(*hook.opened.lock().unwrap(), ["https://example.com/jobs/1"]);

        let bare = import_job(&h, None)["id"].clone();
        assert_eq!(h.code("job_open", json!({ "id": bare })), "refused");
    }

    #[test]
    fn job_open_refuses_a_link_with_a_user_name_or_a_port() {
        let h = ready();
        let id = import_job(&h, Some("https://example.com/jobs/1"))["id"].clone();
        let hook = h.app.state::<NativeHook>();
        for bad in [
            "https://user:secret@evil.example/x",
            "https://user@evil.example/x",
            "https://evil.example:8443/x",
        ] {
            with_db(&h, |db| async move {
                sqlx::query("UPDATE jobs SET url = ?")
                    .bind(bad)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            });
            assert_eq!(h.code("job_open", json!({ "id": id })), "refused", "{bad}");
        }
        assert!(hook.opened.lock().unwrap().is_empty());
        let input = json!({
            "title": "T", "company": "Acme", "text": "Rust",
            "url": "https://user:secret@evil.example/x",
        });
        assert_eq!(
            h.code("job_import", json!({ "input": input })),
            "validation"
        );
    }

    #[test]
    fn job_to_app_stores_a_job_with_no_company() {
        let h = ready();
        let input =
            json!({ "title": "Backend Engineer", "company": "", "url": null, "text": "Rust" });
        let job = h.ok("job_import", json!({ "input": input }));
        assert_eq!(job["company"], Value::Null);
        let app = h.ok("job_to_app", json!({ "id": job["id"] }));
        assert_eq!(app["company"], "Company not stated");
        assert_eq!(app["role"], "Backend Engineer");
    }

    #[test]
    fn a_pause_refuses_before_a_count_dialog_is_shown() {
        let h = ready();
        h.ok(
            "ai_enable",
            json!({ "provider": "claude_code", "enabled": true }),
        );
        h.ok(
            "ai_consent_give",
            json!({ "provider": "claude_code", "categories": ["job_text"] }),
        );
        let id = import_job(&h, None)["id"].clone();
        let company = h.ok("companies_list", json!({}))[0]["id"].clone();
        h.answer(true);
        h.ok("pause_set", json!({}));
        let before = h.asked().len();
        assert_eq!(
            h.code("jobs_analyze", json!({ "ids": [id], "ai": true })),
            "paused"
        );
        assert_eq!(
            h.code("job_research", json!({ "companyId": company })),
            "paused"
        );
        assert_eq!(h.asked().len(), before, "no dialog was shown");
    }

    #[test]
    fn jobs_analyze_names_the_count_natively() {
        let h = ready();
        h.ok(
            "ai_enable",
            json!({ "provider": "claude_code", "enabled": true }),
        );
        h.ok(
            "ai_consent_give",
            json!({ "provider": "claude_code", "categories": ["job_text"] }),
        );
        let id = import_job(&h, None)["id"].clone();
        let plan = h.ok("jobs_analyze_plan", json!({ "ids": [id] }));
        let calls = plan["external_calls"].as_u64().unwrap();
        assert!(calls > 0, "{plan}");

        h.answer(false);
        let args = json!({ "ids": [id], "ai": true });
        assert_eq!(h.code("jobs_analyze", args), "confirmation_required");
        let asked = h.asked();
        let dialog = asked.last().unwrap();
        assert!(
            dialog.contains(&format!(
                "This will make at most {calls} paid or subscription calls"
            )),
            "{asked:?}"
        );
        assert_eq!(h.ok("agents_list", json!({ "limit": 10 })), json!([]));
        let hook = h.app.state::<NativeHook>();
        assert_eq!(hook.work_at_dialog.lock().unwrap().last(), Some(&false));

        // A count that moves while the dialog is open is refused after the yes.
        let db = with_db(&h, |db| async move { db });
        *hook.on_ask.lock().unwrap() = Some(Box::new(move || {
            let db = db.clone();
            Box::pin(async move {
                let input = jobpilot_core::jobs::PastedJob {
                    title: "Second".into(),
                    company: String::new(),
                    url: None,
                    text: "Another posting".into(),
                };
                jobpilot_core::jobs::import_pasted(&db, input)
                    .await
                    .unwrap();
            })
        }));
        h.answer(true);
        let all = json!({ "ids": [], "ai": true });
        assert_eq!(h.code("jobs_analyze", all), "validation");

        // No model means no dialog.
        let before = h.asked().len();
        let done = h.ok("jobs_analyze", json!({ "ids": [id], "ai": false }));
        assert_eq!(done["scored"], 1);
        assert_eq!(h.asked().len(), before);
    }

    #[test]
    fn job_to_app_links_the_text_and_stores_no_note() {
        let h = ready();
        let job = import_job(&h, Some("https://example.com/jobs/1"));
        let app = h.ok("job_to_app", json!({ "id": job["id"] }));
        assert_eq!(app["company"], "Acme");
        assert_eq!(app["role"], "Backend Engineer");
        assert_eq!(app["job_text_id"], job["job_text_id"]);
        assert_eq!(app["link"], "https://example.com/jobs/1");
        assert_eq!(app["notes"], "");
        assert_eq!(app["status"], "saved");
        assert_eq!(h.ok("app_list", json!({})).as_array().unwrap().len(), 1);
    }

    #[test]
    fn jobs_prefs_are_not_reachable_through_the_generic_setting() {
        let h = ready();
        let set = json!({ "key": "jobs.prefs", "value": {} });
        assert_eq!(h.code("settings_set", set), "refused");

        let prefs = json!({
            "min_pay": { "amount": 90000, "currency": "eur", "period": "year" },
            "seniority": null,
        });
        h.ok("match_prefs_set", json!({ "prefs": prefs }));
        let stored = h.ok("match_prefs_get", json!({}));
        assert_eq!(stored["min_pay"]["currency"], "EUR");
        assert_eq!(
            h.code("match_prefs_set", json!({ "prefs": { "min_pay": { "amount": 0, "currency": "EUR", "period": "year" } } })),
            "validation"
        );
    }

    fn code_of(result: Result<Value, Value>) -> String {
        result.unwrap_err()["code"].as_str().unwrap().to_owned()
    }
}
