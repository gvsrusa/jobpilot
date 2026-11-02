//! The Anthropic and Google wires, each against a local fake. The rest of the road, consent and
//! retries and the capped read, is `api_chat.rs`'s. What is here pins that these wires are on it.
//! No test here reaches a real host, a real key, or the keychain.

#[path = "fake_http/mod.rs"]
mod fake_http;

use std::time::{Duration, Instant};

use fake_http::{Canned, Fake};
use jobpilot_core::ai::api::{ApiProvider, KeySource, check_model, set_model};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    self, CancellationToken, DataCategory, Outcome, Prompt, Provider, ProviderId, TaskType,
    consent, spec,
};
use jobpilot_core::secrets::Zeroizing;
use jobpilot_core::{AppPaths, Db, Error};
use serde_json::{Value, json};
use tempfile::TempDir;

const MESSAGES: &str = "/v1/messages";
const GENERATE: &str = "/v1beta/models/gemini-x:generateContent";
const CLAUDE_ROW: &str = "anthropic/api_key";
const GOOGLE_ROW: &str = "google/paid_key";
const JOB: &str = r#"{"title":"Engineer","company":"Acme","location":"Oslo","remote":"hybrid",
    "skills":["Rust"],"seniority":"senior","compensation":""}"#;
const DRAFTING: [DataCategory; 3] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::ResumeText,
];

async fn db() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn parse_prompt() -> Prompt {
    Prompt {
        system: "system text".into(),
        user: "user text".into(),
        schema: spec(TaskType::ParseJob).schema,
    }
}

fn fixed(key: &str) -> KeySource {
    KeySource::Fixed(Zeroizing::new(key.to_string()))
}

fn anthropic(fake: &Fake) -> ApiProvider {
    ApiProvider::for_test("anthropic", &fake.url, "claude-x", fixed("sk-ant-test")).unwrap()
}

fn google(fake: &Fake) -> ApiProvider {
    ApiProvider::for_test("google", &fake.url, "gemini-x", fixed("test-key")).unwrap()
}

fn router(providers: Vec<ApiProvider>) -> Router {
    let boxed = providers
        .into_iter()
        .map(|p| Box::new(p) as Box<dyn Provider>);
    Router::with_providers(boxed.collect())
}

async fn allow(db: &Db, name: &str) {
    consent::give_for(db, name, &DRAFTING, true, "test")
        .await
        .unwrap();
}

async fn parse(router: &Router, db: &Db) -> Outcome {
    let cancel = CancellationToken::new();
    router
        .run(db, "test", TaskType::ParseJob, parse_prompt(), cancel)
        .await
        .unwrap()
}

fn keys(value: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    names
}

fn manual(reason: &str) -> Outcome {
    Outcome::ManualRequired {
        reason: reason.into(),
    }
}

async fn complete(provider: &ApiProvider) -> Result<String, Error> {
    provider
        .complete(&parse_prompt(), CancellationToken::new())
        .await
}

fn claude_says(content: Value, stop_reason: &str) -> Canned {
    let reply = json!({"type": "message", "content": content, "stop_reason": stop_reason});
    Canned::json(200, &reply)
}

fn claude_text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn gemini_says(parts: Value, finish_reason: &str) -> Canned {
    let content = json!({"role": "model", "parts": parts});
    let candidate = json!({"content": content, "finishReason": finish_reason});
    Canned::json(200, &json!({"candidates": [candidate]}))
}

async fn reasons(db: &Db, provider: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT coalesce(reason, outcome) FROM task_runs WHERE provider = ? ORDER BY rowid",
    )
    .bind(provider)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

/// Every row a provider's text could have reached.
async fn every_row(db: &Db) -> String {
    let runs: Vec<String> =
        sqlx::query_scalar("SELECT provider || outcome || coalesce(reason, '') FROM task_runs")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action || coalesce(target, '') || coalesce(detail, '') FROM audit_log",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(!runs.is_empty());
    format!("{runs:?}{audit:?}")
}

