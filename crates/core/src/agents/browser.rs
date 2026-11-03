//! Copilot and Autonomous: Rust drives the browser as an MCP client of the pinned Playwright
//! server, fills what the rules and the owner's approved answers cover, and uploads the two
//! prepared files. Copilot then stops: a person reviews the form in the visible browser and
//! clicks submit. Autonomous checks the page again and clicks submit once, and only on a site
//! with an `allowed` row, a verified submit, and the owner's grant (decision 24). No model
//! chooses a step, a file, or a tool: Copilot's `map_form_field` only names an answer by its
//! id (decision 23), and Autonomous calls no model at all.
//!
//! Every step asks `permissions::decide` again, so a register change or a revoked grant stops
//! the run. Every page the run reads goes through `guard::scan`, and only a person ends a stop.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::ai::register::REGISTER;
use crate::ai::router::Router;
use crate::ai::{BoxFuture, CancellationToken, ProviderId, TaskType, spec};
use crate::applications::{self, Application};
use crate::apply::brief::{Brief, BriefAnswer};
use crate::apply::guard::{self, PauseKind};
use crate::apply::proxy::Knobs;
use crate::apply::proxy::judge_field;
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
use crate::permissions::{self, Action, Decision, Grant, Mode, Site, VERIFIED};
use crate::runner::{self, Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};
use crate::{audit, pause, profile};

use Permission::*;

/// The review stop's text: the run fills and stops here, and the person submits.
pub const REVIEW: &str = "Review the form in the browser and click submit yourself.";
const NO_FORM: &str = "Open the application form, then press Continue.";
/// Autonomous: fields the rules left, before anything is submitted.
const LEFT: &str = "JobPilot will not submit with these empty. Fill them in the browser, then \
                    continue:";
/// Autonomous: the page is not what the run filled, or not what the last stop showed (M-37).
const CHANGED: &str = "The form changed after JobPilot filled it. Check it in the browser, \
                       and leave it as it is: Continue submits it only if nothing changes \
                       again.";
const OFF_ORIGIN: &str = "This page is not on the posting's site. Go back to the form in the \
                          browser, then continue.";
/// A landing page's own way to its form, followed only as a link.
const OPENERS: [&str; 3] = ["apply", "apply now", "apply for this job"];

static COPILOT_STEPS: &[StepDef] = &[
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

/// Copilot's steps with no model, then a check of the page, one click, and the capture. Only
/// `submit` holds `BrowserSubmit`, and no step is tried twice.
static AUTONOMOUS_STEPS: &[StepDef] = &[
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
        ],
        attempts: 1,
    },
    StepDef {
        name: "fill",
        needs: &[BrowserDrive, BrowserFill, BrowserUpload],
        attempts: 1,
    },
    StepDef {
        name: "check",
        needs: &[BrowserDrive],
        attempts: 1,
    },
    StepDef {
        name: "submit",
        needs: &[BrowserDrive, BrowserSubmit, WriteApplications],
        attempts: 1,
    },
    StepDef {
        name: "confirm",
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
    /// Fields the browser refused to fill or upload, by ref and label: the person's, with the
    /// unmapped ones.
    left: Vec<(String, String)>,
    /// Autonomous: the controls the page showed when the fill began, by role and name.
    base: Option<Vec<Control>>,
    /// Autonomous: the page state a check or a person accepted, which the click must find.
    accepted: Option<Vec<Control>>,
    /// The item is no longer the run's: a person took it over, or a stop timed out.
    done: bool,
}

