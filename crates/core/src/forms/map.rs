//! What goes into each field. Exact rules first, from the profile; then an approved answer whose
//! question is the field's label; then, for an ordinary field, a model that may only name an
//! answer by its id. Rust fills every value. A sensitive, legal, or work-authorization field is
//! never filled by a rule: it takes a confirmed answer, or a person fills it (FR-3.22, M-37).

use serde_json::Value;

use super::{Form, FormField, file_kind, fold_label};
use crate::ai::router::Router;
use crate::ai::{CancellationToken, Outcome, ProviderId, TaskType};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::generate::map_field_prompt;
use crate::memory::Class;
use crate::memory::rules::classify;
use crate::profile::ProfileBasic;

/// Fields one form may send to a model, each at most `attempts` calls.
// ponytail: a form past this many unmapped fields leaves the rest to the person.
pub const MAX_MODEL_FIELDS: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum ValueSource {
    Profile,
    /// The approved answer's id.
    Answer(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mapped {
    Value {
        field: FormField,
        text: String,
        source: ValueSource,
    },
    /// `resume` or `letter`.
    File {
        field: FormField,
        kind: &'static str,
    },
    /// A person fills it: `prefilled`, `legal`, `sensitive`, `work_authorization`, `frame`,
    /// `unmapped`, or, from Copilot's guards, `guard`, `unapproved`, or `unlabeled`.
    Person { field: FormField, why: &'static str },
}

impl Mapped {
    pub fn field(&self) -> &FormField {
        match self {
            Mapped::Value { field, .. }
            | Mapped::File { field, .. }
            | Mapped::Person { field, .. } => field,
        }
    }
}

/// An approved answer this application may use, with its text, as `memory::use_answer` gave it.
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerText {
    pub answer_id: String,
    pub question: String,
    pub text: String,
}

/// An answer a model may name: its id and question, never its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub answer_id: String,
    pub question: String,
}

fn first(list: &[String]) -> Option<String> {
    list.iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The profile fact an exactly folded label names.
fn rule(label: &str, basic: &ProfileBasic) -> Option<String> {
    let name: Vec<&str> = basic.name.split_whitespace().collect();
    let value = match label {
        "full name" | "name" => basic.name.trim().to_owned(),
        "first name" => name.first()?.to_string(),
        "last name" if name.len() > 1 => name.last()?.to_string(),
        "email" => basic.email.trim().to_owned(),
        "city" => first(&basic.cities)?,
        "country" => first(&basic.countries)?,
        "state" | "region" | "province" => first(&basic.regions)?,
        "language" => first(&basic.languages)?,
        _ => return None,
    };
    (!value.is_empty()).then_some(value)
}

/// What a field can hold of `text`: a text field takes it as it is, a combobox only one of its
/// own options, by folded name, and a choice nothing.
pub(crate) fn fits(field: &FormField, text: &str) -> Option<String> {
    match field.role.as_str() {
        "textbox" => Some(text.to_owned()),
        "combobox" => {
            let wanted = fold_label(text);
            field
                .options
                .iter()
                .find(|option| fold_label(option) == wanted)
                .cloned()
        }
        _ => None,
    }
}

fn answer_for(field: &FormField, answers: &[AnswerText]) -> Option<Mapped> {
    let label = fold_label(&field.label);
    let answer = answers
        .iter()
        .find(|answer| !label.is_empty() && fold_label(&answer.question) == label)?;
    Some(Mapped::Value {
        text: fits(field, &answer.text)?,
        source: ValueSource::Answer(answer.answer_id.clone()),
        field: field.clone(),
    })
}

/// A ref Playwright gives an element inside a frame: `f<n>e<m>`.
fn is_framed(target: &str) -> bool {
    target
        .strip_prefix('f')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

/// One entry per field, in the form's order. `answers` hold only what this application may
/// use: a sealed one is here only with a current confirmation.
pub fn map_rules(form: &Form, basic: &ProfileBasic, answers: &[AnswerText]) -> Vec<Mapped> {
    form.fields
        .iter()
        .map(|field| {
            let person = |why| Mapped::Person {
                field: field.clone(),
                why,
            };
            // A frame's page may be another site's: its fields are judged by no origin here.
            if is_framed(&field.target) {
                return person("frame");
            }
            if field.role == "button" {
                return match file_kind(&field.label) {
                    Some(kind) => Mapped::File {
                        field: field.clone(),
                        kind,
                    },
                    None => person("unmapped"),
                };
            }
            let class = classify(&field.label);
            if field.value.is_some() && class != Class::Normal {
                return person("prefilled");
            }
            match class {
                Class::Legal => person("legal"),
                Class::WorkAuthorization => person("work_authorization"),
                Class::Sensitive => {
                    answer_for(field, answers).unwrap_or_else(|| person("sensitive"))
                }
                Class::Normal => {
                    let ruled = rule(&fold_label(&field.label), basic).and_then(|value| {
                        Some(Mapped::Value {
                            text: fits(field, &value)?,
                            source: ValueSource::Profile,
                            field: field.clone(),
                        })
                    });
                    ruled
                        .or_else(|| answer_for(field, answers))
                        .unwrap_or_else(|| person("unmapped"))
                }
            }
        })
        .collect()
}

/// What the model said for one field.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// A listed answer's id. Rust reads its text.
    Answer(String),
    /// None of the answers fits.
    None,
    /// No model took the task, or none gave a usable reply.
    Unavailable,
}

/// Asks `map_form_field` which listed answer fits `label`. The prompt holds the label inside
/// the data markers and the candidates' ids and questions, never an answer's text. A reply
/// naming an id that is not listed fails the check, as a schema failure does. With the pick
/// comes the provider that made it, for the run log.
pub async fn map_field(
    db: &Db,
    router: &Router,
    actor: &str,
    label: &str,
    candidates: &[Candidate],
    cancel: CancellationToken,
) -> Result<(Pick, Option<ProviderId>)> {
    let listed: Vec<(&str, &str)> = candidates
        .iter()
        .map(|c| (c.answer_id.as_str(), c.question.as_str()))
        .collect();
    let ids: Vec<String> = candidates.iter().map(|c| c.answer_id.clone()).collect();
    let check = |output: Value| {
        let ids = ids.clone();
        async move {
            match &output["answer_id"] {
                Value::Null => Ok(()),
                Value::String(id) if ids.contains(id) => Ok(()),
                _ => Err(Error::Validation("not a listed answer".into())),
            }
        }
    };
    let outcome = router
        .run_checked(
            db,
            actor,
            TaskType::MapFormField,
            map_field_prompt(label, &listed),
            cancel,
            check,
        )
        .await?;
    Ok(match outcome {
        Outcome::Ok {
            output, provider, ..
        } => {
            let pick = match output["answer_id"].as_str() {
                // Checked again here: only a listed id is ever used.
                Some(id) if ids.iter().any(|known| known == id) => Pick::Answer(id.to_owned()),
                Some(_) => Pick::Unavailable,
                None => Pick::None,
            };
            (pick, Some(provider))
        }
        Outcome::Cancelled => return Err(Error::Cancelled),
        Outcome::ManualRequired { .. } | Outcome::Failed { .. } => (Pick::Unavailable, None),
    })
}
