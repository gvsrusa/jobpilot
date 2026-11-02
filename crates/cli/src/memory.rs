//! Questions and answers you reuse.

use clap::Subcommand;
use jobpilot_core::agents;
use jobpilot_core::ai::router::Router;
use jobpilot_core::memory::{self, Answer, Class, MemoryHit, QuestionRow};
use jobpilot_core::profile::Stated;
use jobpilot_core::{Db, Error, Result};
use serde_json::json;

use crate::ai::with_interrupts;
use crate::{ACTOR, Output, out};

#[derive(Subcommand)]
pub enum MemoryCommand {
    AddQuestion {
        text: String,
        /// Two letters, like PT. Never read from the question's words.
        #[arg(long)]
        country: Option<String>,
    },
    AddAnswer {
        question: String,
        text: String,
        /// The knowledge item this answer came from, by id.
        #[arg(long = "from-item")]
        from_item: Option<String>,
    },
    List {
        /// normal, sensitive, legal, or work_authorization.
        #[arg(long)]
        class: Option<Class>,
    },
    Show {
        question: String,
    },
    /// New text waits for approval again.
    EditAnswer {
        answer: String,
        text: String,
    },
    Approve {
        answer: String,
    },
    Unapprove {
        answer: String,
    },
    /// Says an answer still holds, which makes it fresh again.
    Reviewed {
        answer: String,
    },
    /// normal, sensitive, legal, or work_authorization.
    SetClass {
        question: String,
        class: Class,
    },
    /// Prints an answer, sealed or not.
    Reveal {
        answer: String,
    },
    /// You confirm this answer is true today for one application, and it is marked reviewed.
    /// Do this in an ordinary terminal or in the app.
    Confirm {
        answer: String,
        #[arg(long)]
        application: String,
    },
    /// Prints an answer for one application. A sensitive or legal answer needs a current
    /// confirmation first.
    Use {
        answer: String,
        #[arg(long)]
        application: String,
    },
    /// Your work authorization for one country, from your profile: "not stated" when you have
    /// not stated it.
    Authorization {
        /// Two letters, like PT.
        country: String,
    },
    RemoveQuestion {
        id: String,
    },
    RemoveAnswer {
        id: String,
    },
    /// Finds questions by meaning when a local embedding model is set, else by words. A
    /// sensitive or legal answer is never shown here, and work authorization is never found.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Asks a model to double-check classes (up to 20 questions, shown first when a paid or
    /// subscription provider is used), seals what a raise left plain, and embeds the questions
    /// with no vector yet.
    Refresh {
        /// Go ahead with the calls the count above announces.
        #[arg(long)]
        yes: bool,
    },
}

fn stated(value: &Stated) -> &'static str {
    match value {
        Stated::Yes => "yes",
        Stated::No => "no",
        Stated::NotStated => "not stated",
    }
}

fn answer_line(answer: &Answer) -> String {
    let state = if answer.approved {
        "approved"
    } else {
        "waiting"
    };
    let stale = if answer.stale { " stale" } else { "" };
    let body = if answer.sealed {
        "[sealed]"
    } else {
        answer.body.as_str()
    };
    let source = match &answer.source_ref {
        Some(item) => format!("{} {item}", answer.source),
        None => answer.source.clone(),
    };
    format!(
        "  {}  v{}  {state}{stale}  {source}  {body}",
        answer.id, answer.version
    )
}

fn row_text(row: &QuestionRow) -> String {
    let question = &row.question;
    let country = question
        .country
        .as_deref()
        .map(|c| format!(" {c}"))
        .unwrap_or_default();
    let mut lines = vec![format!(
        "{}  {}{country}  {}",
        question.id,
        question.class.as_str(),
        question.text
    )];
    lines.extend(
        question
            .findings
            .iter()
            .map(|f| format!("  finding ({}): {}", f.kind, f.excerpt)),
    );
    lines.extend(row.answers.iter().map(answer_line));
    lines.join("\n")
}

fn hit_line(hit: &MemoryHit) -> String {
    let shown = match (&hit.answer, hit.needs_confirmation) {
        (Some(answer), _) => answer.as_str(),
        (None, true) => "[confirm on an application]",
        (None, false) => "[no approved answer]",
    };
    format!(
        "{}  {}  {}  {}  {shown}",
        hit.question_id,
        hit.class.as_str(),
        hit.via,
        hit.question
    )
}

