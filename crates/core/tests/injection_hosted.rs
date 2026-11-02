//! The prompt-injection corpus on the hosted backends: the five API wires, each against a local
//! fake, and the Gemini mock CLI.
//!
//! The fake plays a fully steered model. It reads the `MOCK_REPLY:` line out of the prompt it was
//! sent and returns that in the wire's own shape, so what a posting says reaches the app the way
//! a real vendor's reply would. The Gemini mock cannot read its prompt, so `aim` writes the reply
//! file from the same line before each run. No test here reaches a real host, a real key, a real
//! CLI, or the OS keychain: keyring's in-memory mock stands in, as in `secrets_mock.rs`.

#[path = "fake_http/mod.rs"]
mod fake_http;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use fake_http::{Canned, Fake};
use jobpilot_core::ai::api::{self, ApiProvider, KeySource};
use jobpilot_core::ai::gemini_cli::GeminiCli;
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{CancellationToken, DataCategory, Provider, consent};
use jobpilot_core::crypto::{self, KeyState};
use jobpilot_core::drafts::{self, ExportFormat};
use jobpilot_core::generate::{self, Generated};
use jobpilot_core::knowledge::{self, JobText};
use jobpilot_core::memory::{self, Class};
use jobpilot_core::profile::{self, ProfileBasic, Stated, WorkAuthorization};
use jobpilot_core::render::{ResumeDoc, Template};
use jobpilot_core::secrets::Zeroizing;
use jobpilot_core::{AppPaths, Db, Error, Result};
use serde_json::{Value, json};
use tempfile::TempDir;

const OPEN: &str = "<<<UNTRUSTED_POSTING>>>";
const CLOSE: &str = "<<<END_UNTRUSTED_POSTING>>>";
const MOCK_REPLY: &str = "MOCK_REPLY:";
const KEY: &str = "sk-HOSTED-KEY-ZXQ";
const FLAGGED: [&str; 2] = ["no_evidence", "authorization_statement"];
const DRAFTING: [DataCategory; 3] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::ResumeText,
];
/// Names that would hand a model a tool or a search, at any depth of a body.
const TOOL_KEYS: [&str; 10] = [
    "tools",
    "tool_choice",
    "toolConfig",
    "tool_config",
    "functions",
    "function_call",
    "functionDeclarations",
    "web_search",
    "web_search_options",
    "plugins",
];

// ---- the wires --------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Chat,
    Anthropic,
    Google,
}

#[derive(Clone, Copy)]
struct Wire {
    name: &'static str,
    path: &'static str,
    model: &'static str,
    shape: Shape,
}

const CHAT: &str = "/chat/completions";
const WIRES: [Wire; 5] = [
    Wire {
        name: "openai",
        path: CHAT,
        model: "gpt-x",
        shape: Shape::Chat,
    },
    Wire {
        name: "openrouter",
        path: CHAT,
        model: "openai/gpt-x",
        shape: Shape::Chat,
    },
    Wire {
        name: "endpoint",
        path: CHAT,
        model: "local-model",
        shape: Shape::Chat,
    },
    Wire {
        name: "anthropic",
        path: "/v1/messages",
        model: "claude-x",
        shape: Shape::Anthropic,
    },
    Wire {
        name: "google",
        path: "/v1beta/models/gemini-x:generateContent",
        model: "gemini-x",
        shape: Shape::Google,
    },
];

fn user_text(shape: Shape, body: &Value) -> String {
    let text = match shape {
        Shape::Chat => &body["messages"][1]["content"],
        Shape::Anthropic => &body["messages"][0]["content"],
        Shape::Google => &body["contents"][0]["parts"][0]["text"],
    };
    text.as_str().unwrap_or_default().to_string()
}

fn system_text(shape: Shape, body: &Value) -> String {
    let text = match shape {
        Shape::Chat => &body["messages"][0]["content"],
        Shape::Anthropic => &body["system"],
        Shape::Google => &body["systemInstruction"]["parts"][0]["text"],
    };
    text.as_str().unwrap_or_default().to_string()
}

