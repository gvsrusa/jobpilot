//! Profile, preferences, knowledge items, and pasted job postings.

use clap::{Subcommand, ValueEnum};
use jobpilot_core::ai::router::Router;
use jobpilot_core::profile::{
    self, EmploymentType, ProfileBasic, Stated, WorkAuthorization, WorkMode,
};
use jobpilot_core::{Db, Error, Result, embed, knowledge};
use serde_json::{Value, json};

use crate::{Output, out};

#[derive(Clone, Copy, ValueEnum)]
pub enum ListField {
    Countries,
    Regions,
    Cities,
    Currencies,
    TimeZones,
    Languages,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StatedArg {
    NotStated,
    Yes,
    No,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum WorkModeArg {
    Remote,
    Hybrid,
    Onsite,
}

#[derive(Subcommand)]
pub enum ProfileCommand {
    Show,
    Set {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        email: Option<String>,
    },
    /// Add values to a list field.
    Add {
        field: ListField,
        #[arg(required = true)]
        values: Vec<String>,
    },
    Remove {
        field: ListField,
        value: String,
    },
    /// Work authorization, stored only as you state it.
    #[command(subcommand)]
    Auth(AuthCommand),
}

#[derive(Subcommand)]
pub enum AuthCommand {
    Set {
        country: String,
        #[arg(long, value_enum, default_value = "not-stated")]
        authorized: StatedArg,
        #[arg(long, value_enum, default_value = "not-stated")]
        sponsorship: StatedArg,
        #[arg(long, default_value = "")]
        note: String,
    },
    Remove {
        country: String,
    },
}

#[derive(Subcommand)]
pub enum PrefsCommand {
    /// Replace the work modes, for example `remote,hybrid`.
    WorkMode {
        #[arg(value_enum, required = true, value_delimiter = ',')]
        modes: Vec<WorkModeArg>,
    },
    #[command(subcommand)]
    Employment(EmploymentCommand),
}

#[derive(Subcommand)]
pub enum EmploymentCommand {
    Add { country: String, kind: String },
    Remove { country: String, kind: String },
}

#[derive(Subcommand)]
pub enum KbCommand {
    List,
    Add {
        kind: String,
        body: String,
    },
    Remove {
        id: String,
    },
    /// Let drafts cite this item.
    Approve {
        id: String,
    },
    Unapprove {
        id: String,
    },
    /// Keep this item out of every prompt.
    MarkSensitive {
        id: String,
    },
    Unmark {
        id: String,
    },
    /// Print a sealed item's text. Asks the keychain; refused in the terminal panel.
    Reveal {
        id: String,
    },
    Search {
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
}

fn stated(arg: StatedArg) -> Stated {
    match arg {
        StatedArg::NotStated => Stated::NotStated,
        StatedArg::Yes => Stated::Yes,
        StatedArg::No => Stated::No,
    }
}

fn list_of(basic: &mut ProfileBasic, field: ListField) -> &mut Vec<String> {
    match field {
        ListField::Countries => &mut basic.countries,
        ListField::Regions => &mut basic.regions,
        ListField::Cities => &mut basic.cities,
        ListField::Currencies => &mut basic.currencies,
        ListField::TimeZones => &mut basic.time_zones,
        ListField::Languages => &mut basic.languages,
    }
}

fn shown(basic: &ProfileBasic) -> Value {
    json!({
        "name": basic.name,
        "email": basic.email,
        "countries": basic.countries,
        "regions": basic.regions,
        "cities": basic.cities,
        "currencies": basic.currencies,
        "time_zones": basic.time_zones,
        "languages": basic.languages,
        "work_modes": basic.work_modes,
        "employment_types": basic.employment_types,
    })
}

fn basic_text(basic: &ProfileBasic) -> String {
    let list = |label: &str, items: &[String]| format!("{label}: {}", items.join(", "));
    let modes: Vec<String> = basic
        .work_modes
        .iter()
        .map(|m| format!("{m:?}").to_lowercase())
        .collect();
    let jobs: Vec<String> = basic
        .employment_types
        .iter()
        .map(|e| format!("{} {}", e.country, e.kind))
        .collect();
    [
        format!("name: {}", basic.name),
        format!("email: {}", basic.email),
        list("countries", &basic.countries),
        list("regions", &basic.regions),
        list("cities", &basic.cities),
        list("currencies", &basic.currencies),
        list("time zones", &basic.time_zones),
        list("languages", &basic.languages),
        list("work modes", &modes),
        list("employment types", &jobs),
    ]
    .join("\n")
}

pub async fn profile(db: &Db, command: ProfileCommand) -> Result<Output> {
    match command {
        ProfileCommand::Show => {
            let full = profile::show(db).await?;
            let mut value = shown(&full.basic);
            value["authorization"] = json!(full.authorization);
            value["locked"] = json!(full.locked);
            value["key_state"] = json!(full.key_state);
            let mut text = basic_text(&full.basic);
            if full.locked {
                text.push_str("\nwork authorization: locked");
            }
            for row in &full.authorization {
                text.push_str(&format!(
                    "\nauthorization {}: authorized {:?}, sponsorship {:?}",
                    row.country, row.authorized, row.needs_sponsorship
                ));
            }
            out(value, text)
        }
        ProfileCommand::Set { name, email } => {
            let mut basic = profile::basic(db).await?;
            if let Some(name) = name {
                basic.name = name;
            }
            if let Some(email) = email {
                basic.email = email;
            }
            save(db, basic).await
        }
        ProfileCommand::Add { field, values } => {
            let mut basic = profile::basic(db).await?;
            let list = list_of(&mut basic, field);
            for value in values {
                if !list
                    .iter()
                    .any(|have| have.eq_ignore_ascii_case(value.trim()))
                {
                    list.push(value);
                }
            }
            save(db, basic).await
        }
        ProfileCommand::Remove { field, value } => {
            let mut basic = profile::basic(db).await?;
            list_of(&mut basic, field).retain(|have| !have.eq_ignore_ascii_case(value.trim()));
            save(db, basic).await
        }
        ProfileCommand::Auth(AuthCommand::Set {
            country,
            authorized,
            sponsorship,
            note,
        }) => {
            let row = WorkAuthorization {
                country,
                authorized: stated(authorized),
                needs_sponsorship: stated(sponsorship),
                note,
            };
            profile::set_authorization(db, row).await?;
            out(json!({ "saved": true }), "work authorization saved".into())
        }
        ProfileCommand::Auth(AuthCommand::Remove { country }) => {
            profile::remove_authorization(db, &country).await?;
            out(
                json!({ "removed": true }),
                "work authorization removed".into(),
            )
        }
    }
}

async fn save(db: &Db, basic: ProfileBasic) -> Result<Output> {
    let saved = profile::set_basic(db, basic).await?;
    out(shown(&saved), basic_text(&saved))
}

pub async fn prefs(db: &Db, command: PrefsCommand) -> Result<Output> {
    let mut basic = profile::basic(db).await?;
    match command {
        PrefsCommand::WorkMode { modes } => {
            basic.work_modes.clear();
            for mode in modes {
                let mode = match mode {
                    WorkModeArg::Remote => WorkMode::Remote,
                    WorkModeArg::Hybrid => WorkMode::Hybrid,
                    WorkModeArg::Onsite => WorkMode::Onsite,
                };
                if !basic.work_modes.contains(&mode) {
                    basic.work_modes.push(mode);
                }
            }
        }
        PrefsCommand::Employment(EmploymentCommand::Add { country, kind }) => {
            let has = |e: &EmploymentType| {
                e.country.eq_ignore_ascii_case(country.trim())
                    && e.kind.eq_ignore_ascii_case(kind.trim())
            };
            if !basic.employment_types.iter().any(has) {
                basic
                    .employment_types
                    .push(EmploymentType { country, kind });
            }
        }
        PrefsCommand::Employment(EmploymentCommand::Remove { country, kind }) => {
            basic.employment_types.retain(|e| {
                !(e.country.eq_ignore_ascii_case(country.trim())
                    && e.kind.eq_ignore_ascii_case(kind.trim()))
            });
        }
    }
    save(db, basic).await
}

fn first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or_default();
    match line.char_indices().nth(80) {
        Some((end, _)) => format!("{}...", &line[..end]),
        None => line.to_string(),
    }
}

pub async fn kb(db: &Db, command: KbCommand) -> Result<Output> {
    match command {
        KbCommand::List => {
            let items = knowledge::list(db).await?;
            let text = items
                .iter()
                .map(|i| {
                    let mut flags = vec![if i.approved { "approved" } else { "pending" }];
                    if i.marked_sensitive {
                        flags.push("sensitive");
                    }
                    if i.sealed {
                        flags.push("sealed");
                    }
                    format!(
                        "{}  {}  [{}]  {}",
                        i.id,
                        i.kind,
                        flags.join(","),
                        first_line(&i.body)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            out(items, text)
        }
        KbCommand::Add { kind, body } => {
            let item = knowledge::add(db, &kind, &body, "user", None).await?;
            let text = format!("added {}", item.id);
            out(item, text)
        }
        KbCommand::Remove { id } => {
            knowledge::remove(db, &id).await?;
            out(json!({ "removed": id }), format!("removed {id}"))
        }
        KbCommand::Approve { id } => flag(db, &id, "approved", true).await,
        KbCommand::Unapprove { id } => flag(db, &id, "approved", false).await,
        KbCommand::MarkSensitive { id } => flag(db, &id, "marked_sensitive", true).await,
        KbCommand::Unmark { id } => flag(db, &id, "marked_sensitive", false).await,
        KbCommand::Reveal { id } => {
            let body = knowledge::reveal(db, &id).await?;
            out(json!({ "id": id, "body": body }), body)
        }
        KbCommand::Search { query, limit } => search(db, &query, limit).await,
    }
}

async fn flag(db: &Db, id: &str, which: &str, on: bool) -> Result<Output> {
    if which == "approved" {
        knowledge::approve(db, id, on).await?;
    } else {
        knowledge::mark_sensitive(db, id, on).await?;
    }
    out(
        json!({ "id": id, which: on }),
        format!("{id}: {which} {on}"),
    )
}

/// Embeddings when a local model is set up, else a plain text match.
async fn search(db: &Db, query: &str, limit: usize) -> Result<Output> {
    let router = Router::from_settings(db).await?;
    let (method, hits) = match embed::search(db, &router, query, limit).await {
        Ok(hits) => ("embeddings", hits_json(&hits)),
        Err(Error::ManualRequired(_)) => {
            let hits = knowledge::search_text(db, query, limit).await?;
            ("text", hits_json(&hits))
        }
        Err(error) => return Err(error),
    };
    let text = hits
        .iter()
        .map(|h| {
            format!(
                "{:.3}  {}  {}",
                h["score"].as_f64().unwrap_or(0.0),
                h["item_id"].as_str().unwrap_or(""),
                first_line(h["body"].as_str().unwrap_or(""))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    out(json!({ "method": method, "hits": hits }), text)
}

fn hits_json(hits: &[impl serde::Serialize]) -> Vec<Value> {
    hits.iter()
        .filter_map(|h| serde_json::to_value(h).ok())
        .collect()
}
