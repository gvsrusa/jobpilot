//! What every program JobPilot starts for an AI provider shares: which environment it gets,
//! which names and folders count as that program, how it is found, and the pipe run. The
//! prompt goes by stdin, never argv, and a `.cmd` shim gets Rust's own batch escaping, never a
//! shell string. Claude Code's adapter calls the same code with its own list and flags.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::db::Db;
use crate::error::{Error, Result};
use crate::settings;

/// Node CLIs take seconds to start. `claude --version` keeps its own 5 s.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// One program JobPilot can run as a provider, and where its path is kept.
pub struct CliSpec {
    pub program: &'static str,
    pub env_bin: &'static str,
    pub setting: &'static str,
    /// The npm package and its entry file. The macOS open panel resolves a link, so a picked
    /// `gemini` comes back as `.../node_modules/@google/gemini-cli/.../gemini.js`.
    pub npm_entry: Option<(&'static str, &'static str)>,
    /// Sign-in variables, copied by name on top of `BASE_ENV` (decision 17). Vertex stays off.
    pub extra_env: &'static [&'static str],
}

pub const GEMINI: CliSpec = CliSpec {
    program: "gemini",
    env_bin: "JOBPILOT_GEMINI_BIN",
    setting: "ai.gemini_path",
    npm_entry: Some(("@google/gemini-cli", "gemini.js")),
    extra_env: &[
        "GEMINI_API_KEY",
        "GEMINI_CLI_HOME",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_LOCATION",
    ],
};

pub const CODEX: CliSpec = CliSpec {
    program: "codex",
    env_bin: "JOBPILOT_CODEX_BIN",
    setting: "ai.codex_path",
    npm_entry: Some(("@openai/codex", "codex.js")),
    extra_env: &["CODEX_HOME", "CODEX_API_KEY"],
};

/// `gemini_cli` and `codex_cli`; every other provider name is not a program here.
pub fn spec_for(name: &str) -> Option<&'static CliSpec> {
    match name {
        "gemini_cli" => Some(&GEMINI),
        "codex_cli" => Some(&CODEX),
        _ => None,
    }
}

/// Copied from the parent; everything else is dropped, API keys and tokens above all.
const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "SystemRoot",
    "ComSpec",
    "LANG",
    "LC_ALL",
    "TERM",
    "XDG_CONFIG_HOME",
    "USER",
    "TMPDIR",
];

/// `BASE_ENV` then `spec_extra`, each only when set. Names are copied, values never read.
pub fn child_env(spec_extra: &[&str]) -> Vec<(String, String)> {
    child_env_from(spec_extra, &|name| std::env::var(name).ok())
}

pub fn child_env_from(
    spec_extra: &[&str],
    get: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    BASE_ENV
        .iter()
        .chain(spec_extra)
        .filter_map(|name| Some(((*name).to_string(), get(name)?)))
        .collect()
}

/// What a run needs. `env` is final: `run_piped` adds nothing to it.
pub struct ChildRun<'a> {
    pub bin: &'a Path,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub cwd: &'a Path,
    pub stdin: &'a [u8],
    pub limit: u64,
}

#[derive(Debug)]
pub struct ChildOutput {
    pub stdout: Vec<u8>,
    pub status: ExitStatus,
}

/// The child leads a process group of its own (macOS), so `KillTree` reaches what it
/// started. Stderr is never read, so there is no pipe to drain and nothing to leak.
pub(crate) fn command(bin: &Path, env: &[(String, String)], cwd: &Path) -> Command {
    let mut command = Command::new(bin);
    command
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Rust refuses a `.cmd` argument it cannot escape with `InvalidInput`: that is the
/// program's text being wrong for this shim, not an I/O fault.
fn spawn_error(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        Error::Failed("cli_error".into())
    } else {
        Error::from(error)
    }
}

/// Starts the program, writes `stdin`, and reads at most `limit` bytes of stdout. Dropping the
/// future ends the child and what it started.
#[doc(hidden)]
pub async fn run_piped(run: ChildRun<'_>) -> Result<ChildOutput> {
    let mut command = command(run.bin, &run.env, run.cwd);
    command
        .args(&run.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = command.spawn().map_err(spawn_error)?;
    let tree = KillTree::of(&child);
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(Error::Failed("no_pipes".into()));
    };

    // Together: a child that writes before it has read everything must not stall us.
    let write = async move {
        // A child that stops reading says why in its exit status and stdout.
        let _ = stdin.write_all(run.stdin).await;
    };
    let limit = run.limit;
    let mut output = Vec::new();
    let mut capped = stdout.take(limit + 1);
    let read = capped.read_to_end(&mut output);
    let ((), read) = tokio::join!(write, read);
    read?;
    if output.len() as u64 > limit {
        return Err(Error::TooLarge { limit });
    }
    let status = child.wait().await?;
    tree.disarm();
    Ok(ChildOutput {
        stdout: output,
        status,
    })
}

