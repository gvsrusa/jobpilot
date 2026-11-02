//! Pay, read as written. A figure is pay only with a currency mark beside it, and a figure with
//! no stated period is a year only if it is 1,000 or more. Equity, bonus, and funding figures
//! are not pay. Comparison never converts a currency and never guesses a period.

use std::sync::LazyLock;

use regex::Regex;

use super::{PayPeriod, PayRange, PayWish};

/// The ISO codes read when written out, and `CHF`.
const CODES: &str = "USD|EUR|GBP|CAD|AUD|NZD|SGD|HKD|JPY|CNY|INR|CHF|SEK|NOK|DKK|PLN|CZK|HUF|BRL|MXN|ZAR|AED|ILS|KRW|TRY|RUB|THB|MYR|IDR|PHP";
const SYMBOLS: &str = "$€£¥₹";
const NUMBER: &str = r"\d{1,3}(?:[.,]\d{3})+(?:[.,]\d{1,2})?|\d+(?:[.,]\d+)?";

/// A figure or a range of two, with the currency before either or after the last.
static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(
        r"(?:(?P<code1>{CODES})\s?)?(?P<sym1>[{SYMBOLS}])?\s?(?P<a>{NUMBER})\s?(?P<ak>[kK]\b)?(?:\s*(?:[-–—]|(?i:\bto\b))\s*(?:(?P<code2>{CODES})\s?)?(?P<sym2>[{SYMBOLS}])?\s?(?P<b>{NUMBER})\s?(?P<bk>[kK]\b)?)?(?:\s?(?P<code3>{CODES})\b)?"
    );
    Regex::new(&pattern).expect("pay pattern")
});

static PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?P<hour>per\s+hour|/\s?h(?:ou)?r\b|an\s+hour|\bhourly\b|\ban\s+hr\b)|(?P<year>per\s+year|per\s+annum|\bannual(?:ly)?\b|/\s?(?:yr|year)\b|\ba\s+year\b)|(?P<month>per\s+month|/\s?(?:mo|month)\b|\ba\s+month\b)|(?P<day>per\s+day|/\s?day\b|\ba\s+day\b)|(?P<week>per\s+week|/\s?(?:wk|week)\b|\ba\s+week\b)",
    )
    .expect("period pattern")
});

/// A bonus or equity word just before a figure.
static BEFORE_NOT_PAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:bonus|equity|stock|funding|raised|revenue|valuation)(?:\s+(?:of|is|up to|worth|target|at))?\W*$")
        .expect("before pattern")
});

/// A bonus, equity, or company-size word just after a figure.
static AFTER_NOT_PAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\W{0,2}(?:(?:signing|sign-on|annual|performance|retention|target|referral|relocation|stock)\s+)?(?:bonus|equity|options|in\s+(?:funding|revenue|sales|arr)|funding|revenue|valuation|[mb]\b|million|billion)")
        .expect("after pattern")
});

const WINDOW: usize = 30;

fn symbol_currency(symbol: &str) -> &'static str {
    match symbol {
        // ponytail: a bare `$` is read as USD and `¥` as JPY, though other dollars and the yuan
        // share them. A written CAD, AUD, NZD, SGD, or HKD wins over the sign.
        "$" => "USD",
        "€" => "EUR",
        "£" => "GBP",
        "¥" => "JPY",
        "₹" => "INR",
        _ => "",
    }
}

/// `70.000` and `120,000` are thousands; `1.5` and `45,50` are fractions. The last mark is a
/// thousand mark when exactly three digits follow it.
fn number(text: &str) -> Option<f64> {
    let marks: Vec<usize> = text.match_indices(['.', ',']).map(|(at, _)| at).collect();
    let Some(&last) = marks.last() else {
        return text.parse().ok();
    };
    let digits = |s: &str| -> String { s.chars().filter(char::is_ascii_digit).collect() };
    let tail = &text[last + 1..];
    let cleaned = if tail.len() == 3 {
        digits(text)
    } else {
        format!("{}.{}", digits(&text[..last]), tail)
    };
    cleaned.parse().ok()
}