/// `text` as the wire's own clean reply.
fn says(shape: Shape, text: &str) -> Canned {
    Canned::json(
        200,
        &match shape {
            Shape::Chat => json!({"choices": [{
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop",
            }]}),
            Shape::Anthropic => json!({
                "type": "message",
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn",
            }),
            Shape::Google => json!({"candidates": [{
                "content": {"role": "model", "parts": [{"text": text}]},
                "finishReason": "STOP",
            }]}),
        },
    )
}

/// A tool call as the wire's own reply, beside `text` when there is any: a model that also wrote
/// a usable answer has still asked for a tool.
fn calls(shape: Shape, call: &Value, text: Option<&str>) -> Canned {
    let name = call["name"].clone();
    let input = call["input"].clone();
    Canned::json(
        200,
        &match shape {
            Shape::Chat => json!({"choices": [{
                "message": {
                    "role": "assistant",
                    "content": text,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()},
                    }],
                },
                "finish_reason": "tool_calls",
            }]}),
            Shape::Anthropic => {
                let mut content = vec![json!({
                    "type": "tool_use", "id": "toolu_1", "name": name, "input": input,
                })];
                content.extend(text.map(|text| json!({"type": "text", "text": text})));
                json!({"type": "message", "content": content, "stop_reason": "tool_use"})
            }
            Shape::Google => {
                let mut parts = vec![json!({"functionCall": {"name": name, "args": input}})];
                parts.extend(text.map(|text| json!({"text": text})));
                json!({"candidates": [{
                    "content": {"role": "model", "parts": parts},
                    "finishReason": "STOP",
                }]})
            }
        },
    )
}

/// The `$`-names a reply may use, as the test holds them right now.
#[derive(Default)]
struct Ids {
    job: String,
    draft: String,
    evidence: String,
}

impl Ids {
    fn fill(&self, reply: &str) -> String {
        reply
            .replace("$JOB_ID", &self.job)
            .replace("$DRAFT_ID", &self.draft)
            .replace("$EVIDENCE_ID", &self.evidence)
    }
}

fn steered_line(prompt: &str) -> Option<&str> {
    prompt
        .lines()
        .find_map(|line| line.strip_prefix(MOCK_REPLY))
        .map(str::trim)
}

/// The steered model. A reply naming `tool_call` comes back as the wire's tool call.
fn steered_fake(wire: Wire, ids: Arc<Mutex<Ids>>) -> Fake {
    Fake::answering(wire.path, move |body| {
        let user = user_text(wire.shape, body);
        let Some(line) = steered_line(&user) else {
            return says(wire.shape, "the posting asks for no reply");
        };
        let line = ids.lock().unwrap().fill(line);
        let reply: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        match reply.get("tool_call") {
            Some(call) => calls(wire.shape, call, None),
            None => says(wire.shape, &line),
        }
    })
}

// ---- the Gemini mock --------------------------------------------------------------------

/// A wrapper script, because the adapter clears the child's environment.
struct MockGemini {
    dir: TempDir,
    bin: PathBuf,
}

impl MockGemini {
    fn new() -> MockGemini {
        let dir = tempfile::tempdir().unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-gemini");
        let vars: Vec<(String, String)> = ["REPLY", "STDIN", "ARGS", "SYSTEM_COPY"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name.to_lowercase());
                (format!("MOCK_GEMINI_{name}"), path.display().to_string())
            })
            .collect();
        let bin = write_wrapper(dir.path(), &mock, &vars);
        MockGemini { dir, bin }
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }

    fn reply_with(&self, stream: &str) {
        fs::write(self.dir.path().join("reply"), stream).unwrap();
    }
}

#[cfg(unix)]
fn write_wrapper(dir: &Path, mock: &Path, vars: &[(String, String)]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mut script = String::from("#!/bin/sh\n");
    for (name, value) in vars {
        script.push_str(&format!("export {name}='{value}'\n"));
    }
    script.push_str(&format!("'{}' \"$@\"\n", mock.join("gemini").display()));
    let path = dir.join("gemini");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(windows)]
