//! Applications you are tracking.

use clap::{Args, Subcommand};
use jobpilot_core::applications::{self, Application, ApplicationInput};
use jobpilot_core::{Db, Result, agents};

use crate::ai::with_interrupts;
use crate::{ACTOR, Output, out};

#[derive(Args)]
pub struct AddArgs {
    #[arg(long)]
    company: String,
    #[arg(long)]
    role: String,
    /// A stored job posting, by id.
    #[arg(long = "job")]
    job: Option<String>,
    /// A resume draft, by id.
    #[arg(long)]
    resume: Option<String>,
    /// A cover letter draft, by id.
    #[arg(long)]
    letter: Option<String>,
    #[arg(long)]
    link: Option<String>,
    /// Two letters, like PT.
    #[arg(long)]
    country: Option<String>,
    #[arg(long)]
    notes: Option<String>,
}

/// `add` with every flag optional: a flag left out keeps what is stored.
#[derive(Args)]
pub struct EditArgs {
    id: String,
    #[arg(long)]
    company: Option<String>,
    #[arg(long)]
    role: Option<String>,
    #[arg(long = "job")]
    job: Option<String>,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long)]
    letter: Option<String>,
    #[arg(long)]
    link: Option<String>,
    #[arg(long)]
    country: Option<String>,
    #[arg(long)]
    notes: Option<String>,
}

#[derive(Subcommand)]
pub enum AppCommand {
    Add(AddArgs),
    List {
        #[arg(long)]
        status: Option<String>,
    },
    Show {
        id: String,
    },
    /// Every change to one application, oldest first.
    History {
        id: String,
    },
    Edit(EditArgs),
    /// saved, applied, screening, interviewing, offer, accepted, rejected, or withdrawn.
    Status {
        id: String,
        status: String,
        /// The day you applied, YYYY-MM-DD; kept from the first time.
        #[arg(long)]
        on: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
    Note {
        id: String,
        text: String,
    },
    /// The answers this application has used or confirmed, with no answer text.
    Answers {
        id: String,
    },
    Remove {
        id: String,
    },
}

fn line(app: &Application) -> String {
    format!("{}  {}  {} / {}", app.id, app.status, app.company, app.role)
}

fn detail(app: &Application) -> String {
    let or_dash = |text: &str| {
        if text.is_empty() {
            "-".into()
        } else {
            text.to_string()
        }
    };
    let opt = |text: &Option<String>| text.clone().unwrap_or_else(|| "-".into());
    format!(
        "{}\nstatus: {}\napplied: {}\nupdated: {}\njob: {}\nresume: {}\nletter: {}\n\
         country: {}\nlink (untrusted text): {}\nnotes: {}",
        line(app),
        app.status,
        opt(&app.applied_on),
        app.updated_at,
        opt(&app.job_text_id),
        opt(&app.resume_draft_id),
        opt(&app.letter_draft_id),
        or_dash(&app.country),
        or_dash(&app.link),
        or_dash(&app.notes),
    )
}

pub async fn app(db: &Db, command: AppCommand) -> Result<Output> {
    match command {
        AppCommand::Add(a) => {
            let added = applications::add(
                db,
                ACTOR,
                ApplicationInput {
                    company: a.company,
                    role: a.role,
                    job_text_id: a.job,
                    resume_draft_id: a.resume,
                    letter_draft_id: a.letter,
                    link: a.link.unwrap_or_default(),
                    country: a.country.unwrap_or_default(),
                    notes: a.notes.unwrap_or_default(),
                },
            )
            .await?;
            out(&added, format!("saved {}", added.id))
        }
        AppCommand::List { status } => {
            let apps = applications::list(db, status.as_deref()).await?;
            let text = apps.iter().map(line).collect::<Vec<_>>().join("\n");
            out(apps, text)
        }
        AppCommand::Show { id } => {
            let app = applications::get(db, &id).await?;
            out(&app, detail(&app))
        }
        AppCommand::History { id } => {
            let events = applications::history(db, &id).await?;
            let text = events
                .iter()
                .map(|e| {
                    let change = match (&e.from_status, &e.to_status) {
                        (Some(from), Some(to)) => format!(" {from} -> {to}"),
                        (None, Some(to)) => format!(" {to}"),
                        _ => String::new(),
                    };
                    let note = if e.note.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", e.note)
                    };
                    format!("{}  {}  {}{change}{note}", e.at, e.actor, e.kind)
                })
                .collect::<Vec<_>>()
                .join("\n");
            out(events, text)
        }
        AppCommand::Edit(a) => {
            let current = applications::get(db, &a.id).await?;
            let input = ApplicationInput {
                company: a.company.unwrap_or(current.company),
                role: a.role.unwrap_or(current.role),
                job_text_id: a.job.or(current.job_text_id),
                resume_draft_id: a.resume.or(current.resume_draft_id),
                letter_draft_id: a.letter.or(current.letter_draft_id),
                link: a.link.unwrap_or(current.link),
                country: a.country.unwrap_or(current.country),
                notes: a.notes.unwrap_or(current.notes),
            };
            let saved = applications::update(db, ACTOR, &a.id, input).await?;
            out(&saved, format!("saved {}", saved.id))
        }
        AppCommand::Status {
            id,
            status,
            on,
            note,
        } => {
            let saved = with_interrupts(db, |cancel| {
                agents::set_status(db, ACTOR, &id, &status, on.as_deref(), &note, cancel)
            })
            .await?;
            out(&saved, format!("{} is {}", saved.id, saved.status))
        }
        AppCommand::Note { id, text } => {
            applications::add_note(db, ACTOR, &id, &text).await?;
            out(serde_json::json!({ "id": id }), "noted".into())
        }
        AppCommand::Answers { id } => {
            let answers = jobpilot_core::memory::application_answers(db, &id).await?;
            let text = answers
                .iter()
                .map(|a| {
                    let state = if a.current {
                        "confirmed"
                    } else {
                        "needs confirmation"
                    };
                    let used = a
                        .used_at
                        .as_deref()
                        .map_or(String::new(), |at| format!("  used {at}"));
                    format!(
                        "{}  {}  {state}{used}  {}",
                        a.answer_id,
                        a.class.as_str(),
                        a.question
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            out(answers, text)
        }
        AppCommand::Remove { id } => {
            applications::remove(db, ACTOR, &id).await?;
            out(serde_json::json!({ "id": id }), "removed".into())
        }
    }
}
