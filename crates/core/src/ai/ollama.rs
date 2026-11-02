//! Ollama on this machine. The address must be loopback, and a model counts as local only
//! when Ollama itself says it runs here: cloud models answer on the same address.
//! Both the drafting and the embedding model are checked, because resume text goes to both.

use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};

use super::net::{loopback_client, net, retry_after};
use super::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Prompt, Provider,
    ProviderId,
};
use crate::audit;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::hardware::{self, Hardware};
use crate::pause;

pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";

/// Sent as `num_ctx`, so this is the window a call really gets, up to the model's own maximum.
pub const CONTEXT_LIMIT: usize = 8192;
const CANCEL_POLL: Duration = Duration::from_millis(100);
const QUICK: Duration = Duration::from_secs(3);
/// A pull progress line is a few hundred bytes. A longer one is not Ollama talking.
const MAX_LINE: usize = 1 << 20;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Model {
    pub name: String,
    pub size: u64,
    pub local: bool,
    pub family: String,
    pub parameter_size: String,
    pub fits: Fit,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    Fits,
    MaySlow,
    TooLarge,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OllamaStatus {
    Running {
        url: String,
        version: String,
        models: Vec<Model>,
    },
    NotRunning {
        url: String,
    },
    Refused {
        reason: String,
    },
}

pub struct Ollama {
    url: String,
    drafting_model: Option<String>,
    embedding_model: Option<String>,
    client: Client,
}

impl Ollama {
    /// Refuses any host but 127.0.0.1, ::1, or localhost (M-21), and any scheme but http.
    pub fn new(
        url: &str,
        drafting_model: Option<String>,
        embedding_model: Option<String>,
    ) -> Result<Ollama> {
        let (parsed, client) = loopback_client(url)?;
        Ok(Ollama {
            url: parsed.origin().ascii_serialization(),
            drafting_model: drafting_model.filter(|m| !m.is_empty()),
            embedding_model: embedding_model.filter(|m| !m.is_empty()),
            client,
        })
    }

    /// GET /api/version, then /api/tags. Remote models are dropped from the list and
    /// counted in one log line.
    pub async fn status(&self) -> OllamaStatus {
        let not_running = || OllamaStatus::NotRunning {
            url: self.url.clone(),
        };
        let Ok(version) = self.get_json("/api/version").await else {
            return not_running();
        };
        let Ok(rows) = self.tags().await else {
            return not_running();
        };
        let hw = hardware::detect();
        let (local, remote): (Vec<_>, Vec<_>) = rows.iter().partition(|row| Ollama::is_local(row));
        if !remote.is_empty() {
            eprintln!("ollama: {} remote model(s) hidden", remote.len());
        }
        OllamaStatus::Running {
            url: self.url.clone(),
            version: version["version"].as_str().unwrap_or_default().to_string(),
            models: local.into_iter().map(|row| model(row, &hw)).collect(),
        }
    }

    /// Downloads a model. Started only by the user (FR-3.4); the audit row comes first, so a
    /// download that cannot be recorded does not start. `progress` gets (completed, total).
    pub async fn pull(
        &self,
        db: &Db,
        actor: &str,
        model: &str,
        mut progress: impl FnMut(u64, u64),
        cancel: CancellationToken,
    ) -> Result<()> {
        if !name_is_local(model) {
            return Err(Error::Refused("not_local".into()));
        }
        pause::ensure_off(db).await?;
        audit::record(
            db,
            actor,
            "model_download_started",
            Some(model),
            json!({"model": model}),
        )
        .await?;

        let request = self
            .client
            .post(self.endpoint("/api/pull"))
            .json(&json!({"model": model, "stream": true}));
        let mut response = cancellable(request.send(), &cancel).await?.map_err(net)?;
        if !response.status().is_success() {
            return Err(Error::Failed(format!(
                "http_{}",
                response.status().as_u16()
            )));
        }

        let mut buffer: Vec<u8> = Vec::new();
        loop {
            let chunk = cancellable(response.chunk(), &cancel).await?.map_err(net)?;
            let ended = chunk.is_none();
            buffer.extend(chunk.unwrap_or_default());
            while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                if pull_line(&line, &mut progress)? {
                    return Ok(());
                }
            }
            if buffer.len() > MAX_LINE {
                return Err(Error::Failed("pull_reply_too_long".into()));
            }
            if ended {
                if pull_line(&std::mem::take(&mut buffer), &mut progress)? {
                    return Ok(());
                }
                return Err(Error::Failed("pull_incomplete".into()));
            }
        }
    }

    /// The settled rule (docs/ai-providers.md, Ollama local-model detection): local only when
    /// no `remote_*` field holds a value, `size > 0`, and the name is not a cloud name.
    pub fn is_local(entry: &Value) -> bool {
        let Some(row) = entry.as_object() else {
            return false;
        };
        let named = row
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(name_is_local);
        let sized = row.get("size").and_then(Value::as_u64).unwrap_or(0) > 0;
        let owned = row
            .iter()
            .filter(|(key, _)| key.to_ascii_lowercase().starts_with("remote_"))
            .all(|(_, value)| value.is_null() || value.as_str() == Some(""));
        named && sized && owned
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let request = self.client.get(self.endpoint(path)).timeout(QUICK);
        let response = request.send().await.map_err(net)?;
        read_json(response).await
    }

    async fn tags(&self) -> Result<Vec<Value>> {
        let body = self.get_json("/api/tags").await?;
        Ok(body["models"].as_array().cloned().unwrap_or_default())
    }

    /// The model must be installed and local, by Ollama's own listing. Checked before every
    /// call that sends text, since a model can change between the probe and the call.
    async fn ensure_local(&self, name: &str) -> Result<()> {
        if !name_is_local(name) {
            return Err(Error::Refused("not_local".into()));
        }
        match find(&self.tags().await?, name) {
            None => Err(Error::Refused("model_not_found".into())),
            Some(row) if !Ollama::is_local(row) => Err(Error::Refused("not_local".into())),
            Some(_) => Ok(()),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .client
            .post(self.endpoint(path))
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(Error::Refused("model_not_found".into()));
        }
        if matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            let header = response.headers().get(reqwest::header::RETRY_AFTER);
            let retry_after = retry_after(header.and_then(|value| value.to_str().ok()));
            return Err(Error::RateLimited { retry_after });
        }
        if !response.status().is_success() {
            let status = response.status().as_u16();
            // Ollama's two wordings of an overflow both say "context length".
            let body = response.text().await.map_err(net)?;
            return Err(if body.contains("context length") {
                Error::Refused("prompt_too_large".into())
            } else {
                Error::Failed(format!("http_{status}"))
            });
        }
        read_json(response).await
    }
}

