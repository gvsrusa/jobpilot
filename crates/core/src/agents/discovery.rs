//! Reads the sources a person added and stores what they list. The steps are fixed: `fetch`
//! goes source by source and page by page through the `Fetcher` alone, then `dedupe` groups.
//! No model is called, and nothing a posting says chooses a step or a URL.

use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::ai::{BoxFuture, CancellationToken};
use crate::connectors::fetch::Fetcher;
use crate::connectors::{self, Operation, Posting, Target};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::jobs::dedupe::fold;
use crate::jobs::{self, SourceRow, Upserted, dedupe};
use crate::knowledge;
use crate::runner::{self, Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

const FETCH: &[Permission] = &[
    Permission::NetConnector,
    Permission::ReadJobText,
    Permission::WriteJobs,
];
const DEDUPE: &[Permission] = &[Permission::ReadJobText, Permission::WriteJobs];

pub static STEPS: &[StepDef] = &[
    StepDef {
        name: "fetch",
        needs: FETCH,
        attempts: 1,
    },
    StepDef {
        name: "dedupe",
        needs: DEDUPE,
        attempts: 1,
    },
];

/// How one source went. Counts and a code, never posting text.
#[derive(Debug, Clone, Serialize)]
pub struct SourceResult {
    pub source_id: String,
    pub connector: String,
    pub target: String,
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Why the source stopped early: a refusal code, or a failure's code.
    pub reason: Option<String>,
    /// When a rate-limited source may be read again (UTC, ISO 8601), from the stored hold. It
    /// is the wait the source or the limit asked for, so a long `Retry-After` shows as long.
    pub next_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryReport {
    pub sources: Vec<SourceResult>,
    /// Rows that took or lost a `duplicate_of` in this run.
    pub grouped: usize,
}

/// Public so `resume_run_with` can rebuild it from the stored input. It holds the source ids
/// and the fetcher; what each source did is kept in memory for the report and never stored.
pub struct DiscoveryWorkflow<'f> {
    source_ids: Vec<String>,
    fetcher: &'f Fetcher,
    results: Mutex<Vec<SourceResult>>,
    grouped: Mutex<usize>,
}

impl<'f> DiscoveryWorkflow<'f> {
    pub fn new(source_ids: Vec<String>, fetcher: &'f Fetcher) -> Self {
        DiscoveryWorkflow {
            source_ids,
            fetcher,
            results: Mutex::default(),
            grouped: Mutex::default(),
        }
    }

    fn report(&self) -> DiscoveryReport {
        DiscoveryReport {
            sources: self
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            grouped: *self.grouped.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// The chosen sources in order, or all of them. A source removed since the run began is
    /// left out.
    async fn sources(&self, db: &Db) -> Result<Vec<SourceRow>> {
        if self.source_ids.is_empty() {
            return jobs::list_sources(db).await;
        }
        let mut rows = Vec::new();
        for id in &self.source_ids {
            match jobs::get_source(db, id).await {
                Ok(row) => rows.push(row),
                Err(Error::NotFound) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(rows)
    }

    async fn fetch(&self, ctx: &Ctx<'_>) -> Result<()> {
        let db = ctx.db;
        let picked = self.sources(db).await?;
        // A cursor is honoured only on a resume: one left by a run that was cancelled or lost
        // must not make a new run skip pages.
        if !ctx.resumed() {
            for source in &picked {
                set_cursor(db, &source.id, 0).await?;
            }
        }
        let done = ctx
            .saved("source")
            .and_then(|id| picked.iter().position(|source| source.id == id));
        for source in self
            .sources(db)
            .await?
            .into_iter()
            .skip(done.map_or(0, |at| at + 1))
        {
            ctx.stop_point()?;
            // A source removed since the run began has nothing left to read.
            let Some(result) = self.read(ctx, &source).await? else {
                continue;
            };
            set_cursor(db, &source.id, 0).await?;
            ctx.save("source", &source.id)?;
            self.results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(result);
        }
        Ok(())
    }

    /// One source, from its stored cursor. Only a pause, a cancel, or a store failure ends the
    /// step; anything else is the source's own and goes in its result.
    async fn read(&self, ctx: &Ctx<'_>, source: &SourceRow) -> Result<Option<SourceResult>> {
        let db = ctx.db;
        let mut result = SourceResult {
            source_id: source.id.clone(),
            connector: source.connector.clone(),
            target: source.target.clone(),
            added: 0,
            updated: 0,
            skipped: 0,
            reason: None,
            next_at: None,
        };
        let Some(connector) = connectors::find(&source.connector) else {
            result.reason = Some("source_unknown".into());
            return Ok(Some(result));
        };
        let decl = connector.decl();
        let operation = if decl.operations.contains(&Operation::ListBoard) {
            Operation::ListBoard
        } else {
            Operation::ListFeed
        };
        let target = Target {
            name: source.target.clone(),
        };
        let mut page_no = match jobs::get_source(db, &source.id).await {
            Ok(row) => u32::try_from(row.cursor).unwrap_or(0),
            Err(Error::NotFound) => return Ok(None),
            Err(other) => return Err(other),
        };
        loop {
            ctx.stop_point()?;
            let page = match self
                .fetcher
                .page(db, &*connector, &target, page_no, operation, &ctx.cancel)
                .await
            {
                Ok(page) => page,
                Err(error @ (Error::Paused | Error::Cancelled | Error::Db(_))) => {
                    return Err(error);
                }
                Err(error) => {
                    if let Error::RateLimited {
                        retry_after: Some(wait),
                    } = &error
                    {
                        result.next_at = after(db, *wait).await?;
                    }
                    result.reason = Some(match error {
                        Error::Refused(why) => why,
                        other => other.code().to_owned(),
                    });
                    return Ok(Some(result));
                }
            };
            result.skipped += page.skipped;
            let mut last = None;
            for posting in &page.postings {
                ctx.stop_point()?;
                if !wanted(&source.query, posting) {
                    continue;
                }
                let stored =
                    jobs::upsert_posting(db, decl, &source.target, Some(&source.id), posting).await;
                match stored {
                    Ok(Upserted::Added(id)) => {
                        result.added += 1;
                        last = Some(id);
                    }
                    Ok(Upserted::Updated(id)) => {
                        result.updated += 1;
                        last = Some(id);
                    }
                    Ok(Upserted::Unchanged(id)) => last = Some(id),
                    // Too large, or nothing readable: dropped and counted, like a vetted row.
                    Err(Error::TooLarge { .. } | Error::Validation(_)) => result.skipped += 1,
                    Err(other) => return Err(other),
                }
            }
            set_cursor(db, &source.id, page.next.map_or(0, i64::from)).await?;
            if let Some(id) = last {
                ctx.save("job", &id)?;
            }
            match page.next {
                Some(next) => page_no = next,
                None => return Ok(Some(result)),
            }
        }
    }
}

/// The UTC time `wait` from now, from SQLite so no date crate is needed.
async fn after(db: &Db, wait: std::time::Duration) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)")
            .bind(format!("+{} seconds", wait.as_secs().max(1)))
            .fetch_optional(&db.pool)
            .await?,
    )
}

async fn set_cursor(db: &Db, id: &str, cursor: i64) -> Result<()> {
    sqlx::query("UPDATE job_sources SET cursor = ? WHERE id = ?")
        .bind(cursor)
        .bind(id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

// ponytail: a filter is every word of the query found, as part of a word, in the title,
// company, tags, or text. It is not ranked and takes no operators.
fn wanted(query: &str, posting: &Posting) -> bool {
    let query = fold(query);
    if query.is_empty() {
        return true;
    }
    let head = fold(&format!(
        "{} {} {}",
        posting.title,
        posting.company,
        posting.tags.join(" ")
    ));
    let mut body = None;
    query.split(' ').all(|word| {
        head.contains(word)
            || body
                .get_or_insert_with(|| fold(&knowledge::normalize(&posting.raw).0))
                .contains(word)
    })
}

impl Workflow for DiscoveryWorkflow<'_> {
    fn agent(&self) -> Agent {
        Agent::Discovery
    }

    fn input(&self) -> RunInput {
        RunInput::Discovery {
            source_ids: self.source_ids.clone(),
        }
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            let needs = STEPS
                .get(index)
                .ok_or_else(|| Error::Failed("no_such_step".into()))?
                .needs;
            for &permission in needs {
                ctx.check(permission)?;
            }
            ctx.stop_point()?;
            match index {
                0 => self.fetch(ctx).await?,
                _ => {
                    let grouped = dedupe::run(ctx.db).await?;
                    *self.grouped.lock().unwrap_or_else(PoisonError::into_inner) = grouped;
                }
            }
            Ok(StepOutput::default())
        })
    }
}

/// Reads the chosen sources, or all of them when `source_ids` is empty, through the hosted
/// fetcher. Only a person starts it.
pub async fn discover(
    db: &Db,
    actor: &str,
    source_ids: Vec<String>,
    cancel: CancellationToken,
) -> Result<DiscoveryReport> {
    runner::person(actor)?;
    discover_with(db, &Fetcher::hosted()?, actor, source_ids, cancel).await
}

/// [`discover`] through any fetcher, so a test can point it at a loopback server.
#[doc(hidden)]
pub async fn discover_with(
    db: &Db,
    fetcher: &Fetcher,
    actor: &str,
    source_ids: Vec<String>,
    cancel: CancellationToken,
) -> Result<DiscoveryReport> {
    runner::person(actor)?;
    // An unknown id is plain bad input: no run starts for it.
    for id in &source_ids {
        jobs::get_source(db, id).await?;
    }
    let workflow = DiscoveryWorkflow::new(source_ids, fetcher);
    runner::start(db, actor, &workflow, cancel)
        .await?
        .into_result()?;
    Ok(workflow.report())
}
