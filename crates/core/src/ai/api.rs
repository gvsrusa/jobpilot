//! Providers reached by HTTP: OpenAI, OpenRouter, and a server the user runs on loopback on the
//! chat wire, Anthropic and Google on their own. Everything a vendor sends is untrusted: reply
//! text goes back to the router for its schema check, an error body is read only to match fixed
//! words, and no error holds a vendor's text. No request names a tool, so the model has none.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue, RETRY_AFTER};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};

use super::net::{
    MAX_ERROR_BODY, MAX_REPLY_BYTES, hosted_client, loopback_client, net, read_capped, retry_after,
};
use super::ollama::name_is_local;
use super::register::{self, ProviderInfo, ProviderKind};
use super::{
    Availability, BoxFuture, CancellationToken, Declaration, Prompt, Provider, text_setting,
};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::secrets::{self, Zeroizing};
use crate::settings;

const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The API requires a limit. The chat wire sends none.
const ANTHROPIC_MAX_TOKENS: u32 = 16_000;
const GOOGLE_BASE: &str = "https://generativelanguage.googleapis.com";
/// `SAFETY` and its kin: each ends a reply because a filter did, whatever text came before it.
const GOOGLE_REFUSED: [&str; 5] = [
    "SAFETY",
    "RECITATION",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
];
/// A tool or function call that ended the reply, even a malformed one.
const GOOGLE_TOOL_ENDS: [&str; 3] = [
    "MALFORMED_FUNCTION_CALL",
    "UNEXPECTED_TOOL_CALL",
    "TOO_MANY_TOOL_CALLS",
];
/// Parts only a tool call or its result can hold.
const GOOGLE_TOOL_PARTS: [&str; 6] = [
    "functionCall",
    "functionResponse",
    "executableCode",
    "codeExecutionResult",
    "toolCall",
    "toolResponse",
];
const ENDPOINT_URL: &str = "ai.endpoint.url";
const ENDPOINT_MODEL: &str = "ai.model.endpoint";
const ENDPOINT: &str = "endpoint";
/// The most digits in `/v<digits>`, far too few to hold a key.
const MAX_VERSION_DIGITS: usize = 2;
const MAX_MODEL: usize = 200;
/// An aggregator cannot widen a vendor's verdict, so only these vendors' models pass it.
const OPENROUTER_ORIGINS: [&str; 3] = ["openai/", "anthropic/", "google/"];
/// What may follow the `:`, and nothing else may. Both only rank the vendor's hosts, under the
/// same `data_collection` and `require_parameters`. `:free` is the free-tier row.
const OPENROUTER_SUFFIXES: [&str; 2] = ["nitro", "floor"];
/// OpenRouter's suffix that turns on web search. Named so the refusal can say why.
const OPENROUTER_SEARCH_SUFFIX: &str = "online";
/// These models search or drive a computer by themselves, with no `tools` field to leave out, so
/// `NoTools` would be false. `search` also catches `research`. The endpoint is exempt: its names
/// are the user's, and it has no such vendor model.
const TOOL_MODEL_WORDS: [&str; 2] = ["search", "computer-use"];
const QUOTA_WORDS: [&str; 3] = [
    "insufficient_quota",
    "credit_balance_exhausted",
    "usage_limit",
];
/// Anthropic's limit on a workspace's spend: the `error_code`, and the start of the message.
/// Sent as a 400, so unlike `QUOTA_WORDS` these count there too.
const SPEND_CAP_WORDS: [&str; 2] = [
    "enforced_spend_limit_reached",
    "you have reached your specified api usage limits",
];
/// Google answers a bad key with a 400, in `details[].reason`. Any other 400 stays `http_400`.
const GOOGLE_BAD_KEY_WORD: &str = "api_key_invalid";
const OVERFLOW_WORDS: [&str; 4] = [
    "context_length_exceeded",
    "maximum context length",
    "prompt is too long",
    "exceeds the maximum number of tokens",
];

/// Where a send gets its key. A provider name, for `Keychain`.
pub enum KeySource {
    Keychain(String),
    Fixed(Zeroizing<String>),
    None,
}

#[derive(Clone, Copy, PartialEq)]
enum Wire {
    OpenAi,
    OpenRouter,
    Endpoint,
    Anthropic,
    Google,
}

