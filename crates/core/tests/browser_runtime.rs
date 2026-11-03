//! The pinned Playwright MCP runtime. No test downloads a browser or starts a server: the
//! node lookup reads a fake environment, and `setup` is only called where it refuses.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use jobpilot_core::ai::CancellationToken;
use jobpilot_core::browser::runtime::{
    BrowserSpec, NODE_MIN, PLAYWRIGHT, PLAYWRIGHT_MCP, RuntimeState, playwright_config, setup,
    setup_by, state_by,
};
use jobpilot_core::{AppPaths, Db, Error, backup, pause, transfer};
use serde_json::Value;

fn spec() -> BrowserSpec {
    BrowserSpec {
        profile: "/p/profile".into(),
        output: "/p/output".into(),
    }
}

async fn open() -> (tempfile::TempDir, AppPaths, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    (tmp, paths, db)
}

/// A program that prints `version` for `--version`, so the node check accepts or refuses it.
#[cfg(unix)]
fn fake_node(dir: &std::path::Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let node = dir.join("node");
    fs::write(&node, format!("#!/bin/sh\necho {version}\n")).unwrap();
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    node
}

#[cfg(windows)]
fn fake_node(dir: &std::path::Path, version: &str) -> PathBuf {
    let node = dir.join("node.cmd");
    fs::write(&node, format!("@echo off\r\necho {version}\r\n")).unwrap();
    node
}