async fn claude_reason(canned: Canned) -> (Outcome, Vec<String>, usize) {
    let (_tmp, db) = db().await;
    allow(&db, "anthropic").await;
    let fake = Fake::start(vec![(MESSAGES, canned)]);
    let outcome = parse(&router(vec![anthropic(&fake)]), &db).await;
    assert!(!every_row(&db).await.contains("ZXQ"));
    let hits = fake.hits(MESSAGES).len();
    (outcome, reasons(&db, CLAUDE_ROW).await, hits)
}

async fn gemini_reason(canned: Canned) -> (Outcome, Vec<String>, usize) {
    let (_tmp, db) = db().await;
    allow(&db, "google").await;
    let fake = Fake::start(vec![(GENERATE, canned)]);
    let outcome = parse(&router(vec![google(&fake)]), &db).await;
    assert!(!every_row(&db).await.contains("ZXQ"));
    let hits = fake.hits(GENERATE).len();
    (outcome, reasons(&db, GOOGLE_ROW).await, hits)
}

#[tokio::test]
async fn anthropic_request_is_the_documented_shape() {
    let reply = claude_says(json!([claude_text(JOB)]), "end_turn");
    let fake = Fake::start(vec![(MESSAGES, reply)]);
    assert_eq!(complete(&anthropic(&fake)).await.unwrap(), JOB);

    let hits = fake.hits(MESSAGES);
    let body = &hits[0];
    assert_eq!(
        keys(body),
        ["max_tokens", "messages", "model", "output_config", "system"]
    );
    assert_eq!(keys(&body["output_config"]), ["format"]);
    assert_eq!(keys(&body["output_config"]["format"]), ["schema", "type"]);
    assert_eq!(body["model"], "claude-x");
    assert_eq!(body["max_tokens"], 16000);
    assert_eq!(body["system"], "system text");
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "user text"}])
    );
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    let schema = &body["output_config"]["format"]["schema"];
    assert_eq!(schema["type"], "object");
    assert!(schema.to_string().find("maxLength").is_none());
    for absent in [
        "tools",
        "tool_choice",
        "functions",
        "output_format",
        "temperature",
        "response_format",
        "store",
    ] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    let head = fake.headers(MESSAGES);
    assert!(
        head.contains(&"x-api-key: sk-ant-test".to_string()),
        "{head:?}"
    );
    assert!(head.contains(&"anthropic-version: 2023-06-01".to_string()));
    assert!(!head.iter().any(|line| line.contains("anthropic-beta")));
    assert!(!head.iter().any(|line| line.starts_with("authorization")));
}

#[tokio::test]
async fn anthropic_reads_the_text_block_after_thinking() {
    let content = json!([
        {"type": "thinking", "thinking": "ZXQ-thought", "signature": "s", "text": "ZXQ"},
        {"type": "redacted_thinking", "data": "ZXQ"},
        claude_text(JOB),
    ]);
    let fake = Fake::start(vec![(MESSAGES, claude_says(content, "end_turn"))]);

    assert_eq!(complete(&anthropic(&fake)).await.unwrap(), JOB);
}

#[tokio::test]
async fn anthropic_refusal_and_max_tokens_map() {
    let content = json!([claude_text(JOB)]);
    let refused = claude_reason(claude_says(content.clone(), "refusal")).await;
    assert_eq!(
        refused,
        (
            manual("model_refused"),
            vec!["model_refused".to_string()],
            1
        )
    );

    let cut = claude_reason(claude_says(content, "max_tokens")).await;
    assert_eq!(
        cut,
        (manual("no_ai"), vec!["output_truncated".to_string()], 1)
    );
}

