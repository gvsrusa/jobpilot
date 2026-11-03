//! Copilot: Rust drives the browser as an MCP client of the pinned Playwright server, fills
//! what the rules and the owner's approved answers cover, uploads the two prepared files, and
//! stops. A person reviews the form in the visible browser and clicks submit. Nothing here
//! clicks a submit button, and no model chooses a step, a file, or a tool: `map_form_field`
//! only names an answer by its id (decision 23).
//!
//! Every step asks `permissions::decide` again, so a register change stops the run. Every
//! page the run reads goes through `guard::scan`, and only a person ends a stop.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::ai::router::Router;
use crate::ai::{BoxFuture, CancellationToken, ProviderId, TaskType, spec};
use crate::applications::{self, Application};
use crate::apply::guard::{self, PauseKind};
use crate::apply::proxy::Knobs;
use crate::apply::{self, Held, Item, PAUSE_HOLD, RunView, Submitter, session};
use crate::browser::mcp::{McpClient, ToolReply};
use crate::browser::runtime::{self, BrowserSpec, RuntimeState};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::forms::map::fits;
use crate::forms::{
    self, AnswerText, Candidate, Form, MAX_MODEL_FIELDS, Mapped, Pick, ValueSource, map_field,
    map_rules, parse_snapshot,
};
use crate::memory::{self, Class, usage};
use crate::permissions::{self, Action, Decision, Grant, Mode};
use crate::runner::{self, Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};
use crate::{audit, pause, profile};

use Permission::*;

/// The review stop's text: the run fills and stops here, and the person submits.
pub const REVIEW: &str = "Review the form in the browser and click submit yourself.";
const NO_FORM: &str =
    "No form fields found on this page. Open the form in the browser, then continue.";
const OFF_ORIGIN: &str = "This page is not on the posting's site. Go back to the form in the \
                          browser, then continue.";
/// A landing page's own way to its form. Not a submit: these words never send anything.
const OPENERS: [&str; 3] = ["apply", "apply now", "apply for this job"];

static STEPS: &[StepDef] = &[
    StepDef {
        name: "open",
        needs: &[ReadApplications, BrowserDrive],
        attempts: 1,
    },
    StepDef {
        name: "detect",
        needs: &[
            BrowserDrive,
            ReadJobText,
            ReadProfileBasic,
            ReadCareerMemory,
            ReadSensitive,
            WriteCareerMemory,
            Model(TaskType::MapFormField),
        ],
        attempts: 1,
    },
    StepDef {
        name: "fill",
        needs: &[BrowserDrive, BrowserFill, BrowserUpload],
        attempts: 1,
    },
    StepDef {
        name: "review",
        needs: &[BrowserDrive, WriteApplications],
        attempts: 1,
    },
];

/// What the run holds between steps: the browser, the item, and what the page showed.
struct Driver {
    client: McpClient,
    item: Item,
    app: Application,
    files: PathBuf,
    /// The newest snapshot.
    last: String,
    /// Stops a person already answered, by `guard::hit_key`.
    acks: HashSet<String>,
    /// What the run typed into each field, by ref, so the scan knows the owner's own words.
    typed: guard::Typed,
    form: Form,
    mapped: Vec<Mapped>,
    /// Fields the browser refused to fill or upload: the person's, with the unmapped ones.
    left: Vec<String>,
    /// The item is no longer the run's: a person took it over, or a stop timed out.
    done: bool,
}

/// One Copilot run of one application. `mode` is Copilot; Autonomous would share the driver.
pub struct BrowserWorkflow<'a> {
    apply_id: String,
    application_id: String,
    mode: Mode,
    router: &'a Router,
    knobs: &'a Knobs,
    slot: Mutex<Option<Driver>>,
}

enum Step {
    Go,
    Stopped,
}