/// `--version`. `Err` is the word for `Availability::Unavailable`, as Claude's probe gives
/// them: `not_installed`, `version_failed`, or `no_answer` past `VERSION_TIMEOUT`.
pub(crate) async fn version_of(
    bin: &Path,
    env: Vec<(String, String)>,
) -> std::result::Result<String, &'static str> {
    let dir = tempfile::tempdir().map_err(|_| "no_temp_dir")?;
    let mut probe = command(bin, &env, dir.path());
    probe.arg("--version").stdout(Stdio::piped());
    let mut child = probe.spawn().map_err(|_| "not_installed")?;
    let tree = KillTree::of(&child);
    let stdout = child.stdout.take();
    let ask = async {
        let mut text = Vec::new();
        if let Some(stdout) = stdout {
            // A short read is enough; the rest is dropped with the pipe.
            let _ = stdout.take(4096).read_to_end(&mut text).await;
        }
        child.wait().await.map(|status| (status, text))
    };
    let Ok(Ok((status, text))) = tokio::time::timeout(VERSION_TIMEOUT, ask).await else {
        return Err("no_answer");
    };
    tree.disarm();
    if !status.success() {
        return Err("version_failed");
    }
    let text = String::from_utf8_lossy(&text);
    Ok(text.lines().next().unwrap_or_default().trim().to_string())
}

/// Ends the child and everything it started when dropped armed: `kill_on_drop` reaches the
/// direct child only, and a `.cmd` shim or a helper would live on. It must drop before the
/// `Child`, which callers get by declaring it later: on Windows the tree walk starts from a
/// live parent, and on macOS the pid must not have been reaped and reused.
// ponytail: nothing kills the child if the app dies without dropping this. The 60 s limit
// lives in the router, so it dies too, and `process_group(0)` also hides the child from a
// terminal Ctrl-C. The ceiling is the CLI's own run, which this app does not bound; broken
// pipes are all it notices. A Windows job object and a macOS watchdog would close that.
pub(crate) struct KillTree(Option<u32>);

impl KillTree {
    pub(crate) fn of(child: &Child) -> KillTree {
        KillTree(child.id())
    }

    /// The child was waited for, so its pid is no longer ours to signal.
    pub(crate) fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for KillTree {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_tree(pid);
        }
    }
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    if let Ok(group) = i32::try_from(pid) {
        // SAFETY: no pointers. `group` is the group `process_group(0)` made for this child,
        // and the child is unreaped, so the id cannot have been reused.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
}

// ponytail: blocks the runtime thread for the taskkill run, on cancel and timeout only. It
// has to finish first, because `kill_on_drop` then ends the parent the walk starts from.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let _ = std::process::Command::new(PathBuf::from(root).join(r"System32\taskkill.exe"))
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The native binary first on Windows: cmd.exe caps a command line at 8191 characters, the
/// .exe does not.
pub fn bin_names(program: &str, windows: bool) -> Vec<String> {
    if windows {
        vec![format!("{program}.exe"), format!("{program}.cmd")]
    } else {
        vec![program.to_string()]
    }
}

#[cfg(unix)]
pub(crate) fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
pub(crate) fn is_program(path: &Path) -> bool {
    path.is_file()
}

/// A relative entry would run whatever sits in the working directory.
pub fn absolute_dirs(path: &OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path)
        .filter(|entry| entry.is_absolute())
        .collect()
}

/// The first program named `names` in `dirs`, in that order.
pub(crate) fn search(dirs: impl IntoIterator<Item = PathBuf>, names: &[String]) -> Option<PathBuf> {
    dirs.into_iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| is_program(candidate))
}