pub struct ApiProvider {
    wire: Wire,
    base: String,
    /// Empty when none is chosen.
    model: String,
    key: KeySource,
    /// From `secret_index`, so status never touches the keychain.
    key_stored: bool,
    client: Client,
    info: &'static ProviderInfo,
}

fn wire_of(name: &str) -> Option<Wire> {
    match name {
        "openai" => Some(Wire::OpenAi),
        "openrouter" => Some(Wire::OpenRouter),
        "endpoint" => Some(Wire::Endpoint),
        "anthropic" => Some(Wire::Anthropic),
        "google" => Some(Wire::Google),
        _ => None,
    }
}

impl ApiProvider {
    /// `None` is left out of routing, as a refused Ollama address is. Reads settings and
    /// `secret_index`. Opens no socket and reads no key.
    pub async fn from_settings(db: &Db, name: &str) -> Result<Option<ApiProvider>> {
        let (Some(wire), Some(info)) = (wire_of(name), register::info(name)) else {
            return Ok(None);
        };
        let (base, client) = match wire {
            Wire::OpenAi => (OPENAI_BASE.to_string(), hosted_client()?),
            Wire::OpenRouter => (OPENROUTER_BASE.to_string(), hosted_client()?),
            Wire::Anthropic => (ANTHROPIC_BASE.to_string(), hosted_client()?),
            Wire::Google => (GOOGLE_BASE.to_string(), hosted_client()?),
            Wire::Endpoint => match stored_endpoint(db).await? {
                Some(endpoint) => endpoint,
                None => return Ok(None),
            },
        };
        let model = text_setting(db, &format!("ai.model.{name}")).await?;
        let key_stored = secrets::names(db)
            .await?
            .contains(&secrets::api_key_name(name));
        let key = if wire == Wire::Endpoint && !key_stored {
            KeySource::None
        } else {
            KeySource::Keychain(name.to_string())
        };
        Ok(Some(ApiProvider {
            wire,
            base,
            model: model.unwrap_or_default(),
            key,
            key_stored,
            client,
            info,
        }))
    }

    /// Fake servers only: refuses any base that is not loopback.
    #[doc(hidden)]
    pub fn for_test(name: &str, base: &str, model: &str, key: KeySource) -> Result<ApiProvider> {
        let (Some(wire), Some(info)) = (wire_of(name), register::info(name)) else {
            return Err(Error::Validation("not an API provider".into()));
        };
        let (_, client) = loopback_client(base)?;
        Ok(ApiProvider {
            wire,
            base: base.trim_end_matches('/').to_string(),
            model: model.to_string(),
            key_stored: !matches!(key, KeySource::None),
            key,
            client,
            info,
        })
    }

    /// Read on each attempt and dropped with it. A missing entry reads as `no_key`, whatever
    /// `secret_index` said.
    async fn read_key(&self) -> Result<Option<Zeroizing<String>>> {
        let no_key = || Error::Refused("no_key".into());
        match &self.key {
            KeySource::Keychain(name) => {
                let key = secrets::read_api_key(name).await?;
                key.map(Some).ok_or_else(no_key)
            }
            KeySource::Fixed(key) => Ok(Some(key.clone())),
            KeySource::None if self.wire == Wire::Endpoint => Ok(None),
            KeySource::None => Err(no_key()),
        }
    }

