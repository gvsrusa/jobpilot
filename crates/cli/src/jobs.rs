//! Job commands: a thin adapter over `jobpilot_core::jobs`, `connectors`, and `agents`.
//! Each arm parses, calls one core function as the person, and prints. Posting text is
//! untrusted: `render` in `main.rs` cleans every line before it reaches the terminal.

use std::io::Read;
use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use jobpilot_core::agents::{self, AiPlan, MatchRequest};
use jobpilot_core::ai::router::Router;
use jobpilot_core::jobs::company;
use jobpilot_core::jobs::prefs::{self, MatchPrefs};
use jobpilot_core::jobs::rules::Seniority;
use jobpilot_core::jobs::{
    self as core, AuthNote, Job, JobDetail, JobFilter, PastedJob, PayPeriod, PayRange, PayWish,
};
use jobpilot_core::{Db, Error, Result, connectors, pause};
use serde_json::{Value, json};

use crate::agents::word;
use crate::ai::with_interrupts;
use crate::{ACTOR, Output, message, out, source_reason};

/// Readers stop here: a posting is at most 200,000 characters after cleanup.
const READ_LIMIT: u64 = 4 * 1024 * 1024;

// ponytail: a list shows its first 200 jobs; there is no paging flag yet.
const LIST_LIMIT: i64 = 200;

#[derive(Clone, Copy, ValueEnum)]
pub enum ModeArg {
    Remote,
    Hybrid,
    Onsite,
}

