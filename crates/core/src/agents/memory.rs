//! Refreshing career memory: classes from the model, bodies sealed, questions embedded. Only
//! a person starts it. No timer, no startup, and no hook on a write calls it (FR-8.8).

use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::ai::router::Router;
use crate::ai::{BoxFuture, TaskType, spec};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::memory::classify::{Classified, classify_pending};
use crate::memory::{index_missing, seal_pending};
use crate::runner::{Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

use Permission::*;

pub static STEPS: &[StepDef] = &[
    StepDef {
        name: "classify",
        needs: &[
            ReadCareerMemory,
            ReadJobText,
            Model(TaskType::ClassifyAnswer),
            WriteCareerMemory,
        ],
        attempts: 1,
    },
    // After classify, so a raise seals in the same run.
    StepDef {
        name: "seal",
        needs: &[ReadCareerMemory, ReadSensitive, WriteCareerMemory],
        attempts: 1,
    },
    StepDef {
        name: "embed",
        needs: &[ReadCareerMemory, EmbedLocal, WriteCareerMemory],
        attempts: 1,
    },
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryRefreshed {
    pub sealed: usize,
    pub classified: Classified,
    pub embedded: usize,
}

/// Public so a test can wrap it. `resumed` is set when `resume_run` rebuilt it: classify then
/// makes no model call, so only a fresh press spends one. These steps take no `Ctx`, so each
/// stops between items through the token; `classified_at` and the missing vectors say what
/// is left, which makes the batch the checkpoint.
pub struct MemoryWorkflow<'r> {
    pub router: &'r Router,
    pub resumed: bool,
    done: Mutex<MemoryRefreshed>,
}

impl<'r> MemoryWorkflow<'r> {
    pub fn new(router: &'r Router, resumed: bool) -> Self {
        MemoryWorkflow {
            router,
            resumed,
            done: Mutex::new(MemoryRefreshed::default()),
        }
    }

    pub fn done(&self) -> MemoryRefreshed {
        self.done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn note(&self, change: impl FnOnce(&mut MemoryRefreshed)) {
        change(&mut self.done.lock().unwrap_or_else(PoisonError::into_inner));
    }

    async fn classify(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        if self.resumed {
            self.note(|done| done.classified.skipped = true);
            return Ok(StepOutput::default());
        }
        let classified =
            classify_pending(ctx.db, self.router, ctx.actor, ctx.cancel.clone()).await?;
        let categories = if classified.checked > 0 {
            spec(TaskType::ClassifyAnswer).categories
        } else {
            &[]
        };
        self.note(|done| done.classified = classified);
        Ok(StepOutput {
            provider: None,
            categories,
        })
    }

    /// A key that cannot be had leaves the bodies hidden, as every read already shows them,
    /// and the run goes on.
    async fn seal(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let sealed = match seal_pending(ctx.db).await {
            Ok(sealed) => sealed,
            Err(
                Error::Locked
                | Error::KeyUnavailable(_)
                | Error::KeyMissing { .. }
                | Error::KeyMismatch { .. }
                | Error::Secret(_),
            ) => 0,
            Err(error) => return Err(error),
        };
        self.note(|done| done.sealed = sealed);
        Ok(StepOutput::default())
    }

    async fn embed(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let embedded = match index_missing(ctx.db, self.router, ctx.cancel.clone()).await {
            Ok(embedded) => embedded,
            Err(Error::ManualRequired(why)) if why == "no_embedding_model" => 0,
            Err(error) => return Err(error),
        };
        self.note(|done| done.embedded = embedded);
        Ok(StepOutput::default())
    }
}

impl Workflow for MemoryWorkflow<'_> {
    fn agent(&self) -> Agent {
        Agent::CareerMemory
    }

    fn input(&self) -> RunInput {
        RunInput::MemoryRefresh
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            let def = STEPS
                .get(index)
                .ok_or_else(|| Error::Failed("no_such_step".into()))?;
            for &permission in def.needs {
                ctx.check(permission)?;
            }
            ctx.stop_point()?;
            match index {
                0 => self.classify(ctx).await,
                1 => self.seal(ctx).await,
                _ => self.embed(ctx).await,
            }
        })
    }
}

/// One press of Refresh. The caller shows `refresh_plan` first.
pub async fn memory_refresh(
    db: &Db,
    router: &Router,
    actor: &str,
    cancel: crate::ai::CancellationToken,
) -> Result<MemoryRefreshed> {
    let workflow = MemoryWorkflow::new(router, false);
    crate::runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    Ok(workflow.done())
}