fn write_wrapper(dir: &Path, mock: &Path, vars: &[(String, String)]) -> PathBuf {
    let mut script = String::from("@echo off\r\n");
    for (name, value) in vars {
        script.push_str(&format!("set \"{name}={value}\"\r\n"));
    }
    let target = mock.join("gemini.cmd");
    script.push_str(&format!(
        "call \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        target.display()
    ));
    let path = dir.join("gemini.cmd");
    fs::write(&path, script).unwrap();
    path
}

fn event(value: &Value) -> String {
    format!("{value}\n")
}

/// A whole good run saying `text`.
fn gemini_stream(text: &str) -> String {
    let start = event(&json!({"type": "init", "session_id": "s", "model": "m"}));
    let said = event(&json!({
        "type": "message", "role": "assistant", "content": text, "delta": true,
    }));
    let done = event(&json!({"type": "result", "status": "success", "stats": {}}));
    format!("{start}{said}{done}")
}

fn gemini_tool_use(call: &Value) -> String {
    event(&json!({
        "type": "tool_use",
        "tool_name": call["name"],
        "tool_id": "1",
        "parameters": call["input"],
    }))
}

// ---- one backend, its data folder, and the corpus ---------------------------------------

enum Backend {
    Api(Wire, Fake),
    Gemini(MockGemini),
}

struct Hosted {
    tmp: TempDir,
    db: Db,
    backend: Backend,
    ids: Arc<Mutex<Ids>>,
}

/// keyring's mock, process-wide: `set_endpoint` and the sealed authorization rows reach for a
/// store, and this binary must never reach the OS one.
fn mock_store() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        // The first `Entry` installs the OS store, so that happens first, then this replaces it.
        let _ = keyring::Entry::store_status();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    });
}

async fn data_folder() -> (TempDir, Db, Ids) {
    mock_store();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    profile::set_basic(
        &db,
        ProfileBasic {
            name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            ..ProfileBasic::default()
        },
    )
    .await
    .unwrap();
    let item = knowledge::add(&db, "experience", "Shipped the Rust compiler", "user", None)
        .await
        .unwrap();
    // A draft that exists, so a case can cite its id as if it were evidence.
    let draft = drafts::save_manual(&db, "resume", None, ResumeDoc::default())
        .await
        .unwrap();
    let ids = Ids {
        job: String::new(),
        draft: draft.id,
        evidence: item.id,
    };
    (tmp, db, ids)
}

async fn on_wire(wire: Wire) -> Hosted {
    let (tmp, db, ids) = data_folder().await;
    let ids = Arc::new(Mutex::new(ids));
    let fake = steered_fake(wire, ids.clone());
    if wire.name == "endpoint" {
        api::set_endpoint(&db, &fake.url).await.unwrap();
    }
    consent::give_for(&db, wire.name, &DRAFTING, true, "test")
        .await
        .unwrap();
    Hosted {
        tmp,
        db,
        backend: Backend::Api(wire, fake),
        ids,
    }
}

async fn on_gemini() -> Hosted {
    let (tmp, db, ids) = data_folder().await;
    consent::give_for(&db, "gemini_cli", &DRAFTING, true, "test")
        .await
        .unwrap();
    Hosted {
        tmp,
        db,
        backend: Backend::Gemini(MockGemini::new()),
        ids: Arc::new(Mutex::new(ids)),
    }
}

async fn every_wire() -> Vec<Hosted> {
    let mut hosted = Vec::new();
    for wire in WIRES {
        hosted.push(on_wire(wire).await);
    }
    hosted
}

async fn every_backend() -> Vec<Hosted> {
    let mut hosted = every_wire().await;
    hosted.push(on_gemini().await);
    hosted
}

