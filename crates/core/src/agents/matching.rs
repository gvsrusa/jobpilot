//! Scoring jobs, and the model's two optional looks at them. `fields` lets a model fill the
//! blanks of a job the rules left empty, `score` is rules only, and `semantic` stores a
//! separate evaluation. A model never writes the score and never chooses a filter, a step,
//! or a job. With `ai` off no router is touched.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use super::memory::{asked_since, now};
use crate::ai::router::Router;
use crate::ai::{
    BoxFuture, CancellationToken, Capability, DataCategory, Outcome, TaskType, consent, spec,
};
use crate::db::Db;
use crate::drafts::states_authorization;
use crate::error::{Error, Result};
use crate::jobs::semantic::{
    check_fit, keep_grounded, merge_parsed, needs_parsing, parse_job_prompt, semantic_prompt,
    store_parsed, store_semantic,
};
use crate::jobs::{self, JobFilter, score};
use crate::knowledge;
use crate::runner::{self, Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

use Permission::*;

/// Jobs one AI press looks at, so the most it pays for is bounded.
pub const MATCH_BATCH: usize = 20;

const FIELDS: &[Permission] = &[ReadJobText, Model(TaskType::ParseJob), WriteJobFields];
const SCORE: &[Permission] = &[ReadJobText, ReadProfileBasic, ReadResumeText, WriteMatches];
const SEMANTIC: &[Permission] = &[
    ReadJobText,
    ReadResumeText,
    Model(TaskType::SemanticMatch),
    WriteMatches,
];

pub static STEPS: &[StepDef] = &[
    StepDef {
        name: "fields",
        needs: FIELDS,
        attempts: 1,
    },
    StepDef {
        name: "score",
        needs: SCORE,
        attempts: 1,
    },
    StepDef {
        name: "semantic",
        needs: SEMANTIC,
        attempts: 1,
    },
];

#[derive(Debug, Clone, Default)]
pub struct MatchRequest {
    /// With `ai` off, empty means every job that is unscored or stale.
    pub job_ids: Vec<String>,
    pub ai: bool,
}

/// Counts and a code, never text. `reason` is why a model step stopped early, and the rules
/// score stands whatever it says.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MatchReport {
    pub scored: usize,
    pub parsed: usize,
    pub evaluated: usize,
    /// Model output the checks cut, over both model steps.
    pub dropped: usize,
    pub reason: Option<String>,
}

/// The most a press would send out. Reads only.
#[derive(Debug, Clone, Serialize)]
pub struct AiPlan {
    pub jobs: usize,
    pub external_calls: usize,
}

/// Public so a test can wrap it. `resumed` is set when `resume_run` rebuilt it: the model
/// steps then make no call, so only a fresh press spends one.
pub struct MatchingWorkflow<'r> {
    router: &'r Router,
    job_ids: Vec<String>,
    ai: bool,
    resumed: bool,
    done: Mutex<MatchReport>,
}

impl<'r> MatchingWorkflow<'r> {
    pub fn new(router: &'r Router, job_ids: Vec<String>, ai: bool, resumed: bool) -> Self {
        MatchingWorkflow {
            router,
            job_ids,
            ai,
            resumed,
            done: Mutex::default(),
        }
    }