pub async fn memory(db: &Db, command: MemoryCommand) -> Result<Output> {
    match command {
        MemoryCommand::AddQuestion { text, country } => {
            let added = memory::add_question(db, &text, country.as_deref()).await?;
            let line = format!("added {}  {}", added.id, added.class.as_str());
            out(&added, line)
        }
        MemoryCommand::AddAnswer {
            question,
            text,
            from_item,
        } => {
            let added = memory::add_answer(db, &question, &text, from_item.as_deref()).await?;
            out(&added, format!("added {}", added.id))
        }
        MemoryCommand::List { class } => {
            let rows = memory::list(db, class).await?;
            let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");
            out(rows, text)
        }
        MemoryCommand::Show { question } => {
            let row = memory::get(db, &question).await?;
            out(&row, row_text(&row))
        }
        MemoryCommand::EditAnswer { answer, text } => {
            let saved = memory::edit_answer(db, &answer, &text).await?;
            out(&saved, format!("saved {} as v{}", saved.id, saved.version))
        }
        MemoryCommand::Approve { answer } => {
            memory::approve(db, ACTOR, &answer, true).await?;
            out(json!({ "id": answer }), "approved".into())
        }
        MemoryCommand::Unapprove { answer } => {
            memory::approve(db, ACTOR, &answer, false).await?;
            out(json!({ "id": answer }), "unapproved".into())
        }
        MemoryCommand::Reviewed { answer } => {
            memory::mark_reviewed(db, ACTOR, &answer).await?;
            out(json!({ "id": answer }), "reviewed".into())
        }
        MemoryCommand::SetClass { question, class } => {
            let saved = memory::set_class(db, ACTOR, &question, class).await?;
            let line = format!("{} is {}", saved.id, saved.class.as_str());
            out(&saved, line)
        }
        MemoryCommand::Reveal { answer } => {
            let body = memory::reveal(db, ACTOR, &answer).await?;
            out(json!({ "id": answer, "body": body }), body)
        }
        MemoryCommand::Confirm {
            answer,
            application,
        } => {
            memory::confirm_use(db, ACTOR, &answer, &application).await?;
            let done = json!({ "answer": answer, "application": application });
            out(done, "confirmed".into())
        }
        MemoryCommand::Use {
            answer,
            application,
        } => {
            let used = memory::use_answer(db, ACTOR, &answer, &application)
                .await
                .inspect_err(|error| {
                    if matches!(error, Error::ConfirmationRequired) {
                        eprintln!(
                            "Confirm it first in an ordinary terminal: \
                             jobpilot memory confirm <answer> --application <application>"
                        );
                    }
                })?;
            // On stderr, so what is piped from stdout is the answer and nothing else.
            if used.stale {
                eprintln!("note: this answer has not been reviewed for over 180 days");
            }
            let text = used.text.clone();
            out(&used, text)
        }
        MemoryCommand::Authorization { country } => {
            let answer = memory::authorization_for(db, &country).await?;
            let text = if answer.stated {
                format!(
                    "{}: authorized {}, needs sponsorship {}{}",
                    answer.country,
                    stated(&answer.authorized),
                    stated(&answer.needs_sponsorship),
                    if answer.note.is_empty() {
                        String::new()
                    } else {
                        format!("\nnote: {}", answer.note)
                    }
                )
            } else {
                format!("{}: not stated by you", answer.country)
            };
            out(&answer, text)
        }
        MemoryCommand::RemoveQuestion { id } => {
            memory::remove_question(db, &id).await?;
            out(json!({ "id": id }), "removed".into())
        }
        MemoryCommand::RemoveAnswer { id } => {
            memory::remove_answer(db, &id).await?;
            out(json!({ "id": id }), "removed".into())
        }
        MemoryCommand::Search { query, limit } => {
            let router = Router::from_settings(db).await?;
            let hits = memory::search(db, &router, &query, limit).await?;
            let text = hits.iter().map(hit_line).collect::<Vec<_>>().join("\n");
            out(&hits, text)
        }
        MemoryCommand::Refresh { yes } => {
            let router = Router::from_settings(db).await?;
            let plan = memory::refresh_plan(db, &router).await?;
            if plan.external_calls > 0 && !yes {
                let text = format!(
                    "This will make at most {} paid or subscription calls for {} questions \
                     (a press checks {} questions at most). Run \
                     `jobpilot memory refresh --yes` to go ahead.",
                    plan.external_calls,
                    plan.pending,
                    memory::CLASSIFY_BATCH
                );
                let shown = json!({
                    "external_calls": plan.external_calls,
                    "pending": plan.pending,
                });
                return out(shown, text);
            }
            if plan.external_calls > 0 {
                // Before the run, on stderr so a JSON reader still gets clean stdout.
                eprintln!(
                    "Making at most {} paid or subscription calls for {} questions \
                     (a press checks {} questions at most).",
                    plan.external_calls,
                    plan.pending,
                    memory::CLASSIFY_BATCH
                );
            }
            let done = with_interrupts(db, |cancel| {
                agents::memory_refresh(db, &router, ACTOR, cancel)
            })
            .await?;
            let mut text = format!(
                "Checked {}, raised {}, indexed {}",
                done.classified.checked, done.classified.raised, done.embedded
            );
            if done.classified.skipped {
                text.push_str("\nNo AI took this; classes come from the rules.");
            }
            if let Some(code) = &done.seal_blocked {
                text.push_str(&format!(
                    "\nSome answers stay hidden until they can be sealed: error[{code}]."
                ));
            }
            if done.embedded == 0 && !embedding_model_set(db).await? {
                text.push_str("\nNo embedding model is set; search uses text.");
            }
            let mut shown = serde_json::to_value(&done).unwrap_or_default();
            shown["external_calls"] = json!(plan.external_calls);
            out(shown, text)
        }
    }
}

async fn embedding_model_set(db: &Db) -> Result<bool> {
    let value = jobpilot_core::settings::get(db, "ai.embedding_model").await?;
    Ok(value.is_some_and(|value| value.as_str().is_some_and(|model| !model.is_empty())))
}
