//! The official `gemini` CLI on its own sign-in (M-32): nothing here reads `~/.gemini`, and
//! the prompt goes in on stdin. Job text is untrusted, so every attempt runs with every tool
//! denied, in an empty folder, with the owner's hooks off.
//!
//! Flags, the policy, and the two variables are the plan's no-tools evidence, re-read on
//! 2026-11-02 from the CLI's docs and its 0.40.0 bundle. The stream's shape, exit 41, and the
//! error classes come from the bundle, so the ignored smoke test is what re-checks them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use serde_json::Value;

use super::child::{self, ChildRun, CliSpec};
use super::validate::MAX_OUTPUT_BYTES;
use super::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Prompt, Provider,
    ProviderId,
};
use crate::db::Db;
use crate::error::{Error, Result};

pub const SPEC: &CliSpec = &child::GEMINI;
/// The oldest version the no-tools evidence was read from.
pub const MIN_VERSION: (u32, u32, u32) = (0, 40, 0);
/// The last version the evidence was read from. Past it the row says so, and nothing stops.
pub const LAST_CHECKED: (u32, u32, u32) = (0, 40, 0);
pub const DENY_ALL_POLICY: &str =
    "[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 999\n";

const POLICY_FILE: &str = "deny-all.toml";
const SYSTEM_FILE: &str = "system.md";
const SETTINGS_FILE: &str = "settings.json";
const WORK_DIR: &str = "work";
/// As the System Overrides file, which beats every other settings file. A sandbox would move
/// the whole prompt into a child's argv and `sh -c`, and Docker drops these policy files.
const SETTINGS_OVERRIDE: &str = r#"{"hooksConfig":{"enabled":false},"tools":{"sandbox":false}}"#;
/// Beats `tools.sandbox`. A value already set also beats any `.env` file the CLI loads.
const SANDBOX_VAR: &str = "GEMINI_SANDBOX";
const CLI_TOO_OLD: &str = "cli_too_old";
/// `FatalAuthenticationError`, read from the 0.40.0 bundle.
const EXIT_NOT_SIGNED_IN: i32 = 41;
const STDERR_LIMIT: u64 = 64 * 1024;
/// Lower-case words that name why a run failed. The text they are found in goes nowhere: only
/// the code does. Google's account errors are unverified, so their copy stays a hint.
const ACCOUNT_WORDS: &[&str] = &[
    "ineligibletiererror",
    "projectidrequirederror",
    "validationrequirederror",
];
const RATE_WORDS: &[&str] = &["retryablequotaerror", "rate limit"];
const QUOTA_WORDS: &[&str] = &["quota", "resource_exhausted"];
const MODEL_WORDS: &[&str] = &["modelnotfounderror"];
const AUTH_WORDS: &[&str] = &["unauthenticated", "unauthorized", "api key not valid"];

pub struct GeminiCli {
    bin: PathBuf,
}

impl GeminiCli {
    pub fn with_binary(bin: PathBuf) -> GeminiCli {
        GeminiCli { bin }
    }

    /// The shared discovery, so a GUI launch with a minimal `PATH` still finds `gemini`.
    pub async fn locate_in(db: &Db) -> Result<GeminiCli> {
        child::find_cli(SPEC, Some(db))
            .await
            .map(GeminiCli::with_binary)
            .map_err(|_| Error::Unavailable)
    }

    pub fn path(&self) -> &Path {
        &self.bin
    }

    /// The exact argument list, from "No-tools evidence". No `-p`: a run with no terminal is
    /// already headless, and the prompt stays out of argv. Both policy flags name one file,
    /// because a system-wide admin policy makes the CLI ignore `--admin-policy`.
    pub fn argv(dir: &Path) -> Vec<OsString> {
        let policy = dir.join(POLICY_FILE).into_os_string();
        let word = OsString::from;
        vec![
            word("--output-format"),
            word("stream-json"),
            word("--approval-mode"),
            word("default"),
            word("--policy"),
            policy.clone(),
            word("--admin-policy"),
            policy,
            word("--extensions"),
            word("none"),
            word("--allowed-mcp-server-names"),
            word("jobpilot-none"),
            word("--skip-trust"),
        ]
    }

    fn env(&self) -> Vec<(String, String)> {
        child::program_env(&self.bin, SPEC)
    }

