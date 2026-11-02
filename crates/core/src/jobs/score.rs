//! The match score: four weighted criteria, each shown with its weight, its points, and a note.
//! It is pure Rust on stored fields: no model, no network. A criterion either side leaves
//! unknown is left out and says so, and the rest rescale to 100. Nothing here is guessed.

use serde::{Deserialize, Serialize};

use super::geo::{self, Scope};
use super::pay::{PayCompare, compare};
use super::prefs::{self, MatchPrefs};
use super::rules::{self, Seniority};
use super::{Job, PayRange};
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
    if job.skills.is_empty() || me.skills.is_empty() {
        let why = if job.skills.is_empty() {
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
    for skill in &job.skills {
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

fn folded(text: &str) -> String {
    text.to_lowercase()
}

/// Location points for a mode and a place against the profile. `mode` is `remote`, `hybrid`,
/// or `onsite` (anything else is read as on site); `place` is the posting's location text. A
/// mode outside the profile's work modes is 0. When the profile names no country or region,
/// only the mode is judged.
pub fn location_points(mode: &str, place: &str, basic: &ProfileBasic) -> u32 {
    let weight = weight_of(Criterion::Location);
    if !basic.work_modes.is_empty() && !basic.work_modes.iter().any(|m| mode_name(m) == mode) {
        return 0;
    }
    if basic.countries.is_empty() && basic.regions.is_empty() {
        return weight;
    }
    let scope = geo::scope_of(place);
    let in_country = |code: &str| {
        basic.countries.iter().any(|c| c.eq_ignore_ascii_case(code))
            || basic.regions.iter().any(|r| geo::in_region(r, code))
    };
    let region_fits = |region: &str| {
        basic.regions.iter().any(|r| r.eq_ignore_ascii_case(region))
            || basic.countries.iter().any(|c| geo::in_region(region, c))
    };
    if mode == "remote" {
        return match scope {
            Scope::Anywhere => weight,
            Scope::Country(code) if in_country(code) => weight,
            Scope::Region(region) if region_fits(region) => weight,
            Scope::Unknown => 15,
            _ => 0,
        };
    }
    let lower = folded(place);
    if basic
        .cities
        .iter()
        .any(|city| !city.trim().is_empty() && lower.contains(&folded(city.trim())))
    {
        return weight;
    }
    match scope {
        Scope::Country(code) if in_country(code) => weight / 2,
        Scope::Region(region) if region_fits(region) => weight / 2,
        _ => 0,
    }
}

fn location_row(job: &Job, me: &Candidate) -> Row {
    let weight = weight_of(Criterion::Location);
    let place = match (&job.country, geo::scope_of(&job.location)) {
        (Some(country), Scope::Unknown) if !job.location.trim().is_empty() => {
            format!("{}, {country}", job.location)
        }
        (Some(country), Scope::Unknown) => country.clone(),
        _ => job.location.clone(),
    };
    let place_known = geo::scope_of(&place) != Scope::Unknown;
    let has_place = place_known || !job.location.trim().is_empty();
    let profile_cares = !me.basic.work_modes.is_empty() || !me.basic.countries.is_empty();
    if !profile_cares || (job.work_mode.is_none() && !has_place) {
        let why = if profile_cares {
            "the posting states no mode or place"
        } else {
            "you set no work mode or country"
        };
        return Row {
            criterion: Criterion::Location,
            weight,
            points: None,
            note: format!("not compared: {why}"),
        };
    }
    let mode = job.work_mode.as_deref().unwrap_or("onsite");
    let points = location_points(mode, &place, &me.basic);
    let shown = if job.location.trim().is_empty() {
        "no place stated"
    } else {
        job.location.trim()
    };
    let mode_note = job.work_mode.as_deref().unwrap_or("mode not stated");
    Row {
        criterion: Criterion::Location,
        weight,
        points: Some(points),
        note: format!("{mode_note}, {shown}"),
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
    let note = format!(
        "posting {}, you want {}+ {}",
        pay_text(pay),
        thousands(wish.amount),
        wish.currency
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
