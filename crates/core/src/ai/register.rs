//! The provider table: one row per provider a consent record can name, tied to the policy
//! register's text. `consent` keeps the row text a record was given under, so a changed row
//! suspends that record. A provider the owner decisions leave out has no row, so no way in.

use serde::Serialize;

use super::{Capability, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Api,
    Cli,
    Endpoint,
}

#[derive(Debug)]
pub struct ProviderInfo {
    /// The name in settings, the CLI, and IPC.
    pub name: &'static str,
    pub id: ProviderId,
    pub label: &'static str,
    pub kind: ProviderKind,
    /// Where the data goes, in words. Shown on the consent screen.
    pub destination: &'static str,
    pub capabilities: &'static [Capability],
    /// The Integration and Operation cells of the vendor table row.
    pub vendor_row: (&'static str, &'static str),
    /// What the first cell of the owner table row starts with.
    pub owner_row: Option<&'static str>,
    pub data_use: &'static str,
    pub attestation: Option<&'static str>,
}

const NO_TOOLS: &[Capability] = &[Capability::StructuredOutput, Capability::NoTools];
const DIRECT_API: Option<&str> = Some("Direct API: OpenAI");
const CLI_ADAPTERS: Option<&str> = Some("Gemini CLI and Codex CLI adapters");
const OWN_KEY: &str = "Direct API calls with the user's own key";
const CLI_TERMS: &str = "The terms of whatever sign-in the installed CLI uses.";

pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        name: "claude_code",
        id: ProviderId::ClaudeCode,
        label: "Claude Code",
        kind: ProviderKind::Cli,
        destination: "Anthropic, through the claude program on this computer",
        capabilities: NO_TOOLS,
        vendor_row: (
            "Claude Code",
            "JobPilot runs `claude -p` or the Agent SDK in the background under the user's \
             consumer login",
        ),
        owner_row: Some("Claude Code: `claude --bare -p`"),
        data_use: "Consumer Terms allow training unless the user opts out.",
        attestation: None,
    },
    ProviderInfo {
        name: "openai",
        id: ProviderId::OpenAi,
        label: "OpenAI",
        kind: ProviderKind::Api,
        destination: "OpenAI's API, at api.openai.com",
        capabilities: NO_TOOLS,
        vendor_row: ("OpenAI API", OWN_KEY),
        owner_row: DIRECT_API,
        data_use: "Not used to improve services unless the customer opts in (Services Agreement \
                   4.2).",
        attestation: None,
    },
    ProviderInfo {
        name: "anthropic",
        id: ProviderId::Anthropic,
        label: "Anthropic",
        kind: ProviderKind::Api,
        destination: "Anthropic's API, at api.anthropic.com",
        capabilities: NO_TOOLS,
        vendor_row: ("Anthropic API", OWN_KEY),
        owner_row: DIRECT_API,
        data_use: "\"Anthropic may not train models on Customer Content from Services.\"",
        attestation: None,
    },
    ProviderInfo {
        name: "google",
        id: ProviderId::Google,
        label: "Google Gemini",
        kind: ProviderKind::Api,
        destination: "Google's Gemini API, at generativelanguage.googleapis.com",
        capabilities: NO_TOOLS,
        vendor_row: (
            "Google Gemini API",
            "Direct API calls with the user's own paid key",
        ),
        owner_row: DIRECT_API,
        data_use: "Not used to improve products; logged for a limited period for abuse detection.",
        attestation: Some(
            "This key belongs to a Google project with billing on. A free Gemini key is refused \
             for personal data.",
        ),
    },
    ProviderInfo {
        name: "openrouter",
        id: ProviderId::OpenRouter,
        label: "OpenRouter",
        kind: ProviderKind::Api,
        destination: "OpenRouter, at openrouter.ai, which passes it to the model's vendor",
        capabilities: NO_TOOLS,
        vendor_row: ("OpenRouter", "API calls with the user's own key"),
        owner_row: DIRECT_API,
        data_use: "The chosen model's terms apply. Prompt logging is opt-in and stays off.",
        attestation: None,
    },
    ProviderInfo {
        name: "endpoint",
        id: ProviderId::Endpoint,
        label: "Compatible endpoint",
        kind: ProviderKind::Endpoint,
        destination: "The server you named at a loopback address on this computer",
        capabilities: NO_TOOLS,
        vendor_row: (
            "Compatible endpoints",
            "Calls to a server the user runs on loopback",
        ),
        owner_row: None,
        data_use: "This goes to the server you named at this address. What that server does \
                   with it is up to that server.",
        attestation: None,
    },
    ProviderInfo {
        name: "gemini_cli",
        id: ProviderId::GeminiCli,
        label: "Gemini CLI",
        kind: ProviderKind::Cli,
        destination: "Google, through the gemini program on this computer",
        capabilities: NO_TOOLS,
        vendor_row: (
            "Gemini CLI",
            "JobPilot runs `gemini` with the user's paid Gemini API key",
        ),
        owner_row: CLI_ADAPTERS,
        data_use: CLI_TERMS,
        attestation: Some(
            "Gemini CLI here is not signed in with a free Gemini API key. A free key is refused \
             for personal data.",
        ),
    },
    ProviderInfo {
        name: "codex_cli",
        id: ProviderId::CodexCli,
        label: "Codex CLI",
        kind: ProviderKind::Cli,
        destination: "OpenAI, through the codex program on this computer",
        capabilities: &[Capability::StructuredOutput],
        vendor_row: (
            "Codex CLI",
            "JobPilot runs `codex exec` in the background on the user's saved ChatGPT login",
        ),
        owner_row: CLI_ADAPTERS,
        data_use: CLI_TERMS,
        attestation: None,
    },
];

