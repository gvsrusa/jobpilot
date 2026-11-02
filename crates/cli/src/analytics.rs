//! Counts and rates from the application store. No model is involved.

use clap::Subcommand;
use jobpilot_core::analytics::{self, Report, Timing};
use jobpilot_core::{Db, Result, agents};

use crate::ai::with_interrupts;
use crate::{ACTOR, Output, out};

const NO_DATA: &str = "not enough data yet";

#[derive(Subcommand)]
pub enum AnalyticsCommand {
    /// Build a new report from your applications.
    Report,
    /// The latest report.
    Show,
}

/// A rate that is not zero never reads 0%, and one under 1 never reads 100%.
fn percent(rate: Option<f64>) -> String {
    let Some(rate) = rate else {
        return NO_DATA.to_owned();
    };
    let whole = (rate * 100.0).round();
    if rate > 0.0 && whole < 1.0 {
        "<1%".to_owned()
    } else if rate < 1.0 && whole > 99.0 {
        ">99%".to_owned()
    } else {
        format!("{whole}%")
    }
}

fn median(timing: &Timing) -> String {
    let Some(days) = timing.median_days_to_response else {
        return NO_DATA.to_owned();
    };
    let n = timing.responses_counted;
    format!(
        "{days:.1} (from {n} response{})",
        if n == 1 { "" } else { "s" }
    )
}

fn text(report: &Report) -> String {
    let mut lines = vec![
        format!("As of {}", report.created_at),
        format!("Applications: {}", report.volume.total),
        format!(
            "Applied in the last 30 days: {}",
            report.volume.applied_last_30_days
        ),
        format!("Response rate: {}", percent(report.funnel.response_rate)),
        format!("Offer rate: {}", percent(report.funnel.offer_rate)),
        format!("Median days to a response: {}", median(&report.timing)),
    ];
    for (status, count) in &report.volume.by_status {
        lines.push(format!("  {status}: {count}"));
    }
    lines.join("\n")
}

pub async fn analytics(db: &Db, command: AnalyticsCommand) -> Result<Output> {
    match command {
        AnalyticsCommand::Report => {
            let report =
                with_interrupts(db, |cancel| agents::analytics_report(db, ACTOR, cancel)).await?;
            out(&report, text(&report))
        }
        AnalyticsCommand::Show => match analytics::latest(db).await? {
            Some(report) => out(&report, text(&report)),
            None => out(Option::<Report>::None, "No report yet.".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use jobpilot_core::analytics::{Funnel, Timing, Volume};

    use super::*;

    fn report(response_rate: Option<f64>, median: Option<f64>, counted: i64) -> Report {
        Report {
            id: "r".into(),
            created_at: "2026-11-01T10:00:00.000Z".into(),
            volume: Volume::default(),
            funnel: Funnel {
                response_rate,
                ..Funnel::default()
            },
            timing: Timing {
                median_days_to_response: median,
                responses_counted: counted,
            },
        }
    }

    #[test]
    fn a_rate_never_rounds_to_an_end_it_has_not_reached() {
        assert_eq!(percent(Some(0.0)), "0%");
        assert_eq!(percent(Some(0.002)), "<1%");
        assert_eq!(percent(Some(0.25)), "25%");
        assert_eq!(percent(Some(0.998)), ">99%");
        assert_eq!(percent(Some(1.0)), "100%");
        assert_eq!(percent(None), "not enough data yet");
    }

    #[test]
    fn the_text_names_its_time_and_the_sample_behind_the_median() {
        let text = text(&report(Some(0.5), Some(3.5), 2));
        assert!(text.starts_with("As of 2026-11-01T10:00:00.000Z"), "{text}");
        assert!(
            text.contains("Median days to a response: 3.5 (from 2 responses)"),
            "{text}"
        );
        let one = super::text(&report(None, Some(1.0), 1));
        assert!(one.contains("1.0 (from 1 response)"), "{one}");
    }
}