/// Asks the owner's login shell where `program` is: a fixed command, never user input. The
/// shell's startup files may print noise, so the last absolute line the caller accepts is the
/// answer.
#[cfg(unix)]
pub(crate) async fn login_shell_lookup(
    shell: &OsStr,
    program: &'static str,
    accepts: &(dyn Fn(&Path) -> bool + Sync),
) -> Option<PathBuf> {
    let shell = Path::new(shell);
    if !shell.is_absolute() {
        return None;
    }
    let lookup = Command::new(shell)
        .args(["-l", "-c", &format!("command -v {program}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(LOGIN_SHELL_TIMEOUT, lookup)
        .await
        .ok()?
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| PathBuf::from(line.trim()))
        .rfind(|path| path.is_absolute() && accepts(path) && is_program(path))
}

/// Folders a Node CLI lands in: the package managers' and version managers' own, newest Node
/// first. Pure but for the version folders it lists; `windows` picks the layout.
pub fn cli_dirs(
    home: Option<PathBuf>,
    env: &dyn Fn(&str) -> Option<OsString>,
    windows: bool,
) -> Vec<PathBuf> {
    let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut dirs = Vec::new();
    if windows {
        if let Some(base) = var("APPDATA") {
            dirs.push(base.join("npm"));
        }
        if let Some(base) = var("LOCALAPPDATA") {
            dirs.push(base.join("Volta").join("bin"));
        }
        if let Some(home) = &home {
            dirs.push(home.join("scoop").join("shims"));
            dirs.push(home.join(".bun").join("bin"));
        }
        // nvm-windows' link, and the active fnm shell's folder.
        dirs.extend(var("NVM_SYMLINK"));
        dirs.extend(var("FNM_MULTISHELL_PATH"));
        if let Some(base) = var("ProgramFiles") {
            dirs.push(base.join("nodejs"));
        }
        return dirs;
    }
    if let Some(home) = &home {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".npm-global").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(
            var("VOLTA_HOME")
                .unwrap_or_else(|| home.join(".volta"))
                .join("bin"),
        );
        let nvm = var("NVM_DIR").unwrap_or_else(|| home.join(".nvm"));
        dirs.extend(newest_first(&nvm.join("versions").join("node"), &["bin"]));
        let fnm = var("FNM_DIR").into_iter().chain([
            home.join(".local").join("share").join("fnm"),
            home.join("Library").join("Application Support").join("fnm"),
        ]);
        for fnm in fnm {
            dirs.extend(newest_first(
                &fnm.join("node-versions"),
                &["installation", "bin"],
            ));
        }
    }
    dirs.extend(var("FNM_MULTISHELL_PATH").map(|path| path.join("bin")));
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs
}

/// `<root>/v24.1.0/<below>` for each version folder, highest version first.
fn newest_first(root: &Path, below: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let numbers = name
                .strip_prefix('v')
                .unwrap_or(&name)
                .split('.')
                .map(|part| part.parse().ok())
                .collect::<Option<Vec<u64>>>()?;
            let dir = below.iter().fold(entry.path(), |dir, part| dir.join(part));
            Some((numbers, dir))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, dir)| dir).collect()
}

/// Only a program named like the spec's own is ever run from a configured path, so a setting
/// cannot turn into "run this other program". The npm entry file is the one exception: a
/// resolved link ends there.
pub fn accepts_name(path: &Path, spec: &CliSpec, windows: bool) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let named = if windows {
        bin_names(spec.program, true)
            .into_iter()
            .chain([spec.program.to_string()])
            .any(|allowed| name.eq_ignore_ascii_case(&allowed))
    } else {
        name == spec.program
    };
    named || is_npm_entry(path, spec)
}

/// `.../node_modules/<package>/.../<entry>`, with the package's own folders in that order.
fn is_npm_entry(path: &Path, spec: &CliSpec) -> bool {
    let Some((package, entry)) = spec.npm_entry else {
        return false;
    };
    if path.file_name() != Some(OsStr::new(entry)) {
        return false;
    }
    let wanted: Vec<&str> = std::iter::once("node_modules")
        .chain(package.split('/'))
        .collect();
    let folders: Vec<_> = path
        .parent()
        .map(|parent| parent.components().map(|c| c.as_os_str()).collect())
        .unwrap_or_default();
    folders.windows(wanted.len()).any(|run| {
        run.iter()
            .zip(&wanted)
            .all(|(have, want)| *have == OsStr::new(want))
    })
}