/// Every reply holds text that would pass validation.
#[tokio::test]
async fn an_anthropic_reply_that_did_not_end_cleanly_is_not_an_answer() {
    let text = json!([claude_text(JOB)]);
    for (stop, reason) in [
        ("tool_use", "permission_denied"),
        ("pause_turn", "permission_denied"),
        ("model_context_window_exceeded", "output_truncated"),
        ("something_new", "bad_reply"),
        ("", "bad_reply"),
    ] {
        let (outcome, rows, _) = claude_reason(claude_says(text.clone(), stop)).await;
        assert!(!matches!(outcome, Outcome::Ok { .. }), "{stop}");
        assert_eq!(rows, [reason], "{stop}");
    }
    for reply in [
        json!({"content": text}),
        json!({"content": text, "stop_reason": null}),
    ] {
        let (_, rows, _) = claude_reason(Canned::json(200, &reply)).await;
        assert_eq!(rows, ["bad_reply"], "{reply}");
    }
    for stop in ["end_turn", "stop_sequence"] {
        let fake = Fake::start(vec![(MESSAGES, claude_says(text.clone(), stop))]);
        assert_eq!(complete(&anthropic(&fake)).await.unwrap(), JOB, "{stop}");
    }
}

#[tokio::test]
async fn a_tool_use_block_stores_nothing() {
    for kind in [
        "tool_use",
        "server_tool_use",
        "mcp_tool_use",
        "tool_result",
        "web_search_tool_result",
        "mcp_tool_result",
    ] {
        // The text would pass validation: the block is what refuses it, wherever it sits.
        let call = json!({"type": kind, "id": "1", "name": "search", "input": {}});
        for content in [
            json!([claude_text(JOB), call]),
            json!([call, claude_text(JOB)]),
        ] {
            let (outcome, rows, hits) = claude_reason(claude_says(content, "end_turn")).await;
            assert_eq!(outcome, manual("no_ai"), "{kind}");
            assert_eq!(rows, ["permission_denied"], "{kind}");
            assert_eq!(hits, 1);
        }
    }
}

#[tokio::test]
async fn a_529_with_retry_after_retries_once() {
    let (_tmp, db) = db().await;
    allow(&db, "anthropic").await;
    let busy = Canned::json(529, &json!({"type": "error"})).with_header("Retry-After", "1");
    let good = claude_says(json!([claude_text(JOB)]), "end_turn");
    let fake = Fake::start(vec![(MESSAGES, busy), (MESSAGES, good)]);

    let clock = Instant::now();
    let outcome = parse(&router(vec![anthropic(&fake)]), &db).await;

    assert!(matches!(outcome, Outcome::Ok { .. }), "{outcome:?}");
    assert_eq!(fake.hits(MESSAGES).len(), 2);
    assert!(clock.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn a_spend_cap_429_with_no_header_does_not_loop() {
    let body = json!({"type": "error", "error": {"type": "rate_limit_error",
                                                 "message": "ZXQ limit reached"}});
    let (outcome, _, hits) = claude_reason(Canned::json(429, &body)).await;
    assert_eq!(outcome, manual("rate_limited"));
    assert_eq!(hits, 1);

    let body = json!({"error": {"type": "credit_balance_exhausted"}});
    let (outcome, _, hits) = claude_reason(Canned::json(429, &body)).await;
    assert_eq!(outcome, manual("quota_exhausted"));
    assert_eq!(hits, 1);
}

#[tokio::test]
async fn a_spend_cap_400_is_quota_exhausted() {
    let message = "You have reached your specified API usage limits. You will regain access soon.";
    let cases = [
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}}),
        json!({"error": {"message": "ZXQ"}, "error_code": "enforced_spend_limit_reached"}),
    ];
    for body in cases {
        let (outcome, rows, hits) = claude_reason(Canned::json(400, &body)).await;
        assert_eq!(outcome, manual("quota_exhausted"), "{body}");
        assert_eq!(rows, ["quota_exhausted"], "{body}");
        assert_eq!(hits, 1);
    }
}

