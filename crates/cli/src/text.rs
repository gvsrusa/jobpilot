//! Terminal-safe text. Job postings, parsed resume lines, and stored titles are untrusted
//! (M-19), and a terminal obeys escape sequences, so everything printed passes through here.

use serde_json::Value;

/// Removes escape sequences (CSI, OSC, DCS/APC/PM/SOS, and two-character escapes), C1
/// controls, and C0 controls other than `\n` and `\t`.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => skip_csi(&mut chars),
                Some(']' | 'P' | '_' | '^' | 'X') => skip_string(&mut chars),
                // Intermediate bytes (0x20-0x2f) then one final byte.
                Some(' '..='/') => {
                    while chars.next_if(|n| matches!(n, ' '..='/')).is_some() {}
                    chars.next();
                }
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => skip_string(&mut chars),
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Parameters and intermediates, then a final byte 0x40-0x7e. A control character in the
/// middle ends the sequence unread, as terminals do.
fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(c) = chars.peek().copied() {
        if matches!(c, '\u{40}'..='\u{7e}') {
            chars.next();
            return;
        }
        if !matches!(c, '\u{20}'..='\u{3f}') {
            return;
        }
        chars.next();
    }
}

/// Up to BEL or ST (`ESC \`, or U+009C). An unterminated string runs to the end.
fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(c) = chars.next() {
        match c {
            '\x07' | '\u{9c}' => return,
            '\x1b' if chars.peek() == Some(&'\\') => {
                chars.next();
                return;
            }
            _ => {}
        }
    }
}

/// Every string in a JSON value, keys included.
pub fn sanitize_value(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(sanitize(&s)),
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize_value).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (sanitize(&k), sanitize_value(v)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn strips_csi_osc_and_controls() {
        assert_eq!(sanitize("a\x1b[31mred\x1b[0mb"), "aredb");
        assert_eq!(sanitize("\x1b]0;evil\x07title"), "title");
        assert_eq!(sanitize("\x1b]8;;http://x\x1b\\link"), "link");
        assert_eq!(sanitize("a\x1bcb\x07\x08\r\x7fc"), "abc");
        assert_eq!(sanitize("\u{9b}31mx\u{85}y"), "xy");
        assert_eq!(sanitize("line\n\tnext"), "line\n\tnext");
        assert_eq!(sanitize("caf\u{e9} \u{4e16}"), "caf\u{e9} \u{4e16}");
    }

    #[test]
    fn unterminated_sequences_do_not_panic() {
        assert_eq!(sanitize("a\x1b"), "a");
        assert_eq!(sanitize("a\x1b]0;never ends"), "a");
        assert_eq!(sanitize("a\x1b[31"), "a");
    }
}