fn period_near(text: &str, from: usize, to: usize) -> Option<PayPeriod> {
    let end = clip(text, to + WINDOW);
    let start = clip_back(text, from.saturating_sub(WINDOW));
    // After the figure first, then before it (`Hourly: $50`).
    for window in [&text[to..end], &text[start..from]] {
        if let Some(found) = PERIOD.captures(window) {
            return [
                ("hour", PayPeriod::Hour),
                ("year", PayPeriod::Year),
                ("month", PayPeriod::Month),
                ("day", PayPeriod::Day),
                ("week", PayPeriod::Week),
            ]
            .into_iter()
            .find(|(name, _)| found.name(name).is_some())
            .map(|(_, period)| period);
        }
    }
    None
}

fn clip(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

fn clip_back(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The first figure in the text that reads as pay. A figure with no currency mark, a percent,
/// a bonus, an equity grant, or a funding round is not pay.
// ponytail: English text, one figure or one range. A second range in the same text is never
// read, and a period is looked for within 30 characters of the figure.
pub fn parse_pay(text: &str) -> Option<PayRange> {
    for found in AMOUNT.captures_iter(text) {
        let whole = found.get(0)?;
        let currency = ["code1", "code2", "code3"]
            .iter()
            .find_map(|name| found.name(name))
            .map(|m| m.as_str())
            .or_else(|| {
                ["sym1", "sym2"]
                    .iter()
                    .find_map(|name| found.name(name))
                    .map(|m| symbol_currency(m.as_str()))
            });
        let Some(currency) = currency else { continue };
        let before = &text[clip_back(text, whole.start().saturating_sub(40))..whole.start()];
        let after = &text[whole.end()..clip(text, whole.end() + 40)];
        if BEFORE_NOT_PAY.is_match(before) || AFTER_NOT_PAY.is_match(after) {
            continue;
        }
        if text[whole.end()..].starts_with('%') {
            continue;
        }
        let Some(first) = found.name("a").and_then(|m| number(m.as_str())) else {
            continue;
        };
        let second = found.name("b").and_then(|m| number(m.as_str()));
        let (ak, bk) = (found.name("ak").is_some(), found.name("bk").is_some());
        // `70-80k` is 70,000 to 80,000: a `k` on one figure also reaches a small other one.
        let (mut a, mut b) = (first, second);
        match second {
            Some(other) => {
                if ak || (bk && first < 1000.0) {
                    a = first * 1000.0;
                }
                if bk || (ak && other < 1000.0) {
                    b = Some(other * 1000.0);
                }
            }
            None if ak => a = first * 1000.0,
            None => {}
        }
        let (min, max) = match b {
            Some(b) if b < a => continue,
            Some(b) => (a, b),
            None => (a, a),
        };
        if min < 1.0 || max > 1.0e9 {
            continue;
        }
        let period = match period_near(text, whole.start(), whole.end()) {
            Some(period) => period,
            None if max >= 1000.0 => PayPeriod::Year,
            None => continue,
        };
        return Some(PayRange {
            min: Some(min.round() as i64),
            max: Some(max.round() as i64),
            currency: Some(currency.to_string()),
            period: Some(period),
        });
    }
    None
}

/// The pinned factors: 2,080 hours, 260 days, 52 weeks, and 12 months make a year.
pub fn yearly(amount: i64, period: PayPeriod) -> i64 {
    let factor = match period {
        PayPeriod::Hour => 2080,
        PayPeriod::Day => 260,
        PayPeriod::Week => 52,
        PayPeriod::Month => 12,
        PayPeriod::Year => 1,
    };
    amount.saturating_mul(factor)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PayCompare {
    Meets,
    /// The posting's top as a share of the wish, both per year.
    Below {
        ratio: f32,
    },
    /// Why the two cannot be compared: `currency`, `period`, or `amount`.
    NotCompared(&'static str),
}

/// The posting's top figure against the wish, per year. A currency is never converted, and a
/// missing currency or period is not compared.
pub fn compare(posting: &PayRange, wish: &PayWish) -> PayCompare {
    match posting.currency.as_deref() {
        Some(currency) if currency.eq_ignore_ascii_case(&wish.currency) => {}
        _ => return PayCompare::NotCompared("currency"),
    }
    let Some(period) = posting.period else {
        return PayCompare::NotCompared("period");
    };
    let Some(top) = posting.max.or(posting.min) else {
        return PayCompare::NotCompared("amount");
    };
    let (offered, wanted) = (yearly(top, period), yearly(wish.amount, wish.period));
    if offered >= wanted {
        PayCompare::Meets
    } else {
        PayCompare::Below {
            ratio: offered as f32 / wanted as f32,
        }
    }
}
