//! Checks on every browser call of an apply session. All pure: the proxy owns the effects.
//! A page that needs a person (a CAPTCHA, a code, an identity check, a login, a legal
//! statement, a dialog) is a stop. Nothing here tries to get past one.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::brief::{AuthorizationFact, Brief};
use crate::jobs::geo;
use crate::memory::Class;
use crate::memory::rules::classify;

pub const BROWSER_TOOLS: [&str; 13] = [
    "browser_navigate",
    "browser_navigate_back",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_fill_form",
    "browser_select_option",
    "browser_press_key",
    "browser_hover",
    "browser_file_upload",
    "browser_handle_dialog",
    "browser_wait_for",
    "browser_tabs",
];

const CAPTCHA: [&str; 11] = [
    "captcha",
    "recaptcha",
    "hcaptcha",
    "turnstile",
    "verify you are human",
    "verify you're human",
    "i'm not a robot",
    "i am not a robot",
    "are you a robot",
    "press and hold",
    "security check",
];
const MFA: [&str; 8] = [
    "verification code",
    "one-time code",
    "one time code",
    "two-factor",
    "2-step verification",
    "authenticator app",
    "enter the code",
    "security code",
];
const IDENTITY: [&str; 9] = [
    "verify your identity",
    "identity verification",
    "government id",
    "government-issued",
    "photo id",
    "selfie",
    "passport number",
    "social security",
    "national insurance number",
];
const LOGIN_LINE: &str = "sign in to continue";
const LOGIN_NAME: &str = "password";
const LEGAL: [&str; 18] = [
    "i certify",
    "i hereby",
    "i declare",
    "i attest",
    "i acknowledge",
    "i agree",
    "under penalty",
    "electronic signature",
    "sign here",
    "truthful",
    "true and complete",
    "ai-generated",
    "written by ai",
    "use of ai",
    "your own work",
    "i confirm",
    "i consent",
    "i authorize",
];
const KEYS: [&str; 3] = ["Tab", "Shift+Tab", "Escape"];
/// The state flags Playwright prints on a line. They change as the page is used.
const FLAGS: [&str; 8] = [
    "checked", "selected", "expanded", "pressed", "disabled", "active", "focused", "cursor",
];
const DIALOG_ROLES: [&str; 2] = ["dialog", "alertdialog"];
const SUBMIT_WORDS: [&str; 3] = ["submit", "send application", "finish application"];
const EXCERPT: usize = 300;
const LONG_VALUE: usize = 2000;
const MIN_DIGITS: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseKind {
    Legal,
    Sensitive,
    Identity,
    Captcha,
    Mfa,
    Login,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: PauseKind,
    /// At most 300 characters from the match, for display.
    pub line: String,
    /// The whole folded line, which names this hit for an acknowledgement.
    pub key: String,
}

fn hidden(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{ad}'
    )
}

/// Lower case, without the characters that join or hide inside a word.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !hidden(*c))
        .collect::<String>()
        .to_lowercase()
}

/// The role, the name that follows it at once, and the inline text after `:`. A key Playwright
/// wrapped in quotes (`- 'role "name: x"':`) is read as the plain form.
fn split_line(line: &str) -> (String, String, String) {
    let body = line.trim_start().trim_start_matches("- ");
    let body = match body.chars().next() {
        Some('\'') => body[1..].replace("''", "'"),
        Some('"') => body[1..].to_owned(),
        _ => body.to_owned(),
    };
    let role = body
        .split(|c: char| c.is_whitespace() || c == ':')
        .next()
        .unwrap_or("")
        .to_owned();
    let mut name = String::new();
    let after = body[role.len()..].trim_start();
    let mut rest = after;
    if let Some(quoted) = after.strip_prefix('"') {
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => name.extend(chars.next()),
                '"' => break,
                c => name.push(c),
            }
        }
        rest = chars.as_str();
    }
    let inline = rest
        .match_indices(':')
        .find(|(at, _)| outside_quotes(rest, *at))
        .map(|(at, _)| rest[at + 1..].trim().trim_matches('"').to_owned())
        .unwrap_or_default();
    (role, name, inline)
}

fn parts(line: &str) -> (String, String) {
    let (role, name, _) = split_line(line);
    (role, name)
}

fn first_of(text: &str, words: &[&str]) -> Option<usize> {
    words.iter().filter_map(|word| text.find(word)).min()
}

