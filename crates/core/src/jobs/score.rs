//! The match score: four weighted criteria, each shown with its weight, its points, and a note.
//! It is pure Rust on stored fields: no model, no network. A criterion either side leaves
//! unknown is left out and says so, and the rest rescale to 100. Nothing here is guessed.

use serde::{Deserialize, Serialize};

use super::geo::{self, Scope};
use super::pay::{PayCompare, compare};
use super::prefs::{self, MatchPrefs};
use super::rules::{self, Seniority};
use super::{Job, PayPeriod, PayRange};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::knowledge;
use crate::profile::{self, ProfileBasic, WorkMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Criterion {
    Skills,
    Seniority,
    Location,
    Pay,
}

pub const WEIGHTS: [(Criterion, u32); 4] = [
    (Criterion::Skills, 40),
    (Criterion::Seniority, 20),
    (Criterion::Location, 20),
    (Criterion::Pay, 20),
];

fn weight_of(criterion: Criterion) -> u32 {
    WEIGHTS
        .iter()
        .find(|(c, _)| *c == criterion)
        .map_or(0, |(_, w)| *w)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub criterion: Criterion,
    pub weight: u32,
    /// `None` when the criterion was not compared.
    pub points: Option<u32>,
    pub note: String,
}

fn by_rules() -> &'static str {
    "rules"
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Explanation {
    pub rows: Vec<Row>,
    /// `None` when nothing was compared.
    pub score: Option<u32>,
    pub compared_weight: u32,
    pub matched_skills: Vec<String>,
    pub missing_skills: Vec<String>,
    #[serde(skip_deserializing, default = "by_rules")]
    pub by: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub skills: Vec<String>,
    pub basic: ProfileBasic,
    pub prefs: MatchPrefs,
}

/// The candidate's skills: `skill` items split at commas, semicolons, and line breaks, plus the
/// vocabulary's skills found anywhere in the approved evidence. Sealed or sensitive items are
/// never opened here.
pub async fn candidate(db: &Db) -> Result<Candidate> {
    let evidence = knowledge::evidence(db).await?;
    let mut skills: Vec<String> = Vec::new();
    let mut add = |skill: &str| {
        let skill = skill.trim();
        if !skill.is_empty() && !skills.iter().any(|s| s.eq_ignore_ascii_case(skill)) {
            skills.push(skill.to_owned());
        }
    };
    for item in evidence.iter().filter(|item| item.kind == "skill") {
        item.body.split([',', ';', '\n', '\r']).for_each(&mut add);
    }
    let all: Vec<&str> = evidence.iter().map(|item| item.body.as_str()).collect();
    for skill in rules::skills_in(&all.join("\n"), &[], &rules::vocabulary(&[])) {
        add(&skill);
    }
    Ok(Candidate {
        skills,
        basic: profile::basic(db).await?,
        prefs: prefs::get(db).await?,
    })
}

fn round_div(numerator: u32, denominator: u32) -> u32 {
    (numerator * 2 + denominator) / (denominator * 2)
}

fn mode_name(mode: &WorkMode) -> &'static str {
    match mode {
        WorkMode::Remote => "remote",
        WorkMode::Hybrid => "hybrid",
        WorkMode::Onsite => "onsite",
    }
}

fn skills_row(
    job: &Job,
    me: &Candidate,
    matched: &mut Vec<String>,
    missing: &mut Vec<String>,
) -> Row {
    let weight = weight_of(Criterion::Skills);
    // The right to work is never scored, so a skill that names it is not compared.
    let named: Vec<&String> = job
        .skills
        .iter()
        .filter(|skill| !rules::names_right_to_work(skill))
        .collect();
    if named.is_empty() || me.skills.is_empty() {
        let why = if named.is_empty() {
            "the posting names no skill"
        } else {
            "you have no skills on file"
        };
        return Row {
            criterion: Criterion::Skills,
            weight,
            points: None,
            note: format!("not compared: {why}"),
        };
    }
    for skill in named {
        let has = me.skills.iter().any(|s| s.eq_ignore_ascii_case(skill));
        if has {
            matched.push(skill.clone());
        } else {
            missing.push(skill.clone());
        }
    }
    let total = (matched.len() + missing.len()) as u32;
    let points = round_div(weight * matched.len() as u32, total);
    Row {
        criterion: Criterion::Skills,
        weight,
        points: Some(points),
        note: format!("{} of {} skills named", matched.len(), total),
    }
}

