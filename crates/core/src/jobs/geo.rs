//! Place, by table. `data/countries.tsv` holds the 249 ISO 3166-1 alpha-2 countries with their
//! names and aliases, US states and Canadian provinces, and a few regions. Text is read by the
//! longest matching phrase on word boundaries; nothing is guessed from a city.
//!
//! Work authorization is not read here or anywhere in rules: this only says where a job is.

use std::collections::HashMap;
use std::sync::OnceLock;

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const TABLE: &str = include_str!("../../data/countries.tsv");

/// The longest phrase in the table, in words, so a scan never looks further.
const LONGEST: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Anywhere,
    Region(&'static str),
    Country(&'static str),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Country {
    pub code: &'static str,
    pub name: &'static str,
}

/// Words that mean any place at all.
const ANYWHERE: [&str; 4] = ["worldwide", "anywhere", "global", "anywhere in the world"];

struct Table {
    countries: Vec<Country>,
    /// A folded phrase to what it names.
    phrases: HashMap<String, Scope>,
    /// `TX`, `ON`: a state or province's two-letter form, matched after a comma only.
    subdivisions: HashMap<&'static str, &'static str>,
    codes: HashMap<&'static str, &'static str>,
    /// A region's key to the country codes it holds, other regions already expanded.
    regions: HashMap<&'static str, Vec<&'static str>>,
}

/// Lower case, diacritics dropped, and every run of non-alphanumerics one space.
fn fold(text: &str) -> String {
    let plain: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn table() -> &'static Table {
    static TABLE_ONCE: OnceLock<Table> = OnceLock::new();
    TABLE_ONCE.get_or_init(build)
}

fn build() -> Table {
    let mut table = Table {
        countries: Vec::new(),
        phrases: HashMap::new(),
        subdivisions: HashMap::new(),
        codes: HashMap::new(),
        regions: HashMap::new(),
    };
    let mut raw_regions: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for line in TABLE
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut cols = line.split('\t');
        let (Some(kind), Some(key), Some(names), Some(extra)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let scope = match kind {
            "C" => {
                let name = names.split(';').next().unwrap_or(names);
                table.countries.push(Country { code: key, name });
                table.codes.insert(key, key);
                Scope::Country(key)
            }
            "S" => {
                table.subdivisions.insert(extra, key);
                Scope::Country(key)
            }
            "R" => {
                raw_regions.push((key, extra.split(';').collect()));
                Scope::Region(key)
            }
            _ => continue,
        };
        // A region is also named by its key (`EMEA`).
        let key_phrase = (kind == "R").then_some(key);
        for name in names.split(';').chain(key_phrase) {
            let phrase = fold(name);
            if !phrase.is_empty() {
                table.phrases.insert(phrase, scope);
            }
        }
    }
    for word in ANYWHERE {
        table.phrases.insert(fold(word), Scope::Anywhere);
    }
    for (key, _) in &raw_regions {
        let mut codes = Vec::new();
        expand(key, &raw_regions, &mut codes, 0);
        table.regions.insert(key, codes);
    }
    table
}

fn expand(
    key: &str,
    all: &[(&'static str, Vec<&'static str>)],
    out: &mut Vec<&'static str>,
    depth: usize,
) {
    // A region that names itself, directly or not, stops at depth 4.
    if depth > 4 {
        return;
    }
    let Some((_, members)) = all.iter().find(|(name, _)| *name == key) else {
        return;
    };
    for &member in members {
        if all.iter().any(|(name, _)| *name == member) {
            expand(member, all, out, depth + 1);
        } else {
            out.push(member);
        }
    }
}

pub fn countries() -> &'static [Country] {
    &table().countries
}

/// What a location says about where. Order of strength: the longest phrase in the table (so
/// `New Mexico` is not `Mexico`, and `Anywhere in the World` is not a place), then a state or
/// province's two-letter form after a comma, then a country's own code after a comma.
// ponytail: a city names nothing. `Lausanne` is `Unknown` until the table grows past states,
// provinces, and countries, and a place written in another language is read only by the
// aliases listed.
pub fn scope_of(location: &str) -> Scope {
    let table = table();
    let folded = fold(location);
    let words: Vec<&str> = folded.split(' ').filter(|w| !w.is_empty()).collect();
    let mut best: Option<(usize, usize, Scope)> = None;
    for start in 0..words.len() {
        for len in 1..=LONGEST.min(words.len() - start) {
            let phrase = words[start..start + len].join(" ");
            if let Some(&scope) = table.phrases.get(&phrase) {
                let strength = (len, phrase.len());
                if best.is_none_or(|(l, p, _)| strength > (l, p)) {
                    best = Some((strength.0, strength.1, scope));
                }
            }
        }
    }
    if let Some((_, _, scope)) = best {
        return scope;
    }
    // After a comma, the first word of a part, in capitals, as it was written.
    for part in location.split(',').skip(1) {
        let Some(word) = part.split_whitespace().next() else {
            continue;
        };
        let word = word.trim_matches(|c: char| !c.is_ascii_alphabetic());
        if word.len() != 2 || !word.chars().all(|c| c.is_ascii_uppercase()) {
            continue;
        }
        if let Some(&country) = table.subdivisions.get(word) {
            return Scope::Country(country);
        }
        if let Some(&code) = table.codes.get(word) {
            return Scope::Country(code);
        }
    }
    Scope::Unknown
}

/// Whether the region (`EMEA`, `eu`) holds the country code. An unknown region holds nothing.
pub fn in_region(region: &str, country: &str) -> bool {
    let table = table();
    let region = region.trim().to_uppercase();
    let country = country.trim().to_uppercase();
    table
        .regions
        .get(region.as_str())
        .is_some_and(|codes| codes.iter().any(|code| *code == country))
}

/// The country a text names, if it names one: not a region, and not `Anywhere`.
pub fn country_code(text: &str) -> Option<&'static str> {
    match scope_of(text) {
        Scope::Country(code) => Some(code),
        _ => None,
    }
}
