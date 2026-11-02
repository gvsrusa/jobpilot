//! A report of counts and rates from the application store. No model, no network (FR-2.12).

use serde::Serialize;

use crate::ai::BoxFuture;
use crate::analytics;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::runner::{Agent, Checkpoint, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

const NEEDS: &[Permission] = &[Permission::ReadApplications, Permission::WriteReports];

pub static STEPS: &[StepDef] = &[
    StepDef {
        name: "volume",
        needs: NEEDS,
        attempts: 2,
    },
    StepDef {
        name: "funnel",
        needs: NEEDS,
        attempts: 2,
    },
    StepDef {
        name: "timing",
        needs: NEEDS,
        attempts: 2,
    },
];

/// Public so a test can wrap it. It holds nothing: a run's input is just the agent.
pub struct Analytics;

fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| Error::Failed("report_encoding".into()))
}

impl Analytics {
    /// Merges one section into the row. A row an earlier cancel dropped is an error, not a
    /// silent no-op, so a resumed run cannot finish without a report.
    async fn merge(db: &Db, id: &str, section: &str, figures: &str, last: bool) -> Result<()> {
        let done = sqlx::query(
            "UPDATE reports SET figures = json_set(figures, '$.' || ?1, json(?2)), \
             complete = ?3 WHERE id = ?4",
        )
        .bind(section)
        .bind(figures)
        .bind(i64::from(last))
        .bind(id)
        .execute(&db.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(Error::Failed("report_missing".into()));
        }
        Ok(())
    }
}

impl Workflow for Analytics {
    fn agent(&self) -> Agent {
        Agent::Analytics
    }

    fn input(&self) -> RunInput {
        RunInput::Analytics
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            for &permission in NEEDS {
                ctx.check(permission)?;
            }
            ctx.stop_point()?;
            let db = ctx.db;
            match index {
                0 => {
                    let figures = json(&analytics::volume(db).await?)?;
                    if let Some(id) = ctx.saved("report") {
                        // A retry after the row exists: write the section again.
                        Self::merge(db, &id, "volume", &figures, false).await?;
                        return Ok(StepOutput::default());
                    }
                    // One statement makes the row and its first section, so a failed
                    // insert leaves no row to retry around.
                    let (id,): (String,) = sqlx::query_as(
                        "INSERT INTO reports (id, created_at, figures) VALUES \
                         (lower(hex(randomblob(16))), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                          json_object('volume', json(?))) RETURNING id",
                    )
                    .bind(figures)
                    .fetch_one(&db.pool)
                    .await?;
                    ctx.save("report", &id)?;
                }
                1 | 2 => {
                    let id = ctx
                        .saved("report")
                        .ok_or_else(|| Error::Failed("report_missing".into()))?;
                    if index == 1 {
                        let figures = json(&analytics::funnel(db).await?)?;
                        Self::merge(db, &id, "funnel", &figures, false).await?;
                    } else {
                        let figures = json(&analytics::timing(db).await?)?;
                        Self::merge(db, &id, "timing", &figures, true).await?;
                    }
                }
                _ => return Err(Error::Failed("no_such_step".into())),
            }
            Ok(StepOutput::default())
        })
    }

    /// A report cut short is not left half built.
    fn abandon<'a>(&'a self, db: &'a Db, saved: &'a Checkpoint) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Some(id) = saved.get("report") {
                sqlx::query("DELETE FROM reports WHERE id = ? AND complete = 0")
                    .bind(id)
                    .execute(&db.pool)
                    .await?;
            }
            Ok(())
        })
    }
}