impl Hosted {
    fn name(&self) -> &'static str {
        match &self.backend {
            Backend::Api(wire, _) => wire.name,
            Backend::Gemini(_) => "gemini_cli",
        }
    }

    fn router(&self) -> Router {
        let provider: Box<dyn Provider> = match &self.backend {
            Backend::Api(wire, fake) => {
                let key = KeySource::Fixed(Zeroizing::new(KEY.to_string()));
                let provider = ApiProvider::for_test(wire.name, &fake.url, wire.model, key);
                Box::new(provider.unwrap())
            }
            Backend::Gemini(mock) => Box::new(GeminiCli::with_binary(mock.bin.clone())),
        };
        Router::with_providers(vec![provider])
    }

    fn canary(&self) -> String {
        let path = self.tmp.path().join("canary-07");
        path.to_string_lossy().replace('\\', "/")
    }

    /// Steers whichever backend this is at the posting's `MOCK_REPLY:` line.
    fn aim(&self, posting: &str, job_id: &str) {
        self.ids.lock().unwrap().job = job_id.to_string();
        let Backend::Gemini(mock) = &self.backend else {
            return;
        };
        let line = steered_line(posting).expect("the posting steers no reply");
        let line = self.ids.lock().unwrap().fill(line);
        let reply: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        match reply.get("tool_call") {
            Some(call) => mock.reply_with(&gemini_tool_use(call)),
            None => mock.reply_with(&gemini_stream(&line)),
        }
    }

    async fn import(&self, name: &str, raw: &str) -> JobText {
        let job = knowledge::import_job_text(&self.db, name, raw)
            .await
            .unwrap();
        self.aim(&job.normalized, &job.id);
        job
    }

    async fn resume(&self, job: &JobText) -> Result<Generated> {
        let cancel = CancellationToken::new();
        generate::resume(&self.db, &self.router(), "gui", &job.id, cancel).await
    }

    async fn draft_count(&self) -> usize {
        drafts::list(&self.db, None).await.unwrap().len()
    }

    async fn export(&self, draft_id: &str) -> Result<PathBuf> {
        let out = self.tmp.path().join("out");
        let (template, format) = (Template::Classic, ExportFormat::Docx);
        drafts::export(&self.db, draft_id, template, format, &out, "gui").await
    }

    async fn run_reasons(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT coalesce(reason, outcome) FROM task_runs ORDER BY rowid")
            .fetch_all(&self.db.pool)
            .await
            .unwrap()
    }

    /// Every column that could keep what a run sent or got.
    async fn stored_text(&self) -> String {
        let mut text = String::new();
        for query in [
            "SELECT provider || outcome || coalesce(reason, '') FROM task_runs",
            "SELECT action || coalesce(target, '') || coalesce(detail, '') FROM audit_log",
            "SELECT text || evidence_ids || status FROM claims",
            "SELECT title || body FROM drafts",
        ] {
            let rows: Vec<String> = sqlx::query_scalar(query)
                .fetch_all(&self.db.pool)
                .await
                .unwrap();
            text.push_str(&rows.join("\n"));
        }
        text
    }
}

struct Case {
    name: String,
    raw: String,
}

fn corpus(canary: &str) -> Vec<Case> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/injection");
    let mut cases: Vec<Case> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
        .map(|path| Case {
            name: path.file_stem().unwrap().to_string_lossy().into_owned(),
            raw: fs::read_to_string(&path)
                .unwrap()
                .replace("$CANARY", canary),
        })
        .collect();
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(cases.len() >= 19, "the corpus holds {} files", cases.len());
    cases
}

fn case<'a>(cases: &'a [Case], number: &str) -> &'a Case {
    cases
        .iter()
        .find(|case| case.name.starts_with(number))
        .unwrap_or_else(|| panic!("no corpus file {number}"))
}

