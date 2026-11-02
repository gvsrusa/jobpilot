//! Seniority, skills, and the other fields rules can read from a posting. Rust never invents a
//! field: what the rules cannot read stays unknown.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::PayRange;
use super::geo::country_code;
use super::pay::parse_pay;

const SKILLS: &str = include_str!("../../data/skills.txt");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Seniority {
    Intern,
    Junior,
    Mid,
    Senior,
    Staff,
    Executive,
}

impl Seniority {
    /// The level a stored form names.
    pub fn parse(stored: &str) -> Option<Self> {
        [
            Self::Intern,
            Self::Junior,
            Self::Mid,
            Self::Senior,
            Self::Staff,
            Self::Executive,
        ]
        .into_iter()
        .find(|level| level.as_str() == stored)
    }

    /// The stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intern => "intern",
            Self::Junior => "junior",
            Self::Mid => "mid",
            Self::Senior => "senior",
            Self::Staff => "staff",
            Self::Executive => "executive",
        }
    }
}

/// Lower-case words of a text, split at anything that is not a letter or digit.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The highest level a title names.
fn title_level(title: &str) -> Option<Seniority> {
    let words = words(title);
    let mut best: Option<Seniority> = None;
    let mut note = |level: Seniority| best = Some(best.map_or(level, |b| b.max(level)));
    for (at, word) in words.iter().enumerate() {
        let next = words.get(at + 1).map(String::as_str);
        match word.as_str() {
            "intern" | "internship" | "trainee" => note(Seniority::Intern),
            "junior" | "jr" | "graduate" => note(Seniority::Junior),
            "entry" if next == Some("level") => note(Seniority::Junior),
            "mid" | "intermediate" => note(Seniority::Mid),
            "senior" | "sr" => note(Seniority::Senior),
            "staff" | "principal" | "lead" | "manager" => note(Seniority::Staff),
            "director" | "vp" | "chief" => note(Seniority::Executive),
            "head" if next == Some("of") => note(Seniority::Executive),
            _ => {}
        }
    }
    best
}

static YEARS_PLUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})\s*\+\s*years?\b").expect("years pattern"));
static YEARS_OF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(\d{1,2})(?:\s*[-–]\s*\d{1,2})?\s+years?\s+(?:of\s+)?(?:\w+\s+){0,2}?experience\b",
    )
    .expect("years of pattern")
});

/// The title's level, else the first stated years of experience: 0 to 1 junior, 2 to 4 mid, 5 or
/// more senior.
// ponytail: English only. A years figure that is about the company (`25+ years in business`)
// reads as a requirement, and the first figure in the text is the only one read.
pub fn seniority_of(title: &str, text: &str) -> Option<Seniority> {
    if let Some(level) = title_level(title) {
        return Some(level);
    }
    let first = [&*YEARS_PLUS, &*YEARS_OF]
        .into_iter()
        .filter_map(|pattern| pattern.captures(text))
        .filter_map(|found| {
            Some((
                found.get(0)?.start(),
                found.get(1)?.as_str().parse::<u32>().ok()?,
            ))
        })
        .min_by_key(|(start, _)| *start)?;
    Some(match first.1 {
        0..=1 => Seniority::Junior,
        2..=4 => Seniority::Mid,
        _ => Seniority::Senior,
    })
}

/// One vocabulary line: `Canonical|alias|=ExactCaseAlias`. The first name is what is stored.
/// A name matches without regard to case unless it starts with `=`.
fn names(entry: &str) -> impl Iterator<Item = (&str, bool)> {
    entry.split('|').filter_map(|name| {
        let exact = name.starts_with('=');
        let name = name.trim_start_matches('=').trim();
        (!name.is_empty()).then_some((name, exact))
    })
}

fn canonical(entry: &str) -> &str {
    names(entry).next().map_or("", |(name, _)| name)
}