    pub fn report(&self) -> MatchReport {
        self.done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn note(&self, change: impl FnOnce(&mut MatchReport)) {
        change(&mut self.done.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// The first reason stands: it is the one that stopped the work.
    fn stopped(&self, reason: &str) {
        self.note(|done| {
            done.reason.get_or_insert_with(|| reason.to_owned());
        });
    }

    fn model_steps_off(&self) -> bool {
        !self.ai || self.resumed
    }

    /// One job's id saved twice: the job, then its result. Both are the job's own id.
    fn saved(ctx: &Ctx<'_>, id: &str) -> Result<()> {
        ctx.save("job", id)?;
        ctx.save("result", id)
    }

    async fn fields(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        if self.model_steps_off() {
            return Ok(StepOutput::default());
        }
        let db = ctx.db;
        let since = now(db).await?;
        let result = self.parse_each(ctx).await;
        let asked = asked_since(db, &since, TaskType::ParseJob).await;
        let dropped = result?;
        let (provider, categories) = asked?;
        self.note(|done| done.dropped += dropped);
        Ok(StepOutput {
            provider,
            categories,
            dropped,
        })
    }

    async fn parse_each(&self, ctx: &Ctx<'_>) -> Result<usize> {
        let db = ctx.db;
        let mut dropped = 0;
        for id in &self.job_ids {
            ctx.stop_point()?;
            let job = match jobs::get(db, id).await {
                Ok(job) => job,
                Err(Error::NotFound) => continue,
                Err(other) => return Err(other),
            };
            if !needs_parsing(&job) {
                continue;
            }
            let posting = knowledge::get_job_text(db, &job.job_text_id).await?;
            let prompt = parse_job_prompt(&posting);
            let outcome = self
                .router
                .run(
                    db,
                    ctx.actor,
                    TaskType::ParseJob,
                    prompt,
                    ctx.cancel.clone(),
                )
                .await?;
            let output = match outcome {
                Outcome::Ok { output, .. } => output,
                Outcome::Cancelled => return Err(Error::Cancelled),
                Outcome::ManualRequired { reason } | Outcome::Failed { reason } => {
                    self.stopped(&reason);
                    return Ok(dropped);
                }
            };
            ctx.stop_point()?;
            let fields = merge_parsed(&job, &posting, &output);
            dropped += fields.dropped;
            match store_parsed(db, id, &fields).await {
                Ok(()) => self.note(|done| done.parsed += usize::from(!fields.is_empty())),
                Err(Error::NotFound) => continue,
                Err(other) => return Err(other),
            }
            Self::saved(ctx, id)?;
        }
        Ok(dropped)
    }

    /// The jobs to score: the ones asked for, or all that are unscored or stale.
    async fn to_score(&self, ctx: &Ctx<'_>) -> Result<Vec<String>> {
        if !self.job_ids.is_empty() {
            return Ok(self.job_ids.clone());
        }
        needing_a_score(ctx.db).await
    }

    async fn score(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        let db = ctx.db;
        let ids = self.to_score(ctx).await?;
        let me = score::candidate(db).await?;
        for id in ids {
            ctx.stop_point()?;
            let job = match jobs::get(db, &id).await {
                Ok(job) => job,
                Err(Error::NotFound) => continue,
                Err(other) => return Err(other),
            };
            let explanation = score::score(&job, &me);
            let inputs = score::inputs_key(&job, &me);
            match score::store(db, &id, &explanation, &inputs).await {
                Ok(()) => self.note(|done| done.scored += 1),
                Err(Error::NotFound) => continue,
                Err(other) => return Err(other),
            }
            Self::saved(ctx, &id)?;
        }
        Ok(StepOutput::default())
    }

    async fn semantic(&self, ctx: &Ctx<'_>) -> Result<StepOutput> {
        if self.model_steps_off() {
            return Ok(StepOutput::default());
        }
        let db = ctx.db;
        // A line that states work authorization is never sent: with nothing else, no call is made.
        let evidence: Vec<_> = knowledge::evidence(db)
            .await?
            .into_iter()
            .filter(|item| !states_authorization(&item.body))
            .collect();
        if evidence.is_empty() {
            self.stopped("no_evidence");
            return Ok(StepOutput::default());
        }
        let since = now(db).await?;
        let result = self.evaluate_each(ctx, &evidence).await;
        let asked = asked_since(db, &since, TaskType::SemanticMatch).await;
        let dropped = result?;
        let (provider, categories) = asked?;
        self.note(|done| done.dropped += dropped);
        Ok(StepOutput {
            provider,
            categories,
            dropped,
        })
    }

    async fn evaluate_each(
        &self,
        ctx: &Ctx<'_>,
        evidence: &[knowledge::KnowledgeItem],
    ) -> Result<usize> {
        let db = ctx.db;
        let mut dropped = 0;
        for id in &self.job_ids {
            ctx.stop_point()?;
            let job = match jobs::get(db, id).await {
                Ok(job) => job,
                Err(Error::NotFound) => continue,
                Err(other) => return Err(other),
            };
            let posting = knowledge::get_job_text(db, &job.job_text_id).await?;
            let prompt = semantic_prompt(evidence, &posting);
            let task = TaskType::SemanticMatch;
            let outcome = self
                .router
                .run_checked(db, ctx.actor, task, prompt, ctx.cancel.clone(), check_fit)
                .await?;
            let (provider, output) = match outcome {
                Outcome::Ok {
                    provider, output, ..
                } => (provider, output),
                Outcome::Cancelled => return Err(Error::Cancelled),
                Outcome::ManualRequired { reason } | Outcome::Failed { reason } => {
                    self.stopped(&reason);
                    return Ok(dropped);
                }
            };
            ctx.stop_point()?;
            let mut semantic = match keep_grounded(&output, evidence) {
                Ok(semantic) => semantic,
                Err(Error::Validation(_)) => {
                    self.stopped("unusable_reply");
                    return Ok(dropped);
                }
                Err(other) => return Err(other),
            };
            dropped += semantic.dropped;
            semantic.provider = provider.to_string();
            semantic.at = now(db).await?;
            match store_semantic(db, id, &semantic).await {
                Ok(()) => self.note(|done| done.evaluated += 1),
                Err(Error::NotFound) => continue,
                Err(other) => return Err(other),
            }
            Self::saved(ctx, id)?;
        }
        Ok(dropped)
    }
}

impl Workflow for MatchingWorkflow<'_> {
    fn agent(&self) -> Agent {
        Agent::Matching
    }

    fn input(&self) -> RunInput {
        RunInput::Matching {
            job_ids: self.job_ids.clone(),
            ai: self.ai,
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
                0 => self.fields(ctx).await,
                1 => self.score(ctx).await,
                _ => self.semantic(ctx).await,
            }
        })
    }
}