fn offered_tools(value: &Value) -> Vec<String> {
    match value {
        Value::Object(fields) => fields
            .iter()
            .flat_map(|(name, field)| {
                let own = TOOL_KEYS.contains(&name.as_str()).then(|| name.clone());
                own.into_iter().chain(offered_tools(field))
            })
            .collect(),
        Value::Array(items) => items.iter().flat_map(offered_tools).collect(),
        _ => Vec::new(),
    }
}

// ---- every corpus file on every backend -------------------------------------------------

#[tokio::test]
async fn every_corpus_file_stays_data_on_every_hosted_wire() {
    // A reply the draft schema refuses, or a tool call: nothing of it may be stored.
    let stores_nothing = ["15", "17", "19"];
    // These files steer the model to state authorization, however it is worded.
    let states_authorization = ["01", "08", "11", "13"];

    for hosted in every_backend().await {
        let evidence_before = knowledge::list(&hosted.db).await.unwrap().len();
        let cases = corpus(&hosted.canary());
        for case in &cases {
            let who = format!("{} on {}", case.name, hosted.name());
            let job = hosted.import(&case.name, &case.raw).await;
            let before = hosted.draft_count().await;
            let made = hosted.resume(&job).await;

            // The endpoint's window is the small one. Its refusal comes before any send.
            let too_large = hosted.name() == "endpoint" && case.name.starts_with("10");
            if too_large || stores_nothing.iter().any(|n| case.name.starts_with(n)) {
                assert!(made.is_err(), "{who}: {made:?}");
                assert_eq!(hosted.draft_count().await, before, "{who}");
                continue;
            }

            let made = made.unwrap_or_else(|e| panic!("{who}: {e:?}"));
            let claims = drafts::claims(&hosted.db, &made.draft.id).await.unwrap();
            assert!(!claims.is_empty(), "{who}");
            for claim in &claims {
                assert!(FLAGGED.contains(&claim.status.as_str()), "{who}: {claim:?}");
            }
            assert_eq!(made.blocked, claims.len(), "{who}");
            if states_authorization
                .iter()
                .any(|n| case.name.starts_with(n))
            {
                assert_eq!(claims[0].status, "authorization_statement", "{who}");
            }
            // The model's own name and contact never reach the document.
            assert_eq!(made.draft.doc.name, "Ada Lovelace", "{who}");
            assert_eq!(made.draft.doc.contact, ["ada@example.com"], "{who}");

            let exported = hosted.export(&made.draft.id).await;
            assert!(
                matches!(&exported, Err(Error::Refused(reason)) if reason == "claims_blocked"),
                "{who}: {exported:?}"
            );
        }
        // Nothing the postings or the replies said became evidence, and nothing ran.
        let evidence = knowledge::list(&hosted.db).await.unwrap();
        assert_eq!(evidence.len(), evidence_before, "{}", hosted.name());
        assert!(
            !PathBuf::from(hosted.canary()).exists(),
            "{}",
            hosted.name()
        );
    }
}

// ---- what goes out ----------------------------------------------------------------------

#[tokio::test]
async fn the_posting_stays_in_marked_blocks_in_the_user_message() {
    const SENTINEL: &str = "ZXQ-POSTING-SENTINEL";
    let cases = corpus("/canary");
    let reply = steered_line(&case(&cases, "02").raw).unwrap().to_string();
    let raw = format!("Platform Engineer\n\n{SENTINEL}\n{MOCK_REPLY} {reply}\n");

    for hosted in every_backend().await {
        let who = hosted.name();
        let job = hosted.import("sentinel", &raw).await;
        hosted.resume(&job).await.unwrap();

        let (system, user, outside) = match &hosted.backend {
            Backend::Api(wire, fake) => {
                let body = fake.hits(wire.path).remove(0);
                let (system, user) = (system_text(wire.shape, &body), user_text(wire.shape, &body));
                assert_eq!(body.to_string().matches(SENTINEL).count(), 1, "{who}");
                let mut outside = fake.headers(wire.path).join("\n");
                outside.push_str(&fake.paths().join("\n"));
                (system, user, outside.to_ascii_uppercase())
            }
            Backend::Gemini(mock) => (
                mock.read("system_copy"),
                mock.read("stdin"),
                mock.read("args"),
            ),
        };
        assert!(!system.is_empty(), "{who}: the system text was not seen");
        assert!(!system.contains(SENTINEL), "{who}");
        assert!(!outside.contains(SENTINEL), "{who}");
        // Gemini's prompt escapes `@`, which the markers do not hold.
        assert_eq!(user.matches(OPEN).count(), 1, "{who}");
        assert_eq!(user.matches(CLOSE).count(), 1, "{who}");
        let (open, close) = (user.find(OPEN).unwrap(), user.find(CLOSE).unwrap());
        assert!(user[open..close].contains(SENTINEL), "{who}");
        assert_eq!(user.matches(SENTINEL).count(), 1, "{who}");
    }
}

