//! A status change to `applied` stamps today's local date. This file holds one test because
//! it changes `TZ` for the whole process.

use jobpilot_core::applications::{self, ApplicationInput};
use jobpilot_core::{AppPaths, Db};

#[tokio::test]
async fn applied_stamps_the_local_date_not_the_utc_date() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    // Pick a zone whose date differs from UTC's right now: UTC+14 from 10:00 UTC, else UTC-12.
    let (hour,): (String,) = sqlx::query_as("SELECT strftime('%H', 'now')")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let zone = if hour.parse::<u32>().unwrap() >= 10 {
        "Etc/GMT-14"
    } else {
        "Etc/GMT+12"
    };
    // SAFETY: this binary has this one test, so no other thread reads the environment.
    unsafe { std::env::set_var("TZ", zone) };
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
    let app = applications::set_status(&db, "cli", &app.id, "applied", None, "")
        .await
        .unwrap();
    let (local, utc): (String, String) =
        sqlx::query_as("SELECT date('now', 'localtime'), date('now')")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_ne!(local, utc, "the chosen zone must differ from UTC today");
    assert_eq!(app.applied_on.as_deref(), Some(local.as_str()));
}