impl Provider for Ollama {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::Ollama,
            capabilities: vec![
                Capability::StructuredOutput,
                Capability::Embeddings,
                Capability::LocalExecution,
                Capability::Offline,
                Capability::NoTools,
            ],
            local: true,
            context_limit: CONTEXT_LIMIT,
            timeout: Duration::from_secs(300),
            sign_in: "none".into(),
        }
    }

    /// Routing check 2: every configured model, drafting and embedding, must be local. A
    /// cloud embedding model stops drafting too, so no setting can send text out unseen.
    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async move {
            let unavailable = |why: &str| Availability::Unavailable(why.into());
            let models: Vec<&str> = [&self.drafting_model, &self.embedding_model]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect();
            if models.is_empty() {
                return unavailable("no_model");
            }
            if !models.iter().all(|m| name_is_local(m)) {
                return unavailable("not_local");
            }
            let Ok(rows) = self.tags().await else {
                return unavailable("not_running");
            };
            for name in models {
                match find(&rows, name) {
                    None => return unavailable("model_missing"),
                    Some(row) if !Ollama::is_local(row) => return unavailable("not_local"),
                    Some(_) => {}
                }
            }
            Availability::Available
        })
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let name = self
                .drafting_model
                .as_deref()
                .ok_or_else(|| Error::Refused("no_model".into()))?;
            self.ensure_local(name).await?;
            let format: Value = serde_json::from_str(prompt.schema)
                .map_err(|_| Error::Validation("the task schema is not JSON".into()))?;
            let body = json!({
                "model": name,
                "messages": [
                    {"role": "system", "content": prompt.system},
                    {"role": "user", "content": prompt.user},
                ],
                "format": format,
                "stream": false,
                // Both default to true, and either would cut the prompt with a normal 200:
                // the rules and the block's markers can go first. A refusal is the safe end.
                "truncate": false,
                "shift": false,
                "options": {"temperature": 0, "num_ctx": CONTEXT_LIMIT},
            });
            let reply = self.post("/api/chat", body).await?;
            reply["message"]["content"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::Failed("bad_reply".into()))
        })
    }

    fn embed<'a>(
        &'a self,
        model: &'a str,
        inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
        Box::pin(async move {
            // The model named here, not the one this provider was built with: the router
            // takes it from settings, and what is sent must be checked against what it is.
            self.ensure_local(model).await?;
            let reply = self
                .post("/api/embed", json!({"model": model, "input": inputs}))
                .await?;
            let bad = || Error::Failed("bad_reply".into());
            let rows = reply["embeddings"].as_array().ok_or_else(bad)?;
            rows.iter()
                .map(|row| {
                    let numbers = row.as_array().ok_or_else(bad)?;
                    // Narrowing to f32 is the stored format.
                    let floats = numbers.iter().map(|n| n.as_f64().map(|f| f as f32));
                    floats.collect::<Option<Vec<f32>>>().ok_or_else(bad)
                })
                .collect()
        })
    }
}

