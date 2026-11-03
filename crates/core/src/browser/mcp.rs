//! Newline-delimited JSON-RPC over a child's stdio: the client that drives Playwright MCP, and
//! the helpers a fake server uses to answer. One request is in flight at a time.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::ai::CancellationToken;
use crate::ai::child::{KillTree, command};
use crate::error::{Error, Result};

pub const PROTOCOL: &str = "2025-06-18";
pub const MAX_LINE: usize = 8 * 1024 * 1024;

const START_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct ToolReply {
    pub text: String,
    pub is_error: bool,
    pub raw: Value,
}

// Field order matters: the tree is killed first, then the child handle drops.
pub struct McpClient {
    tree: Option<KillTree>,
    stdin: ChildStdin,
    lines: BufReader<ChildStdout>,
    next_id: u64,
    broken: bool,
    child: Child,
}

fn gone() -> Error {
    Error::Failed("browser_gone".into())
}

/// One line without its newline, or `None` at the end of the stream. A line past `MAX_LINE`
/// is refused before it is held whole.
async fn read_capped(lines: &mut BufReader<ChildStdout>) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let chunk = lines.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(None);
        }
        let (take, done) = match chunk.iter().position(|byte| *byte == b'\n') {
            Some(at) => (at + 1, true),
            None => (chunk.len(), false),
        };
        line.extend_from_slice(&chunk[..take]);
        lines.consume(take);
        if line.len() > MAX_LINE + 1 {
            return Err(Error::TooLarge {
                limit: MAX_LINE as u64,
            });
        }
        if done {
            line.pop();
            return Ok(Some(line));
        }
    }
}

impl McpClient {
    pub async fn start(
        program: &Path,
        args: &[OsString],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<McpClient> {
        let mut spawn = command(program, env, cwd);
        spawn
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = spawn.spawn()?;
        let tree = KillTree::of(&child);
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Error::Failed("no_pipes".into()));
        };
        let mut client = McpClient {
            tree: Some(tree),
            stdin,
            lines: BufReader::new(stdout),
            next_id: 1,
            broken: false,
            child,
        };
        let hello = json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "jobpilot", "version": env!("CARGO_PKG_VERSION") },
        });
        let never = CancellationToken::new();
        let begun = client.request("initialize", hello, &never);
        match tokio::time::timeout(START_TIMEOUT, begun).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(Error::Failed("browser_start_timeout".into())),
        }
        client
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        Ok(client)
    }

    pub async fn tools(&mut self) -> Result<Vec<Value>> {
        let result = self
            .request("tools/list", json!({}), &CancellationToken::new())
            .await?;
        Ok(result["tools"].as_array().cloned().unwrap_or_default())
    }

    pub async fn call(
        &mut self,
        name: &str,
        args: Value,
        cancel: &CancellationToken,
    ) -> Result<ToolReply> {
        let raw = self
            .request(
                "tools/call",
                json!({"name": name, "arguments": args}),
                cancel,
            )
            .await?;
        let text = raw["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let is_error = raw["isError"].as_bool().unwrap_or(false);
        Ok(ToolReply {
            text,
            is_error,
            raw,
        })
    }

    /// Ends the server and what it started.
    pub async fn close(mut self) {
        drop(self.tree.take());
        let _ = self.child.kill().await;
    }

    async fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = message.to_string();
        line.push('\n');
        if self.stdin.write_all(line.as_bytes()).await.is_err() || self.stdin.flush().await.is_err()
        {
            self.broken = true;
            return Err(gone());
        }
        Ok(())
    }

    /// The answer to one request. A cancel, a refused line or a lost server leaves an answer
    /// unread, so the client is finished after any of them.
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        if self.broken {
            return Err(gone());
        }
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        let outcome = self.answer(id, cancel).await;
        if outcome
            .as_ref()
            .is_err_and(|error| !matches!(error, Error::Failed(why) if why == "mcp_error"))
        {
            self.broken = true;
        }
        outcome
    }

    async fn answer(&mut self, id: u64, cancel: &CancellationToken) -> Result<Value> {
        loop {
            let line = tokio::select! {
                line = read_capped(&mut self.lines) => line?,
                () = wait_cancelled(cancel) => return Err(Error::Cancelled),
            };
            let Some(line) = line else {
                return Err(gone());
            };
            let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if message.get("id") != Some(&json!(id)) || message.get("method").is_some() {
                continue;
            }
            if message.get("error").is_some() {
                return Err(Error::Failed("mcp_error".into()));
            }
            return Ok(message["result"].clone());
        }
    }
}

async fn wait_cancelled(cancel: &CancellationToken) {
    while !cancel.is_cancelled() {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

#[derive(Debug)]
pub enum Incoming {
    Initialize {
        id: Value,
    },
    ToolsList {
        id: Value,
    },
    ToolCall {
        id: Value,
        name: String,
        args: Value,
    },
    Notification,
    Other {
        id: Value,
    },
}

/// Sorts one line a client sent. Ids stay as they came, number or string.
pub fn read_message(line: &str) -> Result<Incoming> {
    let bad = || Error::Failed("bad_message".into());
    let message: Value = serde_json::from_str(line).map_err(|_| bad())?;
    let method = message["method"].as_str().ok_or_else(bad)?;
    let Some(id) = message.get("id").cloned() else {
        return Ok(Incoming::Notification);
    };
    Ok(match method {
        "initialize" => Incoming::Initialize { id },
        "tools/list" => Incoming::ToolsList { id },
        "tools/call" => Incoming::ToolCall {
            id,
            name: message["params"]["name"]
                .as_str()
                .ok_or_else(bad)?
                .to_string(),
            args: message["params"]
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({})),
        },
        _ => Incoming::Other { id },
    })
}

pub fn reply(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub fn error_reply(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

pub fn tool_text(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}
