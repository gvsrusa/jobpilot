//! AI providers: status, models, settings, consent, and indexing.

use std::future::Future;
use std::io::{BufRead, IsTerminal};

use clap::{Subcommand, ValueEnum};
use jobpilot_core::ai::ollama::{Ollama, OllamaStatus};
use jobpilot_core::ai::router::{Router, ollama_from_settings};
use jobpilot_core::ai::{
    self, CancellationToken, DataCategory, Sharing, TaskType, consent, register,
};
use jobpilot_core::ai::{api, claude_code};
use jobpilot_core::secrets::{self, Zeroizing};
use jobpilot_core::{Db, Error, Result, embed, generate, pause};
use serde_json::json;

use crate::{ACTOR, Output, out};

#[derive(Clone, Copy, ValueEnum)]
pub enum ProviderArg {
    Ollama,
    #[value(name = "claude_code")]
    ClaudeCode,
    #[value(name = "openai")]
    OpenAi,
    Anthropic,
    Google,
    #[value(name = "openrouter")]
    OpenRouter,
    Endpoint,
    #[value(name = "gemini_cli")]
    GeminiCli,
    #[value(name = "codex_cli")]
    CodexCli,
}

impl ProviderArg {
    fn name(self) -> &'static str {
        match self {
            ProviderArg::Ollama => "ollama",
            ProviderArg::ClaudeCode => "claude_code",
            ProviderArg::OpenAi => "openai",
            ProviderArg::Anthropic => "anthropic",
            ProviderArg::Google => "google",
            ProviderArg::OpenRouter => "openrouter",
            ProviderArg::Endpoint => "endpoint",
            ProviderArg::GeminiCli => "gemini_cli",
            ProviderArg::CodexCli => "codex_cli",
        }
    }
}

#[derive(Subcommand)]
pub enum AiCommand {
    Status,
    /// Local models Ollama has installed.
    Models,
    /// Download a model. Only you start this.
    Pull {
        model: String,
    },
    SetModel {
        #[arg(long)]
        drafting: Option<String>,
        #[arg(long)]
        embedding: Option<String>,
    },
    /// The order providers are tried in, for example `ollama,claude_code`.
    Order {
        #[arg(value_enum, required = true, value_delimiter = ',')]
        providers: Vec<ProviderArg>,
    },
    Enable {
        provider: ProviderArg,
    },
    Disable {
        provider: ProviderArg,
    },
    /// Where the `claude` program is, when it cannot be found by itself. Empty clears it.
    ClaudePath {
        path: String,
    },
    #[command(subcommand)]
    Consent(ConsentCommand),
    /// Provider API keys, kept in the system keychain.
    #[command(subcommand)]
    Key(KeyCommand),
    /// The model id an API provider or the endpoint uses, as its vendor spells it.
    Model {
        provider: ProviderArg,
        model: String,
    },
    /// The address of a server you run on this computer. Empty clears it and its model.
    Endpoint {
        url: String,
    },
    /// Embed knowledge items that have no vector yet.
    Index,
}

#[derive(Subcommand)]
pub enum KeyCommand {
    /// Store a key, read from a hidden prompt or one line of piped input. Never an argument.
    Set { provider: ProviderArg },
    /// Remove a stored key.
    Remove { provider: ProviderArg },
}

#[derive(Subcommand)]
pub enum ConsentCommand {
    /// Allow these categories to be sent to a provider.
    Give {
        provider: ProviderArg,
        #[arg(long, required = true, value_delimiter = ',', value_parser = category)]
        categories: Vec<DataCategory>,
        /// State the line this provider needs from you, such as that a key is a paid one.
        #[arg(long)]
        attest: bool,
    },
    Revoke {
        provider: ProviderArg,
    },
    /// What a provider would receive, where it goes, and how it is used.
    Show {
        provider: ProviderArg,
        /// Also print the exact resume prompt for this pasted posting. Sends nothing.
        #[arg(long)]
        job: Option<String>,
    },
}

fn category(text: &str) -> std::result::Result<DataCategory, String> {
    serde_json::from_value(json!(text)).map_err(|_| {
        format!("unknown category '{text}'; use job_text, company_text, profile_basic, resume_text, career_memory, or sensitive")
    })
}

