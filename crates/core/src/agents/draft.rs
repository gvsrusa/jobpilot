//! A resume or cover letter for one stored posting, and its file when asked. The model gets
//! no tools; the draft step is the only one that reaches a provider (`docs/agents.md`).

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::ai::router::Router;
use crate::ai::{BoxFuture, CancellationToken, ProviderId, TaskType, spec};
use crate::db::Db;
use crate::drafts;
use crate::error::{Error, Result};
use crate::generate::{self, Generated};
use crate::runner::{
    Agent, Checkpoint, Ctx, ExportChoice, Permission, RunInput, StepDef, StepOutput, Workflow,
};

use Permission::*;

pub static RESUME_STEPS: &[StepDef] = &[
    StepDef {
        name: "draft",
        needs: &[
            ReadResumeText,
            ReadProfileBasic,
            ReadJobText,
            Model(TaskType::DraftResume),
            WriteDrafts,
        ],
        attempts: 1,
    },
    StepDef {
        name: "render",
        needs: &[ReadResumeText, WriteDrafts],
        attempts: 1,
    },
];

pub static LETTER_STEPS: &[StepDef] = &[
    StepDef {
        name: "draft",
        needs: &[
            ReadResumeText,
            ReadProfileBasic,
            ReadJobText,
            ReadCompanyText,
            Model(TaskType::DraftCoverLetter),
            WriteDrafts,
        ],
        attempts: 1,
    },
    StepDef {
        name: "render",
        needs: &[ReadResumeText, WriteDrafts],
        attempts: 1,
    },
];

/// Public so a test can wrap it. `task` is `DraftResume` or `DraftCoverLetter`.
pub struct DraftWorkflow<'r> {
    pub task: TaskType,
    pub job_text_id: String,
    pub export: Option<ExportChoice>,
    pub router: &'r Router,
    generated: Mutex<Option<Generated>>,
}

impl<'r> DraftWorkflow<'r> {
    pub fn new(
        task: TaskType,
        job_text_id: &str,
        export: Option<ExportChoice>,
        router: &'r Router,
    ) -> Self {
        DraftWorkflow {
            task,
            job_text_id: job_text_id.to_owned(),
            export,
            router,
            generated: Mutex::new(None),
        }
    }

    fn held(&self) -> MutexGuard<'_, Option<Generated>> {
        self.generated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// What the run made: kept from this process's step, or read back from the stored draft
    /// when a resume skipped the step that made it.
    pub async fn into_generated(self, db: &Db, saved: &Checkpoint) -> Result<Generated> {
        let kept = self.held().take();
        match kept {
            Some(generated) => Ok(generated),
            None => rebuild(db, saved.get("draft")).await,
        }
    }

    async fn draft(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        // A version this run already made, whose checkpoint did not land: the model is not
        // asked again and no second version is added.
        if let Some(id) = drafts::of_run(ctx.db, ctx.run_id()).await? {
            ctx.save("draft", &id)?;
            return Ok(StepOutput::default());
        }
        let (db, router, actor, job) = (ctx.db, self.router, ctx.actor, &self.job_text_id);
        let (cancel, run) = (ctx.cancel.clone(), Some(ctx.run_id()));
        let made = if self.task == TaskType::DraftCoverLetter {
            generate::cover_letter_for(db, router, actor, job, run, cancel).await?
        } else {
            generate::resume_for(db, router, actor, job, run, cancel).await?
        };
        ctx.save("draft", &made.draft.id)?;
        let output = StepOutput {
            provider: Some(made.provider.clone()),
            categories: spec(self.task).categories,
        };
        *self.held() = Some(made);
        Ok(output)
    }

