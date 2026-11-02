//! One test in this binary, so its `set_var` runs with no other test thread. The CLIs are
//! the mocks in `mock-gemini` and `mock-codex`: nothing here starts a real one.

use std::path::{Path, PathBuf};

use jobpilot_core::ai;
use jobpilot_core::ai::child::{CODEX, GEMINI, find_cli, set_cli_path};
use jobpilot_core::db::Db;
use jobpilot_core::paths::AppPaths;
use jobpilot_core::{Error, settings};

#[cfg(unix)]
const GEMINI_FILE: &str = "gemini";
#[cfg(windows)]
const GEMINI_FILE: &str = "gemini.cmd";
#[cfg(unix)]
const CODEX_FILE: &str = "codex";
#[cfg(windows)]
const CODEX_FILE: &str = "codex.cmd";

#[cfg(unix)]
const FAILS: &str = "#!/bin/sh\nexit 7\n";
#[cfg(windows)]
const FAILS: &str = "@echo off\r\nexit /b 7\r\n";

fn mock(folder: &str, file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(folder)
        .join(file)
}

fn set(name: &str, value: &std::ffi::OsStr) {
    // SAFETY: the only test in this binary, and it spawns no thread.
    unsafe { std::env::set_var(name, value) };
}

fn clear(name: &str) {
    // SAFETY: as in `set`.
    unsafe { std::env::remove_var(name) };
}

#[tokio::test]
async fn detection_follows_the_override_then_the_setting_and_probes_only_enabled_clis() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    let gemini = mock("mock-gemini", GEMINI_FILE);
    let codex = mock("mock-codex", CODEX_FILE);
    // Nothing on PATH: only the override and the setting can find a program.
    set("PATH", tmp.path().as_os_str());
    set("SHELL", "".as_ref());
    clear("JOBPILOT_GEMINI_BIN");
    clear("JOBPILOT_CODEX_BIN");
    // `ai::status` looks for Claude too; a name that is not there keeps it from starting one.
    set("JOBPILOT_CLAUDE_BIN", tmp.path().join("claude").as_os_str());
    set("HOME", tmp.path().as_os_str());
    set("USERPROFILE", tmp.path().as_os_str());

    // A real install in a system folder could answer for an empty PATH, so a missing program
    // is made by an override naming one that is not there.
    set(
        "JOBPILOT_GEMINI_BIN",
        tmp.path().join(GEMINI_FILE).as_os_str(),
    );
    assert!(find_cli(&GEMINI, Some(&db)).await.is_err());
    let status = ai::status(&db).await.unwrap();
    let row = status
        .external
        .iter()
        .find(|r| r.name == "gemini_cli")
        .unwrap();
    assert!(!row.available);
    assert_eq!(row.reason.as_deref(), Some("not_installed"));
    assert_eq!(row.path, None);
    clear("JOBPILOT_GEMINI_BIN");

    // The setting, through the checked setter: another program is refused, and nothing is stored.
    let err = set_cli_path(&db, "gemini_cli", "/bin/sh")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err:?}");
    let err = set_cli_path(&db, "claude_code", gemini.to_str().unwrap())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err:?}");
    let err = set_cli_path(&db, "gemini_cli", GEMINI_FILE)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Validation(_)),
        "a relative path is refused: {err:?}"
    );
    assert_eq!(settings::get(&db, "ai.gemini_path").await.unwrap(), None);

    set_cli_path(&db, "gemini_cli", gemini.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(find_cli(&GEMINI, Some(&db)).await.unwrap(), gemini);
    // Off: found, and not started. A program whose version fails would show it if it were.
    let failing = tmp.path().join("failing");
    std::fs::create_dir_all(&failing).unwrap();
    let failing = failing.join(GEMINI_FILE);
    std::fs::write(&failing, FAILS).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    set_cli_path(&db, "gemini_cli", failing.to_str().unwrap())
        .await
        .unwrap();
    let status = ai::status(&db).await.unwrap();
    let row = status
        .external
        .iter()
        .find(|r| r.name == "gemini_cli")
        .unwrap();
    assert!(row.available, "an off CLI is not probed: {row:?}");
    assert_eq!(row.path.as_deref(), Some(failing.to_str().unwrap()));
    let sharing = ai::sharing(&db, "gemini_cli").await.unwrap();
    assert_eq!(sharing.path.as_deref(), Some(failing.to_str().unwrap()));

    // On: the version probe runs and its failure is the reason.
    ai::set_enabled(&db, "gemini_cli", true).await.unwrap();
    let status = ai::status(&db).await.unwrap();
    let row = status
        .external
        .iter()
        .find(|r| r.name == "gemini_cli")
        .unwrap();
    assert!(!row.available);
    assert_eq!(row.reason.as_deref(), Some("version_failed"));
    set_cli_path(&db, "gemini_cli", gemini.to_str().unwrap())
        .await
        .unwrap();
    let status = ai::status(&db).await.unwrap();
    let row = status
        .external
        .iter()
        .find(|r| r.name == "gemini_cli")
        .unwrap();
    assert!(row.available, "{row:?}");

    // The override outranks the setting, and must name the program too.
    set("JOBPILOT_CODEX_BIN", codex.as_os_str());
    assert_eq!(find_cli(&CODEX, Some(&db)).await.unwrap(), codex);
    set("JOBPILOT_CODEX_BIN", gemini.as_os_str());
    assert!(find_cli(&CODEX, Some(&db)).await.is_err());

    // Both CLIs on: `status` waits for one probe, not the two in a row.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let slow = tmp.path().join("slow");
        std::fs::create_dir_all(&slow).unwrap();
        // By full path: `PATH` here holds nothing.
        let script = "#!/bin/sh\n/bin/sleep 2\necho 0.40.0\n";
        for (file, provider, variable) in [
            (GEMINI_FILE, "gemini_cli", "JOBPILOT_GEMINI_BIN"),
            (CODEX_FILE, "codex_cli", "JOBPILOT_CODEX_BIN"),
        ] {
            let path = slow.join(file);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            set(variable, path.as_os_str());
            ai::set_enabled(&db, provider, true).await.unwrap();
        }
        let clock = std::time::Instant::now();
        ai::status(&db).await.unwrap();
        assert!(
            clock.elapsed() < std::time::Duration::from_millis(3500),
            "probed one after the other: {:?}",
            clock.elapsed()
        );
        clear("JOBPILOT_GEMINI_BIN");
    }

    // Clearing goes back to discovery.
    set_cli_path(&db, "gemini_cli", "").await.unwrap();
    assert_eq!(
        settings::get(&db, "ai.gemini_path").await.unwrap(),
        Some(serde_json::json!(""))
    );
    assert_ne!(find_cli(&GEMINI, Some(&db)).await.ok(), Some(gemini));
}
