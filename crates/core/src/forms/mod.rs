//! Copilot's view of a form: the fields an aria snapshot shows, read line by line. There is no
//! injected script and no HTML parser. A hidden input is absent from the snapshot, so it is
//! never a field and never filled (FR-9.7, FR-9.8). Page text here is data: it names fields,
//! and it never chooses a step, a file, or a tool.

pub(crate) mod map;

pub use map::{
    AnswerText, Candidate, MAX_MODEL_FIELDS, Mapped, Pick, ValueSource, map_field, map_rules,
};

use crate::apply::guard;

#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// The snapshot ref the field is reached by.
    pub target: String,
    pub role: String,
    pub label: String,
    /// What the page already holds in a text field.
    pub value: Option<String>,
    pub checked: bool,
    /// A combobox's options, as the page names them.
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    /// The `Page URL` the snapshot reports, or `""`.
    pub url: String,
    /// The scheme, host, and port of `url`, or `""` when it has none.
    pub origin: String,
    pub fields: Vec<FormField>,
    /// The ref of the last button that sends the application.
    pub submit: Option<String>,
}

/// Lines that can caption a group with no name of its own.
const CAPTIONS: [&str; 4] = ["paragraph", "text", "generic", "label"];
/// The roles a value goes into. A file input shows as a `button`, read apart.
const VALUE_ROLES: [&str; 4] = ["textbox", "combobox", "checkbox", "radio"];

/// Lower case, letters and digits only, one space between words. `"Resume/CV *"` is
/// `"resume cv"`.
pub fn fold_label(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{ad}'
            )
        })
        .map(|c| match c.is_alphanumeric() {
            true => c,
            false => ' ',
        })
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Which file a label asks for: `cover letter` reads first, so "Resume and cover letter" is
/// the letter's only if it says so. `None` when it asks for neither.
pub(crate) fn file_kind(label: &str) -> Option<&'static str> {
    let folded = fold_label(label);
    let words: Vec<&str> = folded.split(' ').collect();
    if folded.contains("cover letter") {
        Some("letter")
    } else if words.iter().any(|word| matches!(*word, "resume" | "cv")) {
        Some("resume")
    } else {
        None
    }
}

/// A name that may send the application: `apply`, `submit`, or `send` as a whole word.
fn sends(name: &str) -> bool {
    fold_label(name)
        .split(' ')
        .any(|word| matches!(word, "apply" | "submit" | "send"))
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The scheme, host, and port of a web address, or `""`.
pub(crate) fn origin_of(url: &str) -> String {
    match reqwest::Url::parse(url.trim()) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url.origin().ascii_serialization(),
        _ => String::new(),
    }
}

/// The `Page URL` line Playwright puts on a snapshot reply.
fn page_url(text: &str) -> String {
    text.lines()
        .find_map(|line| line.split_once("Page URL:").map(|(_, url)| url.trim()))
        .unwrap_or("")
        .to_owned()
}

/// Reads the snapshot shape in the plan's vendor facts. A field is a `textbox`, `combobox`,
/// `checkbox`, or `radio` with a ref, or a `button` that stands for a file input: its own name,
/// or the caption of the `group` it sits in, asks for a resume, a CV, or a cover letter. A
/// group's caption is its name, or with none, the first text inside it before any button, as
/// Chromium shows a group its `aria-labelledby` names. A combobox's `option` lines are its
/// options. Nothing else is a field.
pub fn parse_snapshot(text: &str) -> Form {
    let url = page_url(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut fields = Vec::new();
    let mut submit = None;
    // The groups that hold the current line: (indent, name, whether a file button of it was
    // taken). One button per group stands for its file: "Attach", not also "Remove".
    let mut groups: Vec<(usize, String, bool)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let body = line.trim_start();
        if !body.starts_with("- ") {
            continue;
        }
        let depth = indent(line);
        groups.retain(|(at, _, _)| *at < depth);
        let (role, name, inline) = guard::split_line(line);
        let name: String = name.chars().filter(|c| !c.is_control()).collect();
        if let Some((_, caption, taken)) = groups.last_mut()
            && caption.is_empty()
            && !*taken
            && CAPTIONS.contains(&role.as_str())
        {
            *caption = match name.is_empty() {
                true => inline.trim().to_owned(),
                false => name.clone(),
            };
        }
        if role == "group" {
            groups.push((depth, name.clone(), false));
        }
        let Some(target) = guard::ref_of(line) else {
            continue;
        };
        if role == "button" {
            if guard::submit_target(text, &target) {
                submit = Some(target);
                continue;
            }
            // A button that may send the application is never a file field.
            if sends(&name) {
                if let Some((_, _, taken)) = groups.last_mut() {
                    *taken = true;
                }
                continue;
            }
            let group = groups.last_mut();
            let label = match (file_kind(&name), group) {
                (Some(_), _) => name,
                (None, Some((_, caption, taken))) if !*taken && file_kind(caption).is_some() => {
                    *taken = true;
                    caption.clone()
                }
                (None, Some((_, _, taken))) => {
                    // A button that is not the file's ends the caption's search.
                    *taken = true;
                    continue;
                }
                _ => continue,
            };
            fields.push(FormField {
                target,
                role,
                label,
                value: None,
                checked: false,
                options: Vec::new(),
            });
            continue;
        }
        if !VALUE_ROLES.contains(&role.as_str()) {
            continue;
        }
        let checked = line
            .match_indices("[checked]")
            .any(|(at, _)| guard::outside_quotes(line, at));
        let options = match role.as_str() {
            "combobox" => lines[index + 1..]
                .iter()
                .take_while(|next| next.trim().is_empty() || indent(next) > depth)
                .filter_map(|next| {
                    let (role, name, _) = guard::split_line(next);
                    (role == "option" && !name.is_empty()).then_some(name)
                })
                .collect(),
            _ => Vec::new(),
        };
        let value = match role.as_str() {
            "textbox" if !inline.trim().is_empty() => Some(inline.trim().to_owned()),
            _ => None,
        };
        fields.push(FormField {
            target,
            role,
            label: name,
            value,
            checked,
            options,
        });
    }
    Form {
        origin: origin_of(&url),
        url,
        fields,
        submit,
    }
}
