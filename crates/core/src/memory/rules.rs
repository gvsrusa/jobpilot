//! Classes by the words of a question: the first and only offline classifier.

use super::Class;
use crate::drafts::{fold, states_authorization};

const LEGAL: &[&str] = &[
    "certify",
    "attest",
    "declare",
    "declaration",
    "acknowledge",
    "under penalty",
    "perjury",
    "signature",
    "sign here",
    "truthful",
    "i agree",
    "i consent",
    "written by",
    "your own work",
    "generative",
    "artificial intelligence",
    "chatgpt",
];
/// Whole words: short enough to begin others.
const LEGAL_WHOLE: &[&str] = &["ai"];
const SENSITIVE: &[&str] = &[
    "salary",
    "compensation",
    "pay expectation",
    "expected pay",
    "wage",
    "disab",
    "gender",
    "sex",
    "male or female",
    "ethnicity",
    "race",
    "racial",
    "religio",
    "nationality",
    "veteran",
    "criminal",
    "convict",
    "felon",
    "date of birth",
    "pregnan",
    "national insurance",
    "social security",
    "marital",
    "marri",
    "how old",
];
/// Whole words: short enough to begin others.
const SENSITIVE_WHOLE: &[&str] = &["age", "ssn", "dob"];

/// Highest rank first, on the folded text, at the start of a word.
// ponytail: these match words, so a paraphrase passes until the model or the user raises the
// class, and a word that only sounds legal ("declare a major") raises it until a person
// lowers it.
pub fn classify(text: &str) -> Class {
    if states_authorization(text) {
        return Class::WorkAuthorization;
    }
    let folded = format!(" {} ", fold(text));
    let hits = |begins: &[&str], whole: &[&str]| {
        begins.iter().any(|w| folded.contains(&format!(" {w}")))
            || whole.iter().any(|w| folded.contains(&format!(" {w} ")))
    };
    if hits(LEGAL, LEGAL_WHOLE) {
        Class::Legal
    } else if hits(SENSITIVE, SENSITIVE_WHOLE) {
        Class::Sensitive
    } else {
        Class::Normal
    }
}