#[tokio::test]
async fn each_vendors_words_count_only_on_its_own_wire() {
    const CAP: &str = "You have reached your specified API usage limits. Access returns on x.";
    let spend = [
        json!({"error": {"code": "enforced_spend_limit_reached"}}),
        json!({"error": {"message": CAP}}),
    ];
    let bad_key = json!({"error": {"details": [{"reason": "API_KEY_INVALID"}]}});
    for body in &spend {
        for status in [400, 429] {
            let fake = Fake::start(vec![(MESSAGES, Canned::json(status, body))]);
            let error = complete(&anthropic(&fake)).await.unwrap_err();
            assert_eq!(error.to_string(), "refused: quota_exhausted", "{body}");

            let fake = Fake::start(vec![(GENERATE, Canned::json(status, body))]);
            let error = complete(&google(&fake)).await.unwrap_err();
            let expected = if status == 400 {
                "failed: http_400"
            } else {
                "rate limited"
            };
            assert_eq!(error.to_string(), expected, "{status} {body}");
        }
    }
    let fake = Fake::start(vec![(MESSAGES, Canned::json(400, &bad_key))]);
    let error = complete(&anthropic(&fake)).await.unwrap_err();
    assert_eq!(error.to_string(), "failed: http_400");
    let fake = Fake::start(vec![(GENERATE, Canned::json(400, &bad_key))]);
    let error = complete(&google(&fake)).await.unwrap_err();
    assert_eq!(error.to_string(), "refused: auth_failed");
}

#[tokio::test]
async fn an_anthropic_reply_with_no_text_is_bad_reply() {
    let thinking_only = json!([{"type": "thinking", "thinking": "ZXQ"}]);
    let error = json!({"type": "error", "error": {"message": "ZXQ-VENDOR-TEXT"}});
    let error_beside_text = json!({"content": [claude_text(JOB)], "error": {"message": "ZXQ"}});
    for canned in [
        claude_says(thinking_only, "end_turn"),
        claude_says(json!("a string, not a list"), "end_turn"),
        Canned::json(200, &error),
        Canned::json(200, &error_beside_text),
    ] {
        let (outcome, rows, hits) = claude_reason(canned).await;
        assert_eq!(outcome, manual("no_ai"));
        assert_eq!(rows, ["bad_reply"]);
        assert_eq!(hits, 1);
    }
}

