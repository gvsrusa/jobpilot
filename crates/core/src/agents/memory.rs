//! Refreshing career memory: classes from the model, bodies sealed, questions embedded. Only
//! a person starts it. No timer, no startup, and no hook on a write calls it (FR-8.8).

use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::ai::router::Router;
use crate::ai::{BoxFuture, DataCategory, ProviderId, TaskType, spec};
use crate::crypto::DataKey;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::memory::classify::{Classified, classify_pending};
use crate::memory::{index_missing, seal_pending, seal_pending_with};
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
    /// The code of the key error that left bodies hidden. They are sealed at the next refresh
    /// with the key available.
    pub seal_blocked: Option<String>,
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
    key: Option<DataKey>,
    done: Mutex<MemoryRefreshed>,
}

impl<'r> MemoryWorkflow<'r> {
    pub fn new(router: &'r Router, resumed: bool) -> Self {
        MemoryWorkflow {
            router,
            resumed,
            key: None,
            done: Mutex::new(MemoryRefreshed::default()),
        }
    }

    /// A key to seal with, in place of the keychain's. For tests, which never touch it.
    pub fn with_key(mut self, key: DataKey) -> Self {
        self.key = Some(key);
        self
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
        let since = now(ctx.db).await?;
        let result = classify_pending(ctx.db, self.router, ctx.actor, ctx.cancel.clone()).await;
        // What left the machine is read from the router's own log, so a call that came back
        // unusable still counts: the text was sent.
        let (provider, categories) = asked_since(ctx.db, &since).await?;
        let classified = result?;
        self.note(|done| done.classified = classified);
        Ok(StepOutput {
            provider,
            categories,
        })
    }

    /// A key that cannot be had leaves the bodies hidden, as every read already shows them,
    /// and the run goes on. The code is kept for the person.
    async fn seal(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let result = match &self.key {
            Some(key) => seal_pending_with(ctx.db, key).await,
            None => seal_pending(ctx.db).await,
        };
        let (sealed, blocked) = sealed(result)?;
        self.note(|done| {
            done.sealed = sealed;
            done.seal_blocked = blocked;
        });
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

/// A key error is noted as its code and counts nothing sealed; any other error stands.
pub fn sealed(result: Result<usize>) -> Result<(usize, Option<String>)> {
    match result {
        Ok(sealed) => Ok((sealed, None)),
        Err(
            error @ (Error::Locked
            | Error::KeyUnavailable(_)
            | Error::KeyMissing { .. }
            | Error::KeyMismatch { .. }
            | Error::Secret(_)),
        ) => Ok((0, Some(error.code().to_owned()))),
        Err(error) => Err(error),
    }
}

async fn now(db: &Db) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')")
            .fetch_one(&db.pool)
            .await?,
    )
}

/// The provider a classify call went to since `since`, and the data it carried. An external
/// one wins over a local one, since that is what left the machine. `NoAi` is no provider.
async fn asked_since(
    db: &Db,
    since: &str,
) -> Result<(Option<ProviderId>, &'static [DataCategory])> {
    let used: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT provider FROM task_runs WHERE task_type = 'classify_answer' \
         AND attempt >= 1 AND provider <> 'no_ai' AND started_at >= ?",
    )
    .bind(since)
    .fetch_all(&db.pool)
    .await?;
    let named = |id: ProviderId| used.contains(&id.to_string()).then_some(id);
    let provider = named(ProviderId::ClaudeCode).or_else(|| named(ProviderId::Ollama));
    let categories = if provider.is_some() {
        spec(TaskType::ClassifyAnswer).categories
    } else {
        &[]
    };
    Ok((provider, categories))
}
