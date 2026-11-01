//! Resume and cover letter drafts: import, generate, compare, edit, and export.

use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use jobpilot_core::ai::router::Router;
use jobpilot_core::drafts::{self, ClaimInput, Draft, ExportFormat};
use jobpilot_core::render::Template;
use jobpilot_core::{Db, Result, generate, parse};
use serde_json::json;

use crate::ai::with_ctrl_c;
use crate::{ACTOR, Output, out};

#[derive(Clone, Copy, ValueEnum)]
pub enum FormatArg {
    Docx,
    Pdf,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum TemplateArg {
    Classic,
    Compact,
}

#[derive(Subcommand)]
pub enum ResumeCommand {
    /// Read a PDF or DOCX resume as version 1.
    Import {
        path: PathBuf,
        #[arg(long)]
        title: Option<String>,
    },
    /// Resumes, with versions grouped by lineage.
    List,
    Versions {
        lineage: String,
    },
    /// Draft a resume for a stored job posting.
    Generate {
        job_id: String,
    },
    /// A draft beside the original text it came from.
    Compare {
        draft: String,
        #[arg(long)]
        original: Option<String>,
    },
    Export(ExportArgs),
    /// Replace a line in a new version; it needs evidence.
    EditClaim {
        claim: String,
        #[arg(long)]
        text: String,
        #[arg(long, value_delimiter = ',')]
        evidence: Vec<String>,
    },
    RemoveClaim {
        claim: String,
    },
}

#[derive(Subcommand)]
pub enum LetterCommand {
    Generate { job_id: String },
    Export(ExportArgs),
}

#[derive(clap::Args)]
pub struct ExportArgs {
    draft: String,
    #[arg(long, value_enum)]
    format: FormatArg,
    #[arg(long, value_enum, default_value = "classic")]
    template: TemplateArg,
    /// Defaults to the exports folder in the data folder.
    #[arg(long)]
    out: Option<PathBuf>,
}

fn draft_line(d: &Draft) -> String {
    format!("{}  v{}  {}  {}", d.id, d.version, d.origin, d.created_at)
}

pub async fn resume(db: &Db, command: ResumeCommand) -> Result<Output> {
    match command {
        ResumeCommand::Import { path, title } => {
            let title = title.unwrap_or_else(|| stem(&path));
            let imported = parse::import_resume(db, &path, &title).await?;
            let mut text = format!(
                "imported as draft {} (lineage {}), {} knowledge item(s) waiting for approval",
                imported.draft_id, imported.lineage_id, imported.items
            );
            for warning in &imported.warnings {
                text.push_str(&format!("\nwarning: {warning}"));
            }
            out(imported, text)
        }
        ResumeCommand::List => {
            let drafts = drafts::list(db, Some("resume")).await?;
            let mut lineages: Vec<(String, Vec<&Draft>)> = Vec::new();
            for draft in &drafts {
                match lineages.iter_mut().find(|(id, _)| *id == draft.lineage_id) {
                    Some((_, versions)) => versions.push(draft),
                    None => lineages.push((draft.lineage_id.clone(), vec![draft])),
                }
            }
            let mut value = Vec::new();
            let mut text = Vec::new();
            for (lineage, mut versions) in lineages {
                versions.sort_by_key(|d| d.version);
                let title = versions.last().map(|d| d.title.clone()).unwrap_or_default();
                text.push(format!("{lineage}  {title}"));
                text.extend(versions.iter().map(|d| format!("  {}", draft_line(d))));
                let rows: Vec<_> = versions
                    .iter()
                    .map(|d| json!({ "id": d.id, "version": d.version, "origin": d.origin, "created_at": d.created_at }))
                    .collect();
                value.push(json!({ "lineage_id": lineage, "title": title, "versions": rows }));
            }
            out(value, text.join("\n"))
        }
        ResumeCommand::Versions { lineage } => {
            let versions = drafts::versions(db, &lineage).await?;
            let text = versions
                .iter()
                .map(draft_line)
                .collect::<Vec<_>>()
                .join("\n");
            out(versions, text)
        }
        ResumeCommand::Generate { job_id } => {
            let router = Router::from_settings(db).await?;
            let made =
                with_ctrl_c(|cancel| generate::resume(db, &router, ACTOR, &job_id, cancel)).await?;
            let text = format!(
                "draft {} (v{}) from {}; {} line(s) need evidence before export",
                made.draft.id, made.draft.version, made.provider, made.blocked
            );
            out(made, text)
        }
        ResumeCommand::Compare { draft, original } => {
            let compared = drafts::compare(db, &draft, original.as_deref()).await?;
            let mut text = String::from("original:");
            for line in &compared.original {
                text.push_str(&format!("\n  {line}"));
            }
            text.push_str("\ngenerated:");
            for claim in &compared.generated {
                text.push_str(&format!(
                    "\n  [{}] {}  {}  (evidence: {})",
                    claim.status,
                    claim.id,
                    claim.text,
                    claim.evidence_ids.join(", ")
                ));
            }
            text.push_str(&format!("\n{} line(s) blocked", compared.blocked));
            out(compared, text)
        }
        ResumeCommand::Export(args) => export(db, args).await,
        ResumeCommand::EditClaim {
            claim,
            text,
            evidence,
        } => {
            let input = ClaimInput {
                text,
                evidence_ids: evidence,
            };
            let draft = drafts::edit_claim(db, &claim, input).await?;
            let text = format!("new version {}", draft_line(&draft));
            out(draft, text)
        }
        ResumeCommand::RemoveClaim { claim } => {
            let draft = drafts::remove_claim(db, &claim).await?;
            let text = format!("new version {}", draft_line(&draft));
            out(draft, text)
        }
    }
}

pub async fn letter(db: &Db, command: LetterCommand) -> Result<Output> {
    match command {
        LetterCommand::Generate { job_id } => {
            let router = Router::from_settings(db).await?;
            let made =
                with_ctrl_c(|cancel| generate::cover_letter(db, &router, ACTOR, &job_id, cancel))
                    .await?;
            let text = format!(
                "draft {} (v{}) from {}; {} line(s) need evidence before export",
                made.draft.id, made.draft.version, made.provider, made.blocked
            );
            out(made, text)
        }
        LetterCommand::Export(args) => export(db, args).await,
    }
}

async fn export(db: &Db, args: ExportArgs) -> Result<Output> {
    let template = match args.template {
        TemplateArg::Classic => Template::Classic,
        TemplateArg::Compact => Template::Compact,
    };
    let format = match args.format {
        FormatArg::Docx => ExportFormat::Docx,
        FormatArg::Pdf => ExportFormat::Pdf,
    };
    let dir = args.out.unwrap_or_else(|| db.paths().exports());
    let path = drafts::export(db, &args.draft, template, format, &dir, ACTOR).await?;
    out(
        json!({ "path": path }),
        format!("exported to {}", path.display()),
    )
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(|| "Resume".into(), |s| s.to_string_lossy().into_owned())
}