    /// Only a draft with no blocked line is rendered, and only into the exports folder.
    async fn render(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let Some(choice) = self.export else {
            return Ok(StepOutput::default());
        };
        let db = ctx.db;
        let id = ctx
            .saved("draft")
            .ok_or_else(|| Error::Failed("draft_missing".into()))?;
        let current = self.held().clone();
        let mut generated = match current {
            Some(generated) => generated,
            None => rebuild(db, Some(&id)).await?,
        };
        if generated.blocked > 0 {
            generated.export_skipped = Some(NOT_EXPORTED.into());
        } else {
            let exported = drafts::export(
                db,
                &id,
                choice.template,
                choice.format,
                &db.paths().exports(),
                ctx.actor,
            )
            .await;
            match exported {
                Ok(path) => generated.exported = Some(path.to_string_lossy().into_owned()),
                // The draft states what a claim may not. It is stored, and not rendered.
                Err(Error::Refused(why)) if why == NOT_EXPORTED => {
                    generated.export_skipped = Some(NOT_EXPORTED.into());
                }
                Err(error) => return Err(error),
            }
        }
        *self.held() = Some(generated);
        Ok(StepOutput::default())
    }
}

/// Why a draft that was asked to export did not: a line of it, or its name, contact, or
/// heading, is one an export refuses.
const NOT_EXPORTED: &str = "claims_blocked";

/// `provider` is matched on its stored text, since a checkpoint holds ids only.
async fn rebuild(db: &Db, id: Option<&str>) -> Result<Generated> {
    let id = id.ok_or_else(|| Error::Failed("draft_missing".into()))?;
    let draft = drafts::get(db, id).await?;
    let claims = drafts::claims(db, id).await?;
    let blocked = claims.iter().filter(|claim| claim.status != "ok").count();
    let stored = draft.provider.as_deref();
    let provider = [ProviderId::Ollama, ProviderId::ClaudeCode, ProviderId::NoAi]
        .into_iter()
        .find(|known| Some(known.to_string().as_str()) == stored)
        .ok_or_else(|| Error::Failed("provider_unknown".into()))?;
    Ok(Generated {
        draft,
        blocked,
        provider,
        exported: None,
        export_skipped: None,
    })
}

impl Workflow for DraftWorkflow<'_> {
    fn agent(&self) -> Agent {
        if self.task == TaskType::DraftCoverLetter {
            Agent::CoverLetter
        } else {
            Agent::Resume
        }
    }

    fn input(&self) -> RunInput {
        let (job_text_id, export) = (self.job_text_id.clone(), self.export);
        if self.task == TaskType::DraftCoverLetter {
            RunInput::CoverLetter {
                job_text_id,
                export,
            }
        } else {
            RunInput::Resume {
                job_text_id,
                export,
            }
        }
    }

    fn steps(&self) -> &'static [StepDef] {
        if self.task == TaskType::DraftCoverLetter {
            LETTER_STEPS
        } else {
            RESUME_STEPS
        }
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            let def = self
                .steps()
                .get(index)
                .ok_or_else(|| Error::Failed("no_such_step".into()))?;
            for &permission in def.needs {
                ctx.check(permission)?;
            }
            ctx.stop_point()?;
            if index == 0 {
                self.draft(ctx).await
            } else {
                self.render(ctx).await
            }
        })
    }
}

/// A run for a resume. Fails as [`generate::resume`] does, and with `Paused` while the
/// emergency pause is set.
pub async fn draft_resume(
    db: &Db,
    router: &Router,
    actor: &str,
    job_text_id: &str,
    export: Option<ExportChoice>,
    cancel: CancellationToken,
) -> Result<Generated> {
    run(
        db,
        router,
        actor,
        TaskType::DraftResume,
        job_text_id,
        export,
        cancel,
    )
    .await
}

/// As [`draft_resume`], for a cover letter.
pub async fn draft_cover_letter(
    db: &Db,
    router: &Router,
    actor: &str,
    job_text_id: &str,
    export: Option<ExportChoice>,
    cancel: CancellationToken,
) -> Result<Generated> {
    run(
        db,
        router,
        actor,
        TaskType::DraftCoverLetter,
        job_text_id,
        export,
        cancel,
    )
    .await
}

async fn run(
    db: &Db,
    router: &Router,
    actor: &str,
    task: TaskType,
    job_text_id: &str,
    export: Option<ExportChoice>,
    cancel: CancellationToken,
) -> Result<Generated> {
    // A missing posting gets its plain error and no run.
    crate::knowledge::get_job_text(db, job_text_id).await?;
    let workflow = DraftWorkflow::new(task, job_text_id, export, router);
    let saved = crate::runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    workflow.into_generated(db, &saved).await
}
