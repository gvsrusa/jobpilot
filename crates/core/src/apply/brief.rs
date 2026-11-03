//! What the session may know about one application: profile facts, approved answers, the
//! work authorization row released for its exact country, and its two files. Nothing here is
//! inferred: a value is the owner's own, or it is left out.

use std::path::Path;

use serde::Serialize;

use super::Item;
use crate::applications;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::memory::Class;
use crate::memory::usage::{self, UsedAnswer};
use crate::profile::{self, Stated};

#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub name: &'static str,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefAnswer {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorizationFact {
    pub country: String,
    pub authorized: String,
    pub needs_sponsorship: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefFile {
    pub kind: &'static str,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub application_id: String,
    pub company: String,
    pub role: String,
    pub link: String,
    pub country: String,
    pub facts: Vec<Fact>,
    pub answers: Vec<BriefAnswer>,
    pub work_authorization: Option<AuthorizationFact>,
    pub files: Vec<BriefFile>,
}

/// Stages the item's files into `files`, then reads what this application may use. Each
/// answer goes through `use_answer`, so a sealed one comes only with this application's
/// confirmation, and each use is audited.
pub async fn build(db: &Db, actor: &str, item: &Item, files: &Path) -> Result<Brief> {
    let application_id = item
        .application_id
        .as_deref()
        .ok_or_else(|| Error::Validation("this application was removed".into()))?;
    let app = applications::get(db, application_id).await?;
    for path in [&item.resume_path, &item.letter_path].into_iter().flatten() {
        if !db.paths().is_inside_root(path) {
            return Err(Error::Refused("file_outside_root".into()));
        }
    }

    let basic = profile::basic(db).await?;
    let mut facts = Vec::new();
    for (name, value) in [("name", &basic.name), ("email", &basic.email)] {
        if !value.trim().is_empty() {
            facts.push(Fact {
                name,
                value: value.trim().to_owned(),
            });
        }
    }
    for (name, list) in [
        ("cities", &basic.cities),
        ("regions", &basic.regions),
        ("countries", &basic.countries),
        ("languages", &basic.languages),
    ] {
        if !list.is_empty() {
            facts.push(Fact {
                name,
                value: list.join(", "),
            });
        }
    }

    let mut answers = Vec::new();
    for row in usage::approved_for(db).await? {
        let wanted = match row.class {
            Class::Normal => !row.stale,
            Class::Sensitive => true,
            Class::Legal | Class::WorkAuthorization => false,
        };
        if !wanted {
            continue;
        }
        match usage::use_answer(db, actor, &row.answer_id, application_id).await {
            Ok(used) if usable(&used) => answers.push(BriefAnswer {
                question: row.question,
                answer: used.text,
            }),
            Ok(_) | Err(Error::ConfirmationRequired) => {}
            // Changed since it was listed: unapproved, or moved to work authorization.
            Err(Error::Refused(_)) => {}
            Err(error) => return Err(error),
        }
    }

    let work_authorization = if item.auth_released && !app.country.is_empty() {
        let stated = usage::authorization_for(db, &app.country).await?;
        stated.stated.then(|| AuthorizationFact {
            country: stated.country,
            authorized: stated_text(&stated.authorized).into(),
            needs_sponsorship: stated_text(&stated.needs_sponsorship).into(),
        })
    } else {
        None
    };

    Ok(Brief {
        files: stage_files(item, files)?,
        application_id: app.id,
        company: app.company,
        role: app.role,
        link: app.link,
        country: app.country,
        facts,
        answers,
        work_authorization,
    })
}

/// The class is read again with the text, so a raise since the list keeps it out.
fn usable(used: &UsedAnswer) -> bool {
    match used.class {
        Class::Normal => !used.stale,
        Class::Sensitive => true,
        Class::Legal | Class::WorkAuthorization => false,
    }
}

fn stated_text(value: &Stated) -> &'static str {
    match value {
        Stated::Yes => "yes",
        Stated::No => "no",
        Stated::NotStated => "not_stated",
    }
}

/// Every value the session may type: each fact, each comma part of one, each answer, and
/// the stated authorization values.
pub fn allowed_values(brief: &Brief) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut add = |value: &str| {
        let value = value.trim();
        if !value.is_empty() && !values.iter().any(|known| known == value) {
            values.push(value.to_owned());
        }
    };
    for fact in &brief.facts {
        add(&fact.value);
        for part in fact.value.split(',') {
            add(part);
        }
    }
    for answer in &brief.answers {
        add(&answer.answer);
    }
    if let Some(fact) = &brief.work_authorization {
        for value in [&fact.authorized, &fact.needs_sponsorship] {
            if value == "yes" || value == "no" {
                add(value);
            }
        }
    }
    values
}

/// Empties `files`, then copies in this item's resume and letter. A source that is not a
/// plain file, a link included, is refused. Returns the canonical paths.
pub fn stage_files(item: &Item, files: &Path) -> Result<Vec<BriefFile>> {
    match std::fs::symlink_metadata(files) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(files)?,
        Ok(_) => std::fs::remove_file(files)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::create_dir_all(files)?;
    let mut staged = Vec::new();
    for (kind, source, name) in [
        ("resume", &item.resume_path, "resume.pdf"),
        ("cover_letter", &item.letter_path, "cover-letter.pdf"),
    ] {
        let Some(source) = source else {
            continue;
        };
        if !std::fs::symlink_metadata(source)?.file_type().is_file() {
            return Err(Error::Refused("not_a_plain_file".into()));
        }
        let target = files.join(name);
        std::fs::copy(source, &target)?;
        let path = std::fs::canonicalize(&target)?;
        let path = path
            .to_str()
            .ok_or_else(|| Error::Validation("a file path is not valid text".into()))?
            .to_owned();
        staged.push(BriefFile { kind, path });
    }
    Ok(staged)
}