#[tokio::test]
async fn google_request_is_the_documented_shape() {
    let reply = gemini_says(json!([{"text": JOB}]), "STOP");
    let fake = Fake::start(vec![(GENERATE, reply)]);
    assert_eq!(complete(&google(&fake)).await.unwrap(), JOB);

    let hits = fake.hits(GENERATE);
    let body = &hits[0];
    assert_eq!(
        keys(body),
        ["contents", "generationConfig", "systemInstruction"]
    );
    let config = &body["generationConfig"];
    assert_eq!(keys(config), ["responseJsonSchema", "responseMimeType"]);
    assert_eq!(config["responseMimeType"], "application/json");
    assert_eq!(config["responseJsonSchema"]["type"], "object");
    assert!(
        !config["responseJsonSchema"]
            .to_string()
            .contains("maxLength")
    );
    assert!(config.get("responseSchema").is_none());
    assert_eq!(
        body["systemInstruction"],
        json!({"parts": [{"text": "system text"}]})
    );
    assert_eq!(
        body["contents"],
        json!([{"role": "user", "parts": [{"text": "user text"}]}])
    );
    for absent in ["tools", "toolConfig", "model", "messages", "safetySettings"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    let head = fake.headers(GENERATE);
    assert!(
        head.contains(&"x-goog-api-key: test-key".to_string()),
        "{head:?}"
    );
    assert!(!head.iter().any(|line| line.starts_with("authorization")));
    assert!(!fake.paths().iter().any(|path| path.contains("key=")));
    assert_eq!(fake.paths(), [GENERATE]);
}

#[tokio::test]
async fn google_model_cannot_change_the_path() {
    for bad in ["x/../y", "models/x", "x:streamGenerateContent", "a/b"] {
        assert_eq!(check_model("google", bad).unwrap_err().code(), "validation");
    }
    check_model("google", "gemini-2.5-flash").unwrap();
    // `/` and `:` stay open on the wires whose id is not in the path.
    check_model("anthropic", "ft:claude/x").unwrap();
}

/// A restored backup puts a model in settings past `set_model`.
#[tokio::test]
async fn a_bad_stored_google_model_sends_nothing() {
    let fake = Fake::start(vec![]);
    let bad = ApiProvider::for_test("google", &fake.url, "x/../y", fixed("test-key")).unwrap();

    assert_eq!(complete(&bad).await.unwrap_err().code(), "validation");
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn google_joins_its_text_parts_and_skips_thoughts() {
    let (start, end) = JOB.split_at(20);
    let parts = json!([{"text": "ZXQ", "thought": true}, {"text": start}, {"text": end}]);
    let fake = Fake::start(vec![(GENERATE, gemini_says(parts, "STOP"))]);

    assert_eq!(complete(&google(&fake)).await.unwrap(), JOB);
}

#[tokio::test]
async fn google_finish_reasons_and_blocks_map() {
    let text = json!([{"text": JOB}]);
    for (finish, reason) in [
        ("MAX_TOKENS", "output_truncated"),
        ("SAFETY", "model_refused"),
        ("RECITATION", "model_refused"),
        ("BLOCKLIST", "model_refused"),
        ("PROHIBITED_CONTENT", "model_refused"),
        ("SPII", "model_refused"),
    ] {
        let (outcome, rows, hits) = gemini_reason(gemini_says(text.clone(), finish)).await;
        let end = if reason == "model_refused" {
            reason
        } else {
            "no_ai"
        };
        assert_eq!(outcome, manual(end), "{finish}");
        assert_eq!(rows, [reason], "{finish}");
        assert_eq!(hits, 1);
    }

    let blocked = json!({"promptFeedback": {"blockReason": "SAFETY"}});
    let (_, rows, _) = gemini_reason(Canned::json(200, &blocked)).await;
    assert_eq!(rows, ["model_refused"]);

    // A block beside a candidate still blocks.
    let candidate = json!({"content": {"parts": text}, "finishReason": "STOP"});
    let both = json!({"candidates": [candidate], "promptFeedback": {"blockReason": "OTHER"}});
    let (_, rows, _) = gemini_reason(Canned::json(200, &both)).await;
    assert_eq!(rows, ["model_refused"]);
}

/// Every reply holds text that would pass validation in its first candidate.
#[tokio::test]
async fn a_google_reply_that_did_not_end_cleanly_is_not_an_answer() {
    let text = json!([{"text": JOB}]);
    for (finish, reason) in [
        ("MALFORMED_FUNCTION_CALL", "permission_denied"),
        ("UNEXPECTED_TOOL_CALL", "permission_denied"),
        ("TOO_MANY_TOOL_CALLS", "permission_denied"),
        ("CONTINUATION", "output_truncated"),
        ("OTHER", "bad_reply"),
        ("SOMETHING_NEW", "bad_reply"),
        ("FINISH_REASON_UNSPECIFIED", "bad_reply"),
    ] {
        let (outcome, rows, _) = gemini_reason(gemini_says(text.clone(), finish)).await;
        assert!(!matches!(outcome, Outcome::Ok { .. }), "{finish}");
        assert_eq!(rows, [reason], "{finish}");
    }
    let good = json!({"content": {"parts": text}, "finishReason": "STOP"});
    let call = json!({"functionCall": {"name": "search", "args": {}}});
    for (second, reason) in [
        (
            json!({"content": {"parts": [call]}, "finishReason": "STOP"}),
            "permission_denied",
        ),
        (json!({"finishReason": "SAFETY"}), "model_refused"),
        (json!({"finishReason": "MAX_TOKENS"}), "output_truncated"),
    ] {
        let reply = json!({"candidates": [good, second]});
        let (_, rows, _) = gemini_reason(Canned::json(200, &reply)).await;
        assert_eq!(rows, [reason], "{reply}");
    }
    // With no finish the model has not said it stopped, and only a normal end accepts text.
    let open = json!({"candidates": [{"content": {"parts": text}}]});
    let (_, rows, _) = gemini_reason(Canned::json(200, &open)).await;
    assert_eq!(rows, ["bad_reply"]);
}

#[tokio::test]
async fn a_function_call_part_stores_nothing() {
    let call = json!({"functionCall": {"name": "search", "args": {}}});
    let code = json!({"executableCode": {"language": "PYTHON", "code": "print(1)"}});
    let others = [
        "functionResponse",
        "toolCall",
        "toolResponse",
        "codeExecutionResult",
    ];
    let others = others.map(|name| json!({ name: {} }));
    for part in [call, code].into_iter().chain(others) {
        for parts in [json!([{"text": JOB}, part]), json!([part, {"text": JOB}])] {
            let (outcome, rows, hits) = gemini_reason(gemini_says(parts, "STOP")).await;
            assert_eq!(outcome, manual("no_ai"));
            assert_eq!(rows, ["permission_denied"]);
            assert_eq!(hits, 1);
        }
    }
}

/// Forum sources only, so Task 11 confirms this body. Without the reason it stays `http_400`.
#[tokio::test]
async fn a_bad_google_key_is_auth_failed_and_any_other_400_stays_http_400() {
    let detail = json!({"@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "reason": "API_KEY_INVALID", "domain": "googleapis.com"});
    let error = |details: Value| {
        let error = json!({"code": 400, "message": "ZXQ", "status": "INVALID_ARGUMENT",
                           "details": details});
        Canned::json(400, &json!({ "error": error }))
    };
    let (outcome, rows, hits) = gemini_reason(error(json!([detail]))).await;
    assert_eq!(outcome, manual("auth_failed"));
    assert_eq!(rows, ["auth_failed"]);
    assert_eq!(hits, 1);

    let fake = Fake::start(vec![(GENERATE, error(json!([])))]);
    let failed = complete(&google(&fake)).await.unwrap_err();
    assert_eq!(failed.to_string(), "failed: http_400");
}

#[tokio::test]
async fn a_google_reply_with_no_text_is_bad_reply() {
    let error = json!({"error": {"code": 500, "message": "ZXQ-VENDOR-TEXT"}});
    for canned in [
        Canned::json(200, &json!({"candidates": []})),
        Canned::json(200, &json!({"candidates": [{"finishReason": "STOP"}]})),
        gemini_says(json!([]), "STOP"),
        gemini_says(json!([{"text": "ZXQ", "thought": true}]), "STOP"),
        gemini_says(json!([{"inlineData": {"data": "ZXQ"}}]), "STOP"),
        Canned::json(200, &error),
    ] {
        let (outcome, rows, hits) = gemini_reason(canned).await;
        assert_eq!(outcome, manual("no_ai"));
        assert_eq!(rows, ["bad_reply"]);
        assert_eq!(hits, 1);
    }
}

#[tokio::test]
async fn a_google_key_with_no_attestation_sends_nothing() {
    // The exit test's refusal: key row present, enabled, model set, no consent because give_for
    // refused. The row alone, with nothing in any keychain.
    let (_tmp, db) = db().await;
    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES ('ai-key-google', 'now')")
        .execute(&db.pool)
        .await
        .unwrap();
    set_model(&db, "google", "gemini-x").await.unwrap();
    ai::set_enabled(&db, "google", true).await.unwrap();

    let refused = consent::give_for(&db, "google", &DRAFTING, false, "test").await;
    assert_eq!(
        refused.unwrap_err().to_string(),
        "refused: attestation_needed"
    );

    let status = ai::status(&db).await.unwrap();
    let row = status.external.iter().find(|row| row.name == "google");
    assert!(row.unwrap().available);
    assert!(row.unwrap().consent.is_none());

    let fake = Fake::start(vec![(
        GENERATE,
        gemini_says(json!([{"text": JOB}]), "STOP"),
    )]);
    let outcome = parse(&router(vec![google(&fake)]), &db).await;

    assert_eq!(outcome, manual("consent_needed"));
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn reply_text_goes_through_the_routers_validation() {
    let wrong = r#"{"title": "not the schema"}"#;
    let (outcome, rows, _) =
        claude_reason(claude_says(json!([claude_text(wrong)]), "end_turn")).await;
    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(rows, ["validation", "validation"]);

    let (outcome, rows, _) = gemini_reason(gemini_says(json!([{"text": wrong}]), "STOP")).await;
    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(rows, ["validation", "validation"]);
}

fn both(fake_for: impl Fn(&'static str) -> Fake) -> [(ApiProvider, Fake, &'static str); 2] {
    let (claude, gemini) = (fake_for(MESSAGES), fake_for(GENERATE));
    [
        (anthropic(&claude), claude, MESSAGES),
        (google(&gemini), gemini, GENERATE),
    ]
}

#[tokio::test]
async fn both_wires_cap_the_reply_and_its_error_body() {
    let huge = "x".repeat(4 * 1024 * 1024 + 1);
    let replies = both(|path| Fake::start(vec![(path, Canned::json(200, &json!({"a": huge})))]));
    for (provider, _fake, _) in replies {
        assert_eq!(complete(&provider).await.unwrap_err().code(), "too_large");
    }

    let late = format!("{} insufficient_quota", "x".repeat(70 * 1024));
    let late = json!({ "error": late });
    let errors = both(|path| Fake::start(vec![(path, Canned::json(429, &late))]));
    for (provider, _fake, _) in errors {
        let error = complete(&provider).await.unwrap_err();
        assert_eq!(error.to_string(), "rate limited", "the word was never read");
    }
}

/// The body stalls after the headers came, so only `bounded` around the read can end this.
#[tokio::test]
async fn both_wires_read_inside_the_attempt_deadline() {
    for (provider, fake, path) in
        both(|path| Fake::start(vec![(path, Canned::stalled(Duration::from_secs(10)))]))
    {
        let (_tmp, db) = db().await;
        let name = if path == MESSAGES {
            "anthropic"
        } else {
            "google"
        };
        allow(&db, name).await;
        let router = router(vec![provider]).with_timeout(Duration::from_millis(300));

        let clock = Instant::now();
        let outcome = parse(&router, &db).await;

        assert!(clock.elapsed() < Duration::from_secs(5));
        assert_eq!(outcome, manual("no_ai"));
        assert_eq!(fake.hits(path).len(), 1);
    }
}

#[tokio::test]
async fn both_wires_keep_a_vendors_error_text_and_the_key_out_of_every_error() {
    let body = json!({"error": {"message": "bad key ZXQ test-key sk-ant-test"}});
    for (status, expected) in [
        (401, "refused: auth_failed"),
        (403, "refused: auth_failed"),
        (404, "refused: model_not_found"),
        (500, "failed: http_500"),
    ] {
        for (provider, _fake, _) in
            both(|path| Fake::start(vec![(path, Canned::json(status, &body))]))
        {
            let error = complete(&provider).await.unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }
}

/// Status never reads a key and the router opens no socket, so a hosted client is safe here.
#[tokio::test]
async fn both_vendors_run_from_settings() {
    let (_tmp, db) = db().await;
    ai::set_enabled(&db, "ollama", false).await.unwrap();
    ai::set_enabled(&db, "anthropic", true).await.unwrap();
    ai::set_enabled(&db, "google", true).await.unwrap();

    let router = Router::from_settings(&db).await.unwrap();
    assert_eq!(
        router.provider_ids(),
        [ProviderId::Anthropic, ProviderId::Google, ProviderId::NoAi]
    );

    for name in ["anthropic", "google"] {
        let reason = async |db: &Db| {
            let status = ai::status(db).await.unwrap();
            let row = status.external.iter().find(|row| row.name == name);
            row.unwrap().reason.clone()
        };
        assert_eq!(reason(&db).await.as_deref(), Some("no_api_model"));
        set_model(&db, name, "m").await.unwrap();
        assert_eq!(reason(&db).await.as_deref(), Some("no_key"));
    }
}