fn seniority_row(job: &Job, me: &Candidate) -> Row {
    let weight = weight_of(Criterion::Seniority);
    let posted = job.seniority.as_deref().and_then(Seniority::parse);
    let (Some(posted), Some(wanted)) = (posted, me.prefs.seniority) else {
        let why = if posted.is_none() {
            "the posting states no level"
        } else {
            "you set no level"
        };
        return Row {
            criterion: Criterion::Seniority,
            weight,
            points: None,
            note: format!("not compared: {why}"),
        };
    };
    let steps = (posted as i32 - wanted as i32).unsigned_abs();
    let points = match steps {
        0 => weight,
        1 => weight / 2,
        _ => 0,
    };
    Row {
        criterion: Criterion::Seniority,
        weight,
        points: Some(points),
        note: format!("posting {}, you want {}", posted.as_str(), wanted.as_str()),
    }
}

/// The parts of a place, folded: `New York, United States` is `new york` and `united states`.
fn parts_of(place: &str) -> Vec<String> {
    place
        .split([',', ';', '/', '|', '(', ')'])
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Where a posting is: the text's own reading, with the stored country standing in when the
/// text names no place and over a country the text names.
enum Place {
    Anywhere,
    Region(String),
    Country(String),
    Unknown,
}

fn place_of(place: &str, country: Option<&str>) -> Place {
    match (geo::scope_of(place), country.map(str::trim)) {
        (Scope::Anywhere, _) => Place::Anywhere,
        (_, Some(code)) if !code.is_empty() => Place::Country(code.to_ascii_uppercase()),
        (Scope::Country(code), _) => Place::Country(code.to_owned()),
        (Scope::Region(region), _) => Place::Region(region.to_owned()),
        (Scope::Unknown, _) => Place::Unknown,
    }
}

/// Location points, or why the place and the profile cannot be compared. Unknown is never a
/// mismatch: a mode the posting leaves out skips the mode check, a profile with no country or
/// region cannot judge a country, and a place the table cannot read judges nothing.
fn location_score(
    mode: Option<&str>,
    place: &str,
    country: Option<&str>,
    basic: &ProfileBasic,
) -> std::result::Result<u32, &'static str> {
    let weight = weight_of(Criterion::Location);
    if let Some(mode) = mode
        && !basic.work_modes.is_empty()
        && !basic.work_modes.iter().any(|m| mode_name(m) == mode)
    {
        return Ok(0);
    }
    let found = place_of(place, country);
    let profile_places = !basic.countries.is_empty() || !basic.regions.is_empty();
    let in_country = |code: &str| {
        basic.countries.iter().any(|c| c.eq_ignore_ascii_case(code))
            || basic.regions.iter().any(|r| geo::in_region(r, code))
    };
    let region_fits = |region: &str| {
        basic.regions.iter().any(|r| r.eq_ignore_ascii_case(region))
            || basic.countries.iter().any(|c| geo::in_region(region, c))
    };
    if mode == Some("remote") {
        return match found {
            Place::Anywhere => Ok(weight),
            Place::Unknown if place.trim().is_empty() => Ok(15),
            Place::Unknown => Err("the posting's place is not recognised"),
            _ if !profile_places => Err("you name no country or region"),
            Place::Country(code) => Ok(if in_country(&code) { weight } else { 0 }),
            Place::Region(region) => Ok(if region_fits(&region) { weight } else { 0 }),
        };
    }
    if matches!(found, Place::Anywhere) {
        return Ok(weight);
    }
    let parts = parts_of(place);
    if basic.cities.iter().any(|city| {
        let city = city.trim().to_lowercase();
        !city.is_empty() && parts.contains(&city)
    }) {
        return Ok(weight);
    }
    if !profile_places {
        return Err("you name no country or region");
    }
    match found {
        Place::Country(code) if in_country(&code) => Ok(weight / 2),
        Place::Region(region) if region_fits(&region) => Ok(weight / 2),
        Place::Country(_) | Place::Region(_) => Ok(0),
        _ if place.trim().is_empty() => Err("the posting states no place"),
        _ => Err("the posting's place is not recognised"),
    }
}

/// Location points for a mode and a place against the profile, where the two can be compared;
/// `remote`, `hybrid`, or `onsite` are the modes.
pub fn location_points(mode: &str, place: &str, basic: &ProfileBasic) -> u32 {
    location_score(Some(mode), place, None, basic).unwrap_or(0)
}

fn location_row(job: &Job, me: &Candidate) -> Row {
    let weight = weight_of(Criterion::Location);
    let not = |why: &str| Row {
        criterion: Criterion::Location,
        weight,
        points: None,
        note: format!("not compared: {why}"),
    };
    let has_place = !job.location.trim().is_empty() || job.country.is_some();
    if me.basic.work_modes.is_empty() && me.basic.countries.is_empty() {
        return not("you set no work mode or country");
    }
    if job.work_mode.is_none() && !has_place {
        return not("the posting states no mode or place");
    }
    match location_score(
        job.work_mode.as_deref(),
        &job.location,
        job.country.as_deref(),
        &me.basic,
    ) {
        Ok(points) => {
            let shown = match (job.location.trim(), &job.country) {
                ("", Some(country)) => country.as_str(),
                ("", None) => "no place stated",
                (text, _) => text,
            };
            let mode_note = job.work_mode.as_deref().unwrap_or("mode not stated");
            Row {
                criterion: Criterion::Location,
                weight,
                points: Some(points),
                note: format!("{mode_note}, {shown}"),
            }
        }
        Err(why) => not(why),
    }
}

