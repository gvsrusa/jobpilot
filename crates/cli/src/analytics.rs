//! Counts and rates from the application store. No model is involved.

use clap::Subcommand;
use jobpilot_core::analytics::{self, Report};
use jobpilot_core::{Db, Result, agents};

use crate::ai::with_ctrl_c;
use crate::{ACTOR, Output, out};

#[derive(Subcommand)]
pub enum AnalyticsCommand {
    /// Build a new report from your applications.
    Report,
    /// The latest report.
    Show,
}

fn percent(rate: Option<f64>) -> String {
    rate.map_or_else(
        || "not enough data yet".to_owned(),
        |rate| format!("{}%", (rate * 100.0).round()),
    )
}

fn text(report: &Report) -> String {
    let mut lines = vec![
        format!("Applications: {}", report.volume.total),
        format!(
            "Applied in the last 30 days: {}",
            report.volume.applied_last_30_days
        ),
        format!("Response rate: {}", percent(report.funnel.response_rate)),
        format!("Offer rate: {}", percent(report.funnel.offer_rate)),
        format!(
            "Median days to a response: {}",
            report.timing.median_days_to_response.map_or_else(
                || "not enough data yet".to_owned(),
                |days| format!("{days:.1}")
            ),
        ),
    ];
    for (status, count) in &report.volume.by_status {
        lines.push(format!("  {status}: {count}"));
    }
    lines.join("\n")
}

pub async fn analytics(db: &Db, command: AnalyticsCommand) -> Result<Output> {
    match command {
        AnalyticsCommand::Report => {
            let report = with_ctrl_c(|cancel| agents::analytics_report(db, ACTOR, cancel)).await?;
            out(&report, text(&report))
        }
        AnalyticsCommand::Show => match analytics::latest(db).await? {
            Some(report) => out(&report, text(&report)),
            None => out(Option::<Report>::None, "No report yet.".into()),
        },
    }
}
