//! The Playwright MCP server JobPilot drives: pinned, installed on request into
//! `<root>/browser-runtime` from a committed lockfile, run with the owner's Node.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::ai::CancellationToken;
#[cfg(unix)]
use crate::ai::child::login_shell_lookup;
use crate::ai::child::{
    KillTree, absolute_dirs, bin_names, child_env, cli_dirs, command, is_program, search,
};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::{audit, pause};

pub const PLAYWRIGHT_MCP: &str = "0.0.84";
pub const PLAYWRIGHT: &str = "1.65.0-alpha-2026-10-30";

/// The strictest `engines.node` in the lock (`playwright`, `playwright-core`). A test pins it.
pub const NODE_MIN: u64 = 20;

const PACKAGE: &str = include_str!("../../assets/browser-runtime/package.json");
const LOCK: &str = include_str!("../../assets/browser-runtime/package-lock.json");

#[derive(Debug, Clone)]
pub struct Runtime {
    pub node: PathBuf,
    /// The server's entry file.
    pub cli: PathBuf,
    pub browsers: PathBuf,
}

#[derive(Debug)]
pub enum RuntimeState {
    NoNode,
    /// The Node found, as it reported itself, is older than `NODE_MIN`.
    NodeTooOld(String),
    NotInstalled,
    WrongVersion(String),
    Ready(Runtime),
}

pub struct BrowserSpec {
    pub profile: PathBuf,
    pub output: PathBuf,
}

type Env<'a> = &'a (dyn Fn(&str) -> Option<OsString> + Sync);

fn real_env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// A debug build reads `name`; a release build never does.
fn debug_var(get: Env, name: &str) -> Option<OsString> {
    if cfg!(debug_assertions) {
        get(name).filter(|value| !value.is_empty())
    } else {
        None
    }
}

/// `JOBPILOT_NODE_BIN` (debug only; one that names nothing is no node), then `PATH`, the
/// usual folders, then the owner's login shell.
async fn find_node(get: Env<'_>) -> Option<PathBuf> {
    if let Some(bin) = debug_var(get, "JOBPILOT_NODE_BIN") {
        return Some(PathBuf::from(bin)).filter(|bin| is_program(bin));
    }
    let path = get("PATH").unwrap_or_default();
    let home = ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|name| get(name))
        .find(|home| !home.is_empty())
        .map(PathBuf::from);
    let dirs = absolute_dirs(&path)
        .into_iter()
        .chain(cli_dirs(home, get, cfg!(windows)));
    if let Some(found) = search(dirs, &bin_names("node", cfg!(windows))) {
        return Some(found);
    }
    #[cfg(unix)]
    if let Some(found) =
        login_shell_lookup(&get("SHELL").unwrap_or_default(), "node", &|_| true).await
    {
        return Some(found);
    }
    None
}

/// What `node --version` prints, and whether its major version is new enough. A node that
/// does not answer reads as too old, with `unknown` for its version.
async fn node_check(node: &Path) -> std::result::Result<(), String> {
    let mut probe = command(node, &child_env(&[]), Path::new("."));
    probe.arg("--version").stdout(std::process::Stdio::piped());
    let answer = async {
        let output = probe.spawn().ok()?.wait_with_output().await.ok()?;
        String::from_utf8(output.stdout).ok()
    };
    let text = tokio::time::timeout(Duration::from_secs(10), answer)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let text = text.trim();
    let major = text
        .strip_prefix('v')
        .and_then(|v| v.split('.').next()?.parse::<u64>().ok());
    match major {
        Some(major) if major >= NODE_MIN => Ok(()),
        _ if text.is_empty() => Err("unknown".into()),
        _ => Err(text.to_owned()),
    }
}

fn runtime_of(db: &Db, node: PathBuf) -> Runtime {
    let dir = db.paths().browser_runtime();
    Runtime {
        node,
        cli: dir.join("node_modules/@playwright/mcp/cli.js"),
        browsers: dir.join("browsers"),
    }
}

/// Reads files only. It never runs the server.
pub async fn state(db: &Db) -> Result<RuntimeState> {
    state_by(db, &real_env).await
}

#[doc(hidden)]
pub async fn state_by(db: &Db, get: Env<'_>) -> Result<RuntimeState> {
    let Some(node) = find_node(get).await else {
        return Ok(RuntimeState::NoNode);
    };
    if let Err(found) = node_check(&node).await {
        return Ok(RuntimeState::NodeTooOld(found));
    }
    let mut runtime = runtime_of(db, node);
    // Tasks 6 and 7 point this at a fake server on a root with no install.
    if let Some(cli) = debug_var(get, "JOBPILOT_PLAYWRIGHT_CLI") {
        runtime.cli = cli.into();
        return Ok(RuntimeState::Ready(runtime));
    }
    let manifest = runtime.cli.with_file_name("package.json");
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return Ok(RuntimeState::NotInstalled);
    };
    let found = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|manifest| manifest["version"].as_str().map(str::to_owned))
        .unwrap_or_default();
    if found != PLAYWRIGHT_MCP {
        return Ok(RuntimeState::WrongVersion(found));
    }
    // A download cut short leaves no browsers folder, and running setup again finishes it.
    if !runtime.cli.is_file() || !runtime.browsers.is_dir() {
        return Ok(RuntimeState::NotInstalled);
    }
    Ok(RuntimeState::Ready(runtime))
}

