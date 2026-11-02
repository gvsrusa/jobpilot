//! Ollama provider. Every server here is a local fake on 127.0.0.1: no test reaches a real
//! Ollama, and none needs one installed.

mod fake_http;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jobpilot_core::ai::ollama::{CONTEXT_LIMIT, DEFAULT_URL, Fit, Ollama, OllamaStatus};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    Availability, CancellationToken, Capability, Outcome, Prompt, Provider, TaskType, spec,
};
use jobpilot_core::hardware::{self, Hardware};
use jobpilot_core::{AppPaths, Db, Error, audit, settings};
use serde_json::{Value, json};
use tempfile::TempDir;

const GIB: u64 = 1024 * 1024 * 1024;
const JOB: &str = r#"{"title":"Engineer","company":"Acme","location":"Oslo","remote":"hybrid",
    "skills":["Rust"],"seniority":"senior","compensation":""}"#;

enum Reply {
    Json(u16, Value),
    /// Lines, then the connection stays open for `hold` before it closes.
    Stream(Vec<String>, Duration),
}

type Seen = Arc<Mutex<Vec<(String, String)>>>;

struct Fake {
    url: String,
    seen: Seen,
}

impl Fake {
    fn start(routes: Vec<(&'static str, Reply)>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let routes = routes.clone();
                let log = log.clone();
                std::thread::spawn(move || serve(stream, &routes, &log));
            }
        });
        Fake { url, seen }
    }

    fn hits(&self, path: &str) -> Vec<Value> {
        let seen = self.seen.lock().unwrap();
        let bodies = seen.iter().filter(|(p, _)| p == path);
        bodies
            .map(|(_, b)| serde_json::from_str(b).unwrap_or(Value::Null))
            .collect()
    }
}