/// Runs automated work so that Ctrl-C, or an emergency pause set from any process, cancels
/// its token and then waits for the task to finish, which drops the `claude` child.
/// Returning early would orphan a paid child. A task a pause cut short reports `paused`.
pub async fn with_interrupts<T, F, Fut>(db: &Db, run: F) -> Result<T>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let token = CancellationToken::new();
    let task = run(token.clone());
    tokio::pin!(task);
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let paused = tokio::select! {
        done = &mut task => return done,
        () = interrupt => false,
        () = pause::watch(db, &token) => true,
    };
    token.cancel();
    // A second Ctrl-C gives up waiting. The child is not dropped then, so this
    // is the user's explicit choice.
    let again = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let done = tokio::select! {
        done = &mut task => done,
        () = again => std::process::exit(130),
    };
    match done {
        Err(Error::Cancelled) if paused => Err(Error::Paused),
        done => done,
    }
}

/// Past any key, so this buffer never grows and leaves a copy of its own behind. Stdin's
/// internal buffer still holds the line until the process ends, and nothing here can clear it.
const KEY_LINE_BYTES: usize = 1024;

/// A hidden prompt at a terminal, one line of stdin otherwise. Argv would reach the process
/// list and the shell history.
fn read_key() -> Result<Zeroizing<String>> {
    if std::io::stdin().is_terminal() {
        let typed = rpassword::prompt_password("Key (not shown): ")?;
        return Ok(Zeroizing::new(typed));
    }
    key_line(std::io::stdin().lock())
}

fn key_line(input: impl BufRead) -> Result<Zeroizing<String>> {
    let mut line = Zeroizing::new(String::with_capacity(KEY_LINE_BYTES));
    input.take(KEY_LINE_BYTES as u64).read_line(&mut line)?;
    Ok(line)
}

async fn store_key(db: &Db, name: &str, key: &str) -> Result<Output> {
    secrets::set_api_key(db, name, key, ACTOR).await?;
    out(
        json!({ "key_stored": name }),
        format!("key stored for {name}"),
    )
}

async fn ollama(db: &Db) -> Result<Ollama> {
    ollama_from_settings(db)
        .await?
        .ok_or_else(|| Error::Refused("not_loopback".into()))
}