/// The same bytes for every run. No environment variable changes any of it.
pub fn playwright_config(spec: &BrowserSpec) -> Value {
    json!({
        "browser": {
            "browserName": "chromium",
            "userDataDir": spec.profile,
            // A value that disables nothing, so the server skips its own
            // AutomationControlled flag. JobPilot hides nothing from a site.
            "launchOptions": {"headless": false, "args": ["--disable-blink-features=JobPilotNone"]},
            "contextOptions": {"acceptDownloads": false}
        },
        "capabilities": ["core"],
        "webmcp": false,
        "saveSession": false,
        "ignoreRoots": true,
        "allowUnrestrictedFileAccess": false,
        "imageResponses": "omit",
        "codegen": "none",
        "outputDir": spec.output,
        "console": {"level": "error"},
        "timeouts": {"action": 10000, "navigation": 60000}
    })
}

pub fn command_for(
    runtime: &Runtime,
    config: &Path,
    _cwd: &Path, // the caller starts the process there
) -> (PathBuf, Vec<OsString>, Vec<(String, String)>) {
    let args = vec![
        runtime.cli.clone().into_os_string(),
        "--config".into(),
        config.as_os_str().to_owned(),
    ];
    (
        runtime.node.clone(),
        args,
        env_with_browsers(&runtime.browsers, None),
    )
}

/// `child_env`, `PATH` led by `node_dir` when given, and where the browsers live.
fn env_with_browsers(browsers: &Path, node_dir: Option<&Path>) -> Vec<(String, String)> {
    let mut env = child_env(&[]);
    if let Some(dir) = node_dir {
        env.retain(|(name, _)| name != "PATH");
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&inherited)),
        )
        .unwrap_or(inherited);
        env.push(("PATH".into(), path.to_string_lossy().into_owned()));
    }
    env.push((
        "PLAYWRIGHT_BROWSERS_PATH".into(),
        browsers.to_string_lossy().into_owned(),
    ));
    env
}

/// Installs the pinned server and its Chromium. The download is the owner's to start, and
/// running it again finishes a stopped one.
pub async fn setup(db: &Db, actor: &str, cancel: CancellationToken) -> Result<Runtime> {
    setup_by(db, actor, cancel, &real_env).await
}

#[doc(hidden)]
pub async fn setup_by(
    db: &Db,
    actor: &str,
    cancel: CancellationToken,
    get: Env<'_>,
) -> Result<Runtime> {
    pause::ensure_off(db).await?;
    let node = find_node(get)
        .await
        .ok_or_else(|| Error::ManualRequired("no_node".into()))?;
    if node_check(&node).await.is_err() {
        return Err(Error::ManualRequired("node_too_old".into()));
    }
    let node_dir = node.parent().unwrap_or(Path::new("")).to_path_buf();
    let npm = search([node_dir.clone()], &bin_names("npm", cfg!(windows)))
        .ok_or_else(|| Error::ManualRequired("no_node".into()))?;
    let runtime = runtime_of(db, node.clone());
    let dir = db.paths().browser_runtime();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("package.json"), PACKAGE)?;
    std::fs::write(dir.join("package-lock.json"), LOCK)?;

    let env = env_with_browsers(&runtime.browsers, Some(&node_dir));
    let install = ["ci", "--ignore-scripts"].map(OsString::from);
    run(&npm, &install, &env, &dir, &cancel, "npm_ci_failed").await?;
    let chromium = ["node_modules/playwright/cli.js", "install", "chromium"].map(OsString::from);
    run(&node, &chromium, &env, &dir, &cancel, "chromium_failed").await?;

    audit::record(
        db,
        actor,
        "browser_runtime_installed",
        None,
        json!({"playwright_mcp": PLAYWRIGHT_MCP, "playwright": PLAYWRIGHT}),
    )
    .await?;
    Ok(runtime)
}

/// Waits for the program, polling the token. Dropping the future ends it and its children.
async fn run(
    bin: &Path,
    args: &[OsString],
    env: &[(String, String)],
    cwd: &Path,
    cancel: &CancellationToken,
    failure: &str,
) -> Result<()> {
    let mut child = command(bin, env, cwd)
        .args(args)
        .spawn()
        .map_err(|_| Error::Failed(failure.into()))?;
    let tree = KillTree::of(&child);
    let status = loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Ok(waited) = tokio::time::timeout(Duration::from_millis(100), child.wait()).await {
            break waited?;
        }
    };
    tree.disarm();
    if status.success() {
        Ok(())
    } else {
        Err(Error::Failed(failure.into()))
    }
}
