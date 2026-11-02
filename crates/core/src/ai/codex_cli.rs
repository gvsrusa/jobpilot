//! The official `codex` CLI on its own sign-in (M-32): nothing here reads `~/.codex`, and the
//! prompt goes in on stdin. It ships inert. No documented switch proves it has no tools, so it
//! never declares `NoTools`, and the router gives it no task that carries untrusted text. The
//! argument list is the plan's "No-tools evidence", every documented tool switch off, in case
//! a task with no untrusted text ever qualifies.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::api::provider_schema;
use super::child::{self, ChildRun, CliSpec};
use super::validate::MAX_OUTPUT_BYTES;
use super::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Prompt, Provider,
    ProviderId,
};
use crate::db::Db;
use crate::error::{Error, Result};

pub const SPEC: &CliSpec = &child::CODEX;

const SCHEMA_FILE: &str = "schema.json";
const REPLY_FILE: &str = "reply.txt";
const WORK_DIR: &str = "work";

pub struct CodexCli {
    bin: PathBuf,
}

impl CodexCli {
    pub fn with_binary(bin: PathBuf) -> CodexCli {
        CodexCli { bin }
    }

    /// The shared discovery, so a GUI launch with a minimal `PATH` still finds `codex`.
    pub async fn locate_in(db: &Db) -> Result<CodexCli> {
        child::find_cli(SPEC, Some(db))
            .await
            .map(CodexCli::with_binary)
            .map_err(|_| Error::Unavailable)
    }

    pub fn path(&self) -> &Path {
        &self.bin
    }

    /// The exact argument list. The prompt is not in it: the trailing `-` reads stdin.
    pub fn argv(dir: &Path) -> Vec<OsString> {
        let word = OsString::from;
        vec![
            word("exec"),
            word("--ephemeral"),
            word("--skip-git-repo-check"),
            word("--ignore-user-config"),
            word("--ignore-rules"),
            word("--sandbox"),
            word("read-only"),
            word("--disable"),
            word("shell_tool"),
            word("--disable"),
            word("apps"),
            word("--disable"),
            word("multi_agent"),
            word("--disable"),
            word("hooks"),
            word("-c"),
            word("web_search=\"disabled\""),
            word("-c"),
            word("tools.view_image=false"),
            word("-c"),
            word("approval_policy=\"never\""),
            word("--output-schema"),
            dir.join(SCHEMA_FILE).into_os_string(),
            word("--output-last-message"),
            dir.join(REPLY_FILE).into_os_string(),
            word("-"),
        ]
    }

    async fn run(&self, prompt: &Prompt) -> Result<String> {
        // Dropped last, once the child is gone: Windows keeps a live cwd in use.
        let dir = tempfile::tempdir()?;
        let schema = provider_schema(prompt.schema)?;
        std::fs::write(dir.path().join(SCHEMA_FILE), schema.to_string())?;
        std::fs::create_dir(dir.path().join(WORK_DIR))?;
        let stdin = format!("{}\n\n{}", prompt.system, prompt.user);
        let run = ChildRun {
            bin: &self.bin,
            args: CodexCli::argv(dir.path()),
            env: child::program_env(&self.bin, SPEC),
            cwd: &dir.path().join(WORK_DIR),
            stdin: stdin.as_bytes(),
            limit: 4 * MAX_OUTPUT_BYTES as u64,
            stderr_limit: 0,
        };
        let output = child::run_piped(run).await?;
        if !output.status.success() {
            return Err(Error::Failed("cli_error".into()));
        }
        read_reply(&dir.path().join(REPLY_FILE))
    }
}

/// The reply file, up to the output cap. Missing, empty, or over the cap is a bad reply.
fn read_reply(path: &Path) -> Result<String> {
    let bad_reply = || Error::Failed("bad_reply".into());
    let file = std::fs::File::open(path).map_err(|_| bad_reply())?;
    let mut text = String::new();
    file.take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| bad_reply())?;
    if text.trim().is_empty() || text.len() > MAX_OUTPUT_BYTES {
        return Err(bad_reply());
    }
    Ok(text)
}

impl Provider for CodexCli {
    // ponytail: "no documented way to remove `apply_patch`", so no `NoTools`. When OpenAI
    // documents one, the flag and the capability are a small change here and in the register.
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::CodexCli,
            capabilities: vec![Capability::StructuredOutput],
            local: false,
            context_limit: 200_000,
            timeout: Duration::from_secs(180),
            sign_in: format!("codex cli on its own sign-in ({})", self.bin.display()),
        }
    }

    /// `codex --version`. Sign-in is not probed: the first real call reports it.
    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async move {
            match child::version_of(&self.bin, child::program_env(&self.bin, SPEC)).await {
                Ok(_) => Availability::Available,
                Err(why) => Availability::Unavailable(why.to_string()),
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
