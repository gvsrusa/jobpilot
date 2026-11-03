//! Researching a company from text already stored: its postings and the notes a person
//! pasted. No crawler, no search, and no socket: the agent holds no `NetConnector`. A model
//! summarizes through the router on a no-tools path, and Rust keeps only the facts a listed
//! source holds. Only a person starts it.

use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use super::matching::AiPlan;
use super::memory::{asked_since, now};
use crate::ai::router::Router;
use crate::ai::{
    BoxFuture, CancellationToken, Capability, DataCategory, Outcome, TaskType, consent, spec,
};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::jobs::company::{self, keep_cited, replace_summary, sources, summarize_prompt};
use crate::runner::{self, Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

use Permission::*;

pub static STEPS: &[StepDef] = &[
    StepDef {
        name: "gather",
        needs: &[ReadJobText, ReadCompanyText],
        attempts: 1,
    },
    StepDef {
        name: "summarize",
        needs: &[
            ReadJobText,
            ReadCompanyText,
            Model(TaskType::SummarizeCompany),
            WriteCompanyNotes,
        ],
        attempts: 1,
    },
];

/// Counts and a code, never text. `reason` is why no summary was stored, and the notes that
/// were there stay whatever it says.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CompanyResearch {
    pub company_id: String,
    pub sources: usize,
    pub facts: usize,
    pub dropped: usize,
    pub reason: Option<String>,
}

/// Public so a test can wrap it. `resumed` is set when `resume_run` rebuilt it: the model step
/// then makes no call, so only a fresh press spends one.
pub struct ResearchWorkflow<'r> {
    router: &'r Router,
    company_id: String,
    resumed: bool,
    done: Mutex<CompanyResearch>,
}

impl<'r> ResearchWorkflow<'r> {
    pub fn new(router: &'r Router, company_id: String, resumed: bool) -> Self {
        let done = CompanyResearch {
            company_id: company_id.clone(),
            ..CompanyResearch::default()
        };
        ResearchWorkflow {
            router,
            company_id,
            resumed,
            done: Mutex::new(done),
        }
    }

    pub fn report(&self) -> CompanyResearch {
        self.done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn note(&self, change: impl FnOnce(&mut CompanyResearch)) {
        change(&mut self.done.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// The first reason stands: it is the one that stopped the work.
    fn stopped(&self, reason: &str) {
        self.note(|done| {
            done.reason.get_or_insert_with(|| reason.to_owned());
        });
    }

    async fn gather(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let found = sources(ctx.db, &self.company_id).await?;
        self.note(|done| done.sources = found.len());
        Ok(StepOutput::default())
    }

    async fn summarize(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        if self.resumed {
            return Ok(StepOutput::default());
        }
        let db = ctx.db;
        let since = now(db).await?;
        let result = self.ask(ctx).await;
        let asked = asked_since(db, &since, TaskType::SummarizeCompany).await;
        result?;
        let (provider, categories) = asked?;
        let dropped = self
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dropped;
        Ok(StepOutput {
            provider,
            categories,
            dropped,
        })
    }

    async fn ask(&self, ctx: &Ctx<'_>) -> Result<()> {
        let db = ctx.db;
        let found = sources(db, &self.company_id).await?;
        self.note(|done| done.sources = found.len());
        if found.is_empty() {
            self.stopped("no_sources");
            return Ok(());
        }
        let name = company::name(db, &self.company_id).await?;
        let task = TaskType::SummarizeCompany;
        let prompt = summarize_prompt(&name, &found);
        let outcome = self
            .router
            .run(db, ctx.actor, task, prompt, ctx.cancel.clone())
            .await?;
        let (provider, output) = match outcome {
            Outcome::Ok {
                provider, output, ..
            } => (provider, output),
            Outcome::Cancelled => return Err(Error::Cancelled),
            Outcome::ManualRequired { reason } | Outcome::Failed { reason } => {
                self.stopped(&reason);
                return Ok(());
            }
        };
        ctx.stop_point()?;
        let (kept, dropped) = keep_cited(&output, &found);
        self.note(|done| done.dropped += dropped);
        // A reply with nothing left is not a summary: the one before it stays.
        if kept.is_empty() {
            self.stopped("unusable_reply");
            return Ok(());
        }
        match replace_summary(db, &self.company_id, &kept, &provider.to_string()).await {
            Ok(()) => self.note(|done| done.facts = kept.len()),
            Err(Error::NotFound) => self.stopped("company_gone"),
            Err(other) => return Err(other),
        }
        Ok(())
    }
}

impl Workflow for ResearchWorkflow<'_> {
    fn agent(&self) -> Agent {
        Agent::Research
    }

    fn input(&self) -> RunInput {
        RunInput::Research {
            company_id: self.company_id.clone(),
        }
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
                0 => self.gather(ctx).await,
                _ => self.summarize(ctx).await,
            }
        })
    }
}

/// Reads the company's stored text and, if a model answers, replaces its summary with the
/// facts a listed source holds. A model that cannot answer leaves what is stored and puts its
/// reason in the report.
pub async fn research_company(
    db: &Db,
    router: &Router,
    actor: &str,
    company_id: &str,
    cancel: CancellationToken,
) -> Result<CompanyResearch> {
    runner::person(actor)?;
    company::name(db, company_id).await?;
    let workflow = ResearchWorkflow::new(router, company_id.to_owned(), false);
    runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    Ok(workflow.report())
}

/// What a press would spend, at most: one summary call to each external provider that
/// declares `NoTools` and holds an active consent for company and job text, times the attempts
/// of the task. `jobs` is the
/// number of stored texts it would read. Reads only: it starts no provider and asks no key.
pub async fn research_plan(db: &Db, router: &Router, company_id: &str) -> Result<AiPlan> {
    company::name(db, company_id).await?;
    let texts = sources(db, company_id).await?.len();
    let wanted = spec(TaskType::SummarizeCompany).categories;
    let mut providers = 0;
    for declaration in router.declarations() {
        if crate::ai::router::is_external(&declaration)
            && declaration.capabilities.contains(&Capability::NoTools)
        {
            let granted = consent::active(db, &declaration.id.to_string()).await?;
            let covers = |wanted: &[DataCategory]| {
                granted
                    .as_ref()
                    .is_some_and(|given| wanted.iter().all(|c| given.contains(c)))
            };
            providers += usize::from(covers(wanted));
        }
    }
    Ok(AiPlan {
        jobs: texts,
        // A bad reply is asked for again, so the ceiling is every attempt of the task.
        external_calls: if texts == 0 {
            0
        } else {
            providers * spec(TaskType::SummarizeCompany).attempts as usize
        },
    })
}