    fn body(&self, prompt: &Prompt) -> Result<Value> {
        let schema = provider_schema(prompt.schema)?;
        Ok(match self.wire {
            Wire::OpenAi | Wire::OpenRouter | Wire::Endpoint => self.chat_body(prompt, schema),
            Wire::Anthropic => json!({
                "model": self.model,
                "max_tokens": ANTHROPIC_MAX_TOKENS,
                "system": prompt.system,
                "messages": [{"role": "user", "content": prompt.user}],
                "output_config": {"format": {"type": "json_schema", "schema": schema}},
            }),
            Wire::Google => json!({
                "systemInstruction": {"parts": [{"text": prompt.system}]},
                "contents": [{"role": "user", "parts": [{"text": prompt.user}]}],
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseJsonSchema": schema,
                },
            }),
        })
    }

    fn chat_body(&self, prompt: &Prompt, schema: Value) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": prompt.system},
                {"role": "user", "content": prompt.user},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "jobpilot_output",
                    "strict": true,
                    "schema": schema,
                },
            },
        });
        if self.wire == Wire::OpenAi {
            body["store"] = json!(false);
        }
        if self.wire == Wire::OpenRouter {
            body["provider"] = json!({"data_collection": "deny", "require_parameters": true});
        }
        body
    }

    /// Google's model is in the path. `complete` runs `check_model` first, so it cannot leave
    /// the segment.
    fn url(&self) -> String {
        match self.wire {
            Wire::OpenAi | Wire::OpenRouter | Wire::Endpoint => {
                format!("{}/chat/completions", self.base)
            }
            Wire::Anthropic => format!("{}/v1/messages", self.base),
            Wire::Google => {
                format!("{}/v1beta/models/{}:generateContent", self.base, self.model)
            }
        }
    }

    /// The key goes in this header and nowhere else. Never in a URL: errors and proxies log those.
    fn with_key(&self, request: RequestBuilder, key: &str) -> Result<RequestBuilder> {
        Ok(match self.wire {
            Wire::OpenAi | Wire::OpenRouter | Wire::Endpoint => {
                let bearer = Zeroizing::new(format!("Bearer {key}"));
                request.header(AUTHORIZATION, sensitive(&bearer)?)
            }
            Wire::Anthropic => request
                .header("x-api-key", sensitive(key)?)
                .header("anthropic-version", ANTHROPIC_VERSION),
            Wire::Google => request.header("x-goog-api-key", sensitive(key)?),
        })
    }

    /// One POST and its capped read. Both stay inside the router's `bounded`, the only deadline
    /// a hosted call has, so a server that drips bytes is cut off at the attempt's limit.
    async fn send(&self, body: &Value) -> Result<Vec<u8>> {
        let mut request = self.client.post(self.url()).json(body);
        if let Some(key) = self.read_key().await? {
            request = self.with_key(request, &key)?;
        }
        let response = request.send().await.map_err(net)?;
        let status = response.status();
        if status.is_success() {
            return read_capped(response, MAX_REPLY_BYTES).await;
        }
        let header = response.headers().get(RETRY_AFTER);
        let wait = retry_after(header.and_then(|value| value.to_str().ok()));
        // The status is the answer. A body too long to read just matches no word.
        let text = read_capped(response, MAX_ERROR_BODY).await;
        Err(classify_status(
            status.as_u16(),
            wait,
            &text.unwrap_or_default(),
        ))
    }
}

/// `set_sensitive` keeps the value out of `Debug` output. reqwest's copy of it is not zeroed.
fn sensitive(text: &str) -> Result<HeaderValue> {
    let mut value =
        HeaderValue::from_str(text).map_err(|_| Error::Refused("auth_failed".into()))?;
    value.set_sensitive(true);
    Ok(value)
}

impl Provider for ApiProvider {
    fn declaration(&self) -> Declaration {
        let endpoint = self.info.kind == ProviderKind::Endpoint;
        Declaration {
            id: self.info.id,
            capabilities: self.info.capabilities.to_vec(),
            local: false,
            context_limit: if endpoint { 8_192 } else { 200_000 },
            timeout: Duration::from_secs(if endpoint { 300 } else { 120 }),
            sign_in: if endpoint {
                "loopback server"
            } else {
                "api key in the OS keychain"
            }
            .into(),
        }
    }

    /// Opens no socket. An endpoint needs no key.
    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async move {
            if self.model.is_empty() {
                return Availability::Unavailable("no_api_model".into());
            }
            if self.wire != Wire::Endpoint && !self.key_stored {
                return Availability::Unavailable("no_key".into());
            }
            Availability::Available
        })
    }

    /// The router drops this future on a cancel or a timeout, which ends the request.
    fn complete<'a>(
        &'a self,
        prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            // Again here: a restored backup puts a model in settings past `set_model`.
            check_model(self.info.name, &self.model)?;
            let reply = self.send(&self.body(prompt)?).await?;
            read_reply(self.wire, &reply)
        })
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

/// What a reply holds, whichever wire it came by.
#[derive(Default)]
struct Reading {
    tool_call: bool,
    refused: bool,
    truncated: bool,
    /// An error inside a reply, or an end the wire does not call a clean one.
    failed: bool,
    text: Option<String>,
}