/// Where in the folded line the stop for this kind begins.
fn matched(kind: PauseKind, folded: &str) -> Option<usize> {
    let (role, name) = parts(folded);
    match kind {
        PauseKind::Captcha => first_of(folded, &CAPTCHA),
        PauseKind::Mfa => first_of(folded, &MFA),
        PauseKind::Identity => first_of(folded, &IDENTITY),
        PauseKind::Login => {
            if role == "textbox" && name.contains(LOGIN_NAME) {
                folded.find(LOGIN_NAME)
            } else {
                folded.find(LOGIN_LINE)
            }
        }
        PauseKind::Legal => first_of(folded, &LEGAL),
        PauseKind::Other => DIALOG_ROLES.contains(&role.as_str()).then_some(0),
        PauseKind::Sensitive => None,
    }
}

const SCAN_ORDER: [PauseKind; 6] = [
    PauseKind::Captcha,
    PauseKind::Mfa,
    PauseKind::Identity,
    PauseKind::Login,
    PauseKind::Legal,
    PauseKind::Other,
];

/// The first stop on the page that a person has not already acknowledged. The whole line is
/// read; the excerpt is cut after the match starts.
pub fn scan(snapshot: &str, acknowledged: &HashSet<String>) -> Option<Hit> {
    let lines: Vec<String> = snapshot.lines().map(clean).collect();
    for kind in SCAN_ORDER {
        for folded in &lines {
            let Some(at) = matched(kind, folded) else {
                continue;
            };
            let line: String = folded[at..].chars().take(EXCERPT).collect();
            let key = format!("{}:{}", kind_name(kind), without_flags(folded));
            let hit = Hit { kind, line, key };
            if !acknowledged.contains(&hit.key) {
                return Some(hit);
            }
        }
    }
    None
}

/// The line without its state flags, so a ticked box keeps the key it had.
fn without_flags(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find('[') {
        let (before, tail) = rest.split_at(at);
        out.push_str(before);
        let end = tail.find(']').map_or(tail.len(), |e| e + 1);
        let flag = tail[1..end.saturating_sub(1).max(1)]
            .split('=')
            .next()
            .unwrap_or("");
        if !FLAGS.contains(&flag) {
            out.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn hit_key(hit: &Hit) -> String {
    hit.key.clone()
}

fn kind_name(kind: PauseKind) -> &'static str {
    match kind {
        PauseKind::Legal => "legal",
        PauseKind::Sensitive => "sensitive",
        PauseKind::Identity => "identity",
        PauseKind::Captcha => "captcha",
        PauseKind::Mfa => "mfa",
        PauseKind::Login => "login",
        PauseKind::Other => "other",
    }
}

/// True when `key` is present at any depth.
fn has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => map.contains_key(key) || map.values().any(|v| has_key(v, key)),
        Value::Array(items) => items.iter().any(|v| has_key(v, key)),
        _ => false,
    }
}

/// A reason a call is refused whatever the page says, or `None`.
pub fn refused_args(tool: &str, args: &Value, debug: bool) -> Option<&'static str> {
    if has_key(args, "filename") {
        return Some("filename_refused");
    }
    if has_key(args, "isolatedContext") || has_key(args, "isolated_context") {
        return Some("isolated_context_refused");
    }
    if tool == "browser_type" && args.get("slowly").and_then(Value::as_bool) == Some(true) {
        return Some("slowly_refused");
    }
    if tool == "browser_snapshot" && (args.get("target").is_some() || args.get("depth").is_some()) {
        return Some("snapshot_scope_refused");
    }
    if tool == "browser_press_key" {
        let allowed = args
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|key| KEYS.contains(&key.trim()));
        if !allowed {
            return Some("key_refused");
        }
    }
    if tool == "browser_type" && args.get("submit").and_then(Value::as_bool) == Some(true) {
        return Some("submit_key_refused");
    }
    if matches!(tool, "browser_navigate" | "browser_tabs")
        && let Some(url) = args.get("url")
    {
        let ok = url.as_str().is_some_and(|url| url_ok(url, debug));
        if !ok {
            return Some("url_refused");
        }
    }
    None
}

fn url_ok(url: &str, debug: bool) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => true,
        "http" => debug && url.host_str() == Some("127.0.0.1"),
        _ => false,
    }
}

/// Each path, with links resolved, is exactly one of the two staged files.
pub fn upload_ok(paths: &[String], files: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(files) else {
        return false;
    };
    let staged = [root.join("resume.pdf"), root.join("cover-letter.pdf")];
    paths.iter().all(|path| {
        Path::new(path).is_absolute()
            && std::fs::canonicalize(path).is_ok_and(|real| staged.contains(&real))
    })
}

fn fold(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| c.is_whitespace() || ",;:()/".contains(c))
        .filter(|word| !word.is_empty())
        .collect()
}

