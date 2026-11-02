//! The official `gemini` CLI on its own sign-in (M-32): nothing here reads `~/.gemini`, and
//! the prompt goes in on stdin. Job text is untrusted, so every attempt runs with every tool
//! denied, in an empty folder, with the owner's hooks off.
//!
//! Flags, the policy, and the two variables are the plan's no-tools evidence, re-read on
//! 2026-11-02 from the CLI's docs and its 0.40.0 bundle. The stream's shape and exit 41 come
//! from the bundle, so the ignored smoke test is what re-checks them.

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
pub const DENY_ALL_POLICY: &str =
    "[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 999\n";

const POLICY_FILE: &str = "deny-all.toml";
const SYSTEM_FILE: &str = "system.md";
const SETTINGS_FILE: &str = "settings.json";
const WORK_DIR: &str = "work";
/// As the System Overrides file, which beats every other settings file.
const HOOKS_OFF: &str = r#"{"hooksConfig":{"enabled":false}}"#;
const CLI_TOO_OLD: &str = "cli_too_old";
/// `FatalAuthenticationError`, read from the 0.40.0 bundle.
const EXIT_NOT_SIGNED_IN: i32 = 41;

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

    /// `PATH` starts with the CLI's own folder, so its `#!/usr/bin/env node` works from a GUI.
    fn env(&self) -> Vec<(String, String)> {
        let mut env = child::child_env(SPEC.extra_env);
        env.retain(|(name, _)| name != "PATH");
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = child::child_path(&self.bin, SPEC, &inherited);
        env.push(("PATH".into(), path.to_string_lossy().into_owned()));
        env
    }

    async fn run(&self, prompt: &Prompt) -> Result<String> {
        // Dropped last, once the child is gone: Windows keeps a live cwd in use.
        let dir = tempfile::tempdir()?;
        write_attempt(dir.path(), prompt)?;
        let mut env = self.env();
        env.push(path_var("GEMINI_SYSTEM_MD", &dir.path().join(SYSTEM_FILE)));
        env.push(path_var(
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
            &dir.path().join(SETTINGS_FILE),
        ));
        let stdin = stdin_text(&prompt.user);
        let run = ChildRun {
            bin: &self.bin,
            args: GeminiCli::argv(dir.path()),
            env,
            cwd: &dir.path().join(WORK_DIR),
            stdin: stdin.as_bytes(),
            // The stream's framing sits around the reply.
            limit: 4 * MAX_OUTPUT_BYTES as u64,
        };
        let output = child::run_piped(run).await?;
        read_stream(&output.stdout, output.status)
    }
}

fn path_var(name: &str, path: &Path) -> (String, String) {
    (name.into(), path.to_string_lossy().into_owned())
}

/// In headless mode the CLI reads any `@path` in its stdin text into the prompt. That read is
/// its own: it passes no policy check and no `tool_use` event, so neither the deny rule nor
/// the guard sees it. `\@` is the CLI's literal form, at the cost of the model seeing it.
// ponytail: this leans on `(?<!\\)@` in the 0.40.0 bundle and on main, and the reach without
// it is the empty cwd plus the owner's `context.includeDirectories`. The smoke test re-checks.
// A `/` command needs no such step: every prompt opens with fixed text.
fn stdin_text(user: &str) -> String {
    user.replace('@', "\\@")
}

/// The three files of one attempt and its empty cwd, so the model sees nothing even if a tool
/// slipped through. The CLI splits `--policy` on commas and only warns about a path it cannot
/// read, so a comma here would run with no rule at all: that fails closed instead, as does a
/// folder the child's text variables cannot name.
fn write_attempt(dir: &Path, prompt: &Prompt) -> Result<()> {
    if dir.to_str().is_none_or(|text| text.contains(',')) {
        return Err(Error::Failed("cli_error".into()));
    }
    let system = format!(
        "{}\n\nReply with one JSON object and nothing else. It must match this JSON Schema:\n{}",
        prompt.system, prompt.schema
    );
    std::fs::write(dir.join(POLICY_FILE), DENY_ALL_POLICY)?;
    std::fs::write(dir.join(SYSTEM_FILE), system)?;
    std::fs::write(dir.join(SETTINGS_FILE), HOOKS_OFF)?;
    std::fs::create_dir(dir.join(WORK_DIR))?;
    Ok(())
}

/// `x.y.z`, with any pre-release tail and trailing words dropped.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let word = text.split_whitespace().next()?;
    let mut parts = word.split(['-', '+']).next()?.split('.');
    let mut number = || parts.next()?.parse().ok();
    let version = (number()?, number()?, number()?);
    parts.next().is_none().then_some(version)
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
            match child::version_of(&self.bin, self.env()).await {
                Err(why) => Availability::Unavailable(why.into()),
                Ok(text) if parse_version(&text).is_some_and(|found| found >= MIN_VERSION) => {
                    Availability::Available
                }
                Ok(_) => Availability::Unavailable(CLI_TOO_OLD.into()),
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

/// The whole stream is read before anything is decided, so a tool event anywhere outranks the
/// exit status and a clean end. Only a line's own `type` counts: a posting can quote one.
// ponytail: only a child-reported event can trip the guard, and the deny rule is what stops a
// tool. A CLI that ran one and did not say so passes here.
pub(crate) fn read_stream(stdout: &[u8], status: ExitStatus) -> Result<String> {
    let events: Vec<Value> = stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    let kind = |event: &Value| event["type"].as_str().map(str::to_owned);
    let saw = |wanted: &[&str]| {
        events
            .iter()
            .filter_map(kind)
            .any(|found| wanted.contains(&found.as_str()))
    };
    if saw(&["tool_use", "tool_result"]) {
        return Err(Error::Refused("permission_denied".into()));
    }
    if status.code() == Some(EXIT_NOT_SIGNED_IN) {
        return Err(Error::Refused("not_logged_in".into()));
    }
    let results = || events.iter().filter(|event| event["type"] == "result");
    if !status.success() || results().any(|event| event["status"] != "success") {
        return Err(Error::Failed("cli_error".into()));
    }
    if !saw(&["result"]) {
        return Err(Error::Failed("bad_reply".into()));
    }
    Ok(events
        .iter()
        .filter(|event| event["type"] == "message" && event["role"] == "assistant")
        .filter_map(|event| event["content"].as_str())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comma_in_the_attempt_folder_fails_closed_before_anything_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a,b");
        std::fs::create_dir(&dir).unwrap();
        let prompt = Prompt {
            system: "s".into(),
            user: "u".into(),
            schema: "{}",
        };
        let error = write_attempt(&dir, &prompt).unwrap_err();
        assert!(matches!(&error, Error::Failed(why) if why == "cli_error"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }
}
