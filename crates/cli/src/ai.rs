//! AI providers: status, models, settings, consent, and indexing.

use std::future::Future;

use clap::{Subcommand, ValueEnum};
use jobpilot_core::ai::claude_code::TERMS_DATE;
use jobpilot_core::ai::ollama::{Ollama, OllamaStatus};
use jobpilot_core::ai::router::{Router, ollama_from_settings};
use jobpilot_core::ai::{self, CancellationToken, DataCategory, ProviderId, consent};
use jobpilot_core::{Db, Error, Result, embed};
use serde_json::json;

use crate::{ACTOR, Output, out};

#[derive(Clone, Copy, ValueEnum)]
pub enum ProviderArg {
    Ollama,
    #[value(name = "claude_code")]
    ClaudeCode,
}

impl ProviderArg {
    fn name(self) -> &'static str {
        match self {
            ProviderArg::Ollama => "ollama",
            ProviderArg::ClaudeCode => "claude_code",
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
    /// Embed knowledge items that have no vector yet.
    Index,
}

#[derive(Subcommand)]
pub enum ConsentCommand {
    /// Allow these categories to be sent to Claude Code.
    Give {
        provider: ProviderArg,
        #[arg(long, required = true, value_delimiter = ',', value_parser = category)]
        categories: Vec<DataCategory>,
    },
    Revoke {
        provider: ProviderArg,
    },
}

fn category(text: &str) -> std::result::Result<DataCategory, String> {
    serde_json::from_value(json!(text)).map_err(|_| {
        format!("unknown category '{text}'; use job_text, company_text, profile_basic, resume_text, career_memory, or sensitive")
    })
}

/// Runs AI work so that Ctrl-C cancels its token and then waits for the task to finish,
/// which drops the `claude` child. Returning early would orphan a paid child.
pub async fn with_ctrl_c<T, F, Fut>(run: F) -> Result<T>
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
    tokio::select! {
        done = &mut task => done,
        () = interrupt => {
            token.cancel();
            // A second Ctrl-C gives up waiting. The child is not dropped then, so this
            // is the user's explicit choice.
            let again = async {
                if tokio::signal::ctrl_c().await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                done = &mut task => done,
                () = again => std::process::exit(130),
            }
        }
    }
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
            let text = format!(
                "ollama: {state} ({})\nclaude code: {} ({}){}\norder: {}\ndrafting model: {}\nembedding model: {}\nmemory: {} MB, {} cpus, {}",
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
                match &status.claude_code.consent {
                    Some(categories) => format!(", consent for {} categories", categories.len()),
                    None => ", no consent".into(),
                },
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
            with_ctrl_c(|cancel| ollama.pull(db, ACTOR, &model, progress, cancel)).await?;
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
            if !path.is_empty() && !std::path::Path::new(path).is_absolute() {
                return Err(Error::Validation("give the full path to claude".into()));
            }
            let key = crate::shell::CLAUDE_PATH_SETTING;
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
        }) => {
            external_only(provider)?;
            let id = ProviderId::ClaudeCode.to_string();
            consent::give(db, &id, &categories, TERMS_DATE, ACTOR).await?;
            out(
                json!({ "consent": categories }),
                format!("consent given to {id} for {} categories", categories.len()),
            )
        }
        AiCommand::Consent(ConsentCommand::Revoke { provider }) => {
            external_only(provider)?;
            consent::revoke(db, &ProviderId::ClaudeCode.to_string(), ACTOR).await?;
            out(json!({ "revoked": true }), "consent revoked".into())
        }
        AiCommand::Index => {
            let router = Router::from_settings(db).await?;
            let indexed = with_ctrl_c(|cancel| embed::index_missing(db, &router, cancel)).await?;
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

/// Ollama runs on this machine and needs no record; only Claude Code does.
fn external_only(provider: ProviderArg) -> Result<()> {
    match provider {
        ProviderArg::ClaudeCode => Ok(()),
        ProviderArg::Ollama => Err(Error::Validation(
            "consent applies to claude_code only".into(),
        )),
    }
}
