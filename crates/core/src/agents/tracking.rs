//! One status update: the application store only. No model, no network (FR-11.7).

use crate::ai::BoxFuture;
use crate::applications::{self, Application};
use crate::error::Result;
use crate::runner::{Agent, Ctx, Permission, RunInput, StepDef, StepOutput, Workflow};

const NEEDS: &[Permission] = &[Permission::ReadApplications, Permission::WriteApplications];

pub static STEPS: &[StepDef] = &[StepDef {
    name: "update",
    needs: NEEDS,
    attempts: 2,
}];

/// Public so a test can wrap it.
pub struct Tracking {
    pub application_id: String,
    pub status: String,
    pub on: Option<String>,
    /// Only for the update. A run's row never holds it.
    pub note: String,
    /// Manual mode's `application_submitted` detail, written with the status. Only
    /// [`Tracking::manual`] sets it, with its fixed shape. A run's row holds only a flag.
    submission: Option<serde_json::Value>,
}

impl Tracking {
    pub fn new(application_id: &str, status: &str, on: Option<&str>, note: &str) -> Tracking {
        Tracking {
            application_id: application_id.to_owned(),
            status: status.to_owned(),
            on: on.map(str::to_owned),
            note: note.to_owned(),
            submission: None,
        }
    }

    /// A person's Manual-mode record: an `applied` also writes the submission row, once.
    pub fn manual(app: &Application, status: &str, on: Option<&str>, note: &str) -> Tracking {
        Tracking {
            submission: (status == "applied").then(|| crate::apply::manual_submission(app)),
            ..Tracking::new(&app.id, status, on, note)
        }
    }
}

impl Workflow for Tracking {
    fn agent(&self) -> Agent {
        Agent::Tracking
    }

    fn input(&self) -> RunInput {
        RunInput::Tracking {
            application_id: self.application_id.clone(),
            status: self.status.clone(),
            on: self.on.clone(),
            manual: self.submission.is_some(),
        }
    }

    fn steps(&self) -> &'static [StepDef] {
        STEPS
    }

    fn step<'a>(&'a self, ctx: &'a Ctx<'a>, _index: usize) -> BoxFuture<'a, Result<StepOutput>> {
        Box::pin(async move {
            for &permission in NEEDS {
                ctx.check(permission)?;
            }
            ctx.stop_point()?;
            applications::set_status_with(
                ctx.db,
                ctx.actor,
                &self.application_id,
                &self.status,
                self.on.as_deref(),
                &self.note,
                self.submission.as_ref(),
            )
            .await?;
            Ok(StepOutput::default())
        })
    }
}