/// The text a model answered, or why it did not. A reply can hold usable text and still fail
/// here: a refusal or a tool call is never read as an answer. What comes back is untrusted, and
/// the router's validation is its only way in.
fn read_reply(wire: Wire, body: &[u8]) -> Result<String> {
    let bad_reply = || Error::Failed("bad_reply".into());
    let reply: Value = serde_json::from_slice(body).map_err(|_| bad_reply())?;
    let reading = match wire {
        Wire::OpenAi | Wire::OpenRouter | Wire::Endpoint => read_chat(&reply),
        Wire::Anthropic => read_anthropic(&reply),
        Wire::Google => read_google(&reply),
    };
    if reading.tool_call {
        return Err(Error::Refused("permission_denied".into()));
    }
    if reading.refused {
        return Err(Error::Refused("model_refused".into()));
    }
    if reading.truncated {
        return Err(Error::Failed("output_truncated".into()));
    }
    if reading.failed || !reply["error"].is_null() {
        return Err(bad_reply());
    }
    reading.text.ok_or_else(bad_reply)
}

/// Every choice counts, though only the first is read: no request asks for two. A `tool_calls`
/// field counts unless it is empty, as no request here named a tool. A missing finish reads as
/// clean, as some local servers leave it out, and the schema check still stands.
fn read_chat(reply: &Value) -> Reading {
    let mut reading = Reading::default();
    for choice in items(&reply["choices"]) {
        let message = &choice["message"];
        let finish = choice["finish_reason"].as_str().unwrap_or("stop");
        let refusal = &message["refusal"];
        let calls = &message["tool_calls"];
        let no_calls = calls.is_null() || calls.as_array().is_some_and(Vec::is_empty);
        reading.tool_call |= !no_calls
            || !message["function_call"].is_null()
            || matches!(finish, "tool_calls" | "function_call");
        reading.refused |= (!refusal.is_null() && refusal != "") || finish == "content_filter";
        reading.truncated |= finish == "length";
        reading.failed |= !choice["error"].is_null()
            || !matches!(
                finish,
                "stop" | "length" | "content_filter" | "tool_calls" | "function_call"
            );
    }
    let text = reply["choices"][0]["message"]["content"].as_str();
    reading.text = text.map(str::to_string);
    reading
}

/// Only a normal end accepts text. Every other stop reason, a missing one too, is failed here,
/// and `read_reply` names it first if a flag above that says more. `tool_` in a block's type:
/// server and MCP tools and their results count too, and no request here asked for any.
/// Thinking blocks are neither a tool nor the answer.
fn read_anthropic(reply: &Value) -> Reading {
    let blocks = items(&reply["content"]);
    let stop = reply["stop_reason"].as_str().unwrap_or_default();
    Reading {
        tool_call: matches!(stop, "tool_use" | "pause_turn")
            || blocks.iter().any(|b| kind(b).contains("tool_")),
        refused: stop == "refusal",
        truncated: matches!(stop, "max_tokens" | "model_context_window_exceeded"),
        failed: !matches!(stop, "end_turn" | "stop_sequence"),
        text: joined(blocks.iter().filter(|b| kind(b) == "text")),
    }
}

/// Only `STOP` accepts text, as in `read_anthropic`, and a missing finish means the model has not
/// stopped. A thought is not the answer. Every candidate counts, though only the first is read:
/// no request asks for two.
fn read_google(reply: &Value) -> Reading {
    let mut reading = Reading {
        refused: !reply["promptFeedback"]["blockReason"].is_null(),
        ..Reading::default()
    };
    for candidate in items(&reply["candidates"]) {
        let finish = candidate["finishReason"].as_str().unwrap_or_default();
        let called = |p: &Value| GOOGLE_TOOL_PARTS.iter().any(|name| !p[*name].is_null());
        reading.tool_call |= GOOGLE_TOOL_ENDS.contains(&finish)
            || items(&candidate["content"]["parts"]).iter().any(called);
        reading.refused |= GOOGLE_REFUSED.contains(&finish);
        reading.truncated |= matches!(finish, "MAX_TOKENS" | "CONTINUATION");
        reading.failed |= finish != "STOP";
    }
    let parts = items(&reply["candidates"][0]["content"]["parts"]);
    reading.text = joined(parts.iter().filter(|p| p["thought"] != true));
    reading
}