/// The ids once each, in order. With `ai` on there must be 1 to [`MATCH_BATCH`] of them, and
/// each must exist: bad input gets its plain error and no run.
async fn checked_ids(db: &Db, ids: Vec<String>, ai: bool) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let ids: Vec<String> = ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();
    if ai && !(1..=MATCH_BATCH).contains(&ids.len()) {
        return Err(Error::Validation(
            "an AI evaluation takes 1 to 20 jobs at a time".into(),
        ));
    }
    for id in &ids {
        jobs::get(db, id).await?;
    }
    Ok(ids)
}

/// The jobs with no stored result, or one made from other inputs. A job whose result is a null
/// score (an empty profile) is done: it waits for its inputs to change, not for another pass.
async fn needing_a_score(db: &Db) -> Result<Vec<String>> {
    let filter = JobFilter {
        include_duplicates: true,
        ..JobFilter::default()
    };
    let done: std::collections::HashSet<String> =
        sqlx::query_scalar("SELECT job_id FROM match_results")
            .fetch_all(&db.pool)
            .await?
            .into_iter()
            .collect();
    Ok(jobs::list(db, &filter, i64::MAX)
        .await?
        .into_iter()
        .filter(|job| job.stale || !done.contains(&job.id))
        .map(|job| job.id)
        .collect())
}

/// Scores the jobs, and with `ai` asks a model to fill blank fields and to write a separate
/// evaluation. Only a person starts it. A model that cannot answer leaves the rules score and
/// puts its reason in the report.
pub async fn match_jobs(
    db: &Db,
    router: &Router,
    actor: &str,
    req: MatchRequest,
    cancel: CancellationToken,
) -> Result<MatchReport> {
    runner::person(actor)?;
    let ids = checked_ids(db, req.job_ids, req.ai).await?;
    // The pass a screen starts on load must not write a run when there is nothing to do.
    if ids.is_empty() && !req.ai && needing_a_score(db).await?.is_empty() {
        return Ok(MatchReport::default());
    }
    let workflow = MatchingWorkflow::new(router, ids, req.ai, false);
    runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    Ok(workflow.report())
}

/// What a press with `ai` would spend, at most: each job, times each external provider that
/// declares `NoTools` and holds an active consent, times the attempts of each of the two
/// tasks that consent covers. Reads only: it starts no provider and asks no key. Fewer calls
/// happen when the first reply works.
pub async fn match_plan(db: &Db, router: &Router, job_ids: &[String]) -> Result<AiPlan> {
    let ids = checked_ids(db, job_ids.to_vec(), true).await?;
    let tasks = [TaskType::ParseJob, TaskType::SemanticMatch];
    let mut per_job = 0;
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
            per_job += tasks
                .iter()
                .filter(|task| covers(spec(**task).categories))
                .map(|task| spec(*task).attempts as usize)
                .sum::<usize>();
        }
    }
    Ok(AiPlan {
        jobs: ids.len(),
        external_calls: ids.len() * per_job,
    })
}