/// A cloud model is named `<name>:cloud` (docs.ollama.com/cloud); `-cloud` covers variants.
fn name_is_local(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    !name.is_empty() && !name.ends_with(":cloud") && !name.contains("-cloud")
}

/// `gemma3` means `gemma3:latest`, as in Ollama.
fn find<'a>(rows: &'a [Value], wanted: &str) -> Option<&'a Value> {
    let tagged = if wanted.contains(':') {
        wanted.to_string()
    } else {
        format!("{wanted}:latest")
    };
    rows.iter()
        .find(|row| matches!(row["name"].as_str(), Some(n) if n == wanted || n == tagged))
}

fn model(row: &Value, hw: &Hardware) -> Model {
    let size = row["size"].as_u64().unwrap_or(0);
    let text = |key: &str| row["details"][key].as_str().unwrap_or_default().to_string();
    Model {
        name: row["name"].as_str().unwrap_or_default().to_string(),
        size,
        local: true,
        family: text("family"),
        parameter_size: text("parameter_size"),
        fits: hardware::fit(size, hw),
    }
}

/// Returns true on the closing "success" line.
fn pull_line(line: &[u8], progress: &mut impl FnMut(u64, u64)) -> Result<bool> {
    let line = line.trim_ascii();
    if line.is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_slice(line).map_err(|_| Error::Failed("bad_reply".into()))?;
    if let Some(message) = value["error"].as_str() {
        return Err(Error::Failed(message.to_string()));
    }
    if let Some(total) = value["total"].as_u64().filter(|t| *t > 0) {
        progress(value["completed"].as_u64().unwrap_or(0), total);
    }
    Ok(value["status"].as_str() == Some("success"))
}

async fn read_json(response: Response) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Failed(format!("http_{}", status.as_u16())));
    }
    response.json().await.map_err(net)
}

/// Ends `work` within one poll of the cancel.
async fn cancellable<T>(work: impl Future<Output = T>, cancel: &CancellationToken) -> Result<T> {
    let cancelled = async {
        while !cancel.is_cancelled() {
            tokio::time::sleep(CANCEL_POLL).await;
        }
    };
    tokio::select! {
        biased;
        () = cancelled => Err(Error::Cancelled),
        value = work => Ok(value),
    }
}