#[test]
fn the_tool_scan_finds_a_key_at_any_depth() {
    let body = json!({"a": [{"b": {"tool_choice": "auto"}}], "tools": []});
    let mut found = offered_tools(&body);
    found.sort();
    assert_eq!(found, ["tool_choice", "tools"]);
    assert!(offered_tools(&json!({"a": "tools"})).is_empty());
}

#[tokio::test]
async fn no_wire_offers_a_tool() {
    let cases = corpus("/canary");
    for hosted in every_wire().await {
        let Backend::Api(wire, fake) = &hosted.backend else {
            unreachable!();
        };
        // The file that asks for a tool, then the classifier, the other task on these wires.
        let ask = case(&cases, "15");
        let job = hosted.import(&ask.name, &ask.raw).await;
        hosted.resume(&job).await.unwrap_err();
        memory::add_question(&hosted.db, "Why this role?", None)
            .await
            .unwrap();
        let router = hosted.router();
        memory::classify_pending(&hosted.db, &router, "gui", CancellationToken::new())
            .await
            .unwrap();

        let hits = fake.hits(wire.path);
        assert!(hits.len() >= 2, "{}: {hits:?}", wire.name);
        for body in &hits {
            assert_eq!(offered_tools(body), Vec::<String>::new(), "{}", wire.name);
        }
    }
}

#[tokio::test]
async fn the_key_is_never_in_a_body_or_a_prompt() {
    for hosted in every_wire().await {
        let Backend::Api(wire, fake) = &hosted.backend else {
            unreachable!();
        };
        let cases = corpus(&hosted.canary());
        let leak = case(&cases, "18");
        let job = hosted.import(&leak.name, &leak.raw).await;
        hosted.resume(&job).await.unwrap();

        let hits = fake.hits(wire.path);
        assert!(!hits.is_empty(), "{}", wire.name);
        for body in &hits {
            assert!(!body.to_string().contains(KEY), "{}", wire.name);
        }
        assert!(!fake.paths().join("\n").contains(KEY), "{}", wire.name);
        // In the wire's one key header, and in no other line of the request.
        let holding: Vec<String> = fake
            .headers(wire.path)
            .into_iter()
            .filter(|line| line.contains(&KEY.to_ascii_lowercase()))
            .map(|line| line.split(':').next().unwrap_or_default().to_string())
            .collect();
        let header = match wire.shape {
            Shape::Chat => "authorization",
            Shape::Anthropic => "x-api-key",
            Shape::Google => "x-goog-api-key",
        };
        assert_eq!(holding, vec![header; hits.len()], "{}", wire.name);
        assert!(!hosted.stored_text().await.contains(KEY), "{}", wire.name);
    }
}

