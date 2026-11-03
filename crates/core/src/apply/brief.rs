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
use crate::memory::usage;
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

/// Stages the item's files into `files`, then reads what this application may use: fresh
/// normal answers, and the `Sensitive` ones released on this item at start. An answer scoped
/// to a country serves only that country. Each use is audited, after the files are staged.
pub async fn build(db: &Db, actor: &str, item: &Item, files: &Path) -> Result<Brief> {
    let application_id = item
        .application_id
        .as_deref()
        .ok_or_else(|| Error::Validation("this application was removed".into()))?;
    let app = applications::get(db, application_id).await?;
    for path in [&item.resume_path, &item.letter_path].into_iter().flatten() {
        if !db.paths().is_surely_inside_root(path) {
            return Err(Error::Refused("file_outside_root".into()));
        }
    }

    let staged = stage_files(item, files)?;
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
        if !row.serves(&app.country) || row.stale {
            continue;
        }
        let used = match row.class {
            Class::Normal => usage::use_answer(db, actor, &row.answer_id, application_id).await,
            Class::Sensitive if item.released_answers.contains(&row.answer_id) => {
                usage::use_released(db, actor, &row.answer_id, application_id).await
            }
            Class::Sensitive | Class::Legal | Class::WorkAuthorization => continue,
        };
        match used {
            // The class is read again with the text, so a change since the list keeps it out.
            Ok(used) if used.class == row.class && !used.stale => answers.push(BriefAnswer {
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
        files: staged,
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

/// Empties `files` and keeps the folder, which is the browser server's working folder, then
/// copies in this item's resume and letter. An entry that is a link is removed, never followed.
/// A source that is not a plain file, a link included, is refused. Returns the canonical paths.
pub fn stage_files(item: &Item, files: &Path) -> Result<Vec<BriefFile>> {
    match std::fs::symlink_metadata(files) {
        Ok(meta) if meta.is_dir() => {
            for entry in std::fs::read_dir(files)? {
                let path = entry?.path();
                // `symlink_metadata` reads the entry itself: a link to a folder is a link.
                let kind = std::fs::symlink_metadata(&path)?.file_type();
                if kind.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else if let Err(error) = std::fs::remove_file(&path) {
                    // Windows removes a link to a folder as a folder, which leaves its target.
                    if !(cfg!(windows) && kind.is_symlink()) {
                        return Err(error.into());
                    }
                    std::fs::remove_dir(&path)?;
                }
            }
        }
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