fn kind(block: &Value) -> &str {
    block["type"].as_str().unwrap_or_default()
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// `None` when nothing holds text: an empty join is no answer.
fn joined<'a>(parts: impl Iterator<Item = &'a Value>) -> Option<String> {
    let text: String = parts.filter_map(|part| part["text"].as_str()).collect();
    (!text.is_empty()).then_some(text)
}

/// Refused before anything is sent. `Ok(())` means eligible.
pub fn check_model(name: &str, model: &str) -> Result<()> {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || b"._:/-".contains(&b);
    let shaped = (1..=MAX_MODEL).contains(&model.len())
        && model.bytes().all(allowed)
        && !model.contains("..")
        // Google's id lands in the URL path.
        && !(name == "google" && model.contains(['/', ':']));
    if !shaped {
        return Err(Error::Validation(
            "a model id is 1 to 200 letters, digits, and . _ : / -".into(),
        ));
    }
    if name == ENDPOINT {
        return match name_is_local(model) {
            true => Ok(()),
            false => Err(Error::Refused("not_local".into())),
        };
    }
    let (id, suffix) = match model.split_once(':') {
        Some((id, suffix)) => (id, Some(suffix)),
        None => (model, None),
    };
    let lower = model.to_ascii_lowercase();
    let has_tools = TOOL_MODEL_WORDS.iter().any(|word| lower.contains(word));
    let online = name == "openrouter"
        && suffix.is_some_and(|s| s.to_ascii_lowercase().contains(OPENROUTER_SEARCH_SUFFIX));
    if has_tools || online {
        return Err(Error::Refused("model_has_tools".into()));
    }
    let eligible = OPENROUTER_ORIGINS.iter().any(|o| id.starts_with(o))
        && suffix.is_none_or(|s| OPENROUTER_SUFFIXES.contains(&s));
    if name == "openrouter" && !eligible {
        return Err(Error::Refused("origin_refused".into()));
    }
    Ok(())
}

/// The task schema with every `maxLength` removed. The router's own validation still holds it.
pub fn provider_schema(schema: &str) -> Result<Value> {
    let mut value = serde_json::from_str(schema)
        .map_err(|_| Error::Validation("the task schema is not JSON".into()))?;
    strip_max_length(&mut value);
    Ok(value)
}

/// The keyword's value is a number. A property that happens to have the name holds an object.
fn strip_max_length(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("maxLength").is_some_and(Value::is_number) {
                map.remove("maxLength");
            }
            map.values_mut().for_each(strip_max_length);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_max_length),
        _ => {}
    }
}

/// Stores a provider's model, by the same rules a send applies.
pub async fn set_model(db: &Db, name: &str, model: &str) -> Result<()> {
    let takes_model = register::info(name)
        .is_some_and(|info| matches!(info.kind, ProviderKind::Api | ProviderKind::Endpoint));
    if !takes_model {
        return Err(Error::Validation("this provider takes no model id".into()));
    }
    let model = model.trim();
    check_model(name, model)?;
    settings::set(db, &format!("ai.model.{name}"), &json!(model)).await
}

/// The address as it is stored, and a client for it. A name, password, query, fragment, or any
/// path but a version would land in settings, and so in an export, and any of them can hold a
/// key. The message leaves the address out for that reason.
fn endpoint_client(url: &str) -> Result<(String, Client)> {
    let (parsed, client) = loopback_client(url)?;
    let bare = parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && is_version_path(parsed.path());
    if !bare {
        return Err(Error::Validation(
            "an endpoint address is a host and port, then at most a version such as /v1".into(),
        ));
    }
    Ok((parsed.as_str().trim_end_matches('/').to_string(), client))
}