#[derive(Subcommand)]
pub enum JobCommand {
    /// Store a posting from a file, or from standard input.
    Paste {
        title: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        company: Option<String>,
    },
    /// Store a posting's text with its link. The link is kept, never requested.
    Import {
        #[arg(long)]
        url: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        company: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Stored jobs, best score first. A filter keeps a job that does not state the field.
    List {
        #[arg(long)]
        country: Option<String>,
        #[arg(long)]
        mode: Option<ModeArg>,
        /// Amount:currency:period, like 90000:EUR:year.
        #[arg(long)]
        min_pay: Option<String>,
        /// Show duplicates too.
        #[arg(long)]
        all: bool,
    },
    Search {
        query: String,
    },
    Show {
        id: String,
    },
    Remove {
        id: String,
    },
    /// Say a job is not a duplicate.
    Split {
        id: String,
    },
    /// Each source, its read status, limit, and attribution.
    Sources,
    /// Job sources to read.
    #[command(subcommand, name = "source", visible_alias = "src")]
    Source(SourceCommand),
    /// Read the sources, or only the ones named.
    Fetch {
        #[arg(long = "src", alias = "source")]
        src: Vec<String>,
    },
    /// Score jobs by rules. With --ai a model also fills blanks and writes an evaluation.
    Analyze {
        ids: Vec<String>,
        #[arg(long)]
        ai: bool,
        /// Go ahead with the calls the count announces.
        #[arg(long)]
        yes: bool,
    },
    /// Summarize a company from its stored text.
    Research {
        company_id: String,
        #[arg(long)]
        yes: bool,
    },
    #[command(subcommand)]
    Company(CompanyCommand),
    #[command(subcommand)]
    Prefs(JobPrefsCommand),
}

#[derive(Subcommand)]
pub enum SourceCommand {
    Add {
        connector: String,
        target: Option<String>,
        #[arg(long)]
        query: Option<String>,
    },
    List,
    Remove {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum CompanyCommand {
    List,
    /// Store a note about a company, read from standard input.
    Note {
        company_id: String,
    },
}

#[derive(Subcommand)]
pub enum JobPrefsCommand {
    Show,
    /// ponytail: sets or replaces a value; clearing one is not offered yet.
    Set {
        /// Amount:currency:period, like 90000:EUR:year.
        #[arg(long)]
        min_pay: Option<String>,
        #[arg(long)]
        seniority: Option<String>,
    },
}

fn read_text(file: Option<PathBuf>) -> Result<String> {
    let mut bytes = Vec::new();
    match file {
        Some(path) => std::fs::File::open(path)?
            .take(READ_LIMIT)
            .read_to_end(&mut bytes)?,
        None => std::io::stdin()
            .lock()
            .take(READ_LIMIT)
            .read_to_end(&mut bytes)?,
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn bad(text: &str) -> Error {
    Error::Validation(text.into())
}

fn parse_pay(text: &str) -> Result<PayWish> {
    let shape = || bad("pay is amount:currency:period, like 90000:EUR:year");
    let mut parts = text.split(':');
    let (Some(amount), Some(currency), Some(period), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(shape());
    };
    let period: PayPeriod = serde_json::from_value(Value::String(period.to_ascii_lowercase()))
        .map_err(|_| bad("a pay period is hour, day, week, month, or year"))?;
    Ok(PayWish {
        amount: amount.trim().parse().map_err(|_| shape())?,
        currency: currency.trim().to_ascii_uppercase(),
        period,
    })
}

fn pay_text(pay: &Option<PayRange>) -> String {
    let Some(pay) = pay else {
        return "not stated".into();
    };
    let range = match (pay.min, pay.max) {
        (Some(min), Some(max)) => format!("{min}-{max}"),
        (Some(one), None) | (None, Some(one)) => one.to_string(),
        (None, None) => return "not stated".into(),
    };
    format!(
        "{range} {} per {}",
        pay.currency.as_deref().unwrap_or("(currency not stated)"),
        pay.period
            .map_or("(period not stated)".into(), |p| word(&p)),
    )
}

fn job_line(job: &Job) -> String {
    let mut line = format!(
        "{}  {}  {}",
        job.id,
        job.score.map_or("--".into(), |s| s.to_string()),
        job.title
    );
    for part in [job.company.as_deref(), Some(job.location.as_str())] {
        if let Some(part) = part.filter(|p| !p.is_empty()) {
            line.push_str(&format!("  {part}"));
        }
    }
    if let Some(mode) = &job.work_mode {
        line.push_str(&format!("  {mode}"));
    }
    if !job.not_stated.is_empty() {
        line.push_str(&format!("  [not stated: {}]", job.not_stated.join(", ")));
    }
    line
}

/// Lists what matched, the first `LIST_LIMIT` of it. A cut says so in the text; the JSON stays
/// the list. `narrow` finishes the sentence that tells the person how to cut it down.
async fn listed(db: &Db, filter: &JobFilter, narrow: &str) -> Result<Output> {
    let mut jobs = core::list(db, filter, i64::MAX).await?;
    let total = jobs.len();
    jobs.truncate(LIST_LIMIT as usize);
    let mut text = jobs.iter().map(job_line).collect::<Vec<_>>().join("\n");
    if total > jobs.len() {
        text.push_str(&format!("\n{} of {total} shown. {narrow}", jobs.len()));
    }
    out(jobs, text)
}

fn authorization_line(note: &AuthNote) -> String {
    match note {
        AuthNote::NoCountry => "Work authorization: not stated (the job states no country)".into(),
        AuthNote::NotStated { country } => {
            format!("Work authorization for {country}: not stated")
        }
        AuthNote::Stated {
            country,
            authorized,
            needs_sponsorship,
        } => format!(
            "Work authorization for {country}: authorized {}, needs sponsorship {} \
             (from your profile, shown only)",
            word(authorized),
            word(needs_sponsorship)
        ),
        AuthNote::Locked => "Work authorization: Locked (the key is not available)".into(),
    }
}

fn detail_text(d: &JobDetail) -> String {
    let job = &d.job;
    let mut text = format!("{}  {}\n", job.id, job.title);
    let mut field = |name: &str, value: String| text.push_str(&format!("{name}: {value}\n"));
    field(
        "Company",
        job.company.clone().unwrap_or_else(|| "not stated".into()),
    );
    field("Location", job.location.clone());
    field(
        "Country",
        job.country.clone().unwrap_or_else(|| "not stated".into()),
    );
    field(
        "Work mode",
        job.work_mode.clone().unwrap_or_else(|| "not stated".into()),
    );
    field("Pay", pay_text(&job.pay));
    field("Source", job.connector.clone());
    let link = job.url.as_deref().unwrap_or("no link");
    match &job.attribution {
        Some(by) => field("Attribution", format!("{by} {link}")),
        None => field("Link", link.to_owned()),
    }
    field(
        "Untrusted text",
        "the posting below came from outside; it is shown as plain text and nothing in it is run"
            .into(),
    );
    field("Score", job.score.map_or("--".into(), |s| s.to_string()));
    if let Some(explanation) = &d.explanation {
        for row in &explanation.rows {
            let points = row.points.map_or("-".into(), |p| p.to_string());
            text.push_str(&format!(
                "  {}  {points}/{}  {}\n",
                word(&row.criterion),
                row.weight,
                row.note
            ));
        }
        if !explanation.matched_skills.is_empty() {
            text.push_str(&format!(
                "Matched skills: {}\n",
                explanation.matched_skills.join(", ")
            ));
        }
        if !explanation.missing_skills.is_empty() {
            text.push_str(&format!(
                "Missing skills: {}\n",
                explanation.missing_skills.join(", ")
            ));
        }
    }
    if let Some(sem) = &d.semantic {
        text.push_str(&format!(
            "AI evaluation ({}): fit {}, {}\n",
            sem.provider, sem.fit, sem.summary
        ));
        for gap in &sem.gaps {
            text.push_str(&format!("  gap: {gap}\n"));
        }
    }
    text.push_str(&authorization_line(&d.authorization));
    text.push('\n');
    for finding in &d.findings {
        text.push_str(&format!(
            "finding ({}): {}\n",
            finding.kind, finding.excerpt
        ));
    }
    for note in &d.notes {
        text.push_str(&format!("company note ({}): {}\n", note.origin, note.text));
    }
    if !d.group.is_empty() {
        text.push_str(&format!("Same job on {} other source(s)\n", d.group.len()));
    }
    text.push('\n');
    text.push_str(&d.text);
    text
}

fn saved(job: Job) -> Result<Output> {
    let mut text = format!("saved {}", job.id);
    if job.findings > 0 {
        text.push_str(&format!("\n{} finding(s); see `job show`", job.findings));
    }
    out(job, text)
}

fn limit_text(status: &connectors::SourceStatus) -> String {
    match (status.min_interval_secs, status.per_day) {
        (Some(secs), Some(day)) => format!("one request per {secs}s, {day} a day"),
        (Some(secs), None) => format!("one request per {secs}s"),
        (None, Some(day)) => format!("{day} a day"),
        (None, None) => "none declared".into(),
    }
}

fn sources_out() -> Result<Output> {
    let statuses = connectors::statuses();
    let mut lines = Vec::new();
    for s in &statuses {
        let ops: Vec<String> = s.operations.iter().map(word).collect();
        lines.push(format!(
            "{}  {}  read: {}  operations: {}  limit: {}  attribution: {}  read on: {}",
            s.name,
            s.label,
            word(&s.read),
            if ops.is_empty() {
                "none".into()
            } else {
                ops.join(",")
            },
            limit_text(s),
            s.attribution.as_deref().unwrap_or("none"),
            s.date_read.as_deref().unwrap_or("-"),
        ));
        if let Some(note) = s.note {
            lines.push(format!("    {note}"));
        }
    }
    out(&statuses, lines.join("\n"))
}

async fn source(db: &Db, command: SourceCommand) -> Result<Output> {
    match command {
        SourceCommand::Add {
            connector,
            target,
            query,
        } => {
            let row = core::add_source(
                db,
                &connector,
                target.as_deref().unwrap_or(""),
                query.as_deref().unwrap_or(""),
            )
            .await?;
            let note = connectors::statuses()
                .into_iter()
                .find(|s| s.name == row.connector)
                .and_then(|s| s.note);
            let mut text = format!("saved {}  {} {}", row.id, row.connector, row.target);
            if let Some(note) = note {
                text.push_str(&format!("\n{note}"));
            }
            let mut value =
                serde_json::to_value(&row).map_err(|e| Error::Io(std::io::Error::other(e)))?;
            value["note"] = json!(note);
            out(value, text)
        }
        SourceCommand::List => {
            let rows = core::list_sources(db).await?;
            let text = rows
                .iter()
                .map(|r| {
                    let query = if r.query.is_empty() {
                        String::new()
                    } else {
                        format!("  filter: {}", r.query)
                    };
                    format!("{}  {}  {}{query}", r.id, r.connector, r.target)
                })
                .collect::<Vec<_>>()
                .join("\n");
            out(rows, text)
        }
        SourceCommand::Remove { id } => {
            core::remove_source(db, &id).await?;
            out(json!({ "id": id }), format!("removed {id}"))
        }
    }
}

async fn fetch(db: &Db, src: Vec<String>) -> Result<Output> {
    if src.is_empty() && core::list_sources(db).await?.is_empty() {
        pause::ensure_off(db).await?;
        return out(
            json!({ "sources": [], "grouped": 0 }),
            "No sources yet, so nothing was requested. Add one with `jobpilot job source add \
             <connector> <board>`."
                .into(),
        );
    }
    let report = with_interrupts(db, |cancel| agents::discover(db, ACTOR, src, cancel)).await?;
    let mut lines: Vec<String> = Vec::new();
    for s in &report.sources {
        lines.push(format!(
            "{}  {} {}: {} added, {} updated, {} skipped",
            s.source_id, s.connector, s.target, s.added, s.updated, s.skipped
        ));
        if let Some(reason) = &s.reason {
            lines.push(format!(
                "    stopped: {}",
                source_reason(reason).unwrap_or(reason)
            ));
            if let Some(at) = &s.next_at {
                lines.push(format!("    next fetch at {at} (UTC)"));
            }
        }
    }
    lines.push(format!("{} grouped as duplicates", report.grouped));
    out(&report, lines.join("\n"))
}

fn plan_text(plan: &AiPlan, again: &str) -> String {
    format!(
        "This will make at most {} paid or subscription calls for {} stored text(s). Run \
         `{again}` to go ahead.",
        plan.external_calls, plan.jobs
    )
}

async fn analyze(db: &Db, ids: Vec<String>, ai: bool, yes: bool) -> Result<Output> {
    pause::ensure_off(db).await?;
    let router = Router::from_settings(db).await?;
    if ai {
        let plan = agents::match_plan(db, &router, &ids).await?;
        if plan.external_calls > 0 && !yes {
            let again = format!("jobpilot job analyze {} --ai --yes", ids.join(" "));
            let text = plan_text(&plan, &again);
            return out(
                json!({ "external_calls": plan.external_calls, "jobs": plan.jobs }),
                text,
            );
        }
        if plan.external_calls > 0 {
            // On stderr so a JSON reader still gets clean stdout.
            eprintln!(
                "Making at most {} paid or subscription calls for {} job(s).",
                plan.external_calls, plan.jobs
            );
        }
    }
    let req = MatchRequest {
        job_ids: ids.clone(),
        ai,
    };
    let report = with_interrupts(db, |cancel| {
        agents::match_jobs(db, &router, ACTOR, req, cancel)
    })
    .await?;
    let shown = if ids.is_empty() {
        core::list(db, &JobFilter::default(), LIST_LIMIT)
            .await?
            .into_iter()
            .map(|j| j.id)
            .collect()
    } else {
        ids
    };
    let mut lines = vec![format!(
        "{} scored, {} filled by a model, {} evaluated by a model",
        report.scored, report.parsed, report.evaluated
    )];
    let mut details = Vec::new();
    for id in &shown {
        let d = core::detail(db, id).await?;
        let mut line = format!(
            "{}  {}  {}",
            d.job.id,
            d.job.score.map_or("--".into(), |s| s.to_string()),
            d.job.title
        );
        for row in d.explanation.iter().flat_map(|e| &e.rows) {
            line.push_str(&format!(
                "\n    {}  {}/{}  {}",
                word(&row.criterion),
                row.points.map_or("-".into(), |p| p.to_string()),
                row.weight,
                row.note
            ));
        }
        lines.push(line);
        details.push(json!({
            "id": d.job.id, "score": d.job.score, "explanation": d.explanation,
        }));
    }
    if let Some(reason) = &report.reason {
        lines.push(message(&Error::ManualRequired(reason.clone())));
    }
    out(
        json!({ "report": report, "jobs": details }),
        lines.join("\n"),
    )
}

async fn research(db: &Db, company_id: String, yes: bool) -> Result<Output> {
    let router = Router::from_settings(db).await?;
    let plan = agents::research_plan(db, &router, &company_id).await?;
    if plan.external_calls > 0 && !yes {
        let again = format!("jobpilot job research {company_id} --yes");
        let text = plan_text(&plan, &again);
        return out(
            json!({ "external_calls": plan.external_calls, "jobs": plan.jobs }),
            text,
        );
    }
    if plan.external_calls > 0 {
        eprintln!(
            "Making at most {} paid or subscription calls for {} stored text(s).",
            plan.external_calls, plan.jobs
        );
    }
    let report = with_interrupts(db, |cancel| {
        agents::research_company(db, &router, ACTOR, &company_id, cancel)
    })
    .await?;
    let notes = company::notes(db, &company_id).await?;
    let mut lines = vec![format!(
        "{} source(s) read, {} fact(s) kept, {} dropped",
        report.sources, report.facts, report.dropped
    )];
    if let Some(reason) = &report.reason {
        lines.push(message(&Error::ManualRequired(reason.clone())));
    }
    lines.push("Stored text:".into());
    lines.extend(notes.iter().map(|n| format!("  ({}) {}", n.origin, n.text)));
    out(
        json!({ "report": report, "notes": notes }),
        lines.join("\n"),
    )
}

async fn company(db: &Db, command: CompanyCommand) -> Result<Output> {
    match command {
        CompanyCommand::List => {
            let companies = company::list(db).await?;
            let text = companies
                .iter()
                .map(|c| {
                    format!(
                        "{}  {}  {} job(s), {} note(s)",
                        c.id, c.name, c.jobs, c.notes
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            out(companies, text)
        }
        CompanyCommand::Note { company_id } => {
            let note = company::add_note(db, ACTOR, &company_id, &read_text(None)?).await?;
            out(&note, format!("saved a note for {company_id}"))
        }
    }
}

fn prefs_text(prefs: &MatchPrefs) -> String {
    let pay = prefs.min_pay.as_ref().map_or("not set".into(), |w| {
        format!("{} {} per {}", w.amount, w.currency, word(&w.period))
    });
    let seniority = prefs.seniority.map_or("not set", Seniority::as_str);
    format!("Minimum pay: {pay}\nSeniority: {seniority}")
}

async fn job_prefs(db: &Db, command: JobPrefsCommand) -> Result<Output> {
    match command {
        JobPrefsCommand::Show => {
            let prefs = prefs::get(db).await?;
            out(&prefs, prefs_text(&prefs))
        }
        JobPrefsCommand::Set { min_pay, seniority } => {
            let mut prefs = prefs::get(db).await?;
            if let Some(pay) = min_pay {
                prefs.min_pay = Some(parse_pay(&pay)?);
            }
            if let Some(level) = seniority {
                prefs.seniority = Some(Seniority::parse(&level.to_ascii_lowercase()).ok_or_else(
                    || bad("seniority is intern, junior, mid, senior, staff, or executive"),
                )?);
            }
            prefs::set(db, &prefs).await?;
            let stored = prefs::get(db).await?;
            out(&stored, prefs_text(&stored))
        }
    }
}

pub async fn job(db: &Db, command: JobCommand) -> Result<Output> {
    match command {
        JobCommand::Paste {
            title,
            file,
            company,
        } => {
            let text = read_text(file)?;
            saved(
                core::import_pasted(
                    db,
                    PastedJob {
                        title,
                        company: company.unwrap_or_default(),
                        url: None,
                        text,
                    },
                )
                .await?,
            )
        }
        JobCommand::Import {
            url,
            title,
            company,
            file,
        } => {
            let text = read_text(file)?;
            saved(
                core::import_pasted(
                    db,
                    PastedJob {
                        title,
                        company: company.unwrap_or_default(),
                        url: Some(url),
                        text,
                    },
                )
                .await?,
            )
        }
        JobCommand::List {
            country,
            mode,
            min_pay,
            all,
        } => {
            let filter = JobFilter {
                country,
                work_mode: mode.map(|m| {
                    match m {
                        ModeArg::Remote => "remote",
                        ModeArg::Hybrid => "hybrid",
                        ModeArg::Onsite => "onsite",
                    }
                    .to_owned()
                }),
                min_pay: min_pay.as_deref().map(parse_pay).transpose()?,
                query: None,
                include_duplicates: all,
            };
            listed(db, &filter, "Narrow with --country, --mode, or --min-pay.").await
        }
        JobCommand::Search { query } => {
            let filter = JobFilter {
                query: Some(query),
                ..JobFilter::default()
            };
            listed(db, &filter, "Narrow the query.").await
        }
        JobCommand::Show { id } => {
            let detail = core::detail(db, &id).await?;
            out(&detail, detail_text(&detail))
        }
        JobCommand::Remove { id } => {
            core::remove(db, ACTOR, &id).await?;
            out(json!({ "id": id }), format!("removed {id}"))
        }
        JobCommand::Split { id } => {
            core::split(db, ACTOR, &id).await?;
            out(json!({ "id": id }), format!("{id} is no longer grouped"))
        }
        JobCommand::Sources => sources_out(),
        JobCommand::Source(command) => source(db, command).await,
        JobCommand::Fetch { src } => fetch(db, src).await,
        JobCommand::Analyze { ids, ai, yes } => analyze(db, ids, ai, yes).await,
        JobCommand::Research { company_id, yes } => research(db, company_id, yes).await,
        JobCommand::Company(command) => company(db, command).await,
        JobCommand::Prefs(command) => job_prefs(db, command).await,
    }
}