/// The skills list plus the candidate's own, which join after it and add nothing already named.
pub fn vocabulary(extra: &[String]) -> Vec<String> {
    let mut all: Vec<String> = SKILLS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    for skill in extra {
        let skill: String = skill.chars().filter(|c| *c != '|').collect();
        let skill = skill.trim().trim_start_matches('=').trim();
        if skill.is_empty() || skill.chars().count() > 60 {
            continue;
        }
        let known = all
            .iter()
            .any(|entry| names(entry).any(|(name, _)| name.eq_ignore_ascii_case(skill)));
        if !known {
            all.push(skill.to_owned());
        }
    }
    all
}

/// A name found in `hay` with no letter or digit against it. `C++` and `.NET` are literal, and
/// `+`, `#`, and `&` also bind (`R&D` is not `R`); a case-exact name is not part of a
/// hyphenated word (`Go-to-market`).
fn found(hay: &str, name: &str, exact: bool) -> bool {
    hay.match_indices(name).any(|(at, _)| {
        let before = &hay[..at];
        let after = &hay[at + name.len()..];
        let binds = |c: char, after_side: bool| {
            c.is_alphanumeric()
                || c == '&'
                || (after_side && matches!(c, '+' | '#'))
                || (exact && c == '-')
        };
        // A dot inside a word (`node.js`, `asp.net`) joins its two sides.
        let dotted_before = before.ends_with('.')
            && before[..before.len() - 1]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        let dotted_after =
            after.starts_with('.') && after[1..].chars().next().is_some_and(char::is_alphanumeric);
        !before.chars().next_back().is_some_and(|c| binds(c, false))
            && !after.chars().next().is_some_and(|c| binds(c, true))
            && !dotted_before
            && !dotted_after
    })
}

/// The vocabulary's skills that the text names or the tags list, in vocabulary order. A tag
/// outside the vocabulary is dropped. `Go` is read as `Golang` or the exact word `Go`, and `R`
/// only as the capital word.
// ponytail: a capitalised `Go` that starts a sentence (`Go to market`) is read as the language.
pub fn skills_in(text: &str, tags: &[String], vocabulary: &[String]) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for entry in vocabulary {
        let hit = names(entry).any(|(name, exact)| {
            if exact {
                found(text, name, true) || tags.iter().any(|tag| tag.trim() == name)
            } else {
                let name = name.to_lowercase();
                found(&lower, &name, false)
                    || tags.iter().any(|tag| tag.trim().to_lowercase() == name)
            }
        });
        let name = canonical(entry);
        if hit && !out.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
            out.push(name.to_owned());
        }
    }
    out
}

/// Whether the text speaks of work authorization at all. It sets a flag to show; nothing reads
/// the words to decide anything (FR-4.10).
pub fn mentions_authorization(text: &str) -> bool {
    crate::drafts::states_authorization(text)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fields {
    pub country: Option<String>,
    pub work_mode: Option<&'static str>,
    pub pay: Option<PayRange>,
    pub seniority: Option<Seniority>,
    pub skills: Vec<String>,
    pub mentions_authorization: bool,
}

/// Remote, hybrid, or on-site, from the title and the location only: one kind named, else none.
// ponytail: the description is not read for a mode, since "not a remote role" says remote.
fn work_mode_of(title: &str, location: &str) -> Option<&'static str> {
    let text = format!(" {} {} ", words(title).join(" "), words(location).join(" "));
    let mut kinds: Vec<&'static str> = [
        ("remote", " remote "),
        ("hybrid", " hybrid "),
        ("onsite", " onsite "),
        ("onsite", " on site "),
    ]
    .into_iter()
    .filter(|(_, word)| text.contains(word))
    .map(|(kind, _)| kind)
    .collect();
    kinds.dedup();
    (kinds.len() == 1).then(|| kinds[0])
}

pub fn fields(
    title: &str,
    location: &str,
    text: &str,
    tags: &[String],
    vocabulary: &[String],
) -> Fields {
    Fields {
        country: country_code(location).map(str::to_owned),
        work_mode: work_mode_of(title, location),
        pay: parse_pay(text),
        seniority: seniority_of(title, text),
        skills: skills_in(&format!("{title}\n{text}"), tags, vocabulary),
        mentions_authorization: mentions_authorization(text),
    }
}