fn is_version_path(path: &str) -> bool {
    let path = path.trim_matches('/');
    path.is_empty()
        || path.strip_prefix('v').is_some_and(|digits| {
            (1..=MAX_VERSION_DIGITS).contains(&digits.len())
                && digits.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The address as it would be stored, or why not. Nothing is stored and nobody is asked.
pub fn check_endpoint(url: &str) -> Result<String> {
    endpoint_client(url).map(|(url, _)| url)
}

/// A stored address passes the rules again: a restored backup comes in past `set_endpoint`.
async fn stored_endpoint(db: &Db) -> Result<Option<(String, Client)>> {
    let url = text_setting(db, ENDPOINT_URL).await?;
    Ok(url.and_then(|url| endpoint_client(&url).ok()))
}

/// The address in use, for the screen. `None` when none is set or the stored one is refused.
pub async fn endpoint_address(db: &Db) -> Result<Option<String>> {
    Ok(stored_endpoint(db).await?.map(|(url, _)| url))
}

/// An empty address clears the address and the model. Any other is checked, then stored with
/// trailing slashes removed, so `<url>/chat/completions` has one. A key was given for one server,
/// so a different address drops it first: a key the keychain will not give up stops the change.
/// Consent needs nothing here, as a record is bound to its address (`consent::state`).
pub async fn set_endpoint(db: &Db, url: &str) -> Result<()> {
    let cleared = url.trim().is_empty();
    let url = if cleared {
        String::new()
    } else {
        check_endpoint(url)?
    };
    let stored = text_setting(db, ENDPOINT_URL).await?;
    if stored.unwrap_or_default() != url {
        match secrets::delete_api_key(db, ENDPOINT, "system").await {
            Ok(()) | Err(Error::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    settings::set(db, ENDPOINT_URL, &json!(url)).await?;
    if cleared {
        settings::set(db, ENDPOINT_MODEL, &json!("")).await?;
    }
    Ok(())
}

/// A restore can swap the address with no dialog. It already revokes consent, so only the key
/// is left to drop. `before` is the live address, `None` when it could not be read.
pub(crate) async fn forget_key_of_replaced_endpoint(db: &Db, before: Option<&str>) {
    let restored = text_setting(db, ENDPOINT_URL).await.ok().flatten();
    if restored.unwrap_or_default() != before.unwrap_or_default() {
        secrets::forget_api_key(db, ENDPOINT).await;
    }
}

/// A failed status as an error, matching the body only against fixed words so its text goes no
/// further.
pub(crate) fn classify_status(status: u16, retry_after: Option<Duration>, body: &[u8]) -> Error {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    let holds = |words: &[&str]| words.iter().any(|word| text.contains(word));
    match status {
        401 | 403 => Error::Refused("auth_failed".into()),
        402 => Error::Refused("quota_exhausted".into()),
        400 | 429 if holds(&SPEND_CAP_WORDS) => Error::Refused("quota_exhausted".into()),
        429 if holds(&QUOTA_WORDS) => Error::Refused("quota_exhausted".into()),
        400 if holds(&[GOOGLE_BAD_KEY_WORD]) => Error::Refused("auth_failed".into()),
        404 => Error::Refused("model_not_found".into()),
        400 | 413 if holds(&OVERFLOW_WORDS) => Error::Refused("prompt_too_large".into()),
        429 | 503 | 529 => Error::RateLimited { retry_after },
        _ => Error::Failed(format!("http_{status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The addresses are spelled out, not the constants, so swapping two bases fails here.
    #[tokio::test]
    async fn from_settings_gives_each_provider_its_own_host_and_key() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::AppPaths::new(tmp.path().join("data"));
        let db = Db::open(&paths).await.unwrap();
        set_model(&db, "google", "m").await.unwrap();
        for (name, url) in [
            ("openai", "https://api.openai.com/v1/chat/completions"),
            (
                "openrouter",
                "https://openrouter.ai/api/v1/chat/completions",
            ),
            ("anthropic", "https://api.anthropic.com/v1/messages"),
            (
                "google",
                "https://generativelanguage.googleapis.com/v1beta/models/m:generateContent",
            ),
        ] {
            let provider = ApiProvider::from_settings(&db, name)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(provider.url(), url, "{name}");
            assert_eq!(provider.info.name, name);
            assert!(
                matches!(&provider.key, KeySource::Keychain(held) if held == name),
                "{name}"
            );
        }
    }

    /// Debug output of a request must never show a key, on any wire.
    #[test]
    fn every_key_header_is_sensitive() {
        for (name, header) in [
            ("openai", "authorization"),
            ("openrouter", "authorization"),
            ("endpoint", "authorization"),
            ("anthropic", "x-api-key"),
            ("google", "x-goog-api-key"),
        ] {
            let url = "http://127.0.0.1:1";
            let provider = ApiProvider::for_test(name, url, "m", KeySource::None).unwrap();
            let request = provider.client.post(url);
            let request = provider.with_key(request, "sk-test").unwrap();
            let built = request.build().unwrap();
            let value = &built.headers()[header];
            assert!(value.is_sensitive(), "{name}");
        }
    }
}
