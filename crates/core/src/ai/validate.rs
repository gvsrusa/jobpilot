use crate::error::{Error, Result};

pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// All model output is untrusted. Fixed messages on purpose: the library's and the JSON
/// parser's text can quote the output, and an error can reach a log or a screen.
pub fn validate(schema: &str, raw: &str) -> Result<serde_json::Value> {
    if raw.len() > MAX_OUTPUT_BYTES {
        return Err(Error::TooLarge {
            limit: MAX_OUTPUT_BYTES as u64,
        });
    }
    let mut output: serde_json::Value = serde_json::from_str(unfence(raw))
        .map_err(|_| Error::Validation("output is not JSON".into()))?;
    lower_headings(&mut output);
    let schema: serde_json::Value = serde_json::from_str(schema)
        .map_err(|_| Error::Validation("task schema is not JSON".into()))?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|_| Error::Validation("task schema does not compile".into()))?;
    if !validator.is_valid(&output) {
        return Err(Error::Validation("output does not match the schema".into()));
    }
    Ok(output)
}

/// A provider that only reads the schema as text can write "Experience". The case says nothing,
/// and the schema's list still decides. ASCII only: the stock headings are, so no look-alike
/// letter folds into one. Only a resume reply has `sections`.
fn lower_headings(output: &mut serde_json::Value) {
    let Some(sections) = output.get_mut("sections").and_then(|s| s.as_array_mut()) else {
        return;
    };
    for heading in sections.iter_mut().filter_map(|s| s.get_mut("heading")) {
        if let Some(text) = heading.as_str() {
            *heading = text.to_ascii_lowercase().into();
        }
    }
}

/// Models that ignore structured output wrap JSON in one ```json fence. One, and only that.
fn unfence(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("```json")
        .and_then(|rest| rest.strip_suffix("```"))
        .unwrap_or(trimmed)
}