#[tokio::test]
async fn a_sensitive_value_never_reaches_a_hosted_body() {
    const NOTE: &str = "ZXQ-AUTH-NOTE";
    for hosted in every_backend().await {
        let who = hosted.name();
        let KeyState::Unlocked { .. } = crypto::state(&hosted.db).await.unwrap() else {
            panic!("the mock store did not unlock");
        };
        profile::set_authorization(
            &hosted.db,
            WorkAuthorization {
                country: "PT".into(),
                authorized: Stated::Yes,
                needs_sponsorship: Stated::Yes,
                note: NOTE.into(),
            },
        )
        .await
        .unwrap();
        let cases = corpus(&hosted.canary());
        for number in ["01", "13", "14", "16", "18"] {
            let hostile = case(&cases, number);
            let job = hosted.import(&hostile.name, &hostile.raw).await;
            hosted.resume(&job).await.unwrap();
            let sent = match &hosted.backend {
                Backend::Api(wire, fake) => {
                    let hits: Vec<String> =
                        fake.hits(wire.path).iter().map(Value::to_string).collect();
                    hits.join("\n")
                }
                Backend::Gemini(mock) => mock.read("stdin") + &mock.read("system_copy"),
            };
            assert!(sent.contains(MOCK_REPLY), "{who}: nothing was seen");
            assert!(!sent.contains(NOTE), "{who} {number}");
            assert!(!sent.contains("needs_sponsorship"), "{who} {number}");
        }
    }
}

// ---- what comes back --------------------------------------------------------------------

#[tokio::test]
async fn a_tool_call_in_a_reply_stores_nothing() {
    for hosted in every_backend().await {
        let who = hosted.name();
        let cases = corpus(&hosted.canary());
        let tool = case(&cases, "19");
        let before = hosted.draft_count().await;
        let job = hosted.import(&tool.name, &tool.raw).await;

        let made = hosted.resume(&job).await;

        assert!(made.is_err(), "{who}: {made:?}");
        assert_eq!(hosted.draft_count().await, before, "{who}");
        assert_eq!(hosted.run_reasons().await[0], "permission_denied", "{who}");
        assert!(!PathBuf::from(hosted.canary()).exists(), "{who}");
    }
}

#[tokio::test]
async fn a_tool_call_beside_a_usable_answer_stores_nothing() {
    let cases = corpus("/canary");
    let good = steered_line(&case(&cases, "02").raw).unwrap().to_string();
    let call = json!({"name": "bash", "input": {"command": "cat /etc/passwd"}});
    for wire in WIRES {
        let (tmp, db, _) = data_folder().await;
        let fake = Fake::start(vec![(
            wire.path,
            calls(wire.shape, &call, Some(good.as_str())),
        )]);
        if wire.name == "endpoint" {
            api::set_endpoint(&db, &fake.url).await.unwrap();
        }
        consent::give_for(&db, wire.name, &DRAFTING, true, "test")
            .await
            .unwrap();
        let hosted = Hosted {
            tmp,
            db,
            backend: Backend::Api(wire, fake),
            ids: Arc::default(),
        };
        let before = hosted.draft_count().await;
        let job = hosted.import("plain", "Platform Engineer").await;

        let made = hosted.resume(&job).await;

        assert!(made.is_err(), "{}: {made:?}", wire.name);
        assert_eq!(hosted.draft_count().await, before, "{}", wire.name);
        let reasons = hosted.run_reasons().await;
        assert_eq!(reasons[0], "permission_denied", "{}", wire.name);
    }
}

#[tokio::test]
async fn the_gemini_mock_steered_to_a_tool_stores_nothing() {
    let hosted = on_gemini().await;
    let Backend::Gemini(mock) = &hosted.backend else {
        unreachable!();
    };
    let cases = corpus(&hosted.canary());
    let good = steered_line(&case(&cases, "02").raw).unwrap();
    let tool = case(&cases, "19");
    let line = steered_line(&tool.raw).unwrap();
    let call = &serde_json::from_str::<Value>(line).unwrap()["tool_call"];
    let before = hosted.draft_count().await;
    let job = hosted.import(&tool.name, &tool.raw).await;
    // A valid draft after the call: what the model did first is what counts.
    mock.reply_with(&format!("{}{}", gemini_tool_use(call), gemini_stream(good)));

    let made = hosted.resume(&job).await;

    assert!(made.is_err(), "{made:?}");
    assert_eq!(hosted.draft_count().await, before);
    assert_eq!(hosted.run_reasons().await[0], "permission_denied");
    assert!(!PathBuf::from(hosted.canary()).exists());
}