pub async fn ai(db: &Db, command: AiCommand) -> Result<Output> {
    match command {
        AiCommand::Status => {
            let status = ai::status(db).await?;
            let state = serde_json::to_value(&status.ollama).map_or_else(
                |_| "unknown".into(),
                |v| v["state"].as_str().unwrap_or("unknown").to_string(),
            );
            let on = |name: &str| status.enabled[name].as_bool().unwrap_or(false);
            let external: String = status
                .external
                .iter()
                .map(|row| {
                    let state = if row.available {
                        "available"
                    } else {
                        row.reason.as_deref().unwrap_or("unavailable")
                    };
                    let word = if on(&row.name) { "enabled" } else { "disabled" };
                    let consent = consent_words(row.consent.as_deref(), row.suspended);
                    format!("{}: {state} ({word}){consent}\n", row.label)
                })
                .collect();
            let text = format!(
                "ollama: {state} ({})\nclaude code: {} ({}){}\n{external}order: {}\ndrafting model: {}\nembedding model: {}\nmemory: {} MB, {} cpus, {}",
                if on("ollama") { "enabled" } else { "disabled" },
                if status.claude_code.available {
                    "available"
                } else {
                    "not found"
                },
                if on("claude_code") {
                    "enabled"
                } else {
                    "disabled"
                },
                consent_words(
                    status.claude_code.consent.as_deref(),
                    status.claude_code.suspended
                ),
                status.order.join(", "),
                status.drafting_model.as_deref().unwrap_or("none"),
                status.embedding_model.as_deref().unwrap_or("none"),
                status.hardware.total_memory / (1024 * 1024),
                status.hardware.cpus,
                status.hardware.gpu,
            );
            out(status, text)
        }
        AiCommand::Models => {
            let status = ollama(db).await?.status().await;
            let text = match &status {
                OllamaStatus::Running { models, .. } => models
                    .iter()
                    .map(|m| format!("{}  {} MB  {:?}", m.name, m.size / (1024 * 1024), m.fits))
                    .collect::<Vec<_>>()
                    .join("\n"),
                OllamaStatus::NotRunning { url } => format!("Ollama is not running at {url}"),
                OllamaStatus::Refused { reason } => format!("Ollama address refused: {reason}"),
            };
            out(status, text)
        }
        AiCommand::Pull { model } => {
            let ollama = ollama(db).await?;
            let mut last = u64::MAX;
            let progress = |done: u64, total: u64| {
                let percent = (done * 100).checked_div(total).unwrap_or(0);
                if percent != last {
                    last = percent;
                    eprintln!("pulling {model}: {percent}%");
                }
            };
            with_interrupts(db, |cancel| {
                ollama.pull(db, ACTOR, &model, progress, cancel)
            })
            .await?;
            out(json!({ "pulled": model }), format!("pulled {model}"))
        }
        AiCommand::SetModel {
            drafting,
            embedding,
        } => {
            if drafting.is_none() && embedding.is_none() {
                return Err(Error::Validation("give --drafting or --embedding".into()));
            }
            if let Some(model) = &drafting {
                ai::set_model(db, "drafting", model).await?;
            }
            if let Some(model) = &embedding {
                ai::set_model(db, "embedding", model).await?;
            }
            out(
                json!({ "drafting": drafting, "embedding": embedding }),
                "models saved".into(),
            )
        }
        AiCommand::Order { providers } => {
            let names: Vec<String> = providers.iter().map(|p| p.name().to_string()).collect();
            ai::set_order(db, &names).await?;
            let text = format!("order: {}", names.join(", "));
            out(json!({ "order": names }), text)
        }
        AiCommand::ClaudePath { path } => {
            let path = path.trim();
            let given = std::path::Path::new(path);
            if !path.is_empty() && !(given.is_absolute() && claude_code::has_claude_name(given)) {
                return Err(Error::Validation(
                    "give the full path to a program named claude".into(),
                ));
            }
            let key = claude_code::PATH_SETTING;
            jobpilot_core::settings::set(db, key, &json!(path)).await?;
            out(
                json!({ "claude_path": path }),
                format!("claude_path: {path}"),
            )
        }
        AiCommand::Enable { provider } => toggle(db, provider, true).await,
        AiCommand::Disable { provider } => toggle(db, provider, false).await,
        AiCommand::Consent(ConsentCommand::Give {
            provider,
            categories,
            attest,
        }) => {
            external_only(provider)?;
            let sharing = ai::sharing(db, provider.name()).await?;
            // On stderr, so `--json` stays one JSON value on stdout.
            eprintln!("{}: {}", uses_heading(&sharing), sharing.data_use);
            if let Some(attestation) = &sharing.attestation {
                eprintln!("{attestation}");
            }
            consent::give_for(db, provider.name(), &categories, attest, ACTOR).await?;
            out(
                json!({ "consent": categories }),
                format!(
                    "consent given to {} for {} categories",
                    sharing.key,
                    categories.len()
                ),
            )
        }
        AiCommand::Consent(ConsentCommand::Revoke { provider }) => {
            let key = external_only(provider)?.id.to_string();
            consent::revoke(db, &key, ACTOR).await?;
            out(json!({ "revoked": true }), "consent revoked".into())
        }
        AiCommand::Consent(ConsentCommand::Show { provider, job }) => {
            external_only(provider)?;
            let sharing = ai::sharing(db, provider.name()).await?;
            let mut text = sharing_text(&sharing);
            let preview = match job {
                Some(job) => {
                    let task = TaskType::DraftResume;
                    Some(generate::preview(db, task, &job).await?)
                }
                None => None,
            };
            let Some(preview) = preview else {
                return out(sharing, text);
            };
            let (system, user) = (&preview.system, &preview.user);
            text.push_str(&format!(
                "\n\nExactly what draft_resume would send:\n\nSYSTEM\n{system}\n\nUSER\n{user}"
            ));
            out(json!({ "sharing": sharing, "preview": preview }), text)
        }
        AiCommand::Key(KeyCommand::Set { provider }) => {
            let name = provider.name();
            secrets::key_provider(name)?;
            let key = read_key()?;
            store_key(db, name, &key).await
        }
        AiCommand::Key(KeyCommand::Remove { provider }) => {
            let name = provider.name();
            secrets::delete_api_key(db, name, ACTOR).await?;
            out(
                json!({ "key_removed": name }),
                format!("key removed for {name}"),
            )
        }
        AiCommand::Model { provider, model } => {
            api::set_model(db, provider.name(), &model).await?;
            let name = provider.name();
            out(
                json!({ "provider": name, "model": model }),
                format!("{name} model: {model}"),
            )
        }
        AiCommand::Endpoint { url } => {
            api::set_endpoint(db, &url).await?;
            let address = api::endpoint_address(db).await?;
            let text = match &address {
                Some(url) => format!("endpoint: {url}"),
                None => "endpoint cleared".into(),
            };
            out(json!({ "endpoint": address }), text)
        }
        AiCommand::Index => {
            let router = Router::from_settings(db).await?;
            let indexed =
                with_interrupts(db, |cancel| embed::index_missing(db, &router, cancel)).await?;
            out(
                json!({ "indexed": indexed }),
                format!("indexed {indexed} item(s)"),
            )
        }
    }
}