    async fn run(&self, prompt: &Prompt) -> Result<String> {
        if opens_a_command(&prompt.user) {
            return Err(Error::Failed("cli_error".into()));
        }
        let mut env = self.env();
        refuse_sandbox(&env)?;
        // Dropped last, once the child is gone: Windows keeps a live cwd in use.
        let dir = tempfile::tempdir()?;
        let (system, stdin) = wire_text(prompt);
        write_attempt(dir.path(), &system)?;
        env.push((SANDBOX_VAR.into(), "false".into()));
        env.push(path_var("GEMINI_SYSTEM_MD", &dir.path().join(SYSTEM_FILE)));
        env.push(path_var(
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
            &dir.path().join(SETTINGS_FILE),
        ));
        let run = ChildRun {
            bin: &self.bin,
            args: GeminiCli::argv(dir.path()),
            env,
            cwd: &dir.path().join(WORK_DIR),
            stdin: stdin.as_bytes(),
            // The stream's framing sits around the reply.
            limit: 4 * MAX_OUTPUT_BYTES as u64,
            stderr_limit: STDERR_LIMIT,
        };
        let output = child::run_piped(run).await?;
        read_stream(&output.stdout, &output.stderr, output.status)
    }

    /// `gemini --version` against the floor: `Ok` says whether it is newer than checked, `Err`
    /// is the word for `Availability::Unavailable`.
    pub(crate) async fn check(&self) -> std::result::Result<bool, String> {
        let text = child::version_of(&self.bin, self.env()).await?;
        match parse_version(&text) {
            Some(version) if meets_floor(version) => Ok(version.0 > LAST_CHECKED),
            _ => Err(CLI_TOO_OLD.into()),
        }
    }
}

/// The system file's text and the stdin text, exactly as a run sends them: the consent
/// preview shows these, so it cannot differ.
pub fn wire_text(prompt: &Prompt) -> (String, String) {
    let system = format!(
        "{}\n\nReply with one JSON object and nothing else. It must match this JSON Schema:\n{}",
        prompt.system, prompt.schema
    );
    (system, stdin_text(&prompt.user))
}

/// The CLI runs a stdin text that opens with `/` as a command, and the owner's own commands
/// can hold shell. Every prompt opens with fixed text, which a test keeps true; this keeps a
/// new one from arriving unnoticed.
fn opens_a_command(user: &str) -> bool {
    user.trim_start().starts_with('/')
}

fn refuse_sandbox(env: &[(String, String)]) -> Result<()> {
    if env.iter().any(|(name, _)| name == SANDBOX_VAR) {
        return Err(Error::Failed("cli_error".into()));
    }
    Ok(())
}

fn path_var(name: &str, path: &Path) -> (String, String) {
    (name.into(), path.to_string_lossy().into_owned())
}

/// In headless mode the CLI reads any `@path` in its stdin text into the prompt. That read is
/// its own: it passes no policy check and no `tool_use` event, so neither the deny rule nor
/// the guard sees it. `\@` is the CLI's literal form, at the cost of the model seeing it.
// ponytail: this leans on `(?<!\\)@` in the 0.40.0 bundle and on main, and the reach without
// it is the empty cwd plus the owner's `context.includeDirectories`. The smoke test re-checks.
fn stdin_text(user: &str) -> String {
    user.replace('@', "\\@")
}

/// The three files of one attempt and its empty cwd, so the model sees nothing even if a tool
/// slipped through. The CLI splits `--policy` on commas and only warns about a path it cannot
/// read, so a comma here would run with no rule at all: that fails closed instead, as does a
/// folder the child's text variables cannot name.
fn write_attempt(dir: &Path, system: &str) -> Result<()> {
    if dir.to_str().is_none_or(|text| text.contains(',')) {
        return Err(Error::Failed("cli_error".into()));
    }
    std::fs::write(dir.join(POLICY_FILE), DENY_ALL_POLICY)?;
    std::fs::write(dir.join(SYSTEM_FILE), system)?;
    std::fs::write(dir.join(SETTINGS_FILE), SETTINGS_OVERRIDE)?;
    std::fs::create_dir(dir.join(WORK_DIR))?;
    Ok(())
}

/// `x.y.z` and whether a pre-release tail follows, with a leading `v`, build metadata, and any
/// trailing words dropped.
fn parse_version(text: &str) -> Option<((u32, u32, u32), bool)> {
    let word = text.split_whitespace().next()?;
    let word = word.strip_prefix('v').unwrap_or(word);
    let word = word.split('+').next()?;
    let (core, tail) = word.split_once('-').unwrap_or((word, ""));
    let mut parts = core.split('.');
    let mut number = || parts.next()?.parse().ok();
    let version = (number()?, number()?, number()?);
    parts
        .next()
        .is_none()
        .then_some((version, !tail.is_empty()))
}

/// A pre-release of a version comes before it, so `0.40.0-nightly` is below a 0.40.0 floor.
fn meets_floor((version, prerelease): ((u32, u32, u32), bool)) -> bool {
    version > MIN_VERSION || (version == MIN_VERSION && !prerelease)
}