/// Where the `node` beside a resolved npm entry lives: `<prefix>/bin` for a Unix layout
/// (`<prefix>/lib/node_modules`), the folder holding `node_modules` on Windows.
fn node_dir(entry: &Path, spec: &CliSpec) -> Option<PathBuf> {
    if !is_npm_entry(entry, spec) {
        return None;
    }
    let modules = entry
        .ancestors()
        .find(|dir| dir.file_name() == Some(OsStr::new("node_modules")))?;
    let parent = modules.parent()?;
    match parent.file_name() {
        Some(name) if name == "lib" => Some(parent.parent()?.join("bin")),
        _ => Some(parent.to_path_buf()),
    }
}

/// `PATH` for the child: the program's own folder first, so a script with `#!/usr/bin/env
/// node` finds the Node that installed it. A resolved npm entry has no sibling folder worth
/// that, so its Node's folder leads instead.
pub fn child_path(bin: &Path, spec: &CliSpec, inherited: &OsStr) -> OsString {
    let first = node_dir(bin, spec).or_else(|| bin.parent().map(Path::to_path_buf));
    std::env::join_paths(first.into_iter().chain(std::env::split_paths(inherited)))
        .unwrap_or_else(|_| inherited.to_os_string())
}

pub fn not_found(spec: &CliSpec) -> String {
    format!(
        "{program} was not found. Install it, or choose its full path in the AI screen, or \
         with `jobpilot ai cli-path` (setting {setting}) in an ordinary terminal.",
        program = spec.program,
        setting = spec.setting,
    )
}

fn checked(bin: PathBuf, spec: &CliSpec, source: &str) -> std::result::Result<PathBuf, String> {
    if bin.is_absolute() && accepts_name(&bin, spec, cfg!(windows)) && is_program(&bin) {
        Ok(bin)
    } else {
        Err(format!(
            "{source} points to {}, which is not an executable named {}. {}",
            bin.display(),
            spec.program,
            not_found(spec)
        ))
    }
}

/// Order: the spec's override variable, its setting, `PATH`, the package managers' folders,
/// then one lookup through the owner's login shell. A setting that names something else is an
/// error, never a silent fall through. One that is well formed and whose file is gone is
/// skipped: an update removes what it named, and discovery is what an unset path gets.
pub async fn find_cli(spec: &CliSpec, db: Option<&Db>) -> std::result::Result<PathBuf, String> {
    if let Some(bin) = std::env::var_os(spec.env_bin).filter(|bin| !bin.is_empty()) {
        return checked(PathBuf::from(bin), spec, spec.env_bin);
    }
    if let Some(bin) = path_setting(spec, db).await.map(PathBuf::from)
        && !is_gone(&bin, spec)
    {
        return checked(bin, spec, spec.setting);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let env = |name: &str| std::env::var_os(name);
    let dirs = absolute_dirs(&path)
        .into_iter()
        .chain(cli_dirs(home_dir(), &env, cfg!(windows)));
    if let Some(found) = search(dirs, &bin_names(spec.program, cfg!(windows))) {
        return Ok(found);
    }
    #[cfg(unix)]
    if let Some(found) = login_shell_lookup(
        &std::env::var_os("SHELL").unwrap_or_default(),
        spec.program,
        &|path| accepts_name(path, spec, false),
    )
    .await
    {
        return Ok(found);
    }
    Err(not_found(spec))
}

fn is_gone(bin: &Path, spec: &CliSpec) -> bool {
    bin.is_absolute()
        && accepts_name(bin, spec, cfg!(windows))
        && matches!(bin.try_exists(), Ok(false))
}

/// A failure to read the setting just means discovery continues without it.
async fn path_setting(spec: &CliSpec, db: Option<&Db>) -> Option<String> {
    let value = settings::get(db?, spec.setting).await.ok()??;
    let path = value.as_str()?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Stores the path a CLI provider runs from. An empty path clears it. Only an absolute path
/// to a program named like the provider's own is stored.
pub async fn set_cli_path(db: &Db, name: &str, path: &str) -> Result<()> {
    let spec = spec_for(name)
        .ok_or_else(|| Error::Validation("a program path is for gemini_cli or codex_cli".into()))?;
    let path = path.trim();
    let given = Path::new(path);
    if !path.is_empty() && !(given.is_absolute() && accepts_name(given, spec, cfg!(windows))) {
        return Err(Error::Validation(format!(
            "give the full path to a program named {}",
            spec.program
        )));
    }
    settings::set(db, spec.setting, &serde_json::json!(path)).await
}