/// A value is allowed when it is one of the owner's own, or a run of whole words of one.
/// A value with no letters is allowed by equality or by the digit rule only.
pub fn value_ok(value: &str, allowed: &[String]) -> bool {
    let value = fold(value);
    if value.is_empty() {
        return true;
    }
    let allowed: Vec<String> = allowed.iter().map(|a| fold(a)).collect();
    if allowed.contains(&value) {
        return true;
    }
    if value.chars().count() > LONG_VALUE {
        return false;
    }
    if value.chars().any(char::is_alphabetic) {
        let mine = words(&value);
        return value.chars().count() >= 2
            && !mine.is_empty()
            && allowed.iter().any(|a| {
                words(a)
                    .windows(mine.len())
                    .any(|run| run == mine.as_slice())
            });
    }
    let mine = digits(&value);
    mine.len() >= MIN_DIGITS
        && allowed.iter().any(|a| {
            let theirs = digits(a);
            theirs.len() >= MIN_DIGITS && theirs.contains(&mine)
        })
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        other => Some(other.to_string()),
    }
}

fn target_of(value: &Value) -> String {
    ["target", "ref"]
        .iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

/// What the value check covers: typed text, textbox values, and a dialog's prompt text.
pub fn values_in(tool: &str, args: &Value) -> Vec<String> {
    match tool {
        "browser_type" => args
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        "browser_fill_form" => args
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|field| {
                matches!(
                    field.get("type").and_then(Value::as_str),
                    Some("textbox" | "slider")
                )
            })
            .filter_map(|field| field.get("value").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
        "browser_handle_dialog" => args
            .get("promptText")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// Every `(target, value)` a call sets, of any field type, for `field_guard`.
pub fn fields_in(tool: &str, args: &Value) -> Vec<(String, String)> {
    match tool {
        "browser_type" => args
            .get("text")
            .and_then(Value::as_str)
            .map(|text| (target_of(args), text.to_owned()))
            .into_iter()
            .collect(),
        "browser_fill_form" => args
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|field| Some((target_of(field), text_of(field.get("value")?)?)))
            .collect(),
        "browser_select_option" => args
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|value| (target_of(args), value.to_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A ref as Playwright prints it: `e12`, or `f1e12` inside a frame. Anything else is a selector.
fn is_ref(target: &str) -> bool {
    let rest = match target.strip_prefix('f') {
        Some(frame) => {
            let after = frame.trim_start_matches(|c: char| c.is_ascii_digit());
            if after.len() == frame.len() {
                return false;
            }
            after
        }
        None => target,
    };
    rest.strip_prefix('e')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The index of the one snapshot line that holds `[ref=<target>]`. A marker inside a quoted
/// name is not a ref, and a ref held twice is no element.
fn index_of(snapshot: &str, target: &str) -> Option<usize> {
    if !is_ref(target) {
        return None;
    }
    let marker = format!("[ref={target}]");
    let mut found = snapshot.lines().enumerate().filter(|(_, line)| {
        line.match_indices(&marker)
            .any(|(at, _)| outside_quotes(line, at))
    });
    let (index, _) = found.next()?;
    found.next().is_none().then_some(index)
}

fn line_of<'a>(snapshot: &'a str, target: &str) -> Option<&'a str> {
    snapshot.lines().nth(index_of(snapshot, target)?)
}

/// True when byte `at` follows an even number of unescaped double quotes.
fn outside_quotes(line: &str, at: usize) -> bool {
    let mut quotes = 0;
    let mut chars = line[..at].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => quotes += 1,
            _ => {}
        }
    }
    quotes % 2 == 0
}

/// True only when the snapshot holds this exact ref. A selector is never known.
pub fn target_known(snapshot: &str, target: &str) -> bool {
    line_of(snapshot, target).is_some()
}

/// Every element a call names, as written. A target that is not text is `""`, never known.
pub fn targets_in(tool: &str, args: &Value) -> Vec<String> {
    let one = |value: &Value| {
        ["target", "ref"]
            .iter()
            .find_map(|key| value.get(key))
            .map(|t| t.as_str().unwrap_or("").to_owned())
    };
    match tool {
        "browser_click" | "browser_type" | "browser_hover" | "browser_select_option" => {
            one(args).into_iter().collect()
        }
        "browser_fill_form" => args
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|field| one(field).unwrap_or_default())
            .collect(),
        _ => Vec::new(),
    }
}

/// The accessible name of the element, as the snapshot shows it, without hidden characters.
pub fn label_of(snapshot: &str, target: &str) -> Option<String> {
    let line = line_of(snapshot, target)?;
    let (_, name) = parts(line);
    let name: String = name.chars().filter(|c| !hidden(*c)).collect();
    (!name.is_empty()).then_some(name)
}

pub fn role_of(snapshot: &str, target: &str) -> Option<String> {
    let (role, _) = parts(&line_of(snapshot, target)?.to_lowercase());
    Some(role)
}

/// A checkbox or radio the snapshot shows with no name (Playwright drops names over 900
/// characters). The proxy pauses on it as `Legal`.
pub fn unnamed_choice(snapshot: &str, target: &str) -> bool {
    matches!(
        role_of(snapshot, target).as_deref(),
        Some("checkbox" | "radio")
    ) && label_of(snapshot, target).is_none()
}

/// A button or link that sends the application. "Apply" and "Apply now" are navigation.
pub fn submit_target(snapshot: &str, target: &str) -> bool {
    let Some(line) = line_of(snapshot, target) else {
        return false;
    };
    let folded = clean(line);
    let (role, name) = parts(&folded);
    matches!(role.as_str(), "button" | "link")
        && SUBMIT_WORDS.iter().any(|word| name.contains(word))
}

fn label_words(label: &str) -> Vec<String> {
    clean(label)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Words that make a work authorization question something other than "authorized?".
const REFUSED_WORDS: [&str; 15] = [
    "citizen",
    "citizenship",
    "resident",
    "residency",
    "permit",
    "unauthorized",
    "not",
    "never",
    "without",
    "and",
    "or",
    "either",
    "other",
    "any",
    "outside",
];
/// A label naming any of these is about a place beyond the one country.
const OTHER_PLACES: [&str; 9] = [
    "europe", "european", "eu", "virgin", "latin", "south", "north", "new", "territor",
];

fn has_phrase(padded: &str, phrase: &str) -> bool {
    padded.contains(&format!(" {} ", label_words(phrase).join(" ")))
}

/// True when the label names the country: a table name, or an alias from the short table. `us`
/// counts only as `US` or `U.S.` in capitals.
fn names_country(label: &str, padded: &str, own: &str) -> bool {
    let aliases: &[&str] = match own {
        "US" => &[
            "united states",
            "united states of america",
            "usa",
            "u s",
            "u s a",
        ],
        "GB" => &["united kingdom", "uk", "u k", "great britain", "britain"],
        _ => &[],
    };
    let capitals = label.contains("U.S.")
        || label
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "US");
    if own == "US" && capitals && has_phrase(padded, "us") {
        return true;
    }
    let table = geo::countries()
        .iter()
        .filter(|country| country.code == own)
        .map(|country| country.name.trim_end_matches(" (country)"));
    aliases
        .iter()
        .copied()
        .chain(table)
        .any(|name| has_phrase(padded, name))
}

/// The stated yes or no for a plainly worded question, compared to the value. Nothing is
/// inferred: a label that is about citizenship, status, a visa, more than one place, or a
/// place other than the fact's country has no answer here.
pub fn authorization_ok(label: &str, value: &str, fact: &AuthorizationFact) -> bool {
    let folded = clean(label).replace("now or in the future", " ");
    if folded.contains("n't") || folded.contains("n\u{2019}t") || folded.contains("green card") {
        return false;
    }
    let sponsor = folded.contains("sponsor");
    let padded = format!(" {} ", label_words(&folded).join(" "));
    let word = |w: &str| padded.contains(&format!(" {w} "));
    if REFUSED_WORDS.iter().any(|w| word(w)) {
        return false;
    }
    if word("visa") && !sponsor {
        return false;
    }
    if !names_country(label, &padded, &fact.country) {
        return false;
    }
    let other_country = geo::countries().iter().any(|country| {
        country.code != fact.country
            && has_phrase(&padded, country.name.trim_end_matches(" (country)"))
    });
    if other_country
        || OTHER_PLACES
            .iter()
            .any(|w| word(w) || *w == "territor" && folded.contains(w))
    {
        return false;
    }
    let stated = if sponsor {
        &fact.needs_sponsorship
    } else {
        &fact.authorized
    };
    (stated == "yes" || stated == "no") && fold(value) == *stated
}

/// One text is the other, or a run of its whole words.
fn same_question(a: &str, b: &str) -> bool {
    let (a, b) = (label_words(a), label_words(b));
    let within = |small: &[String], big: &[String]| {
        !small.is_empty() && big.windows(small.len()).any(|run| run == small)
    };
    within(&a, &b) || within(&b, &a)
}

/// A field that needs a person returns the kind of pause to open. This is never a refusal.
/// A sensitive label takes only the released answer to its own question.
pub fn field_guard(label: &str, value: &str, brief: &Brief) -> Result<(), PauseKind> {
    let class = classify(label);
    if class == Class::Legal || LEGAL.iter().any(|word| clean(label).contains(word)) {
        return Err(PauseKind::Legal);
    }
    match class {
        Class::WorkAuthorization => match &brief.work_authorization {
            Some(fact) if authorization_ok(label, value, fact) => Ok(()),
            _ => Err(PauseKind::Sensitive),
        },
        Class::Sensitive => {
            let value = fold(value);
            let mut matching = brief
                .answers
                .iter()
                .filter(|answer| same_question(label, &answer.question));
            let answered = matches!(
                (matching.next(), matching.next()),
                (Some(answer), None) if fold(&answer.answer) == value
            );
            if answered {
                Ok(())
            } else {
                Err(PauseKind::Sensitive)
            }
        }
        Class::Normal | Class::Legal => Ok(()),
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Roles whose click is not the choice of an answer. Every other role is judged.
const PLAIN_ROLES: [&str; 6] = ["button", "link", "tab", "combobox", "textbox", "searchbox"];
/// Roles that carry the text of a question beside a group.
const LABEL_ROLES: [&str; 6] = ["text", "heading", "paragraph", "label", "legend", "generic"];

/// True when a click on the target picks an answer, so `choice_guard` must pass first. An
/// unknown target counts as a choice.
pub fn is_choice(snapshot: &str, target: &str) -> bool {
    match role_of(snapshot, target) {
        Some(role) => !PLAIN_ROLES.contains(&role.as_str()),
        None => true,
    }
}

/// The text a line carries: its name, or else its inline text.
fn text_of_line(line: &str) -> String {
    let (_, name, inline) = split_line(line);
    let text = if name.is_empty() { inline } else { name };
    text.chars().filter(|c| !hidden(*c)).collect()
}

/// The nearest earlier line at this line's indentation that is a text, heading, paragraph,
/// label, legend, or generic with text.
fn label_before(lines: &[&str], index: usize) -> Option<String> {
    let indent = indent_of(lines[index]);
    for line in lines[..index].iter().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let here = indent_of(line);
        if here < indent {
            return None;
        }
        if here == indent {
            let (role, _, _) = split_line(line);
            let text = text_of_line(line);
            if LABEL_ROLES.contains(&role.to_lowercase().as_str()) && !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn class_of(text: &str) -> Class {
    if LEGAL.iter().any(|word| clean(text).contains(word)) {
        return Class::Legal;
    }
    classify(text)
}

/// A choice is judged by every question around it: each named ancestor, and the label text
/// before the option and before each ancestor. The option's own name counts too. A Legal one
/// pauses `Legal`; a Sensitive or work authorization one needs the released answer. An unnamed
/// checkbox or radio is a `Legal` pause. A role that is not a choice passes.
pub fn choice_guard(snapshot: &str, target: &str, brief: &Brief) -> Result<(), PauseKind> {
    let Some(index) = index_of(snapshot, target) else {
        return Err(PauseKind::Legal);
    };
    let lines: Vec<&str> = snapshot.lines().collect();
    let (role, name) = parts(lines[index]);
    let role = role.to_lowercase();
    if PLAIN_ROLES.contains(&role.as_str()) {
        return Ok(());
    }
    let option: String = name.chars().filter(|c| !hidden(*c)).collect();
    let mut questions: Vec<String> = Vec::new();
    questions.extend(label_before(&lines, index));
    let mut limit = indent_of(lines[index]);
    for at in (0..index).rev() {
        let line = lines[at];
        if line.trim().is_empty() || indent_of(line) >= limit {
            continue;
        }
        limit = indent_of(line);
        let (_, ancestor) = parts(line);
        if !ancestor.is_empty() {
            questions.push(ancestor);
        }
        questions.extend(label_before(&lines, at));
    }
    let own = class_of(&option);
    let classes: Vec<Class> = questions.iter().map(|q| class_of(q)).collect();
    if own == Class::Legal || classes.contains(&Class::Legal) {
        return Err(PauseKind::Legal);
    }
    if matches!(own, Class::Sensitive | Class::WorkAuthorization) {
        return Err(PauseKind::Sensitive);
    }
    for (question, class) in questions.iter().zip(&classes) {
        if matches!(class, Class::Sensitive | Class::WorkAuthorization) {
            field_guard(question, &option, brief)?;
        }
    }
    if option.is_empty() && matches!(role.as_str(), "checkbox" | "radio") {
        return Err(PauseKind::Legal);
    }
    Ok(())
}