fn thousands(amount: i64) -> String {
    if amount >= 1000 && amount % 1000 == 0 {
        format!("{}k", amount / 1000)
    } else {
        amount.to_string()
    }
}

fn pay_text(pay: &PayRange) -> String {
    let currency = pay.currency.as_deref().unwrap_or("");
    let figures = match (pay.min, pay.max) {
        (Some(min), Some(max)) if min != max && min % 1000 == 0 && max % 1000 == 0 => {
            format!("{}-{}k", min / 1000, max / 1000)
        }
        (Some(min), Some(max)) if min != max => format!("{min}-{max}"),
        (Some(one), _) | (_, Some(one)) => thousands(one),
        (None, None) => String::new(),
    };
    format!("{figures} {currency}").trim().to_owned()
}

fn period_word(period: PayPeriod) -> &'static str {
    match period {
        PayPeriod::Hour => "hour",
        PayPeriod::Day => "day",
        PayPeriod::Week => "week",
        PayPeriod::Month => "month",
        PayPeriod::Year => "year",
    }
}

fn pay_row(job: &Job, me: &Candidate) -> Row {
    let weight = weight_of(Criterion::Pay);
    let not = |why: String| Row {
        criterion: Criterion::Pay,
        weight,
        points: None,
        note: format!("not compared: {why}"),
    };
    let Some(wish) = &me.prefs.min_pay else {
        return not("you set no pay wish".into());
    };
    let Some(pay) = &job.pay else {
        return not("the posting states no pay".into());
    };
    let per = |period: Option<PayPeriod>| match period {
        Some(PayPeriod::Year) | None => String::new(),
        Some(other) => format!(" per {}", period_word(other)),
    };
    let note = format!(
        "posting {}{}, you want {}+ {}{}",
        pay_text(pay),
        per(pay.period),
        thousands(wish.amount),
        wish.currency,
        per(Some(wish.period))
    );
    match compare(pay, wish) {
        PayCompare::Meets => Row {
            criterion: Criterion::Pay,
            weight,
            points: Some(weight),
            note,
        },
        PayCompare::Below { ratio } => Row {
            criterion: Criterion::Pay,
            weight,
            points: Some((weight as f32 * ratio).round() as u32),
            note,
        },
        PayCompare::NotCompared(why) => not(format!("{why} differs or is unstated ({note})")),
    }
}

/// Pure: the same job and candidate give the same explanation.
pub fn score(job: &Job, me: &Candidate) -> Explanation {
    let (mut matched, mut missing) = (Vec::new(), Vec::new());
    let rows = vec![
        skills_row(job, me, &mut matched, &mut missing),
        seniority_row(job, me),
        location_row(job, me),
        pay_row(job, me),
    ];
    let compared_weight: u32 = rows
        .iter()
        .filter(|r| r.points.is_some())
        .map(|r| r.weight)
        .sum();
    let earned: u32 = rows.iter().filter_map(|r| r.points).sum();
    let score = (compared_weight > 0).then(|| round_div(earned * 100, compared_weight));
    Explanation {
        rows,
        score,
        compared_weight,
        matched_skills: matched,
        missing_skills: missing,
        by: "rules",
    }
}

/// Every input of the score, as text. A stored score is stale when this differs.
pub fn inputs_key(job: &Job, me: &Candidate) -> String {
    serde_json::json!({
        "job": [job.skills, job.seniority, job.location, job.country, job.work_mode, job.pay],
        "skills": me.skills,
        "countries": me.basic.countries,
        "regions": me.basic.regions,
        "cities": me.basic.cities,
        "modes": me.basic.work_modes,
        "prefs": me.prefs,
    })
    .to_string()
}

pub async fn store(db: &Db, job_id: &str, explanation: &Explanation, inputs: &str) -> Result<()> {
    let text =
        serde_json::to_string(explanation).map_err(|error| Error::Validation(error.to_string()))?;
    let done = sqlx::query(
        "INSERT INTO match_results (job_id, score, compared_weight, explanation, provider, \
                                    inputs, created_at) \
         SELECT id, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         FROM jobs WHERE id = ?1 \
         ON CONFLICT (job_id) DO UPDATE SET score = excluded.score, \
             compared_weight = excluded.compared_weight, explanation = excluded.explanation, \
             provider = excluded.provider, inputs = excluded.inputs, \
             created_at = excluded.created_at",
    )
    .bind(job_id)
    .bind(explanation.score.map(i64::from))
    .bind(i64::from(explanation.compared_weight))
    .bind(text)
    .bind(explanation.by)
    .bind(inputs)
    .execute(&db.pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}