pub const REGISTER: &str = include_str!("../../../../docs/policy-register.md");

const DATE_READ_CELL: usize = 3;
const DATE_LABEL: &str = "Register date: ";

pub fn info(name: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|info| info.name == name)
}

/// By consent key, such as `openai/api_key`.
pub fn info_for(key: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|info| info.id.to_string() == key)
}

/// A table line's cells. The tables hold no `" | "` inside a cell, which a test keeps true.
fn cells(line: &str) -> Vec<&str> {
    let line = line.trim_end_matches('\r');
    let inner = line.strip_prefix("| ").and_then(|l| l.strip_suffix(" |"));
    inner.map_or_else(Vec::new, |inner| inner.split(" | ").collect())
}

fn vendor_cells<'a>(register: &'a str, info: &ProviderInfo) -> Option<Vec<&'a str>> {
    let (integration, operation) = info.vendor_row;
    register
        .lines()
        .map(cells)
        .find(|cells| cells.len() > 1 && cells[0] == integration && cells[1] == operation)
}

/// Vendor row cells except Date read, then the owner row line, `\n`-joined, `\r` stripped.
pub fn row_text(key: &str) -> Option<String> {
    rows_of(REGISTER, key)
}

/// [`row_text`] on any text, so a test can change the register.
#[doc(hidden)]
pub fn rows_of(register: &str, key: &str) -> Option<String> {
    let info = info_for(key)?;
    let mut lines: Vec<&str> = vendor_cells(register, info)?
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != DATE_READ_CELL)
        .map(|(_, cell)| cell)
        .collect();
    if let Some(prefix) = info.owner_row {
        let owner = format!("| {prefix}");
        lines.push(
            register
                .lines()
                .find(|line| line.starts_with(&owner))?
                .trim_end_matches('\r'),
        );
    }
    Some(lines.join("\n"))
}

/// When the vendor's terms were read for this row.
pub fn date_read(key: &str) -> Option<String> {
    let cells = vendor_cells(REGISTER, info_for(key)?)?;
    cells.get(DATE_READ_CELL).map(ToString::to_string)
}

/// From the file's third line.
pub fn register_date() -> String {
    let line = REGISTER.lines().nth(2).unwrap_or_default();
    let date = line.split_once(DATE_LABEL).map_or("", |(_, rest)| rest);
    date.split('.').next().unwrap_or_default().to_string()
}
