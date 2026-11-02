//! The 30-day window follows the local calendar. This file holds one test because it
//! changes `TZ` for the whole process, which no sibling test may observe.

use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::{AppPaths, Db};

#[tokio::test]
async fn a_same_day_applied_date_east_of_utc_counts() {
    // UTC+14 at 20:00 UTC on 1 Nov is 10:00 on 2 Nov.
    // SAFETY: this binary has this one test, so no other thread reads the environment.
    unsafe { std::env::set_var("TZ", "Etc/GMT-14") };
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let app = applications::add(
        &db,
        "cli",
        ApplicationInput {
            company: "A".into(),
            role: "R".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE applications SET applied_on = '2026-11-02' WHERE id = ?")
        .bind(&app.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let volume = jobpilot_core::analytics::volume_at(&db, "2026-11-01 20:00:00")
        .await
        .unwrap();
    assert_eq!(volume.applied_last_30_days, 1);
}