fn env_of(vars: Vec<(&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
    move |name| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn the_pin_matches_the_committed_lock() {
    let lock: Value =
        serde_json::from_str(include_str!("../assets/browser-runtime/package-lock.json")).unwrap();
    let packages = lock["packages"].as_object().unwrap();
    assert_eq!(
        packages["node_modules/@playwright/mcp"]["version"],
        PLAYWRIGHT_MCP
    );
    assert_eq!(packages["node_modules/playwright"]["version"], PLAYWRIGHT);
    assert_eq!(
        packages["node_modules/playwright-core"]["version"],
        PLAYWRIGHT
    );
    for (name, entry) in packages.iter().filter(|(name, _)| !name.is_empty()) {
        assert!(entry["integrity"].is_string(), "{name} has no integrity");
    }
}

#[test]
fn playwright_config_has_no_anti_detection() {
    let config = playwright_config(&spec()).to_string();
    assert!(!config.contains("AutomationControlled"));
    assert!(config.contains("--disable-blink-features=JobPilotNone"));
    assert!(!config.contains("ignoreDefaultArgs"));
    assert!(!config.contains("userAgent"));
    assert!(!config.contains("proxy"));
}

#[test]
fn the_browser_is_visible_and_page_tools_are_off() {
    let config = playwright_config(&spec());
    assert_eq!(config["browser"]["launchOptions"]["headless"], false);
    assert_eq!(config["browser"]["browserName"], "chromium");
    assert_eq!(
        config["browser"]["contextOptions"]["acceptDownloads"],
        false
    );
    assert_eq!(config["webmcp"], false);
    assert_eq!(config["allowUnrestrictedFileAccess"], false);
    assert_eq!(config["browser"]["userDataDir"], "/p/profile");
    assert_eq!(config["outputDir"], "/p/output");
    assert_eq!(config["console"]["level"], "error");
}

#[test]
fn the_config_is_exactly_the_brief() {
    assert_eq!(
        playwright_config(&spec()),
        serde_json::json!({
            "browser": {
                "browserName": "chromium",
                "userDataDir": "/p/profile",
                "launchOptions": {
                    "headless": false,
                    "args": ["--disable-blink-features=JobPilotNone"]
                },
                "contextOptions": {"acceptDownloads": false}
            },
            "capabilities": ["core"],
            "webmcp": false,
            "saveSession": false,
            "ignoreRoots": true,
            "allowUnrestrictedFileAccess": false,
            "imageResponses": "omit",
            "codegen": "none",
            "outputDir": "/p/output",
            "console": {"level": "error"},
            "timeouts": {"action": 10000, "navigation": 60000}
        })
    );
}

#[tokio::test]
async fn no_node_reads_as_no_node() {
    let (_tmp, _paths, db) = open().await;
    let env = env_of(vec![("JOBPILOT_NODE_BIN", "/nowhere/node".into())]);
    assert!(matches!(
        state_by(&db, &env).await.unwrap(),
        RuntimeState::NoNode
    ));
    db.close().await;
}

#[tokio::test]
async fn a_wrong_version_reads_as_wrong_version() {
    let (tmp, paths, db) = open().await;
    let node = fake_node(tmp.path(), "v24.1.0");
    let env = env_of(vec![("JOBPILOT_NODE_BIN", node.into())]);
    assert!(matches!(
        state_by(&db, &env).await.unwrap(),
        RuntimeState::NotInstalled
    ));

    let mcp = paths.browser_runtime().join("node_modules/@playwright/mcp");
    fs::create_dir_all(&mcp).unwrap();
    fs::create_dir_all(paths.browser_runtime().join("browsers")).unwrap();
    fs::write(mcp.join("cli.js"), "").unwrap();
    fs::write(mcp.join("package.json"), r#"{"version":"0.0.80"}"#).unwrap();
    match state_by(&db, &env).await.unwrap() {
        RuntimeState::WrongVersion(found) => assert_eq!(found, "0.0.80"),
        other => panic!("{other:?}"),
    }

    fs::write(
        mcp.join("package.json"),
        format!(r#"{{"version":"{PLAYWRIGHT_MCP}"}}"#),
    )
    .unwrap();
    match state_by(&db, &env).await.unwrap() {
        RuntimeState::Ready(runtime) => {
            assert!(runtime.cli.ends_with("mcp/cli.js"));
            assert_eq!(runtime.browsers, paths.browser_runtime().join("browsers"));
        }
        other => panic!("{other:?}"),
    }
    db.close().await;
}

#[test]
fn the_node_floor_is_the_strictest_engine_in_the_lock() {
    let lock: Value =
        serde_json::from_str(include_str!("../assets/browser-runtime/package-lock.json")).unwrap();
    let floor = lock["packages"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|entry| entry["engines"]["node"].as_str())
        .map(|range| range.trim_start_matches(">=").parse::<u64>().unwrap())
        .max()
        .unwrap();
    assert_eq!(floor, NODE_MIN);
}

#[tokio::test]
async fn an_old_node_reads_as_too_old_and_setup_refuses_it() {
    let (tmp, paths, db) = open().await;
    let node = fake_node(tmp.path(), "v18.19.0");
    let env = env_of(vec![("JOBPILOT_NODE_BIN", node.into())]);
    match state_by(&db, &env).await.unwrap() {
        RuntimeState::NodeTooOld(found) => assert_eq!(found, "v18.19.0"),
        other => panic!("{other:?}"),
    }
    let refused = setup_by(&db, "cli", CancellationToken::new(), &env).await;
    assert!(
        matches!(&refused, Err(Error::ManualRequired(reason)) if reason == "node_too_old"),
        "{refused:?}"
    );
    assert!(!paths.browser_runtime().exists());
    db.close().await;
}

#[tokio::test]
async fn the_debug_cli_reads_ready_with_no_install() {
    let (tmp, _paths, db) = open().await;
    let node = fake_node(tmp.path(), "v24.1.0");
    let env = env_of(vec![
        ("JOBPILOT_NODE_BIN", node.clone().into()),
        ("JOBPILOT_PLAYWRIGHT_CLI", "/fake/cli.js".into()),
    ]);
    match state_by(&db, &env).await.unwrap() {
        RuntimeState::Ready(runtime) => {
            assert_eq!(runtime.node, node);
            assert_eq!(runtime.cli, PathBuf::from("/fake/cli.js"));
        }
        other => panic!("{other:?}"),
    }
    // No node still wins over the override.
    let none = env_of(vec![
        ("JOBPILOT_NODE_BIN", "/nowhere/node".into()),
        ("JOBPILOT_PLAYWRIGHT_CLI", "/fake/cli.js".into()),
    ]);
    assert!(matches!(
        state_by(&db, &none).await.unwrap(),
        RuntimeState::NoNode
    ));
    db.close().await;
}

#[tokio::test]
async fn setup_is_refused_while_paused() {
    let (_tmp, paths, db) = open().await;
    pause::set(&db, "cli").await.unwrap();
    let result = setup(&db, "cli", CancellationToken::new()).await;
    assert!(matches!(result, Err(Error::Paused)), "{result:?}");
    assert!(!paths.browser_runtime().exists(), "nothing was copied");
    db.close().await;
}

#[tokio::test]
async fn backup_and_export_skip_the_runtime_and_apply_folders() {
    let (_tmp, paths, db) = open().await;
    for folder in [paths.browser_runtime(), paths.apply()] {
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("marker.txt"), "x").unwrap();
    }
    let backup = backup::create(&db, "cli").await.unwrap();
    let export = transfer::export_all(&db, "cli").await.unwrap();
    for dir in [backup.path, export] {
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .all(|n| n != "apply" && n != "browser-runtime" && n != "marker.txt"),
            "{names:?}"
        );
    }
    assert_eq!(paths.apply(), paths.root().join("apply"));
    assert_eq!(
        paths.browser_runtime(),
        paths.root().join("browser-runtime")
    );
    db.close().await;
}

#[test]
fn the_command_runs_node_on_the_server_with_the_config_and_browsers() {
    use jobpilot_core::browser::runtime::{Runtime, command_for};
    let runtime = Runtime {
        node: "/n/node".into(),
        cli: "/r/cli.js".into(),
        browsers: "/r/browsers".into(),
    };
    let (bin, args, env) = command_for(&runtime, "/c.json".as_ref(), "/w".as_ref());
    assert_eq!(bin, PathBuf::from("/n/node"));
    assert_eq!(
        args,
        ["/r/cli.js", "--config", "/c.json"].map(OsString::from)
    );
    assert!(env.contains(&("PLAYWRIGHT_BROWSERS_PATH".into(), "/r/browsers".into())));
}
