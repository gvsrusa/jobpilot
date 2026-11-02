//! Questions and answers you reuse.

use clap::Subcommand;
use jobpilot_core::memory::{self, Answer, Class, QuestionRow};
use jobpilot_core::{Db, Result};
use serde_json::json;

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
    RemoveQuestion {
        id: String,
    },
    RemoveAnswer {
        id: String,
    },
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
    format!(
        "  {}  v{}  {state}{stale}  {}  {body}",
        answer.id, answer.version, answer.source
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
        MemoryCommand::RemoveQuestion { id } => {
            memory::remove_question(db, &id).await?;
            out(json!({ "id": id }), "removed".into())
        }
        MemoryCommand::RemoveAnswer { id } => {
            memory::remove_answer(db, &id).await?;
            out(json!({ "id": id }), "removed".into())
        }
    }
}
