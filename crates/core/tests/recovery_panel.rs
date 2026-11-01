//! One test in its own binary, so its `set_var` runs with no other test thread.

use jobpilot_core::{AppPaths, Db, Error, recovery};

#[tokio::test]
async fn export_refuses_the_panel_marker() {
    assert!(!recovery::panel_marker_set());
    // SAFETY: this binary holds one test, and nothing has started another thread yet.
    unsafe { std::env::set_var("JOBPILOT_PANEL", "1") };
    assert!(recovery::panel_marker_set());

    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    let out = tmp.path().join("key.json");

    let export = recovery::export(&db, &out, "cli").await;
    assert!(matches!(export, Err(Error::PanelRefused)), "{export:?}");
    assert!(!out.exists());

    let import = recovery::import(&db, &out, "cli").await;
    assert!(matches!(import, Err(Error::PanelRefused)), "{import:?}");

    let discard = recovery::discard_locked(&db, "anything", "cli").await;
    assert!(matches!(discard, Err(Error::PanelRefused)), "{discard:?}");

    db.close().await;
}