async fn toggle(db: &Db, provider: ProviderArg, on: bool) -> Result<Output> {
    ai::set_enabled(db, provider.name(), on).await?;
    let word = if on { "enabled" } else { "disabled" };
    out(
        json!({ provider.name(): on }),
        format!("{} {word}", provider.name()),
    )
}

/// Ollama runs on this machine and needs no record. Every other provider does.
fn external_only(provider: ProviderArg) -> Result<&'static register::ProviderInfo> {
    register::info(provider.name()).ok_or_else(|| {
        Error::Validation("consent does not apply to Ollama: it runs on this computer".into())
    })
}

fn consent_words(consent: Option<&[DataCategory]>, suspended: bool) -> String {
    match consent {
        _ if suspended => ", consent suspended".into(),
        Some(categories) => format!(", consent for {} categories", categories.len()),
        None => ", no consent".into(),
    }
}

fn uses_heading(sharing: &Sharing) -> String {
    format!(
        "How {} uses it (terms read {})",
        sharing.label, sharing.terms_date
    )
}

/// The serde name of a value: `draft_resume`, `job_text`.
fn word<T: serde::Serialize>(value: &T) -> String {
    json!(value).as_str().unwrap_or_default().to_string()
}

fn sharing_text(sharing: &Sharing) -> String {
    let mut lines = vec![format!("{} ({})", sharing.label, sharing.key)];
    if let Some(path) = &sharing.path {
        lines.push(format!("Program: {path}"));
    }
    lines.push("What will be sent:".into());
    for task in &sharing.tasks {
        let sent = match task.skipped_because.as_deref() {
            None => {
                let names: Vec<String> = task.categories.iter().map(word).collect();
                names.join(", ")
            }
            Some("no_tools") => "skipped, this provider cannot prove it has no tools".into(),
            Some(reason) => format!("skipped, {reason}"),
        };
        lines.push(format!("  {}: {sent}", word(&task.task)));
    }
    lines.push(format!("Where it goes: {}", sharing.destination));
    lines.push(format!("{}: {}", uses_heading(sharing), sharing.data_use));
    if let Some(attestation) = &sharing.attestation {
        lines.push(format!("You state: {attestation}"));
    }
    let given = sharing.given.iter().flatten().map(word);
    let given = given.collect::<Vec<_>>().join(", ");
    lines.push(match (&sharing.given, sharing.suspended) {
        (_, true) => format!("Consent: suspended, given for {given}"),
        (Some(_), false) => format!("Consent: given for {given}"),
        (None, false) => "Consent: none".into(),
    });
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobpilot_core::AppPaths;

    #[test]
    fn a_piped_line_is_read_only_up_to_the_cap_into_a_buffer_that_never_grows() {
        let line = key_line(vec![b'x'; KEY_LINE_BYTES * 3].as_slice()).unwrap();
        assert_eq!(line.len(), KEY_LINE_BYTES);
        assert_eq!(line.capacity(), KEY_LINE_BYTES);
    }

    #[test]
    fn a_piped_line_stops_at_its_end() {
        let line = key_line(b"sk-abcdefgh\nnext line\n".as_slice()).unwrap();
        assert_eq!(line.as_str(), "sk-abcdefgh\n");
    }

    /// On keyring's in-memory mock, so it reaches no OS store. This binary touches no other
    /// keychain path. The default store is process-wide, and the first `Entry` installs the
    /// OS one, so that happens first and this replaces it.
    #[tokio::test]
    async fn the_stored_message_and_every_row_leave_the_key_out() {
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        const SENTINEL: &str = "sk-ZXQ-sentinel-not-a-real-key";
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&AppPaths::new(tmp.path().join("data")))
            .await
            .unwrap();

        let output = store_key(&db, "openai", SENTINEL).await.unwrap();
        assert_eq!(crate::render(output, false), "key stored for openai");
        let output = store_key(&db, "openai", SENTINEL).await.unwrap();
        assert_eq!(crate::render(output, true), r#"{"key_stored":"openai"}"#);

        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT action || coalesce(target, '') || coalesce(detail, '') FROM audit_log",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(!rows.is_empty());
        assert!(!rows.iter().any(|row| row.contains("ZXQ")), "{rows:?}");
        db.close().await;
    }
}