impl BrowserWorkflow<'_> {
    fn take(&self) -> Option<Driver> {
        self.slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn put(&self, driver: Driver) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(driver);
    }

    fn hold_limit(&self) -> Duration {
        match cfg!(debug_assertions) {
            true => self.knobs.hold.unwrap_or(PAUSE_HOLD),
            false => PAUSE_HOLD,
        }
    }

    /// The register's word on this mode, site, and action, read again at each step.
    async fn decided(&self, db: &Db, action: Action) -> Result<Option<&'static str>> {
        let app = applications::get(db, &self.application_id).await?;
        if !permissions::link_allowed(&app.link) {
            return Err(Error::Refused("link_not_https".into()));
        }
        let site = permissions::site_of(&app.link)?;
        let grant = Grant {
            owner_started: true,
            authorized: false,
        };
        match permissions::decide(self.mode, site, action, &grant) {
            Decision::Allow { note } => Ok(note),
            Decision::Refuse(reason) => Err(Error::Refused(reason.into())),
        }
    }

    /// A later step, on the driver the earlier ones left, put back however the step ends. A
    /// step that fails ends the browser at once.
    async fn on_driver(&self, ctx: &Ctx<'_>, index: usize) -> Result<StepOutput> {
        let Some(mut driver) = self.take() else {
            return Err(Error::Failed("browser_gone".into()));
        };
        if driver.done {
            self.put(driver);
            return Ok(StepOutput::default());
        }
        let done = match index {
            1 => self.detect(ctx, &mut driver).await,
            2 => self.fill(ctx, &mut driver).await,
            _ => self.review(ctx, &mut driver).await,
        };
        match done.is_ok() {
            true => self.put(driver),
            false => driver.client.close().await,
        }
        done
    }

    async fn open(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let db = ctx.db;
        let note = self.decided(db, Action::Drive).await?;
        let item = apply::open_assisted(
            db,
            ctx.actor,
            &self.apply_id,
            &self.application_id,
            ctx.run_id(),
            note,
        )
        .await?;
        let app = applications::get(db, &self.application_id).await?;
        for path in [&item.resume_path, &item.letter_path].into_iter().flatten() {
            if !db.paths().is_surely_inside_root(path) {
                return Err(Error::Refused("file_outside_root".into()));
            }
        }
        let dir = apply::run_dir(db, &self.apply_id);
        let files = dir.join("files");
        apply::brief::stage_files(&item, &files)?;
        let client = self.start_browser(db, &dir, &files).await?;
        let mut driver = Driver {
            client,
            item,
            app,
            files,
            last: String::new(),
            acks: HashSet::new(),
            typed: HashMap::new(),
            form: parse_snapshot(""),
            mapped: Vec::new(),
            left: Vec::new(),
            done: false,
        };
        let link = driver.app.link.trim().to_owned();
        let went = self
            .call(ctx, &mut driver, "browser_navigate", json!({ "url": link }))
            .await;
        let went = match went {
            Ok(went) => went,
            Err(error) => {
                driver.client.close().await;
                return Err(error);
            }
        };
        if went.is_error {
            driver.client.close().await;
            return Err(Error::Failed("navigate_failed".into()));
        }
        // The page is read before anything else happens on it.
        let watched = self.watch(ctx, &mut driver, went.text, false).await;
        if let Err(error) = watched {
            driver.client.close().await;
            return Err(error);
        }
        self.put(driver);
        Ok(StepOutput::default())
    }

    async fn start_browser(&self, db: &Db, dir: &Path, files: &Path) -> Result<McpClient> {
        let cli = self.knobs.playwright_cli.clone().map(OsString::from);
        let get = |name: &str| match (name, &cli) {
            ("JOBPILOT_PLAYWRIGHT_CLI", Some(cli)) if cfg!(debug_assertions) => Some(cli.clone()),
            _ => std::env::var_os(name),
        };
        let RuntimeState::Ready(found) = runtime::state_by(db, &get).await? else {
            return Err(Error::ManualRequired("no_browser_runtime".into()));
        };
        let spec = BrowserSpec {
            profile: dir.join("profile"),
            output: dir.join("output"),
        };
        std::fs::create_dir_all(&spec.output)?;
        let config = dir.join("playwright.json");
        std::fs::write(&config, runtime::playwright_config(&spec).to_string())?;
        let (program, args, env) = runtime::command_for(&found, &config, files);
        McpClient::start(&program, &args, &env, files)
            .await
            .map_err(|error| match error {
                Error::Failed(why) => Error::Failed(why),
                _ => Error::Failed("no_browser_runtime".into()),
            })
    }

    /// One call to the browser. The emergency pause and a cancel stop it before it goes.
    async fn call(
        &self,
        ctx: &Ctx<'_>,
        driver: &mut Driver,
        tool: &str,
        args: Value,
    ) -> Result<ToolReply> {
        pause::ensure_off(ctx.db).await?;
        ctx.stop_point()?;
        let reply = driver.client.call(tool, args, &ctx.cancel).await?;
        sqlx::query("UPDATE apply_items SET tool_calls = tool_calls + 1 WHERE id = ?")
            .bind(&driver.item.id)
            .execute(&ctx.db.pool)
            .await?;
        Ok(reply)
    }

    /// A fresh snapshot, or `None` when the browser refuses one (an open file chooser).
    async fn snapshot(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<Option<String>> {
        let seen = self
            .call(ctx, driver, "browser_snapshot", json!({}))
            .await?;
        Ok((!seen.is_error).then_some(seen.text))
    }

    /// Holds for a person. A take-over or a timeout ends the item as `manual_required`.
    async fn hold(
        &self,
        ctx: &Ctx<'_>,
        driver: &mut Driver,
        kind: PauseKind,
        origin: &str,
        excerpt: &str,
    ) -> Result<Step> {
        let held = apply::hold(
            ctx.db,
            &driver.item,
            kind,
            origin,
            excerpt,
            &ctx.cancel,
            self.hold_limit(),
        )
        .await?;
        let reason = match held {
            Held::Continue => return Ok(Step::Go),
            Held::TakenOver => "take_over",
            Held::TimedOut => "pause_timed_out",
        };
        self.end_item(ctx, driver, "manual_required", reason)
            .await?;
        Ok(Step::Stopped)
    }

    async fn end_item(
        &self,
        ctx: &Ctx<'_>,
        driver: &mut Driver,
        state: &str,
        reason: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE apply_items SET state = ?, reason = ? \
             WHERE id = ? AND state IN ('active', 'filled')",
        )
        .bind(state)
        .bind(reason)
        .bind(&driver.item.id)
        .execute(&ctx.db.pool)
        .await?;
        driver.done = true;
        Ok(())
    }

    /// Scans `page`, then fresh snapshots, until one shows no stop a person has not answered.
    /// Each Continue acknowledges that line and looks again. A page can open a stop and can
    /// never end one.
    async fn watch(
        &self,
        ctx: &Ctx<'_>,
        driver: &mut Driver,
        mut page: String,
        mut is_snapshot: bool,
    ) -> Result<Step> {
        loop {
            if is_snapshot {
                driver.last = page.clone();
            }
            match guard::scan(&page, &driver.acks, &driver.typed) {
                Some(hit) => {
                    if let Step::Stopped =
                        self.hold(ctx, driver, hit.kind, "page", &hit.line).await?
                    {
                        return Ok(Step::Stopped);
                    }
                    driver.acks.insert(guard::hit_key(&hit));
                }
                None if is_snapshot => return Ok(Step::Go),
                None => {}
            }
            match self.snapshot(ctx, driver).await? {
                Some(next) => {
                    page = next;
                    is_snapshot = true;
                }
                None => {
                    driver.last.clear();
                    return Ok(Step::Go);
                }
            }
        }
    }

    /// Reads the form on the page: a snapshot, scanned, then parsed. A landing page with no
    /// field gets one click on its own "Apply" button; after that, a page with no field waits
    /// for a person to open the form. Returns false when the item left the run.
    async fn read_form(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<bool> {
        let mut opened = false;
        loop {
            let Some(page) = self.snapshot(ctx, driver).await? else {
                return Err(Error::Failed("browser_gone".into()));
            };
            if let Step::Stopped = self.watch(ctx, driver, page, true).await? {
                return Ok(false);
            }
            driver.form = parse_snapshot(&driver.last);
            if !driver.form.fields.is_empty() {
                return Ok(true);
            }
            let opener = (!opened).then(|| opener(&driver.last)).flatten();
            if let Some(target) = opener {
                opened = true;
                let clicked = self
                    .call(
                        ctx,
                        driver,
                        "browser_click",
                        json!({"target": target, "element": "Apply"}),
                    )
                    .await?;
                if let Step::Stopped = self.watch(ctx, driver, clicked.text, false).await? {
                    return Ok(false);
                }
                continue;
            }
            if let Step::Stopped = self
                .hold(ctx, driver, PauseKind::Other, "guard", NO_FORM)
                .await?
            {
                return Ok(false);
            }
            opened = true;
        }
    }

    async fn detect(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Drive).await?;
        if !self.read_form(ctx, driver).await? {
            return Ok(StepOutput::default());
        }
        let provider = self.map(ctx, driver).await?;
        // Questions the form asked that nothing answered, kept with form provenance and no
        // answer, so the owner can answer them for next time (decision 25).
        let country = Some(driver.app.country.trim()).filter(|country| !country.is_empty());
        for mapped in &driver.mapped {
            if let Mapped::Person {
                field,
                why: "unmapped" | "sensitive",
            } = mapped
                && !field.label.trim().is_empty()
            {
                match memory::add_form_question(ctx.db, &field.label, country).await {
                    Ok(_) | Err(Error::Validation(_)) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(match provider {
            Some(provider) => StepOutput {
                provider: Some(provider),
                categories: spec(TaskType::MapFormField).categories,
                dropped: 0,
            },
            None => StepOutput::default(),
        })
    }

    /// Rules first; then, for an ordinary field nothing filled, `map_form_field` when a model
    /// is set up. Answer text comes from `use_answer` only for an answer a field needs.
    async fn map(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<Option<ProviderId>> {
        let db = ctx.db;
        let basic = profile::basic(db).await?;
        let application = driver.app.id.clone();
        let serving: Vec<usage::ApprovedRow> = usage::approved_for(db)
            .await?
            .into_iter()
            .filter(|row| row.serves(&driver.app.country) && !row.stale)
            .filter(|row| matches!(row.class, Class::Normal | Class::Sensitive))
            .collect();
        let labels: HashSet<String> = driver
            .form
            .fields
            .iter()
            .map(|field| forms::fold_label(&field.label))
            .collect();
        let mut answers = Vec::new();
        for row in &serving {
            if !labels.contains(&forms::fold_label(&row.question)) {
                continue;
            }
            if let Some(text) =
                answer_text(db, ctx.actor, &row.answer_id, &application, row.class).await?
            {
                answers.push(AnswerText {
                    answer_id: row.answer_id.clone(),
                    question: row.question.clone(),
                    text,
                });
            }
        }
        driver.mapped = map_rules(&driver.form, &basic, &answers);

        let candidates: Vec<Candidate> = serving
            .iter()
            .filter(|row| row.class == Class::Normal)
            .map(|row| Candidate {
                answer_id: row.answer_id.clone(),
                question: row.question.clone(),
            })
            .collect();
        if candidates.is_empty() || self.router.declarations().is_empty() {
            return Ok(None);
        }
        let mut provider = None;
        let mut asked = 0;
        for mapped in driver.mapped.iter_mut() {
            let Mapped::Person {
                field,
                why: "unmapped",
            } = mapped
            else {
                continue;
            };
            if !matches!(field.role.as_str(), "textbox" | "combobox")
                || field.label.trim().is_empty()
            {
                continue;
            }
            if asked == MAX_MODEL_FIELDS {
                break;
            }
            asked += 1;
            ctx.stop_point()?;
            let (pick, by) = map_field(
                db,
                self.router,
                ctx.actor,
                &field.label,
                &candidates,
                ctx.cancel.clone(),
            )
            .await?;
            provider = by.or(provider);
            let id = match pick {
                Pick::Answer(id) => id,
                Pick::None => continue,
                // No model took it: the rest would fare no better.
                Pick::Unavailable => break,
            };
            let Some(text) = answer_text(db, ctx.actor, &id, &application, Class::Normal).await?
            else {
                continue;
            };
            if let Some(text) = fits(field, &text) {
                *mapped = Mapped::Value {
                    field: field.clone(),
                    text,
                    source: ValueSource::Answer(id),
                };
            }
        }
        Ok(provider)
    }

    async fn fill(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Fill).await?;
        // The page the form is on must be the posting's site (M-37). Off it, a person decides.
        let home = forms::origin_of(&driver.app.link);
        while driver.form.origin.is_empty() || driver.form.origin != home {
            if let Step::Stopped = self
                .hold(ctx, driver, PauseKind::Other, "guard", OFF_ORIGIN)
                .await?
            {
                return Ok(StepOutput::default());
            }
            if !self.read_form(ctx, driver).await? {
                return Ok(StepOutput::default());
            }
            self.map(ctx, driver).await?;
        }

        let values: Vec<(String, String, String, String)> = driver
            .mapped
            .iter()
            .filter_map(|mapped| match mapped {
                Mapped::Value { field, text, .. } => Some((
                    field.target.clone(),
                    field.role.clone(),
                    field.label.clone(),
                    text.clone(),
                )),
                _ => None,
            })
            .collect();
        if !values.is_empty() {
            let fields: Vec<Value> = values
                .iter()
                .map(|(target, role, label, text)| {
                    json!({"name": label, "type": role, "target": target, "value": text})
                })
                .collect();
            let filled = self
                .call(
                    ctx,
                    driver,
                    "browser_fill_form",
                    json!({ "fields": fields }),
                )
                .await?;
            if filled.is_error {
                driver
                    .left
                    .extend(values.iter().map(|(_, _, label, _)| label.clone()));
            } else {
                driver.typed.extend(
                    values
                        .iter()
                        .filter(|(_, role, _, _)| role == "textbox")
                        .map(|(target, _, _, text)| (target.clone(), text.clone())),
                );
            }
            if let Step::Stopped = self.watch(ctx, driver, filled.text, false).await? {
                return Ok(StepOutput::default());
            }
        }

        let files: Vec<(String, String, &'static str)> = driver
            .mapped
            .iter()
            .filter_map(|mapped| match mapped {
                Mapped::File { field, kind } => {
                    Some((field.target.clone(), field.label.clone(), *kind))
                }
                _ => None,
            })
            .collect();
        for (target, label, kind) in files {
            let name = match kind {
                "resume" => "resume.pdf",
                _ => "cover-letter.pdf",
            };
            let path = driver.files.join(name);
            let path = path.to_string_lossy().into_owned();
            if !guard::upload_ok(std::slice::from_ref(&path), &driver.files) {
                // Nothing prepared for it: the person attaches one.
                driver.left.push(label);
                continue;
            }
            self.decided(ctx.db, Action::Upload).await?;
            let clicked = self
                .call(
                    ctx,
                    driver,
                    "browser_click",
                    json!({"target": target, "element": label}),
                )
                .await?;
            if clicked.is_error {
                driver.left.push(label);
                continue;
            }
            let uploaded = self
                .call(
                    ctx,
                    driver,
                    "browser_file_upload",
                    json!({ "paths": [path] }),
                )
                .await?;
            if uploaded.is_error {
                driver.left.push(label);
            } else {
                // Which file went to this item: a code, never a path or the page's words.
                let detail = json!({ "kind": kind, "mode": "copilot" });
                audit::record(
                    ctx.db,
                    ctx.actor,
                    "apply_file_uploaded",
                    Some(&driver.item.id),
                    detail,
                )
                .await?;
            }
            if let Step::Stopped = self.watch(ctx, driver, uploaded.text, false).await? {
                return Ok(StepOutput::default());
            }
        }
        sqlx::query("UPDATE apply_items SET state = 'filled' WHERE id = ? AND state = 'active'")
            .bind(&driver.item.id)
            .execute(&ctx.db.pool)
            .await?;
        driver.item.state = "filled".into();
        Ok(StepOutput::default())
    }

    /// The person reviews and submits in the browser. Continue is their word that it was
    /// submitted: the page then is captured as the page's claim, and the status is written at
    /// the run's end, by `settle`.
    async fn review(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Drive).await?;
        let left: Vec<&str> = driver
            .mapped
            .iter()
            .filter_map(|mapped| match mapped {
                Mapped::Person { field, .. } if !field.label.trim().is_empty() => {
                    Some(field.label.as_str())
                }
                _ => None,
            })
            .chain(driver.left.iter().map(String::as_str))
            .collect();
        let excerpt = match left.is_empty() {
            true => REVIEW.to_owned(),
            false => format!("{REVIEW} Left for you: {}.", left.join("; ")),
        };
        if let Step::Stopped = self
            .hold(ctx, driver, PauseKind::Other, "guard", &excerpt)
            .await?
        {
            return Ok(StepOutput::default());
        }
        let capture = self.snapshot(ctx, driver).await?;
        let by = Submitter {
            agent: "browser_application",
            mode: "copilot",
        };
        apply::record_submitted(
            ctx.db,
            ctx.actor,
            &self.apply_id,
            &driver.item,
            capture.as_deref(),
            by,
        )
        .await?;
        driver.done = true;
        Ok(StepOutput::default())
    }

    /// Ends the browser, if a step left it open.
    async fn close(&self) {
        if let Some(driver) = self.take() {
            driver.client.close().await;
        }
    }
}

/// An approved answer's text for this application, or `None` when it may not be used here: a
/// sealed one with no current confirmation, or one changed since it was listed.
async fn answer_text(
    db: &Db,
    actor: &str,
    answer_id: &str,
    application_id: &str,
    class: Class,
) -> Result<Option<String>> {
    match usage::use_answer(db, actor, answer_id, application_id).await {
        Ok(used) if used.class == class && !used.stale => Ok(Some(used.text)),
        Ok(_) | Err(Error::ConfirmationRequired | Error::Refused(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The ref of a button or link whose whole name is a landing page's way to its form.
fn opener(snapshot: &str) -> Option<String> {
    snapshot.lines().find_map(|line| {
        let (role, name, _) = guard::split_line(line);
        let target = guard::ref_of(line)?;
        (matches!(role.as_str(), "button" | "link")
            && OPENERS.contains(&forms::fold_label(&name).as_str())
            && !guard::submit_target(snapshot, &target))
        .then_some(target)
    })
}

fn step<'a>(
    workflow: &'a BrowserWorkflow<'a>,
    ctx: &'a Ctx<'a>,
    index: usize,
) -> BoxFuture<'a, Result<StepOutput>> {
    Box::pin(async move {
        match index {
            0 => workflow.open(ctx).await,
            _ => workflow.on_driver(ctx, index).await,
        }
    })
}

impl Workflow for BrowserWorkflow<'_> {
    fn agent(&self) -> Agent {
        Agent::BrowserApplication
    }

    fn input(&self) -> RunInput {
        RunInput::BrowserApply {
            apply_id: self.apply_id.clone(),
        }
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        step(self, ctx, index)
    }
}

/// A person's act: one Copilot run of one application, in a visible browser. It fills and
/// uploads, then holds until the person has reviewed and submitted, and returns the run as it
/// ended. Whatever the ending, the item is settled and the browser and run folder are gone.
pub async fn copilot(
    db: &Db,
    router: &Router,
    actor: &str,
    application_id: &str,
    cancel: CancellationToken,
) -> Result<RunView> {
    copilot_with(db, router, actor, application_id, cancel, &Knobs::default()).await
}

#[doc(hidden)]
pub async fn copilot_with(
    db: &Db,
    router: &Router,
    actor: &str,
    application_id: &str,
    cancel: CancellationToken,
    knobs: &Knobs,
) -> Result<RunView> {
    runner::person(actor)?;
    pause::ensure_off(db).await?;
    session::sweep_orphans(db).await?;
    let apply_id: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
        .fetch_one(&db.pool)
        .await?;
    let workflow = BrowserWorkflow {
        apply_id: apply_id.clone(),
        application_id: application_id.to_owned(),
        mode: Mode::Copilot,
        router,
        knobs,
        slot: Mutex::new(None),
    };
    let started = runner::start(db, actor, &workflow, cancel).await;
    workflow.close().await;
    let opened: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM apply_runs WHERE id = ?)")
        .bind(&apply_id)
        .fetch_one(&db.pool)
        .await?;
    if opened {
        session::wind_up(db, &apply_id, None, None).await?;
    }
    started?.into_result()?;
    apply::view(db, &apply_id).await
}