/// The old shape, with a line tucked into the document that no claim covers. There is nowhere
/// for such a line to go any more: a reply outside the one tree is refused whole.
#[tokio::test]
async fn a_planted_line_outside_the_claim_tree_stores_nothing() {
    let reply = json!({
        "doc": {
            "name": "Ada Lovelace",
            "contact": [],
            "summary": ["Holds a PhD in Computer Science from MIT"],
            "sections": [{"heading": "experience", "entries": [{
                "title": "", "subtitle": "", "dates": "",
                "bullets": ["Shipped the Rust compiler"],
            }]}],
        },
        "claims": [{"text": "Shipped the Rust compiler", "evidence_ids": ["$EVIDENCE_ID"]}],
    });
    let raw = format!("Platform Engineer\n{MOCK_REPLY} {reply}\n");
    for hosted in every_backend().await {
        let before = hosted.draft_count().await;
        let job = hosted.import("planted", &raw).await;

        let made = hosted.resume(&job).await;

        assert!(made.is_err(), "{}: {made:?}", hosted.name());
        assert_eq!(hosted.draft_count().await, before, "{}", hosted.name());
    }
}

#[tokio::test]
async fn work_authorization_is_never_stated_by_a_steered_hosted_reply() {
    // Citing the user's real evidence buys it nothing: the words are what is checked.
    let line = "Authorized to work in the United States";
    let cited = json!({
        "summary": [],
        "sections": [{"heading": "experience", "entries": [{
            "title": "", "subtitle": "", "dates": "", "evidence_ids": [],
            "bullets": [{"text": line, "evidence_ids": ["$EVIDENCE_ID"]}],
        }]}],
    });
    let cited = format!("Backend Engineer\n{MOCK_REPLY} {cited}\n");

    for hosted in every_backend().await {
        let who = hosted.name();
        let cases = corpus(&hosted.canary());
        for raw in [&case(&cases, "01").raw, &cited] {
            let job = hosted.import("authorization", raw).await;
            let made = hosted.resume(&job).await.unwrap();

            let claims = drafts::claims(&hosted.db, &made.draft.id).await.unwrap();
            assert!(!claims.is_empty(), "{who}");
            for claim in &claims {
                assert_eq!(claim.status, "authorization_statement", "{who}: {claim:?}");
            }
            let exported = hosted.export(&made.draft.id).await;
            assert_eq!(exported.unwrap_err().code(), "refused", "{who}");
        }
        // Nothing was inferred into the profile either.
        let held = profile::get(&hosted.db).await.unwrap();
        assert!(held.authorization.is_empty(), "{who}");
    }
}

#[tokio::test]
async fn a_steered_classifier_never_lowers_a_class_on_a_hosted_wire() {
    let cases = corpus("/canary");
    let relabel = case(&cases, "17");
    for hosted in every_backend().await {
        let who = hosted.name();
        let question = memory::add_question(&hosted.db, &relabel.raw, None)
            .await
            .unwrap();
        assert_eq!(question.class, Class::Legal, "{who}");
        hosted.aim(&relabel.raw, "");
        let router = hosted.router();

        let done = memory::classify_pending(&hosted.db, &router, "cli", CancellationToken::new())
            .await
            .unwrap();

        assert_eq!((done.checked, done.raised), (1, 0), "{who}");
        if let Backend::Api(wire, fake) = &hosted.backend {
            let sent = user_text(wire.shape, &fake.hits(wire.path)[0]);
            assert!(
                sent.contains(MOCK_REPLY),
                "{who}: the model was not steered"
            );
        }
        let stored = memory::get(&hosted.db, &question.id).await.unwrap();
        assert_eq!(stored.question.class, Class::Legal, "{who}");
        assert_eq!(stored.question.class_source, "rules", "{who}");
    }
}