fn serve(mut stream: std::net::TcpStream, routes: &[(&'static str, Reply)], log: &Seen) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            return;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while raw.len() < head_end + length {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let body = String::from_utf8_lossy(&raw[head_end..]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
    log.lock().unwrap().push((path.clone(), body));

    let Some((_, reply)) = routes.iter().find(|(p, _)| *p == path) else {
        let _ = stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        return;
    };
    match reply {
        Reply::Json(status, value) => {
            let text = value.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
        Reply::Stream(lines, hold) => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
            );
            for line in lines {
                let _ = stream.write_all(format!("{line}\n").as_bytes());
                let _ = stream.flush();
            }
            std::thread::sleep(*hold);
        }
    }
}

fn tags() -> Value {
    serde_json::from_str(include_str!("fixtures/ollama_tags.json")).unwrap()
}

fn tag_routes(names: &[&str]) -> Value {
    let all = tags();
    let rows: Vec<Value> = all["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| names.contains(&m["name"].as_str().unwrap()))
        .cloned()
        .collect();
    json!({ "models": rows })
}

fn prompt() -> Prompt {
    Prompt {
        system: "sys".into(),
        user: "usr".into(),
        schema: spec(TaskType::ParseJob).schema,
    }
}

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

#[test]
fn is_local_follows_the_settled_rule() {
    let fixture = tags();
    let rows = fixture["models"].as_array().unwrap();
    let verdicts: Vec<(&str, bool)> = rows
        .iter()
        .map(|row| (row["name"].as_str().unwrap(), Ollama::is_local(row)))
        .collect();
    assert_eq!(
        verdicts,
        vec![
            ("gemma3:4b", true),
            ("gpt-oss:120b", false),   // remote_model and remote_host
            ("x:cloud", false),        // name
            ("empty-size:1b", false),  // size 0
            ("cloudera:7b", true),     // "cloud" without "-cloud" or ":cloud" is only a name
            ("blank-remote:1b", true), // empty remote fields count as absent
            ("host-only:1b", false),   // either remote field alone is enough
        ]
    );
    // -cloud in the middle, any case.
    assert!(!Ollama::is_local(
        &json!({"name": "glm-4.6-CLOUD", "size": 5})
    ));
    assert!(!Ollama::is_local(&json!({"name": "a:Cloud", "size": 5})));
    // A tag after it changes nothing.
    assert!(!Ollama::is_local(
        &json!({"name": "kimi:cloud:latest", "size": 5})
    ));
    // A remote_ key with any non-empty value, whatever it is called.
    assert!(!Ollama::is_local(
        &json!({"name": "a:1b", "size": 5, "remote_anything": "x"})
    ));
    // No name, no size, wrong types: not local.
    assert!(!Ollama::is_local(&json!({"size": 5})));
    assert!(!Ollama::is_local(&json!({"name": "a:1b"})));
    assert!(!Ollama::is_local(&json!({"name": "a:1b", "size": "5"})));
    assert!(!Ollama::is_local(&json!("a:1b")));
}

#[test]
fn non_loopback_url_is_refused() {
    for url in [
        "http://192.168.1.5:11434",
        "http://example.com:11434",
        "http://0.0.0.0:11434",
        "http://10.0.0.1",
        "http://127.0.0.1.evil.com:11434",
        "http://localhost.evil.com:11434",
        "https://127.0.0.1:11434",
        "not a url",
        "",
    ] {
        match Ollama::new(url, None, None) {
            Err(Error::Refused(reason)) => assert_eq!(reason, "not_loopback", "{url}"),
            other => panic!("{url}: {:?}", other.map(|_| ())),
        }
    }
    for url in [
        "http://localhost:11434",
        "http://127.0.0.1:11434",
        "http://[::1]:11434",
        DEFAULT_URL,
    ] {
        assert!(Ollama::new(url, None, None).is_ok(), "{url}");
    }
}

#[tokio::test]
async fn status_not_running_when_nothing_listens() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}");
    let ollama = Ollama::new(&url, None, None).unwrap();
    let start = Instant::now();
    assert_eq!(ollama.status().await, OllamaStatus::NotRunning { url });
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn status_hides_remote_models() {
    let fake = Fake::start(vec![
        (
            "/api/version",
            Reply::Json(200, json!({"version": "0.12.3"})),
        ),
        (
            "/api/tags",
            Reply::Json(
                200,
                tag_routes(&["gemma3:4b", "gpt-oss:120b", "blank-remote:1b"]),
            ),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, None, None).unwrap();
    let OllamaStatus::Running {
        version, models, ..
    } = ollama.status().await
    else {
        panic!("not running");
    };
    assert_eq!(version, "0.12.3");
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|m| m.local));
    let gemma = &models[0];
    assert_eq!(gemma.name, "gemma3:4b");
    assert_eq!(gemma.family, "gemma3");
    assert_eq!(gemma.parameter_size, "4.3B");
    assert_eq!(gemma.size, 3_338_801_718);
}

/// A reply cut by `num_ctx` looks like a normal 200. At temperature 0 a retry repeats it, token
/// for token, so it is named here and not left to fail validation twice.
#[tokio::test]
async fn a_reply_cut_by_the_window_is_named() {
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
        (
            "/api/chat",
            Reply::Json(
                200,
                json!({"done_reason": "length", "message": {"content": "{\"summary\": ["}}),
            ),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();

    let error = ollama
        .complete(&prompt(), CancellationToken::new())
        .await
        .unwrap_err();

    assert!(
        matches!(&error, Error::Failed(reason) if reason == "output_truncated"),
        "{error:?}"
    );
}

#[tokio::test]
async fn complete_sends_schema_as_format_and_returns_content() {
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
        (
            "/api/chat",
            Reply::Json(
                200,
                json!({"message": {"role": "assistant", "content": JOB}}),
            ),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();
    let out = ollama
        .complete(&prompt(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out, JOB);

    let sent = &fake.hits("/api/chat")[0];
    let schema: Value = serde_json::from_str(spec(TaskType::ParseJob).schema).unwrap();
    assert_eq!(sent["format"], schema);
    assert_eq!(sent["stream"], json!(false));
    assert_eq!(sent["model"], json!("gemma3:4b"));
    assert_eq!(sent["options"]["temperature"], json!(0));
    assert_eq!(sent["options"]["num_ctx"], json!(CONTEXT_LIMIT));
    assert_eq!(sent["truncate"], json!(false));
    assert_eq!(sent["shift"], json!(false));
    // Left on, a thinking model spent all of num_ctx before its first character of JSON.
    assert_eq!(sent["think"], json!(false));
    assert_eq!(
        sent["messages"][0],
        json!({"role": "system", "content": "sys"})
    );
    assert_eq!(
        sent["messages"][1],
        json!({"role": "user", "content": "usr"})
    );
    assert!(sent.get("tools").is_none());
}

#[tokio::test]
async fn declaration_is_local_structured_and_toolless() {
    let ollama = Ollama::new(DEFAULT_URL, None, None).unwrap();
    let d = ollama.declaration();
    assert!(d.local);
    assert_eq!(d.timeout, Duration::from_secs(300));
    for c in [
        Capability::StructuredOutput,
        Capability::Embeddings,
        Capability::LocalExecution,
        Capability::Offline,
        Capability::NoTools,
    ] {
        assert!(d.capabilities.contains(&c), "{c:?}");
    }
    assert!(!d.capabilities.contains(&Capability::WebSearch));
}

#[tokio::test]
async fn cloud_drafting_model_is_unavailable() {
    // No server is needed: the name alone settles it, before any request.
    let ollama = Ollama::new(DEFAULT_URL, Some("x:cloud".into()), None).unwrap();
    assert_eq!(
        ollama.available().await,
        Availability::Unavailable("not_local".into())
    );
}

#[tokio::test]
async fn cloud_embedding_model_is_unavailable() {
    let ollama = Ollama::new(
        DEFAULT_URL,
        Some("gemma3:4b".into()),
        Some("e:cloud".into()),
    )
    .unwrap();
    assert_eq!(
        ollama.available().await,
        Availability::Unavailable("not_local".into())
    );
}

#[tokio::test]
async fn remote_row_models_are_unavailable_and_local_ones_are() {
    let fake = Fake::start(vec![(
        "/api/tags",
        Reply::Json(
            200,
            tag_routes(&["gemma3:4b", "gpt-oss:120b", "host-only:1b"]),
        ),
    )]);
    let url = &fake.url;
    let new = |d: Option<&str>, e: Option<&str>| {
        Ollama::new(url, d.map(Into::into), e.map(Into::into)).unwrap()
    };
    let unavailable = |reason: &str| Availability::Unavailable(reason.into());
    assert_eq!(
        new(Some("gemma3:4b"), None).available().await,
        Availability::Available
    );
    assert_eq!(
        new(Some("gemma3:4b"), Some("gemma3:4b")).available().await,
        Availability::Available
    );
    assert_eq!(
        new(Some("gpt-oss:120b"), None).available().await,
        unavailable("not_local")
    );
    assert_eq!(
        new(Some("gemma3:4b"), Some("host-only:1b"))
            .available()
            .await,
        unavailable("not_local")
    );
    assert_eq!(
        new(Some("gemma3:4b"), Some("absent:1b")).available().await,
        unavailable("model_missing")
    );
    // Nothing configured: nothing to run, nothing to leak.
    assert_eq!(new(None, None).available().await, unavailable("no_model"));
}

#[tokio::test]
async fn not_running_makes_it_unavailable() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let ollama = Ollama::new(
        &format!("http://127.0.0.1:{port}"),
        Some("gemma3:4b".into()),
        None,
    )
    .unwrap();
    assert!(matches!(
        ollama.available().await,
        Availability::Unavailable(_)
    ));
}

#[tokio::test]
async fn missing_model_is_failed_not_hang() {
    // The model is listed, then vanishes before the chat call.
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
        (
            "/api/chat",
            Reply::Json(404, json!({"error": "model not found"})),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();
    let start = Instant::now();
    match ollama.complete(&prompt(), CancellationToken::new()).await {
        Err(Error::Refused(reason)) => assert_eq!(reason, "model_not_found"),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn context_overflow_is_refused_as_prompt_too_large() {
    for wording in [
        "the input length exceeds the context length",
        "the prompt is longer than the context length currently available to the model",
    ] {
        let fake = Fake::start(vec![
            ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
            ("/api/chat", Reply::Json(400, json!({"error": wording}))),
        ]);
        let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();
        match ollama.complete(&prompt(), CancellationToken::new()).await {
            Err(Error::Refused(reason)) => assert_eq!(reason, "prompt_too_large"),
            other => panic!("{wording}: {:?}", other.map(|_| ())),
        }
    }
}

#[tokio::test]
async fn another_http_error_keeps_its_status_code() {
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
        (
            "/api/chat",
            Reply::Json(400, json!({"error": "bad format"})),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();
    match ollama.complete(&prompt(), CancellationToken::new()).await {
        Err(Error::Failed(reason)) => assert_eq!(reason, "http_400"),
        other => panic!("{:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn overflow_through_the_router_ends_manual_with_its_reason() {
    let (_tmp, db) = setup().await;
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
        (
            "/api/chat",
            Reply::Json(
                400,
                json!({"error": "the input length exceeds the context length"}),
            ),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();
    let router = Router::with_providers(vec![Box::new(ollama)]);

    let outcome = router
        .run(
            &db,
            "gui",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        Outcome::ManualRequired {
            reason: "prompt_too_large".into()
        }
    );
    assert_eq!(fake.hits("/api/chat").len(), 1);
}

#[tokio::test]
async fn complete_refuses_a_remote_model_without_calling_chat() {
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gpt-oss:120b"]))),
        (
            "/api/chat",
            Reply::Json(200, json!({"message": {"content": JOB}})),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gpt-oss:120b".into()), None).unwrap();
    assert!(matches!(
        ollama.complete(&prompt(), CancellationToken::new()).await,
        Err(Error::Refused(r)) if r == "not_local"
    ));
    assert!(fake.hits("/api/chat").is_empty());
}

#[tokio::test]
async fn pull_reports_progress_and_honours_cancel() {
    let (_tmp, db) = setup().await;
    let lines = vec![
        json!({"status": "pulling manifest"}).to_string(),
        json!({"status": "pulling abc", "total": 100, "completed": 10}).to_string(),
        json!({"status": "pulling abc", "total": 100, "completed": 60}).to_string(),
    ];
    let fake = Fake::start(vec![(
        "/api/pull",
        Reply::Stream(lines, Duration::from_secs(5)),
    )]);
    let ollama = Ollama::new(&fake.url, None, None).unwrap();

    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        later.cancel();
    });
    let mut seen: Vec<(u64, u64)> = Vec::new();
    let start = Instant::now();
    let result = ollama
        .pull(
            &db,
            "gui",
            "gemma3:4b",
            |done, total| seen.push((done, total)),
            cancel,
        )
        .await;
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(seen, vec![(10, 100), (60, 100)]);

    let sent = &fake.hits("/api/pull")[0];
    assert_eq!(sent["model"], json!("gemma3:4b"));
    assert_eq!(sent["stream"], json!(true));

    let rows = audit::recent(&db, 10).await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.action == "model_download_started")
        .unwrap();
    assert_eq!(row.actor, "gui");
    assert_eq!(row.detail, json!({"model": "gemma3:4b"}));
}

#[tokio::test]
async fn pull_completes_on_success_and_fails_on_error_line() {
    let (_tmp, db) = setup().await;
    let ok = Fake::start(vec![(
        "/api/pull",
        Reply::Stream(
            vec![
                json!({"status": "pulling", "total": 4, "completed": 4}).to_string(),
                json!({"status": "success"}).to_string(),
            ],
            Duration::ZERO,
        ),
    )]);
    let ollama = Ollama::new(&ok.url, None, None).unwrap();
    ollama
        .pull(&db, "cli", "gemma3:4b", |_, _| {}, CancellationToken::new())
        .await
        .unwrap();

    let bad = Fake::start(vec![(
        "/api/pull",
        Reply::Stream(
            vec![json!({"error": "pull model manifest: file does not exist"}).to_string()],
            Duration::ZERO,
        ),
    )]);
    let ollama = Ollama::new(&bad.url, None, None).unwrap();
    let result = ollama
        .pull(&db, "cli", "nope:1b", |_, _| {}, CancellationToken::new())
        .await;
    assert!(matches!(result, Err(Error::Failed(_))), "{result:?}");
}

#[tokio::test]
async fn pull_refuses_cloud_names_and_writes_no_audit_row() {
    let (_tmp, db) = setup().await;
    let ollama = Ollama::new(DEFAULT_URL, None, None).unwrap();
    let result = ollama
        .pull(&db, "gui", "x:cloud", |_, _| {}, CancellationToken::new())
        .await;
    assert!(matches!(result, Err(Error::Refused(r)) if r == "not_local"));
    let rows = audit::recent(&db, 10).await.unwrap();
    assert!(rows.iter().all(|r| r.action != "model_download_started"));
}

#[tokio::test]
async fn embed_posts_input_and_returns_vectors() {
    let fake = Fake::start(vec![
        ("/api/tags", Reply::Json(200, tag_routes(&["gemma3:4b"]))),
        (
            "/api/embed",
            Reply::Json(200, json!({"embeddings": [[0.5, 0.25], [1.0, 0.0]]})),
        ),
    ]);
    let ollama = Ollama::new(&fake.url, None, Some("gemma3:4b".into())).unwrap();
    let inputs = vec!["a".to_string(), "b".to_string()];
    let vectors = ollama
        .embed("gemma3:4b", &inputs, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(vectors, vec![vec![0.5, 0.25], vec![1.0, 0.0]]);
    let sent = &fake.hits("/api/embed")[0];
    assert_eq!(sent["model"], json!("gemma3:4b"));
    assert_eq!(sent["input"], json!(["a", "b"]));
}

#[test]
fn hardware_detect_reports_memory_and_fit_tiers() {
    let hw = hardware::detect();
    assert!(hw.total_memory > 0);
    assert!(hw.cpus > 0);
    assert!(!hw.gpu.is_empty());
    let sixteen = Hardware {
        total_memory: 16 * GIB,
        cpus: 8,
        gpu: String::new(),
        free_disk: 0,
    };
    assert_eq!(hardware::fit(GIB, &sixteen), Fit::Fits);
    assert_eq!(hardware::fit(13 * GIB, &sixteen), Fit::MaySlow);
    assert_eq!(hardware::fit(40 * GIB, &sixteen), Fit::TooLarge);
    // Edges: size*1.2 == 0.75*total is Fits; == total is MaySlow.
    assert_eq!(hardware::fit(10 * GIB, &sixteen), Fit::Fits);
    assert_eq!(hardware::fit(16 * GIB * 5 / 6, &sixteen), Fit::MaySlow);
    assert_eq!(
        hardware::fit(
            GIB,
            &Hardware {
                total_memory: 0,
                ..sixteen
            }
        ),
        Fit::TooLarge
    );
}

// Router: the embedding model's origin is checked as the drafting model's is.

async fn router_with(fake: &Fake, drafting: &str, embedding: &str) -> Router {
    let ollama = Ollama::new(&fake.url, Some(drafting.into()), Some(embedding.into())).unwrap();
    Router::with_providers(vec![Box::new(ollama)])
}

fn embed_routes(names: &[&str]) -> Vec<(&'static str, Reply)> {
    vec![
        ("/api/tags", Reply::Json(200, tag_routes(names))),
        (
            "/api/embed",
            Reply::Json(200, json!({"embeddings": [[1.0, 2.0]]})),
        ),
        (
            "/api/chat",
            Reply::Json(200, json!({"message": {"content": JOB}})),
        ),
    ]
}

#[tokio::test]
async fn router_embed_refuses_cloud_embedding_model() {
    let (_tmp, db) = setup().await;
    let fake = Fake::start(embed_routes(&["gemma3:4b", "x:cloud"]));
    settings::set(&db, "ai.embedding_model", &json!("x:cloud"))
        .await
        .unwrap();
    let router = router_with(&fake, "gemma3:4b", "x:cloud").await;
    let result = router
        .embed(&db, &["resume text".into()], CancellationToken::new())
        .await;
    assert!(
        matches!(result, Err(Error::ManualRequired(_))),
        "{result:?}"
    );
    assert!(
        fake.hits("/api/embed").is_empty(),
        "resume text must not be sent"
    );
}

#[tokio::test]
async fn router_embed_refuses_remote_row_even_when_settings_differ_from_provider() {
    // The provider was built with a local embedding model, but settings now name a remote one.
    let (_tmp, db) = setup().await;
    let fake = Fake::start(embed_routes(&["gemma3:4b", "gpt-oss:120b"]));
    settings::set(&db, "ai.embedding_model", &json!("gpt-oss:120b"))
        .await
        .unwrap();
    let router = router_with(&fake, "gemma3:4b", "gemma3:4b").await;
    let result = router
        .embed(&db, &["resume text".into()], CancellationToken::new())
        .await;
    assert!(result.is_err(), "{result:?}");
    assert!(fake.hits("/api/embed").is_empty());
}

#[tokio::test]
async fn router_embed_uses_a_local_embedding_model() {
    let (_tmp, db) = setup().await;
    let fake = Fake::start(embed_routes(&["gemma3:4b"]));
    settings::set(&db, "ai.embedding_model", &json!("gemma3:4b"))
        .await
        .unwrap();
    let router = router_with(&fake, "gemma3:4b", "gemma3:4b").await;
    let (model, vectors) = router
        .embed(&db, &["resume text".into()], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(model, "gemma3:4b");
    assert_eq!(vectors, vec![vec![1.0, 2.0]]);
}

#[tokio::test]
async fn cloud_embedding_model_also_stops_drafting_through_ollama() {
    let (_tmp, db) = setup().await;
    let fake = Fake::start(embed_routes(&["gemma3:4b", "x:cloud"]));
    let router = router_with(&fake, "gemma3:4b", "x:cloud").await;
    let outcome = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, Outcome::ManualRequired { .. }),
        "{outcome:?}"
    );
    assert!(fake.hits("/api/chat").is_empty());
}

#[tokio::test]
async fn router_drafts_through_local_ollama() {
    let (_tmp, db) = setup().await;
    let fake = Fake::start(embed_routes(&["gemma3:4b"]));
    let router = router_with(&fake, "gemma3:4b", "gemma3:4b").await;
    let outcome = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok { .. }), "{outcome:?}");
}

fn limited(status: u16, retry_after: Option<&str>) -> fake_http::Canned {
    let canned = fake_http::Canned::json(status, &json!({"error": "busy"}));
    match retry_after {
        Some(value) => canned.with_header("Retry-After", value),
        None => canned,
    }
}

fn chat_ok() -> fake_http::Canned {
    fake_http::Canned::json(
        200,
        &json!({"message": {"role": "assistant", "content": JOB}}),
    )
}

#[tokio::test]
async fn a_429_from_ollama_retries_after_its_header() {
    let (_tmp, db) = setup().await;
    let tags = fake_http::Canned::json(200, &tag_routes(&["gemma3:4b"]));
    let fake = fake_http::Fake::start(vec![
        ("/api/tags", tags),
        ("/api/chat", limited(429, Some("1"))),
        ("/api/chat", chat_ok()),
    ]);
    let ollama = Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap();
    // The error itself carries the header.
    match ollama.complete(&prompt(), CancellationToken::new()).await {
        Err(Error::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(Duration::from_secs(1)));
        }
        other => panic!("{:?}", other.map(|_| ())),
    }

    let router = Router::with_providers(vec![Box::new(ollama)]);
    let clock = Instant::now();
    let outcome = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, Outcome::Ok { attempts: 1, .. }),
        "{outcome:?}"
    );
    assert_eq!(fake.hits("/api/chat").len(), 2);
    assert!(clock.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn a_429_through_the_router_waits_the_header_then_retries() {
    let (_tmp, db) = setup().await;
    let tags = fake_http::Canned::json(200, &tag_routes(&["gemma3:4b"]));
    let fake = fake_http::Fake::start(vec![
        ("/api/tags", tags),
        ("/api/chat", limited(429, Some("1"))),
        ("/api/chat", chat_ok()),
    ]);
    let router = Router::with_providers(vec![Box::new(
        Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap(),
    )]);
    let clock = Instant::now();
    let outcome = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, Outcome::Ok { attempts: 2, .. }),
        "{outcome:?}"
    );
    assert!(clock.elapsed() >= Duration::from_millis(900));
}

#[tokio::test]
async fn a_503_from_ollama_retries() {
    let (_tmp, db) = setup().await;
    let tags = fake_http::Canned::json(200, &tag_routes(&["gemma3:4b"]));
    let fake = fake_http::Fake::start(vec![
        ("/api/tags", tags),
        ("/api/chat", limited(503, None)),
        ("/api/chat", chat_ok()),
    ]);
    let router = Router::with_providers(vec![Box::new(
        Ollama::new(&fake.url, Some("gemma3:4b".into()), None).unwrap(),
    )]);
    let outcome = router
        .run(
            &db,
            "cli",
            TaskType::ParseJob,
            prompt(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, Outcome::Ok { attempts: 2, .. }),
        "{outcome:?}"
    );
    assert_eq!(fake.hits("/api/chat").len(), 2);
}
