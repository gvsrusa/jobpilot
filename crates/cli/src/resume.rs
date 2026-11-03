//! Resume and cover letter drafts: import, generate, compare, edit, and export.

use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use jobpilot_core::agents;
use jobpilot_core::ai::router::Router;
use jobpilot_core::drafts::{self, ClaimInput, Draft, ExportFormat};
use jobpilot_core::generate::Generated;
use jobpilot_core::jobs;
use jobpilot_core::render::Template;
use jobpilot_core::runner::ExportChoice;
use jobpilot_core::{Db, Result, parse};
use serde_json::json;

use crate::ai::with_interrupts;
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
    Generate(GenerateArgs),
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
    /// Draft a cover letter for a stored job posting.
    Generate(GenerateArgs),
    Export(ExportArgs),
}

#[derive(clap::Args)]
pub struct GenerateArgs {
    job_id: String,
    /// Also render the draft into the exports folder, when no line is blocked.
    #[arg(long, value_enum)]
    export: Option<FormatArg>,
    #[arg(long, value_enum, default_value = "classic")]
    template: TemplateArg,
}

impl GenerateArgs {
    fn choice(&self) -> Option<ExportChoice> {
        self.export.map(|format| ExportChoice {
            format: format_of(format),
            template: template_of(self.template),
        })
    }
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
    pub out: Option<PathBuf>,
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
        ResumeCommand::Generate(args) => {
            let router = Router::from_settings(db).await?;
            let job = jobs::text_id(db, &args.job_id).await?;
            let choice = args.choice();
            let made = with_interrupts(db, |cancel| {
                agents::draft_resume(db, &router, ACTOR, &job, choice, cancel)
            })
            .await?;
            generated(made)
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
        LetterCommand::Generate(args) => {
            let router = Router::from_settings(db).await?;
            let job = jobs::text_id(db, &args.job_id).await?;
            let choice = args.choice();
            let made = with_interrupts(db, |cancel| {
                agents::draft_cover_letter(db, &router, ACTOR, &job, choice, cancel)
            })
            .await?;
            generated(made)
        }
        LetterCommand::Export(args) => export(db, args).await,
    }
}

fn template_of(arg: TemplateArg) -> Template {
    match arg {
        TemplateArg::Classic => Template::Classic,
        TemplateArg::Compact => Template::Compact,
    }
}

fn format_of(arg: FormatArg) -> ExportFormat {
    match arg {
        FormatArg::Docx => ExportFormat::Docx,
        FormatArg::Pdf => ExportFormat::Pdf,
    }
}

fn generated(made: Generated) -> Result<Output> {
    let mut text = format!(
        "draft {} (v{}) from {}; {} line(s) need evidence before export",
        made.draft.id, made.draft.version, made.provider, made.blocked
    );
    if let Some(path) = &made.exported {
        text.push_str(&format!("\nexported to {path}"));
    }
    if made.export_skipped.is_some() {
        text.push_str(
            "\nnot exported: a line, the name, a contact, or a heading is one an export refuses; \
             see `jobpilot resume compare`",
        );
    }
    out(made, text)
}

async fn export(db: &Db, args: ExportArgs) -> Result<Output> {
    let template = template_of(args.template);
    let format = format_of(args.format);
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
