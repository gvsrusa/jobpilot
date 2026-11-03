//! Checks on every browser call of an apply session. All pure: the proxy owns the effects.
//! A page that needs a person (a CAPTCHA, a code, an identity check, a login, a legal
//! statement, a dialog) is a stop. Nothing here tries to get past one.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::brief::{self, AuthorizationFact, Brief};
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
const LEGAL: [&str; 15] = [
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
];
const LEGAL_ROLES: [&str; 5] = ["checkbox", "radio", "button", "text", "paragraph"];
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
    pub line: String,
}

/// The role and the first quoted name of a snapshot line, both folded.
fn parts(folded: &str) -> (&str, String) {
    let body = folded.trim_start().trim_start_matches("- ");
    let role = body
        .split(|c: char| c.is_whitespace() || c == ':')
        .next()
        .unwrap_or("");
    let mut name = String::new();
    if let Some(open) = body.find('"') {
        let mut chars = body[open + 1..].chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => name.extend(chars.next()),
                '"' => break,
                c => name.push(c),
            }
        }
    }
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
        PauseKind::Legal => LEGAL_ROLES
            .contains(&role)
            .then(|| first_of(folded, &LEGAL))
            .flatten(),
        PauseKind::Other => DIALOG_ROLES.contains(&role).then_some(0),
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
    let lines: Vec<String> = snapshot.lines().map(str::to_lowercase).collect();
    for kind in SCAN_ORDER {
        for folded in &lines {
            let Some(at) = matched(kind, folded) else {
                continue;
            };
            let line: String = folded[at..].chars().take(EXCERPT).collect();
            let hit = Hit { kind, line };
            if !acknowledged.contains(&hit_key(&hit)) {
                return Some(hit);
            }
        }
    }
    None
}

pub fn hit_key(hit: &Hit) -> String {
    format!(
        "{}:{}",
        serde_json::json!(hit.kind).as_str().unwrap_or(""),
        hit.line
    )
}

/// A reason a call is refused whatever the page says, or `None`.
pub fn refused_args(tool: &str, args: &Value, debug: bool) -> Option<&'static str> {
    if args.get("filename").is_some() {
        return Some("filename_refused");
    }
    if args.get("isolatedContext").is_some() || args.get("isolated_context").is_some() {
        return Some("isolated_context_refused");
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
    paths
        .iter()
        .all(|path| std::fs::canonicalize(path).is_ok_and(|real| staged.contains(&real)))
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

/// A value is allowed when it is one of the owner's own, or a part of one.
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
    if value.chars().count() >= 2 && allowed.iter().any(|a| a.contains(&value)) {
        return true;
    }
    if !value.chars().any(char::is_alphabetic) {
        let mine = digits(&value);
        return mine.len() >= MIN_DIGITS
            && allowed.iter().any(|a| {
                let theirs = digits(a);
                theirs.len() >= MIN_DIGITS && theirs.contains(&mine)
            });
    }
    false
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
            .filter(|field| field.get("type").and_then(Value::as_str) == Some("textbox"))
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

/// The snapshot line that holds `[ref=<target>]`.
fn line_of<'a>(snapshot: &'a str, target: &str) -> Option<&'a str> {
    let marker = format!("[ref={target}]");
    snapshot.lines().find(|line| line.contains(&marker))
}

/// The accessible name of the element, as the snapshot shows it.
pub fn label_of(snapshot: &str, target: &str) -> Option<String> {
    let line = line_of(snapshot, target)?;
    let (_, name) = parts(line);
    (!name.is_empty()).then_some(name)
}

/// A button or link that sends the application. "Apply" and "Apply now" are navigation.
pub fn submit_target(snapshot: &str, target: &str) -> bool {
    let Some(line) = line_of(snapshot, target) else {
        return false;
    };
    let folded = line.to_lowercase();
    let (role, name) = parts(&folded);
    matches!(role, "button" | "link") && SUBMIT_WORDS.iter().any(|word| name.contains(word))
}

/// The stated yes or no for the label, compared to the value. Nothing is inferred.
pub fn authorization_ok(label: &str, value: &str, fact: &AuthorizationFact) -> bool {
    let stated = if fold(label).contains("sponsor") {
        &fact.needs_sponsorship
    } else {
        &fact.authorized
    };
    (stated == "yes" || stated == "no") && fold(value) == *stated
}

/// A field that needs a person returns the kind of pause to open. This is never a refusal.
pub fn field_guard(label: &str, value: &str, brief: &Brief) -> Result<(), PauseKind> {
    match classify(label) {
        Class::WorkAuthorization => match &brief.work_authorization {
            Some(fact) if authorization_ok(label, value, fact) => return Ok(()),
            _ => return Err(PauseKind::Sensitive),
        },
        Class::Legal => return Err(PauseKind::Legal),
        Class::Sensitive => {
            if !value_ok(value, &brief::allowed_values(brief)) {
                return Err(PauseKind::Sensitive);
            }
        }
        Class::Normal => {}
    }
    let folded = label.to_lowercase();
    if LEGAL.iter().any(|word| folded.contains(word)) {
        return Err(PauseKind::Legal);
    }
    Ok(())
}
