//! Duplicate groups, by rule and never by a model. The oldest row leads and later rows take
//! `duplicate_of`; no row is deleted. A row a person split (`not_duplicate`) takes no part.
//!
//! In order, a row matches an earlier one by: the same link once the host is lower-cased and
//! the query and fragment are dropped; or the same `dedupe_key`; or the same company with a
//! title that is [`SIMILAR`] enough, in the same country and work mode. A row with no usable
//! key and no link matches nothing.

use std::collections::{HashMap, HashSet};

use sqlx::FromRow;

use crate::db::Db;
use crate::drafts::fold as plain_fold;
use crate::error::Result;

/// Word-set Jaccard at or above this makes two titles the same title.
pub const SIMILAR: f32 = 0.85;

/// [`plain_fold`] that keeps `+` and `#`, so "C++" and "C#" stay two languages.
// ponytail: each is spelled out as a word before folding, so a run of them stays distinct.
pub(crate) fn fold(text: &str) -> String {
    plain_fold(&text.replace('+', " xplusx ").replace('#', " xsharpx "))
}

/// Legal forms that are not part of a company's name.
const LEGAL: [&str; 6] = ["inc", "llc", "ltd", "gmbh", "corp", "co"];

/// The company name folded for comparison: case, diacritics, punctuation, and legal forms
/// (`inc`, `llc`, `ltd`, `gmbh`, `s.a.`, `corp`, `co`) do not make a second company. A name of
/// nothing but a legal form has no key.
pub fn company_key(name: &str) -> String {
    let folded = fold(name);
    let words: Vec<&str> = folded.split(' ').filter(|word| !word.is_empty()).collect();
    let mut kept = Vec::new();
    let mut at = 0;
    while at < words.len() {
        // `s.a.` folds to two words.
        if words[at] == "s" && words.get(at + 1) == Some(&"a") {
            at += 2;
        } else {
            if !LEGAL.contains(&words[at]) {
                kept.push(words[at]);
            }
            at += 1;
        }
    }
    kept.join(" ")
}

/// Empty when the company or the title folds to nothing: such a row has no key.
pub fn dedupe_key(
    company: &str,
    title: &str,
    country: Option<&str>,
    work_mode: Option<&str>,
) -> String {
    let (company, title) = (company_key(company), fold(title));
    if company.is_empty() || title.is_empty() {
        return String::new();
    }
    format!(
        "{company}|{title}|{}|{}",
        country.map(str::to_lowercase).unwrap_or_default(),
        work_mode.unwrap_or_default()
    )
}

/// Word-set Jaccard on folded text. Two empty texts are not similar.
pub fn similar(a: &str, b: &str) -> f32 {
    let (a, b) = (fold(a), fold(b));
    let words = |text: &'_ str| -> HashSet<String> {
        text.split(' ')
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (a, b) = (words(&a), words(&b));
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f32 / union as f32
}

/// Parameters that name a visitor's route, not a page.
fn is_tracking(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("utm_") || matches!(name.as_str(), "ref" | "source" | "gclid" | "fbclid")
}

/// The link with a lower-case host, no fragment, and only its tracking parameters dropped:
/// `?gh_jid=1` and `?gh_jid=2` are two jobs. Empty when absent or not a link.
fn normal_url(url: Option<&str>) -> String {
    let Some(Ok(mut url)) = url.map(reqwest::Url::parse) else {
        return String::new();
    };
    url.set_fragment(None);
    let mut kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| !is_tracking(name))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    kept.sort();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    url.to_string()
}

#[derive(FromRow)]
struct Row {
    id: String,
    company: Option<String>,
    title: String,
    country: Option<String>,
    work_mode: Option<String>,
    location: String,
    url: Option<String>,
    dedupe_key: String,
    duplicate_of: Option<String>,
    not_duplicate: i64,
}

struct Seen {
    url: HashMap<String, usize>,
    key: HashMap<String, usize>,
    /// Company, country, and mode to the rows that share them, for the title check.
    near: HashMap<(String, String, String), Vec<usize>>,
}

/// Groups every job and returns how many rows changed their `duplicate_of`. Run again with
/// nothing new, it changes none. Rows are read oldest first, so the oldest of a group leads.
// ponytail: one pass over all jobs; the title check compares only within a company, country,
// and mode, which keeps a few thousand rows well under a second.
pub async fn run(db: &Db) -> Result<usize> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT j.id, c.name AS company, j.title, j.country, j.work_mode, j.location, j.url, j.dedupe_key, \
                j.duplicate_of, j.not_duplicate \
         FROM jobs j LEFT JOIN companies c ON c.id = j.company_id \
         ORDER BY j.first_seen_at, j.seq",
    )
    .fetch_all(&db.pool)
    .await?;

    let mut seen = Seen {
        url: HashMap::new(),
        key: HashMap::new(),
        near: HashMap::new(),
    };
    let mut leader: Vec<usize> = (0..rows.len()).collect();
    let mut keys = Vec::with_capacity(rows.len());
    for (at, row) in rows.iter().enumerate() {
        let company = row.company.as_deref().unwrap_or_default();
        let key = dedupe_key(
            company,
            &row.title,
            row.country.as_deref(),
            row.work_mode.as_deref(),
        );
        keys.push(key.clone());
        if row.not_duplicate != 0 {
            continue;
        }
        let url = normal_url(row.url.as_deref());
        // Two postings are one job only if the place they name is the same, folded. With no
        // country and no place, nothing says where, so only a link can match.
        let place = fold(&row.location);
        let placed = row.country.is_some() || !place.is_empty();
        let key = if placed && !key.is_empty() {
            format!("{key}@{place}")
        } else {
            String::new()
        };
        let near = (
            if placed {
                company_key(company)
            } else {
                String::new()
            },
            format!(
                "{}@{place}",
                row.country
                    .as_deref()
                    .map(str::to_lowercase)
                    .unwrap_or_default()
            ),
            row.work_mode.clone().unwrap_or_default(),
        );
        let mut found = None;
        if !url.is_empty() {
            found = seen.url.get(&url).copied();
        }
        if found.is_none() && !key.is_empty() {
            found = seen.key.get(&key).copied();
        }
        if found.is_none() && !near.0.is_empty() {
            found = seen.near.get(&near).and_then(|peers| {
                peers
                    .iter()
                    .copied()
                    .find(|peer| similar(&rows[*peer].title, &row.title) >= SIMILAR)
            });
        }
        if let Some(peer) = found {
            leader[at] = leader[peer];
        }
        if !url.is_empty() {
            seen.url.entry(url).or_insert(at);
        }
        if !key.is_empty() {
            seen.key.entry(key).or_insert(at);
        }
        if !near.0.is_empty() {
            seen.near.entry(near).or_default().push(at);
        }
    }

    let mut changed = 0;
    let mut tx = db.pool.begin().await?;
    for (at, row) in rows.iter().enumerate() {
        if keys[at] != row.dedupe_key {
            sqlx::query("UPDATE jobs SET dedupe_key = ? WHERE id = ?")
                .bind(&keys[at])
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
        }
        if row.not_duplicate != 0 {
            continue;
        }
        let wanted = (leader[at] != at).then(|| rows[leader[at]].id.as_str());
        if wanted != row.duplicate_of.as_deref() {
            sqlx::query("UPDATE jobs SET duplicate_of = ? WHERE id = ?")
                .bind(wanted)
                .bind(&row.id)
                .execute(&mut *tx)
                .await?;
            changed += 1;
        }
    }
    tx.commit().await?;
    Ok(changed)
}