/// One Copilot or Autonomous run of one application.
pub struct BrowserWorkflow<'a> {
    apply_id: String,
    application_id: String,
    mode: Mode,
    /// Copilot's model for `map_form_field`. Autonomous has none: rules mapping only.
    router: Option<&'a Router>,
    /// What `permissions::decide_on` reads: `REGISTER` and `VERIFIED` outside tests.
    register: &'a str,
    verified: &'a [Site],
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

    fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Autonomous => "autonomous",
            _ => "copilot",
        }
    }

    /// The register's word on this mode, site, and action, read again at each step, with the
    /// owner's stored grant as it stands now.
    async fn decided(&self, db: &Db, action: Action) -> Result<Option<&'static str>> {
        let app = applications::get(db, &self.application_id).await?;
        if !permissions::link_allowed(&app.link) {
            return Err(Error::Refused("link_not_https".into()));
        }
        let site = permissions::site_of(&app.link)?;
        let authorized = match self.mode {
            Mode::Autonomous => permissions::granted(db, site).await?,
            _ => false,
        };
        let grant = Grant {
            owner_started: true,
            authorized,
        };
        let decided = permissions::decide_on(
            self.register,
            self.verified,
            self.mode,
            site,
            action,
            &grant,
        );
        match decided {
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
        let done = match self.steps()[index].name {
            "detect" => self.detect(ctx, &mut driver).await,
            "fill" => self.fill(ctx, &mut driver).await,
            "check" => self.check(ctx, &mut driver).await,
            "submit" => self.submit(ctx, &mut driver).await,
            "confirm" => self.confirm(ctx, &mut driver).await,
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
        if self.mode == Mode::Autonomous {
            // Submission is never retried. The preflight blocks any sign of one in any mode;
            // this belt also refuses a status past `saved`, which the preflight names
            // `not_saved`.
            let app = applications::get(db, &self.application_id).await?;
            if app.status != "saved" || apply::submitted_before(db, &app.id).await? {
                return Err(Error::Refused("already_submitted".into()));
            }
        }
        let item = apply::open_assisted(
            db,
            ctx.actor,
            &self.apply_id,
            &self.application_id,
            ctx.run_id(),
            self.mode,
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
            base: None,
            accepted: None,
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
    /// field gets one click on its own "Apply" link; after that, or with only a button, a page
    /// with no field waits for a person to open the form. Returns false when the item left the
    /// run.
    // ponytail: one page. Copilot fills the page the form opens on; a later page of a
    // multi-page form is the person's, after the review stop.
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
        // Off the posting's site a person decides first (M-37), before anything on that page
        // is mapped or kept: an off-origin page's labels never reach career memory.
        if !self.on_origin(ctx, driver).await? {
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
    /// is set up. Every field passes the guards apply-mcp runs (`proxy::judge_field`, and
    /// `guard::value_ok`), judged on the snapshot whose refs the fill uses: a field they hold for
    /// a person is the person's, whatever a rule, an answer, or a model would put there, and a
    /// legal stop a person answered never opens its field to Copilot. Answer text comes from
    /// `use_answer` only for a field that will take it.
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

        // Fields the guards hold whatever value would go in: a legal one, by its label or the
        // questions around it.
        let bare = self.brief(driver, &basic, Vec::new());
        let held: HashMap<String, &'static str> = driver
            .form
            .fields
            .iter()
            .filter_map(|field| {
                held_back(&driver.last, field, &bare).map(|why| (field.target.clone(), why))
            })
            .collect();

        // The rules alone show which fields are still open; only those read an answer.
        let mut open: HashSet<String> = HashSet::new();
        for mapped in map_rules(&driver.form, &basic, &[]) {
            if let Mapped::Person {
                field,
                why: "unmapped" | "sensitive",
            } = &mapped
                && !held.contains_key(&field.target)
            {
                open.insert(forms::fold_label(&field.label));
            }
        }
        let mut answers = Vec::new();
        for row in &serving {
            if !open.contains(&forms::fold_label(&row.question)) {
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
        hold_back_all(&mut driver.mapped, &held);

        // Only ordinary answers are offered to a model, by id and question.
        let candidates: Vec<Candidate> = serving
            .iter()
            .filter(|row| row.class == Class::Normal)
            .map(|row| Candidate {
                answer_id: row.answer_id.clone(),
                question: row.question.clone(),
            })
            .collect();
        let mut provider = None;
        // Autonomous has no router: rules mapping only, and no model is ever called.
        if let Some(router) = self.router
            && !candidates.is_empty()
            && !router.declarations().is_empty()
        {
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
                    router,
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
                let Some(text) =
                    answer_text(db, ctx.actor, &id, &application, Class::Normal).await?
                else {
                    continue;
                };
                if let Some(text) = fits(field, &text) {
                    answers.push(AnswerText {
                        answer_id: id.clone(),
                        question: candidates
                            .iter()
                            .find(|c| c.answer_id == id)
                            .map(|c| c.question.clone())
                            .unwrap_or_default(),
                        text: text.clone(),
                    });
                    *mapped = Mapped::Value {
                        field: field.clone(),
                        text,
                        source: ValueSource::Answer(id),
                    };
                }
            }
        }

        // Each value, as it will be typed or selected, through every guard.
        let brief = self.brief(driver, &basic, answers);
        let allowed = apply::brief::allowed_values(&brief);
        for mapped in driver.mapped.iter_mut() {
            let Mapped::Value { field, text, .. } = mapped else {
                continue;
            };
            let why = if !guard::value_ok(text, &allowed) {
                Some("unapproved")
            } else {
                let picks = field.role == "combobox";
                match judge_field(&driver.last, &field.target, text, picks, &brief) {
                    None => Some("unlabeled"),
                    Some(Ok(())) => None,
                    Some(Err(kind)) => Some(person_why(kind)),
                }
            };
            if let Some(why) = why {
                *mapped = Mapped::Person {
                    field: field.clone(),
                    why,
                };
            }
        }
        Ok(provider)
    }

    /// What the guards judge a value against: the profile's facts and the answers read for
    /// this form. No work authorization: Copilot never fills it.
    fn brief(
        &self,
        driver: &Driver,
        basic: &profile::ProfileBasic,
        answers: Vec<AnswerText>,
    ) -> Brief {
        Brief {
            application_id: driver.app.id.clone(),
            company: driver.app.company.clone(),
            role: driver.app.role.clone(),
            link: driver.app.link.clone(),
            country: driver.app.country.clone(),
            facts: apply::brief::facts_of(basic),
            answers: answers
                .into_iter()
                .map(|answer| BriefAnswer {
                    question: answer.question,
                    answer: answer.text,
                })
                .collect(),
            work_authorization: None,
            files: Vec::new(),
        }
    }

    /// Holds while the form read last is off the posting's site, reading it again after each
    /// Continue. False when the hold ended the item or the read found no form.
    async fn on_origin(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<bool> {
        let home = forms::origin_of(&driver.app.link);
        while driver.form.origin.is_empty() || driver.form.origin != home {
            if let Step::Stopped = self
                .hold(ctx, driver, PauseKind::Other, "guard", OFF_ORIGIN)
                .await?
            {
                return Ok(false);
            }
            if !self.read_form(ctx, driver).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn fill(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Fill).await?;
        // The page the form is on must be the posting's site (M-37). Off it, a person decides.
        let home = forms::origin_of(&driver.app.link);
        while driver.form.origin.is_empty() || driver.form.origin != home {
            if !self.on_origin(ctx, driver).await? {
                return Ok(StepOutput::default());
            }
            self.map(ctx, driver).await?;
        }
        // What the page showed before anything went in: what Autonomous checks against.
        driver.base = page_state(&driver.last);

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
                driver.left.extend(
                    values
                        .iter()
                        .map(|(target, _, label, _)| (target.clone(), label.clone())),
                );
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
                driver.left.push((target.clone(), label));
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
                driver.left.push((target.clone(), label));
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
                driver.left.push((target.clone(), label));
            } else {
                // Which file went to this item: a code, never a path or the page's words.
                let detail = json!({ "kind": kind, "mode": self.mode_name() });
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
    /// the run's end, by `settle`. A pause or cancel that lands after Continue, before the
    /// record, leaves a submission that may have happened: the item ends `uncertain`. One
    /// before Continue leaves it `filled`, which ends `skipped`: nothing was sent.
    async fn review(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Drive).await?;
        let left = left_for_person(driver);
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
        // Not scanned: the item is the person's submission, and this run has no next item
        // for a stop to protect.
        let capture = match self.snapshot(ctx, driver).await {
            Ok(capture) => capture,
            Err(error @ (Error::Paused | Error::Cancelled)) => {
                self.end_item(ctx, driver, "uncertain", "stopped_after_review")
                    .await?;
                return Err(error);
            }
            // The browser is gone: the person's word stands, with no page.
            Err(_) => None,
        };
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

    /// Autonomous, before anything is sent. A page with a legal declaration or an identity
    /// request, answered or not, is never submitted by the run: the person submits it. A
    /// field the rules left pauses for a person. Then the page is read again and must be the
    /// one `fill` wrote; any change pauses (M-37), and see [`Self::verify`].
    async fn check(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Drive).await?;
        let page = driver.last.clone();
        if self.legal_on(ctx, driver, &page).await? {
            self.end_item(ctx, driver, "manual_required", "legal_declaration")
                .await?;
            return Ok(StepOutput::default());
        }
        let left = left_for_person(driver);
        if !left.is_empty() {
            let excerpt = format!("{LEFT} {}.", left.join("; "));
            if let Step::Stopped = self
                .hold(ctx, driver, PauseKind::Other, "guard", &excerpt)
                .await?
            {
                return Ok(StepOutput::default());
            }
        }
        driver.accepted = self.verify(ctx, driver, None).await?;
        Ok(StepOutput::default())
    }

    /// Reads the page until it is the one expected: with `shown` `None`, the page `fill`
    /// wrote; else exactly the controls `shown` holds. Controls are matched by role and name,
    /// never by ref, and two with one role and name stop the run. A change pauses, and
    /// Continue authorizes only the page that stop showed: the next read must equal it, or
    /// it pauses again. Off the posting's site, with a legal declaration, a choice it cannot
    /// read back, or an ambiguous control, the item ends and the person submits. Returns the
    /// accepted controls, with `driver.form` read from the same snapshot, or `None` when the
    /// item left the run.
    async fn verify(
        &self,
        ctx: &Ctx<'_>,
        driver: &mut Driver,
        mut shown: Option<Vec<Control>>,
    ) -> Result<Option<Vec<Control>>> {
        let home = forms::origin_of(&driver.app.link);
        loop {
            let Some(page) = self.snapshot(ctx, driver).await? else {
                return Err(Error::Failed("browser_gone".into()));
            };
            driver.last = page.clone();
            if self.legal_on(ctx, driver, &page).await? {
                self.end_item(ctx, driver, "manual_required", "legal_declaration")
                    .await?;
                return Ok(None);
            }
            if let Some(hit) = guard::scan(&page, &driver.acks, &driver.typed) {
                if let Step::Stopped = self.hold(ctx, driver, hit.kind, "page", &hit.line).await? {
                    return Ok(None);
                }
                driver.acks.insert(guard::hit_key(&hit));
                continue;
            }
            let form = parse_snapshot(&page);
            if form.origin.is_empty() || form.origin != home {
                self.end_item(ctx, driver, "skipped", "origin_changed")
                    .await?;
                return Ok(None);
            }
            let (Some(now), Some(base)) = (page_state(&page), driver.base.clone()) else {
                self.end_item(ctx, driver, "manual_required", "ambiguous_form")
                    .await?;
                return Ok(None);
            };
            let same = match &shown {
                Some(shown) => *shown == now,
                None => match as_filled(driver, &base, &now) {
                    Ok(same) => same,
                    Err(why) => {
                        self.end_item(ctx, driver, "manual_required", why).await?;
                        return Ok(None);
                    }
                },
            };
            if same && form.submit.is_some() {
                driver.form = form;
                return Ok(Some(now));
            }
            if let Step::Stopped = self
                .hold(ctx, driver, PauseKind::Other, "guard", CHANGED)
                .await?
            {
                return Ok(None);
            }
            shown = Some(now);
        }
    }

    /// A legal declaration or identity request anywhere on `page`, answered or not, or a
    /// field the guards hold as legal.
    async fn legal_on(&self, ctx: &Ctx<'_>, driver: &Driver, page: &str) -> Result<bool> {
        let mut seen = HashSet::new();
        while let Some(hit) = guard::scan(page, &seen, &driver.typed) {
            if matches!(hit.kind, PauseKind::Legal | PauseKind::Identity) {
                return Ok(true);
            }
            seen.insert(guard::hit_key(&hit));
        }
        let basic = profile::basic(ctx.db).await?;
        let bare = self.brief(driver, &basic, Vec::new());
        Ok(parse_snapshot(page)
            .fields
            .iter()
            .any(|field| held_back(page, field, &bare) == Some("legal")))
    }

    /// One click on the submit button of a page read just before it, which must be the page
    /// the check or a person accepted. The click is counted in the database before it is
    /// sent, in one statement that also requires the emergency pause off and the grant still
    /// stored, so a crash, a lost reply, or a timeout after it reads `uncertain`, and nothing
    /// here or in a later run clicks again.
    async fn submit(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        self.decided(ctx.db, Action::Submit).await?;
        let Some(accepted) = driver.accepted.clone() else {
            return Err(Error::Failed("not_checked".into()));
        };
        if self.verify(ctx, driver, Some(accepted)).await?.is_none() {
            return Ok(StepOutput::default());
        }
        let Some(target) = driver.form.submit.clone() else {
            return Err(Error::Failed("not_checked".into()));
        };
        let site = permissions::site_of(&driver.app.link)?;
        let connector = permissions::connector_of(site).unwrap_or("");
        pause::ensure_off(ctx.db).await?;
        ctx.stop_point()?;
        let claimed = sqlx::query(
            "UPDATE apply_items SET submit_clicks = 1 \
             WHERE id = ? AND submit_clicks = 0 AND state IN ('active', 'filled') \
             AND NOT EXISTS (SELECT 1 FROM emergency_pause WHERE id = 'me') \
             AND EXISTS (SELECT 1 FROM autonomous_grants WHERE connector = ?)",
        )
        .bind(&driver.item.id)
        .bind(connector)
        .execute(&ctx.db.pool)
        .await?
        .rows_affected();
        if claimed != 1 {
            pause::ensure_off(ctx.db).await?;
            if !permissions::granted(ctx.db, site).await? {
                return Err(Error::Refused("not_authorized".into()));
            }
            return Err(Error::Refused("already_submitted".into()));
        }
        driver.item.submit_clicks = 1;
        let clicked = driver
            .client
            .call(
                "browser_click",
                json!({"target": target, "element": "Submit"}),
                &ctx.cancel,
            )
            .await?;
        sqlx::query("UPDATE apply_items SET tool_calls = tool_calls + 1 WHERE id = ?")
            .bind(&driver.item.id)
            .execute(&ctx.db.pool)
            .await?;
        if clicked.is_error {
            // The browser said no, and whether anything was sent is unknown.
            self.end_item(ctx, driver, "uncertain", "submit_unconfirmed")
                .await?;
        }
        Ok(StepOutput::default())
    }

    /// The page after the click, kept as the page's claim (M-39). A page that still shows the
    /// submit button, a stop, or no snapshot is doubt: `uncertain`, and never another click.
    /// No register check here: a submission already made is recorded whatever the rows say.
    async fn confirm(&self, ctx: &Ctx<'_>, driver: &mut Driver) -> Result<StepOutput> {
        let capture = self.snapshot(ctx, driver).await?;
        let doubt = match (&capture, &driver.form.submit) {
            (None, _) => true,
            (Some(page), submit) => {
                submit
                    .as_deref()
                    .is_some_and(|target| guard::submit_target(page, target))
                    || guard::scan(page, &driver.acks, &driver.typed).is_some()
            }
        };
        if doubt {
            self.end_item(ctx, driver, "uncertain", "submit_unconfirmed")
                .await?;
            return Ok(StepOutput::default());
        }
        let by = Submitter {
            agent: "browser_application",
            mode: "autonomous",
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

/// The labels a person fills, quoted: fields the rules left, and fields the browser refused.
fn left_for_person(driver: &Driver) -> Vec<String> {
    driver
        .mapped
        .iter()
        .filter_map(|mapped| match mapped {
            Mapped::Person { field, .. } if !field.label.trim().is_empty() => {
                Some(field.label.clone())
            }
            _ => None,
        })
        .chain(driver.left.iter().map(|(_, label)| label.clone()))
        .map(|label| quoted(&label))
        .collect()
}

/// One control a page shows, matched by role and name, never by ref: a text field's text,
/// whether a box or radio is checked, a choice's selection, or what a button shows (a file
/// input's chosen file, where the page shows it).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Control {
    role: String,
    name: String,
    value: String,
}

const CONTROL_ROLES: [&str; 6] = [
    "textbox", "combobox", "listbox", "checkbox", "radio", "button",
];

/// Every control on the page, sorted, or `None` when two share a role and a name: which one
/// is which cannot be told without trusting refs.
/// ponytail: radios in two groups with the same option names read as ambiguous, and stop.
fn page_state(snapshot: &str) -> Option<Vec<Control>> {
    let lines: Vec<&str> = snapshot.lines().collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let marked = |line: &str, mark: &str| {
        line.match_indices(mark)
            .any(|(at, _)| guard::outside_quotes(line, at))
    };
    let mut controls = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("- ") {
            continue;
        }
        let (role, name, inline) = guard::split_line(line);
        if !CONTROL_ROLES.contains(&role.as_str()) {
            continue;
        }
        let value = match role.as_str() {
            "checkbox" | "radio" => match marked(line, "[checked]") {
                true => "checked".to_owned(),
                false => String::new(),
            },
            "combobox" | "listbox" if inline.trim().is_empty() => {
                let depth = indent(line);
                lines[index + 1..]
                    .iter()
                    .take_while(|next| next.trim().is_empty() || indent(next) > depth)
                    .filter(|next| {
                        guard::split_line(next).0 == "option" && marked(next, "[selected]")
                    })
                    .map(|next| guard::split_line(next).1)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => inline.trim().to_owned(),
        };
        controls.push(Control { role, name, value });
    }
    controls.sort();
    controls
        .windows(2)
        .all(|pair| (&pair[0].role, &pair[0].name) != (&pair[1].role, &pair[1].name))
        .then_some(controls)
}

/// `now` is the page `fill` worked on, as `base` showed it: the same controls by role and
/// name, and each value the run wrote read back as written. A field a person fills is not
/// compared. `Err` names why it cannot be told: a choice the snapshot shows no selection for.
fn as_filled(
    driver: &Driver,
    base: &[Control],
    now: &[Control],
) -> std::result::Result<bool, &'static str> {
    let keys = |controls: &[Control]| -> Vec<(String, String)> {
        controls
            .iter()
            .map(|control| (control.role.clone(), control.name.clone()))
            .collect()
    };
    if keys(base) != keys(now) {
        return Ok(false);
    }
    for mapped in &driver.mapped {
        let Mapped::Value { field, text, .. } = mapped else {
            continue;
        };
        if driver
            .left
            .iter()
            .any(|(target, _)| *target == field.target)
        {
            continue;
        }
        let Some(shown) = now
            .iter()
            .find(|control| control.role == field.role && control.name == field.label)
        else {
            return Ok(false);
        };
        if field.role == "combobox" && shown.value.is_empty() {
            return Err("choice_unverified");
        }
        if shown.value != *text {
            return Ok(false);
        }
    }
    Ok(true)
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

/// The guards' word on a field before any value: a legal label, or a combobox whose
/// questions around it are legal. `None` lets the field be mapped; its value is judged again.
fn held_back(snapshot: &str, field: &forms::FormField, bare: &Brief) -> Option<&'static str> {
    let legal = |checked: Option<std::result::Result<(), PauseKind>>| {
        matches!(checked, Some(Err(PauseKind::Legal)))
    };
    match field.role.as_str() {
        "textbox" => {
            legal(judge_field(snapshot, &field.target, "", false, bare)).then_some("legal")
        }
        "combobox" => field
            .options
            .iter()
            .any(|option| legal(judge_field(snapshot, &field.target, option, true, bare)))
            .then_some("legal"),
        _ => None,
    }
}

fn hold_back_all(mapped: &mut [Mapped], held: &HashMap<String, &'static str>) {
    for entry in mapped.iter_mut() {
        if matches!(entry, Mapped::File { .. }) {
            continue;
        }
        if let Some(why) = held.get(&entry.field().target) {
            *entry = Mapped::Person {
                field: entry.field().clone(),
                why,
            };
        }
    }
}

fn person_why(kind: PauseKind) -> &'static str {
    match kind {
        PauseKind::Legal => "legal",
        PauseKind::Sensitive => "sensitive",
        _ => "guard",
    }
}

/// A label as the review stop lists it: quoted and cut, so page text reads as a name.
fn quoted(label: &str) -> String {
    const LONGEST: usize = 60;
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    match label.chars().count() > LONGEST {
        true => format!("\"{}...\"", label.chars().take(LONGEST).collect::<String>()),
        false => format!("\"{label}\""),
    }
}

/// The ref of a link whose whole name is a landing page's way to its form. Never a button: a
/// button named "Apply" may send the application (decision 3), so only a person presses one.
fn opener(snapshot: &str) -> Option<String> {
    snapshot.lines().find_map(|line| {
        let (role, name, _) = guard::split_line(line);
        let target = guard::ref_of(line)?;
        (role == "link"
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
        match self.mode {
            Mode::Autonomous => AUTONOMOUS_STEPS,
            _ => COPILOT_STEPS,
        }
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
    let workflow = |apply_id| BrowserWorkflow {
        apply_id,
        application_id: application_id.to_owned(),
        mode: Mode::Copilot,
        router: Some(router),
        register: REGISTER,
        verified: VERIFIED,
        knobs,
        slot: Mutex::new(None),
    };
    run_one(db, actor, cancel, workflow).await
}

/// A person's act: one Autonomous run of one application. Refused, with no browser started,
/// unless the site's submit row is `allowed`, the site is in `permissions::VERIFIED`, and the
/// owner granted its connector: today no site is. It fills by rules only, stops for anything
/// they leave and for any change on the page, clicks submit once, and captures the result.
pub async fn autonomous(
    db: &Db,
    actor: &str,
    application_id: &str,
    cancel: CancellationToken,
) -> Result<RunView> {
    autonomous_with(
        db,
        REGISTER,
        VERIFIED,
        actor,
        application_id,
        cancel,
        &Knobs::default(),
    )
    .await
}

/// The decision an Autonomous run's first step makes, with no browser, no run row, and no
/// dialog: `Ok` with the application only when its site's submit row is `allowed`, the site
/// is verified, and the owner granted its connector. A caller asks the person after this.
pub async fn autonomous_check(db: &Db, application_id: &str) -> Result<Application> {
    let app = applications::get(db, application_id).await?;
    if !permissions::link_allowed(&app.link) {
        return Err(Error::Refused("link_not_https".into()));
    }
    let site = permissions::site_of(&app.link)?;
    let grant = Grant {
        owner_started: true,
        authorized: permissions::granted(db, site).await?,
    };
    match permissions::decide(Mode::Autonomous, site, Action::Submit, &grant) {
        Decision::Allow { .. } => Ok(app),
        Decision::Refuse(reason) => Err(Error::Refused(reason.into())),
    }
}

/// The register decision a Copilot run's first step makes, with no browser and no run row:
/// `Ok` with the application when its site lets Copilot fill. A caller may say a run is
/// starting only after this.
pub async fn copilot_check(db: &Db, application_id: &str) -> Result<Application> {
    let app = applications::get(db, application_id).await?;
    if !permissions::link_allowed(&app.link) {
        return Err(Error::Refused("link_not_https".into()));
    }
    let site = permissions::site_of(&app.link)?;
    let grant = Grant {
        owner_started: true,
        authorized: permissions::granted(db, site).await?,
    };
    match permissions::decide(Mode::Copilot, site, Action::Fill, &grant) {
        Decision::Allow { .. } => Ok(app),
        Decision::Refuse(reason) => Err(Error::Refused(reason.into())),
    }
}

/// [`autonomous`] on any register text and verified list, so a test can make a site eligible.
#[doc(hidden)]
pub async fn autonomous_with(
    db: &Db,
    register: &str,
    verified: &[Site],
    actor: &str,
    application_id: &str,
    cancel: CancellationToken,
    knobs: &Knobs,
) -> Result<RunView> {
    // Only a debug build takes another register or verified list: a release build always
    // decides on the shipped ones, whatever a caller passes.
    let (register, verified) = match cfg!(debug_assertions) {
        true => (register, verified),
        false => (REGISTER, VERIFIED),
    };
    let workflow = |apply_id| BrowserWorkflow {
        apply_id,
        application_id: application_id.to_owned(),
        mode: Mode::Autonomous,
        router: None,
        register,
        verified,
        knobs,
        slot: Mutex::new(None),
    };
    run_one(db, actor, cancel, workflow).await
}

/// Runs the workflow on the runner, then, whatever the ending, `finish_items`, `settle`, and
/// `session::cleanup`, so the browser and run folder are gone and the run row is ended.
async fn run_one<'a>(
    db: &Db,
    actor: &str,
    cancel: CancellationToken,
    workflow: impl FnOnce(String) -> BrowserWorkflow<'a>,
) -> Result<RunView> {
    runner::person(actor)?;
    pause::ensure_off(db).await?;
    session::sweep_orphans(db).await?;
    let apply_id: String = sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
        .fetch_one(&db.pool)
        .await?;
    let workflow = workflow(apply_id.clone());
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