impl Provider for GeminiCli {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::GeminiCli,
            capabilities: vec![Capability::StructuredOutput, Capability::NoTools],
            local: false,
            context_limit: 200_000,
            timeout: Duration::from_secs(180),
            sign_in: format!("gemini cli on its own sign-in ({})", self.bin.display()),
        }
    }

    /// `gemini --version`. Sign-in is not probed: the first real call reports it.
    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async move {
            match self.check().await {
                Ok(_) => Availability::Available,
                Err(why) => Availability::Unavailable(why),
            }
        })
    }

    /// Cancel and timeout are the router's: it drops this future, and `KillTree` does the rest.
    fn complete<'a>(
        &'a self,
        prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.run(prompt))
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        _inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
        Box::pin(async { Err(Error::Refused("no_embeddings".into())) })
    }
}

/// A line that parses as an event the stream is documented to carry, in the shape this code
/// reads. Anything else could be a tool event this code cannot see, so it is not skipped.
fn known_event(line: &[u8]) -> Option<Value> {
    let event: Value = serde_json::from_slice(line).ok()?;
    let fits = match event["type"].as_str()? {
        "init" | "tool_use" | "tool_result" | "error" => true,
        "message" => {
            matches!(event["role"].as_str(), Some("user" | "assistant"))
                && event["content"].is_string()
        }
        "result" => event["status"].is_string(),
        _ => false,
    };
    fits.then_some(event)
}

/// The fixed code for a failure's words, which go no further than this.
fn failure(words: &str) -> Error {
    let words = words.to_lowercase();
    let says = |list: &[&str]| list.iter().any(|word| words.contains(word));
    if says(ACCOUNT_WORDS) {
        Error::Refused("cli_account_refused".into())
    } else if says(RATE_WORDS) {
        Error::RateLimited { retry_after: None }
    } else if says(QUOTA_WORDS) {
        Error::Refused("quota_exhausted".into())
    } else if says(MODEL_WORDS) {
        Error::Refused("model_not_found".into())
    } else if says(AUTH_WORDS) {
        Error::Refused("auth_failed".into())
    } else {
        Error::Failed("cli_error".into())
    }
}

/// An `error` event, warnings included (a loop warning ends the run), or a failed `result`.
fn failed_event(event: &Value) -> Option<String> {
    let failed =
        event["type"] == "error" || (event["type"] == "result" && event["status"] != "success");
    failed.then(|| {
        [
            &event["message"],
            &event["error"]["type"],
            &event["error"]["message"],
        ]
        .map(|word| word.as_str().unwrap_or_default())
        .join(" ")
    })
}

/// Allow-list: the whole stream is read before anything is decided, so a tool event anywhere
/// outranks the exit status and a clean end, and a line that is not a known event fails the
/// attempt. Only a line's own `type` counts: a posting can quote one.
// ponytail: only a child-reported event can trip the guard, and the deny rule is what stops a
// tool. A CLI that ran one and did not say so passes here.
pub(crate) fn read_stream(stdout: &[u8], stderr: &[u8], status: ExitStatus) -> Result<String> {
    let lines: Vec<Option<Value>> = stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.trim_ascii().is_empty())
        .map(known_event)
        .collect();
    let events = || lines.iter().flatten();
    let is_tool = |event: &&Value| event["type"] == "tool_use" || event["type"] == "tool_result";
    if events().any(|event| is_tool(&event)) {
        return Err(Error::Refused("permission_denied".into()));
    }
    if status.code() == Some(EXIT_NOT_SIGNED_IN) {
        return Err(Error::Refused("not_logged_in".into()));
    }
    if let Some(words) = events().find_map(failed_event) {
        return Err(failure(&words));
    }
    if !status.success() {
        return Err(failure(&String::from_utf8_lossy(stderr)));
    }
    let bad_reply = || Error::Failed("bad_reply".into());
    if lines.iter().any(Option::is_none) || !events().any(|event| event["type"] == "result") {
        return Err(bad_reply());
    }
    let text: String = events()
        .filter(|event| event["type"] == "message" && event["role"] == "assistant")
        .filter_map(|event| event["content"].as_str())
        .collect();
    if text.trim().is_empty() {
        return Err(bad_reply());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comma_in_the_attempt_folder_fails_closed_before_anything_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a,b");
        std::fs::create_dir(&dir).unwrap();
        let error = write_attempt(&dir, "s").unwrap_err();
        assert!(matches!(&error, Error::Failed(why) if why == "cli_error"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn a_sandbox_variable_in_the_childs_environment_fails_closed() {
        let env = [("GEMINI_SANDBOX".to_string(), "docker".to_string())];
        let error = refuse_sandbox(&env).unwrap_err();
        assert!(matches!(&error, Error::Failed(why) if why == "cli_error"));
        assert!(refuse_sandbox(&[("HOME".to_string(), "/h".to_string())]).is_ok());
    }
}
