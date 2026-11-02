//! The chat wire: OpenAI, OpenRouter, and the loopback endpoint, each against a local fake.
//! No test here reaches a real host, a real key, or the keychain.

#[path = "fake_http/mod.rs"]
mod fake_http;

use std::time::{Duration, Instant};

use fake_http::{Canned, Fake};
use jobpilot_core::ai::api::{
    ApiProvider, KeySource, check_model, provider_schema, set_endpoint, set_model,
};
use jobpilot_core::ai::router::Router;
use jobpilot_core::ai::{
    self, Availability, CancellationToken, DataCategory, Outcome, Prompt, Provider, ProviderId,
    TaskType, consent, spec,
};
use jobpilot_core::secrets::Zeroizing;
use jobpilot_core::{AppPaths, Db, Error, settings};
use serde_json::{Value, json};
use tempfile::TempDir;

const PATH: &str = "/chat/completions";
const USAGE_CAP: &str = "You have reached your specified API usage limits. Access returns on x.";
const KEY: &str = "sk-test";
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

fn prompt() -> Prompt {
    Prompt {
        system: "system text".into(),
        user: "user text".into(),
        schema: "",
    }
}

fn parse_prompt() -> Prompt {
    Prompt {
        schema: spec(TaskType::ParseJob).schema,
        ..prompt()
    }
}

fn fixed() -> KeySource {
    KeySource::Fixed(Zeroizing::new(KEY.to_string()))
}

fn provider(name: &str, fake: &Fake) -> ApiProvider {
    let model = if name == "openrouter" {
        "openai/gpt-x"
    } else {
        "m"
    };
    ApiProvider::for_test(name, &fake.url, model, fixed()).unwrap()
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

/// The field names of an object, sorted, so a test holds the whole set: a new field that could
/// carry a tool or a search option fails it.
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

fn answer(message: Value, finish_reason: &str) -> Canned {
    let choice = json!({"message": message, "finish_reason": finish_reason});
    Canned::json(200, &json!({"choices": [choice]}))
}

fn says(text: &str) -> Canned {
    answer(json!({"role": "assistant", "content": text}), "stop")
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
    let runs: Vec<String> = sqlx::query_scalar(
        "SELECT provider || outcome || coalesce(reason, '') || data_categories FROM task_runs",
    )
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

#[tokio::test]
async fn openai_request_is_the_documented_shape() {
    let fake = Fake::start(vec![(PATH, says(JOB))]);
    let reply = provider("openai", &fake)
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(reply, JOB);

    let hits = fake.hits(PATH);
    let body = &hits[0];
    assert_eq!(
        keys(body),
        ["messages", "model", "response_format", "store"]
    );
    assert_eq!(keys(&body["response_format"]), ["json_schema", "type"]);
    assert_eq!(
        keys(&body["response_format"]["json_schema"]),
        ["name", "schema", "strict"]
    );
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    for message in body["messages"].as_array().unwrap() {
        assert_eq!(keys(message), ["content", "role"]);
    }
    assert_eq!(body["model"], "m");
    assert_eq!(body["response_format"]["type"], "json_schema");
    let schema = &body["response_format"]["json_schema"];
    assert_eq!(schema["name"], "jobpilot_output");
    assert_eq!(schema["strict"], true);
    assert_eq!(schema["schema"]["type"], "object");
    assert!(!schema["schema"].to_string().contains("maxLength"));
    assert_eq!(body["store"], false);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "system text");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "user text");
    for absent in [
        "tools",
        "tool_choice",
        "functions",
        "temperature",
        "max_tokens",
        "web_search_options",
        "provider",
    ] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    let headers = fake.headers(PATH);
    let bearer = format!("authorization: bearer {KEY}");
    assert!(headers.contains(&bearer), "{headers:?}");
}

#[tokio::test]
async fn openrouter_denies_data_collection_and_requires_parameters() {
    let fake = Fake::start(vec![(PATH, says(JOB))]);
    provider("openrouter", &fake)
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap();

    let hits = fake.hits(PATH);
    let body = &hits[0];
    assert_eq!(
        keys(body),
        ["messages", "model", "provider", "response_format"]
    );
    assert_eq!(
        keys(&body["provider"]),
        ["data_collection", "require_parameters"]
    );
    assert_eq!(keys(&body["response_format"]), ["json_schema", "type"]);
    assert_eq!(body["provider"]["data_collection"], "deny");
    assert_eq!(body["provider"]["require_parameters"], true);
    assert!(body.get("store").is_none());
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert!(body.get("tools").is_none());
}

#[tokio::test]
async fn a_200_holding_an_error_moves_on() {
    let (_tmp, db) = db().await;
    allow(&db, "openrouter").await;
    allow(&db, "openai").await;
    let error = json!({"error": {"code": 502, "message": "upstream ZXQ-VENDOR-TEXT"}});
    let broken = Fake::start(vec![(PATH, Canned::json(200, &error))]);
    let good = Fake::start(vec![(PATH, says(JOB))]);
    let router = router(vec![
        provider("openrouter", &broken),
        provider("openai", &good),
    ]);

    let outcome = parse(&router, &db).await;

    let Outcome::Ok { provider, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(provider, ProviderId::OpenAi);
    assert_eq!(broken.hits(PATH).len(), 1);
    assert_eq!(reasons(&db, "openrouter/api_key").await, ["bad_reply"]);
    assert!(!every_row(&db).await.contains("ZXQ"));
}

/// The text would pass validation, so only the error beside it can stop it.
#[tokio::test]
async fn an_error_beside_valid_text_is_bad_reply() {
    let message = json!({"content": JOB});
    let beside = json!({"choices": [{"message": message, "finish_reason": "stop"}],
                        "error": {"message": "ZXQ"}});
    let in_choice = json!({"choices": [{"message": message, "finish_reason": "stop",
                                        "error": {"message": "ZXQ"}}]});
    for reply in [beside, in_choice] {
        let (_tmp, db) = db().await;
        allow(&db, "openai").await;
        let fake = Fake::start(vec![(PATH, Canned::json(200, &reply))]);

        let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

        assert_eq!(outcome, manual("no_ai"), "{reply}");
        assert_eq!(reasons(&db, "openai/api_key").await, ["bad_reply"]);
    }
}

/// Every reply here holds text that would pass validation. None may be read as an answer.
#[tokio::test]
async fn a_reply_that_did_not_end_cleanly_is_not_an_answer() {
    let good = json!({"message": {"content": JOB}, "finish_reason": "stop"});
    let choices = |second: Value| Canned::json(200, &json!({"choices": [good, second]}));
    let cases = [
        // Only an empty list holds no call. Any other value is still a tool call field.
        (
            answer(json!({"content": JOB, "tool_calls": {}}), "stop"),
            "permission_denied",
        ),
        (
            answer(json!({"content": JOB, "tool_calls": {"id": "1"}}), "stop"),
            "permission_denied",
        ),
        (
            answer(json!({"content": JOB, "tool_calls": "x"}), "stop"),
            "permission_denied",
        ),
        (
            answer(json!({"content": JOB}), "tool_calls"),
            "permission_denied",
        ),
        (
            answer(json!({"content": JOB}), "function_call"),
            "permission_denied",
        ),
        (
            answer(json!({"content": JOB, "refusal": {"text": "no"}}), "stop"),
            "model_refused",
        ),
        (answer(json!({"content": JOB}), "error"), "bad_reply"),
        (
            answer(json!({"content": JOB}), "insufficient_system_resource"),
            "bad_reply",
        ),
        (
            choices(json!({"message": {"tool_calls": [{"id": "1"}]}, "finish_reason": "stop"})),
            "permission_denied",
        ),
        (
            choices(json!({"message": {"refusal": "no"}, "finish_reason": "stop"})),
            "model_refused",
        ),
        (
            choices(json!({"message": {}, "finish_reason": "content_filter"})),
            "model_refused",
        ),
        (
            choices(json!({"message": {}, "finish_reason": "length"})),
            "output_truncated",
        ),
        (choices(json!({"error": {"message": "ZXQ"}})), "bad_reply"),
    ];
    for (canned, reason) in cases {
        let body = canned.body.clone();
        let (_tmp, db) = db().await;
        allow(&db, "openai").await;
        let fake = Fake::start(vec![(PATH, canned)]);

        let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

        assert!(!matches!(outcome, Outcome::Ok { .. }), "{body}");
        assert_eq!(reasons(&db, "openai/api_key").await, [reason], "{body}");
    }
}

/// A local server may leave these out, and an empty refusal is no refusal.
#[tokio::test]
async fn a_clean_reply_still_reads() {
    let cases = [
        json!({"choices": [{"message": {"content": JOB}}]}),
        json!({"choices": [{"message": {"content": JOB}, "finish_reason": null}]}),
        json!({"choices": [{"message": {"content": JOB, "refusal": null, "tool_calls": null},
                            "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": JOB, "refusal": ""},
                            "finish_reason": "stop"}]}),
        json!({"choices": [{"message": {"content": JOB, "tool_calls": []},
                            "finish_reason": "stop"}]}),
    ];
    for reply in cases {
        let fake = Fake::start(vec![(PATH, Canned::json(200, &reply))]);
        let text = provider("openai", &fake)
            .complete(&parse_prompt(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text, JOB, "{reply}");
    }
}

#[tokio::test]
async fn a_200_that_is_not_json_is_bad_reply() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let mut canned = says(JOB);
    canned.body = "<html>ZXQ</html>".into();
    let fake = Fake::start(vec![(PATH, canned)]);

    let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(reasons(&db, "openai/api_key").await, ["bad_reply"]);
}

#[tokio::test]
async fn a_refusal_is_model_refused_and_stores_nothing() {
    let refusals = [
        answer(json!({"content": JOB, "refusal": "I will not"}), "stop"),
        answer(json!({"content": JOB}), "content_filter"),
    ];
    for canned in refusals {
        let (_tmp, db) = db().await;
        allow(&db, "openai").await;
        let fake = Fake::start(vec![(PATH, canned)]);

        let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

        assert_eq!(outcome, manual("model_refused"));
        assert_eq!(reasons(&db, "openai/api_key").await, ["model_refused"]);
        assert_eq!(fake.hits(PATH).len(), 1);
    }
}

#[tokio::test]
async fn a_tool_call_in_a_reply_stores_nothing() {
    let calls = json!([{"id": "1", "type": "function",
                        "function": {"name": "search", "arguments": "{}"}}]);
    let replies = [
        answer(json!({"content": JOB, "tool_calls": calls}), "tool_calls"),
        answer(
            json!({"content": JOB, "function_call": {"name": "search", "arguments": "{}"}}),
            "function_call",
        ),
    ];
    for canned in replies {
        let (_tmp, db) = db().await;
        allow(&db, "openai").await;
        let fake = Fake::start(vec![(PATH, canned)]);

        let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

        assert_eq!(outcome, manual("no_ai"));
        assert_eq!(reasons(&db, "openai/api_key").await, ["permission_denied"]);
    }
}

#[tokio::test]
async fn a_length_stop_is_truncated_not_retried() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let canned = answer(json!({"content": JOB}), "length");
    let fake = Fake::start(vec![(PATH, canned)]);

    let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

    assert_eq!(outcome, manual("output_truncated"));
    assert_eq!(reasons(&db, "openai/api_key").await, ["output_truncated"]);
    assert_eq!(fake.hits(PATH).len(), 1);
}

#[tokio::test]
async fn a_reply_over_the_cap_is_too_large() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let fake = Fake::start(vec![(PATH, says(&"x".repeat(4 * 1024 * 1024 + 1)))]);
    let provider = provider("openai", &fake);

    let error = provider
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "too_large");

    let outcome = parse(&router(vec![provider]), &db).await;
    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(reasons(&db, "openai/api_key").await, ["too_large"]);
}

/// The model's text takes the same road as every provider's: the router's schema check.
#[tokio::test]
async fn reply_text_goes_through_the_routers_validation() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let fake = Fake::start(vec![(PATH, says(r#"{"title": "not the schema"}"#))]);

    let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

    assert_eq!(outcome, manual("unusable_reply"));
    assert_eq!(
        reasons(&db, "openai/api_key").await,
        ["validation", "validation"]
    );
}

async fn error_for(canned: Canned) -> (Error, usize) {
    let fake = Fake::start(vec![(PATH, canned)]);
    let error = provider("openai", &fake)
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap_err();
    (error, fake.hits(PATH).len())
}

#[tokio::test]
async fn statuses_map_to_their_reasons() {
    let word = |text: &str| json!({"error": {"code": text}});
    let cases = [
        (401, json!({}), "refused: auth_failed"),
        (403, json!({}), "refused: auth_failed"),
        (402, json!({}), "refused: quota_exhausted"),
        (404, json!({}), "refused: model_not_found"),
        (429, word("insufficient_quota"), "refused: quota_exhausted"),
        (
            429,
            word("credit_balance_exhausted"),
            "refused: quota_exhausted",
        ),
        (429, word("spend_limit"), "rate limited"),
        (400, word("spend_limit"), "failed: http_400"),
        (429, word("usage_limit"), "refused: quota_exhausted"),
        (
            400,
            word("context_length_exceeded"),
            "refused: prompt_too_large",
        ),
        (
            400,
            word("maximum context length"),
            "refused: prompt_too_large",
        ),
        (413, word("prompt is too long"), "refused: prompt_too_large"),
        (
            400,
            word("exceeds the maximum number of tokens"),
            "refused: prompt_too_large",
        ),
        (400, word("something else"), "failed: http_400"),
        (413, json!({}), "failed: http_413"),
        (500, json!({}), "failed: http_500"),
        (302, json!({}), "failed: http_302"),
        (429, json!({}), "rate limited"),
        (503, json!({}), "rate limited"),
        (529, json!({}), "rate limited"),
    ];
    for (status, body, expected) in cases {
        let (error, hits) = error_for(Canned::json(status, &body)).await;
        assert_eq!(error.to_string(), expected, "{status} {body}");
        assert_eq!(hits, 1);
    }

    let canned = Canned::json(503, &json!({})).with_header("Retry-After", "7");
    let (error, _) = error_for(canned).await;
    let Error::RateLimited { retry_after } = error else {
        panic!("{error:?}");
    };
    assert_eq!(retry_after, Some(Duration::from_secs(7)));

    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let quota = Canned::json(429, &word("insufficient_quota"));
    let fake = Fake::start(vec![(PATH, quota)]);
    let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;
    assert_eq!(outcome, manual("quota_exhausted"));
    assert_eq!(fake.hits(PATH).len(), 1, "a quota error never retries");
}

/// Anthropic's and Google's words mean nothing here. A local server can echo any text it was
/// sent, a posting included, and that text must not pick the reason a task ends with.
#[tokio::test]
async fn another_vendors_words_do_not_steer_the_chat_wire() {
    let bodies = [
        json!({"error": {"code": "enforced_spend_limit_reached"}}),
        json!({"error": {"message": USAGE_CAP}}),
        json!({"error": {"details": [{"reason": "API_KEY_INVALID"}]}}),
    ];
    for name in ["openai", "openrouter", "endpoint"] {
        for body in &bodies {
            for (status, expected) in [(400, "failed: http_400"), (429, "rate limited")] {
                let fake = Fake::start(vec![(PATH, Canned::json(status, body))]);
                let error = provider(name, &fake)
                    .complete(&parse_prompt(), CancellationToken::new())
                    .await
                    .unwrap_err();
                assert_eq!(error.to_string(), expected, "{name} {status} {body}");
            }
        }
    }
}

/// Past 64 KiB the body is not read at all, but the status still says what happened.
#[tokio::test]
async fn an_error_body_past_its_cap_keeps_the_status_reason() {
    let big = "x".repeat(70 * 1024);
    let (error, _) = error_for(Canned::json(401, &json!({ "error": big }))).await;
    assert_eq!(error.to_string(), "refused: auth_failed");

    let late = format!("{big} insufficient_quota");
    let (error, _) = error_for(Canned::json(429, &json!({ "error": late }))).await;
    assert_eq!(error.to_string(), "rate limited", "the word was never read");
}

#[tokio::test]
async fn a_key_in_an_error_body_reaches_no_row_message_or_audit() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let echoed = "sk-ZXQ-echoed-key";
    let body = json!({"error": {"message": format!("bad key {echoed}")}});
    let fake = Fake::start(vec![(PATH, Canned::json(401, &body))]);
    let key = KeySource::Fixed(Zeroizing::new(echoed.to_string()));
    let openai = ApiProvider::for_test("openai", &fake.url, "m", key).unwrap();

    let error = openai
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("ZXQ"), "{error}");

    let outcome = parse(&router(vec![openai]), &db).await;

    assert_eq!(outcome, manual("auth_failed"));
    assert!(!format!("{outcome:?}").contains("ZXQ"));
    let rows = every_row(&db).await;
    assert!(rows.contains("auth_failed"), "{rows}");
    assert!(!rows.contains("ZXQ"), "{rows}");
}

/// The body stalls after the headers came, so this also proves the read is inside `bounded`:
/// `hosted_client` has no deadline of its own.
#[tokio::test]
async fn a_timeout_is_not_retried_on_a_hosted_provider() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let fake = Fake::start(vec![(PATH, Canned::stalled(Duration::from_secs(10)))]);
    let router = router(vec![provider("openai", &fake)]).with_timeout(Duration::from_millis(300));

    let clock = Instant::now();
    let outcome = parse(&router, &db).await;

    assert!(
        clock.elapsed() < Duration::from_secs(5),
        "{:?}",
        clock.elapsed()
    );
    assert_eq!(outcome, manual("no_ai"));
    assert_eq!(fake.hits(PATH).len(), 1);
    assert_eq!(reasons(&db, "openai/api_key").await, ["timeout"]);
}

#[tokio::test]
async fn a_cancel_ends_a_stalled_read() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let fake = Fake::start(vec![(PATH, Canned::stalled(Duration::from_secs(10)))]);
    let router = router(vec![provider("openai", &fake)]);
    let cancel = CancellationToken::new();
    let stop = async {
        while fake.hits(PATH).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
    };

    let clock = Instant::now();
    let run = router.run(
        &db,
        "test",
        TaskType::ParseJob,
        parse_prompt(),
        cancel.clone(),
    );
    let (outcome, ()) = tokio::join!(run, stop);

    assert_eq!(outcome.unwrap(), Outcome::Cancelled);
    assert!(clock.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn fallback_never_widens_consent() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let busy = Fake::start(vec![(PATH, Canned::json(503, &json!({})))]);
    let other = Fake::start(vec![(PATH, says(JOB))]);
    let router = router(vec![
        provider("openai", &busy),
        provider("openrouter", &other),
    ]);

    let outcome = parse(&router, &db).await;

    assert_eq!(outcome, manual("rate_limited"));
    assert_eq!(busy.hits(PATH).len(), 1);
    assert!(other.hits(PATH).is_empty());
    assert_eq!(reasons(&db, "openrouter/api_key").await, ["consent"]);
}

#[tokio::test]
async fn consent_revoked_stops_the_next_call() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let busy = Canned::json(429, &json!({})).with_header("Retry-After", "1");
    let fake = Fake::start(vec![(PATH, busy), (PATH, says(JOB))]);
    let router = router(vec![provider("openai", &fake)]);
    let revoke = async {
        while fake.hits(PATH).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        consent::revoke(&db, "openai/api_key", "test")
            .await
            .unwrap();
    };

    // Revoked during the wait for the second attempt.
    let (outcome, ()) = tokio::join!(parse(&router, &db), revoke);

    assert_eq!(outcome, manual("consent_needed"));
    assert_eq!(fake.hits(PATH).len(), 1);

    // And the next task.
    assert_eq!(parse(&router, &db).await, manual("consent_needed"));
    assert_eq!(fake.hits(PATH).len(), 1);
}

#[tokio::test]
async fn a_retry_after_the_router_can_obey_retries_once() {
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let busy = Canned::json(429, &json!({})).with_header("Retry-After", "0");
    let fake = Fake::start(vec![(PATH, busy), (PATH, says(JOB))]);

    let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

    assert!(
        matches!(outcome, Outcome::Ok { attempts: 2, .. }),
        "{outcome:?}"
    );
    assert_eq!(fake.hits(PATH).len(), 2);
}

/// Each tells the person: a hand-typed id must not read as "no AI". The provider behind still
/// gets its turn, and if it answers the reason never shows.
#[tokio::test]
async fn model_refused_and_model_not_found_end_with_their_own_reason() {
    let not_found = Canned::json(404, &json!({"error": "ZXQ"}));
    let refused = answer(json!({"refusal": "no"}), "stop");
    for (canned, code) in [(not_found, "model_not_found"), (refused, "model_refused")] {
        let (_tmp, db) = db().await;
        allow(&db, "openai").await;
        let fake = Fake::start(vec![(PATH, canned)]);

        let outcome = parse(&router(vec![provider("openai", &fake)]), &db).await;

        assert_eq!(outcome, manual(code));
        assert_eq!(reasons(&db, "openai/api_key").await, [code]);
    }

    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    allow(&db, "openrouter").await;
    let gone = Fake::start(vec![(PATH, Canned::json(404, &json!({})))]);
    let good = Fake::start(vec![(PATH, says(JOB))]);
    let router = router(vec![
        provider("openai", &gone),
        provider("openrouter", &good),
    ]);
    let outcome = parse(&router, &db).await;
    assert!(matches!(outcome, Outcome::Ok { .. }), "{outcome:?}");
}

/// `set_model` holds these rules at the door. A restored backup comes in another way.
#[tokio::test]
async fn a_stored_model_that_breaks_a_rule_sends_nothing() {
    let (_tmp, db) = db().await;
    allow(&db, "openrouter").await;
    let fake = Fake::start(vec![(PATH, says(JOB))]);
    for model in ["meta-llama/llama-x", "openai/gpt-x:free"] {
        let bad = ApiProvider::for_test("openrouter", &fake.url, model, fixed()).unwrap();
        let outcome = parse(&router(vec![bad]), &db).await;
        assert_eq!(outcome, manual("origin_refused"), "{model}");
    }
    let bad = ApiProvider::for_test("openrouter", &fake.url, "openai/x:online", fixed()).unwrap();
    assert_eq!(
        parse(&router(vec![bad]), &db).await,
        manual("model_has_tools")
    );
    let bad = ApiProvider::for_test("openrouter", &fake.url, "../x", fixed()).unwrap();
    assert_eq!(parse(&router(vec![bad]), &db).await, manual("no_ai"));
    assert!(fake.hits(PATH).is_empty());
    let cloud = ApiProvider::for_test("endpoint", &fake.url, "x-cloud", KeySource::None).unwrap();
    let error = cloud
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "refused: not_local");
    assert!(fake.hits(PATH).is_empty());
}

#[test]
fn every_schema_is_strict_ready_after_provider_schema() {
    fn check(value: &Value, path: &str) {
        match value {
            Value::Object(map) => {
                assert!(!map.contains_key("maxLength"), "{path}");
                if map
                    .get("type")
                    .is_some_and(|t| t.to_string().contains("object"))
                {
                    assert_eq!(map["additionalProperties"], false, "{path}");
                    let mut required: Vec<&str> = map["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|name| name.as_str().unwrap())
                        .collect();
                    let mut properties: Vec<&str> = map["properties"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str)
                        .collect();
                    required.sort_unstable();
                    properties.sort_unstable();
                    assert_eq!(required, properties, "{path}");
                }
                for (key, inner) in map {
                    check(inner, &format!("{path}/{key}"));
                }
            }
            Value::Array(items) => items.iter().for_each(|item| check(item, path)),
            _ => {}
        }
    }
    for task in [
        TaskType::ParseJob,
        TaskType::DraftResume,
        TaskType::DraftCoverLetter,
        TaskType::ClassifyAnswer,
    ] {
        let schema = spec(task).schema;
        check(&provider_schema(schema).unwrap(), schema);
    }
    assert!(spec(TaskType::DraftResume).schema.contains("maxLength"));
    assert_eq!(
        provider_schema("not json").unwrap_err().code(),
        "validation"
    );
}

#[test]
fn provider_schema_keeps_a_property_that_happens_to_be_named_max_length() {
    let schema = r#"{"properties": {"maxLength": {"type": "string", "maxLength": 3}}}"#;
    let cleaned = provider_schema(schema).unwrap();
    assert_eq!(
        cleaned,
        json!({"properties": {"maxLength": {"type": "string"}}})
    );
}

#[test]
fn check_model_holds_the_origin_rules() {
    for good in ["anthropic/claude-x", "openai/gpt-x", "google/gemini-x"] {
        check_model("openrouter", good).unwrap();
    }
    for bad in [
        "openrouter/auto",
        "meta-llama/llama-x",
        "openai/gpt-x:free",
        "google/x:free",
        "openai/gpt-x:FREE",
        "openai/gpt-x:free.",
        "openai/gpt-x:free:nitro",
        "openai/gpt-x:nitro:floor",
        "openai/gpt-x:unknown",
        "openai/gpt-x:",
        "openai/gpt-x:NITRO",
        "x",
    ] {
        assert_eq!(
            check_model("openrouter", bad).unwrap_err().to_string(),
            "refused: origin_refused",
            "{bad}"
        );
    }
    for good in ["openai/gpt-x:nitro", "anthropic/claude-x:floor"] {
        check_model("openrouter", good).unwrap();
    }
    // Each searches with no tools field, so `NoTools` would be false.
    for bad in [
        "openai/gpt-x:online",
        "openai/gpt-x:ONLINE",
        "openai/gpt-x:online:nitro",
        "openai/gpt-x:nitro:online",
        "openai/gpt-x:online/",
        "openai/gpt-4o-search-preview",
        "openai/o3-deep-research",
    ] {
        assert_eq!(
            check_model("openrouter", bad).unwrap_err().to_string(),
            "refused: model_has_tools",
            "{bad}"
        );
    }
    for (name, bad) in [
        ("openai", "gpt-4o-search-preview"),
        ("openai", "gpt-4o-mini-SEARCH-preview"),
        ("openai", "o3-deep-research"),
        ("anthropic", "claude-search-x"),
        ("anthropic", "claude-deep-research-x"),
        ("google", "deep-research-pro-preview"),
        ("google", "gemini-search-x"),
        ("anthropic", "claude-research-x"),
        ("google", "gemini-research-x"),
        ("anthropic", "claude-computer-use-x"),
        ("anthropic", "claude-COMPUTER-USE-x"),
        ("google", "gemini-2.5-computer-use-preview"),
        ("openai", "computer-use-preview"),
        ("openrouter", "openai/computer-use-preview"),
    ] {
        assert_eq!(
            check_model(name, bad).unwrap_err().to_string(),
            "refused: model_has_tools",
            "{name} {bad}"
        );
    }
    // The server is the user's own, and its names are too.
    check_model("endpoint", "research-search-7b").unwrap();
    check_model("openai", "gpt-5.1-mini").unwrap();
    check_model("openai", "ft:gpt-x:org/name").unwrap();
    check_model("openai", &"m".repeat(200)).unwrap();
    let long = "m".repeat(201);
    for bad in ["../v1", "a b", "a?x=1", "a#b", "", long.as_str(), "é"] {
        assert_eq!(check_model("openai", bad).unwrap_err().code(), "validation");
    }
}

#[tokio::test]
async fn endpoint_refuses_a_cloud_model_on_any_port() {
    for model in ["kimi:cloud", "x-cloud", "X:CLOUD", "kimi:cloud:latest"] {
        assert_eq!(
            check_model("endpoint", model).unwrap_err().to_string(),
            "refused: not_local"
        );
    }
    check_model("endpoint", "qwen2.5:7b").unwrap();
}

#[tokio::test]
async fn set_model_stores_a_good_id_per_provider_and_refuses_the_rest() {
    let (_tmp, db) = db().await;
    set_model(&db, "openrouter", "anthropic/claude-x")
        .await
        .unwrap();
    let stored = settings::get(&db, "ai.model.openrouter").await.unwrap();
    assert_eq!(stored, Some(json!("anthropic/claude-x")));

    let error = set_model(&db, "openrouter", "x").await.unwrap_err();
    assert_eq!(error.to_string(), "refused: origin_refused");
    let error = set_model(&db, "endpoint", "x:cloud").await.unwrap_err();
    assert_eq!(error.to_string(), "refused: not_local");
    assert_eq!(
        set_model(&db, "openai", "a b").await.unwrap_err().code(),
        "validation"
    );
    for no_model in ["ollama", "claude_code", "gemini_cli", "nobody"] {
        let error = set_model(&db, no_model, "m").await.unwrap_err();
        assert_eq!(error.code(), "validation", "{no_model}");
    }
    assert!(
        settings::get(&db, "ai.model.endpoint")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        settings::get(&db, "ai.model.gemini_cli")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_non_loopback_endpoint_is_refused_and_sends_nothing() {
    let (_tmp, db) = db().await;
    for url in [
        "https://api.example.com/v1",
        "http://192.168.1.2:1234/v1",
        "http://10.0.0.1/v1",
        "http://localhost.example.com/v1",
        "https://127.0.0.1:1234/v1",
    ] {
        assert_eq!(
            set_endpoint(&db, url).await.unwrap_err().to_string(),
            "refused: not_loopback"
        );
    }
    // A credential or query in the address would land in settings, and so in export.
    for url in [
        "http://user:secret@127.0.0.1:1234/v1",
        "http://user@127.0.0.1:1234/v1",
        "http://:secret@127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1?key=x",
        "http://127.0.0.1:1234/v1#x",
        // A key in the path would land there too.
        "http://127.0.0.1:1234/secret/v1",
        "http://127.0.0.1:1234/v1/secret",
        "http://127.0.0.1:1234/secret",
        "http://127.0.0.1:1234/v1beta",
        "http://127.0.0.1:1234/vsecret",
        "http://127.0.0.1:1234/v",
    ] {
        let error = set_endpoint(&db, url).await.unwrap_err();
        assert_eq!(error.code(), "validation");
        assert!(!error.to_string().contains("secret"), "{error}");
    }
    assert!(
        settings::get(&db, "ai.endpoint.url")
            .await
            .unwrap()
            .is_none()
    );
    for good in [
        "http://127.0.0.1:1234",
        "http://127.0.0.1:1234/v1",
        "http://[::1]:1/v2/",
    ] {
        set_endpoint(&db, good).await.unwrap();
    }
}

/// A restored backup does not pass `set_endpoint`, so a read holds the same rules.
#[tokio::test]
async fn a_stored_endpoint_that_breaks_a_rule_is_left_out() {
    let (_tmp, db) = db().await;
    assert!(
        ApiProvider::from_settings(&db, "endpoint")
            .await
            .unwrap()
            .is_none()
    );
    set_model(&db, "endpoint", "m").await.unwrap();
    for url in [
        "https://api.example.com/v1",
        "http://user:secret@127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1?key=x",
        "http://127.0.0.1:1234/sk-secret/v1",
        "",
    ] {
        settings::set(&db, "ai.endpoint.url", &json!(url))
            .await
            .unwrap();
        let found = ApiProvider::from_settings(&db, "endpoint").await.unwrap();
        assert!(found.is_none(), "{url}");
    }
}

#[tokio::test]
async fn clearing_the_endpoint_clears_both_settings() {
    let (_tmp, db) = db().await;
    set_endpoint(&db, "http://127.0.0.1:1234/v1").await.unwrap();
    set_model(&db, "endpoint", "m").await.unwrap();
    assert!(
        ApiProvider::from_settings(&db, "endpoint")
            .await
            .unwrap()
            .is_some()
    );

    set_endpoint(&db, "").await.unwrap();

    assert!(
        ApiProvider::from_settings(&db, "endpoint")
            .await
            .unwrap()
            .is_none()
    );
    let status = ai::status(&db).await.unwrap();
    let row = status
        .external
        .iter()
        .find(|row| row.name == "endpoint")
        .unwrap();
    assert_eq!(row.reason.as_deref(), Some("no_endpoint"));
    assert_eq!(row.model, None);
    assert_eq!(row.path, None);
}

async fn endpoint_row(db: &Db) -> ai::ExternalStatus {
    let status = ai::status(db).await.unwrap();
    let row = status
        .external
        .into_iter()
        .find(|row| row.name == "endpoint");
    row.unwrap()
}

/// Consent names an address. The router reads settings anew on each task, as each caller does.
#[tokio::test]
async fn a_changed_endpoint_address_gets_nothing_until_fresh_consent() {
    let (_tmp, db) = db().await;
    let first = Fake::start(vec![(PATH, says(JOB))]);
    let second = Fake::start(vec![(PATH, says(JOB))]);
    set_endpoint(&db, &first.url).await.unwrap();
    set_model(&db, "endpoint", "m").await.unwrap();
    ai::set_enabled(&db, "ollama", false).await.unwrap();
    ai::set_enabled(&db, "endpoint", true).await.unwrap();
    allow(&db, "endpoint").await;
    let run = async |db: &Db| parse(&Router::from_settings(db).await.unwrap(), db).await;
    assert!(matches!(run(&db).await, Outcome::Ok { .. }));
    assert_eq!(first.hits(PATH).len(), 1);

    set_endpoint(&db, &second.url).await.unwrap();

    assert_eq!(run(&db).await, manual("endpoint_changed"));
    assert!(second.hits(PATH).is_empty());
    assert_eq!(first.hits(PATH).len(), 1);
    assert_eq!(
        reasons(&db, "endpoint/loopback").await.last().unwrap(),
        "endpoint_changed"
    );
    let row = endpoint_row(&db).await;
    assert!(row.suspended);
    assert_eq!(row.consent, None);

    allow(&db, "endpoint").await;
    assert!(matches!(run(&db).await, Outcome::Ok { .. }));
    assert_eq!(second.hits(PATH).len(), 1);
}

#[tokio::test]
async fn clearing_the_address_suspends_its_consent_and_the_same_address_is_no_change() {
    let (_tmp, db) = db().await;
    let url = "http://127.0.0.1:1234/v1";
    set_endpoint(&db, url).await.unwrap();
    allow(&db, "endpoint").await;

    set_endpoint(&db, "http://127.0.0.1:1234/v1/")
        .await
        .unwrap();
    assert!(
        !endpoint_row(&db).await.suspended,
        "the stored form is the same"
    );

    set_endpoint(&db, "").await.unwrap();
    assert!(endpoint_row(&db).await.suspended);
}

#[tokio::test]
async fn the_consent_screen_names_the_stored_address() {
    let (_tmp, db) = db().await;
    assert_eq!(ai::sharing(&db, "endpoint").await.unwrap().path, None);
    set_endpoint(&db, "http://127.0.0.1:1234/v1/")
        .await
        .unwrap();
    let sharing = ai::sharing(&db, "endpoint").await.unwrap();
    assert_eq!(sharing.path.as_deref(), Some("http://127.0.0.1:1234/v1"));
    assert_eq!(ai::sharing(&db, "openai").await.unwrap().path, None);
}

#[tokio::test]
async fn an_endpoint_still_needs_consent() {
    let (_tmp, db) = db().await;
    let fake = Fake::start(vec![(PATH, says(JOB))]);
    let endpoint = ApiProvider::for_test("endpoint", &fake.url, "m", KeySource::None).unwrap();

    let outcome = parse(&router(vec![endpoint]), &db).await;

    assert!(fake.hits(PATH).is_empty());
    assert_eq!(outcome, manual("consent_needed"));
}

async fn endpoint_router(db: &Db, fake: &Fake) -> Router {
    set_endpoint(db, &format!("{}/v1//", fake.url))
        .await
        .unwrap();
    set_model(db, "endpoint", "local-model").await.unwrap();
    ai::set_enabled(db, "ollama", false).await.unwrap();
    ai::set_enabled(db, "endpoint", true).await.unwrap();
    allow(db, "endpoint").await;
    Router::from_settings(db).await.unwrap()
}

#[tokio::test]
async fn the_endpoint_runs_from_settings_with_one_slash_and_no_key() {
    const V1: &str = "/v1/chat/completions";
    let (_tmp, db) = db().await;
    let fake = Fake::start(vec![(V1, says(JOB))]);
    let router = endpoint_router(&db, &fake).await;
    assert_eq!(
        router.provider_ids(),
        [ProviderId::Endpoint, ProviderId::NoAi]
    );

    let outcome = parse(&router, &db).await;

    let Outcome::Ok { provider, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(provider, ProviderId::Endpoint);
    let hits = fake.hits(V1);
    assert_eq!(keys(&hits[0]), ["messages", "model", "response_format"]);
    assert_eq!(keys(&hits[0]["response_format"]), ["json_schema", "type"]);
    assert_eq!(hits[0]["model"], "local-model");
    assert!(hits[0].get("store").is_none());
    assert!(hits[0].get("provider").is_none());
    assert_eq!(hits[0]["response_format"]["json_schema"]["strict"], true);
    let headers = fake.headers(V1);
    assert!(
        !headers.iter().any(|line| line.starts_with("authorization")),
        "{headers:?}"
    );
    let settings_text = settings::get(&db, "ai.endpoint.url").await.unwrap();
    assert_eq!(settings_text, Some(json!(format!("{}/v1", fake.url))));
}

#[tokio::test]
async fn an_endpoint_key_is_sent_when_there_is_one() {
    let fake = Fake::start(vec![(PATH, says(JOB))]);
    provider("endpoint", &fake)
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap();
    let bearer = format!("authorization: bearer {KEY}");
    assert!(fake.headers(PATH).contains(&bearer));
}

/// A server on this machine may be a proxy, so its 3xx is an answer and goes nowhere.
#[tokio::test]
async fn a_redirect_is_not_followed() {
    let target = Fake::start(vec![(PATH, says(JOB))]);
    let moved =
        Canned::json(302, &json!({})).with_header("Location", &format!("{}{PATH}", target.url));
    let fake = Fake::start(vec![(PATH, moved)]);
    let endpoint = ApiProvider::for_test("endpoint", &fake.url, "m", KeySource::None).unwrap();

    let error = endpoint
        .complete(&parse_prompt(), CancellationToken::new())
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "failed: http_302");
    assert!(target.hits(PATH).is_empty());
}

#[tokio::test]
async fn declarations_hold_the_decided_numbers() {
    let fake = Fake::start(vec![]);
    let openai = provider("openai", &fake).declaration();
    assert_eq!(openai.id, ProviderId::OpenAi);
    assert!(!openai.local);
    assert_eq!(openai.context_limit, 200_000);
    assert_eq!(openai.timeout, Duration::from_secs(120));
    assert_eq!(openai.sign_in, "api key in the OS keychain");
    assert_eq!(
        openai.capabilities,
        ai::register::info("openai").unwrap().capabilities
    );
    let endpoint = provider("endpoint", &fake).declaration();
    assert_eq!(endpoint.id, ProviderId::Endpoint);
    assert!(!endpoint.local);
    assert_eq!(endpoint.context_limit, 8_192);
    assert_eq!(endpoint.timeout, Duration::from_secs(300));
    assert_eq!(endpoint.sign_in, "loopback server");
}

#[tokio::test]
async fn embedding_is_refused() {
    let fake = Fake::start(vec![]);
    let error = provider("openai", &fake)
        .embed("m", &["x".to_string()], CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "refused: no_embeddings");
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn for_test_refuses_a_real_host() {
    assert!(
        ApiProvider::for_test("openai", "https://api.openai.com/v1", "m", KeySource::None).is_err()
    );
    assert!(
        ApiProvider::for_test("openrouter", "http://example.com/v1", "m", KeySource::None).is_err()
    );
    let url = "http://127.0.0.1:1";
    assert!(ApiProvider::for_test("not_a_provider", url, "m", KeySource::None).is_err());
    assert!(ApiProvider::for_test("gemini_cli", url, "m", KeySource::None).is_err());
}

#[tokio::test]
async fn no_key_or_no_model_is_unavailable_with_no_socket() {
    let unavailable = |why: &str| Availability::Unavailable(why.into());
    let (_tmp, db) = db().await;
    allow(&db, "openai").await;
    let fake = Fake::start(vec![(PATH, says(JOB))]);

    let no_model = ApiProvider::for_test("openai", &fake.url, "", fixed()).unwrap();
    assert_eq!(no_model.available().await, unavailable("no_api_model"));
    assert_eq!(
        parse(&router(vec![no_model]), &db).await,
        manual("no_api_model")
    );

    let no_key = ApiProvider::for_test("openai", &fake.url, "m", KeySource::None).unwrap();
    assert_eq!(no_key.available().await, unavailable("no_key"));
    assert_eq!(parse(&router(vec![no_key]), &db).await, manual("no_key"));

    let endpoint = ApiProvider::for_test("endpoint", &fake.url, "m", KeySource::None).unwrap();
    assert_eq!(endpoint.available().await, Availability::Available);

    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn availability_comes_from_settings_and_the_index_alone() {
    async fn openai_row(db: &Db) -> (bool, Option<String>) {
        let status = ai::status(db).await.unwrap();
        let row = status.external.iter().find(|row| row.name == "openai");
        row.map(|row| (row.available, row.reason.clone())).unwrap()
    }
    let (_tmp, db) = db().await;
    assert_eq!(openai_row(&db).await, (false, Some("no_api_model".into())));

    set_model(&db, "openai", "gpt-x").await.unwrap();
    assert_eq!(openai_row(&db).await, (false, Some("no_key".into())));

    // The row alone, with nothing in any keychain: status must not look there.
    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES ('ai-key-openai', 'now')")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(openai_row(&db).await, (true, None));

    let status = ai::status(&db).await.unwrap();
    let row = |name: &str| status.external.iter().find(|row| row.name == name).unwrap();
    assert_eq!(row("openrouter").reason.as_deref(), Some("no_api_model"));
    assert_eq!(row("endpoint").reason.as_deref(), Some("no_endpoint"));
    assert_eq!(row("anthropic").reason.as_deref(), Some("no_api_model"));
    // A CLI row is judged by its program and never started while it is off (decision 28). A
    // directory named `gemini` is what the setting names, so the answer does not depend on
    // what this machine has installed: it is not a program, and discovery is not reached.
    let fake = _tmp.path().join("gemini");
    std::fs::create_dir_all(&fake).unwrap();
    jobpilot_core::settings::set(&db, "ai.gemini_path", &json!(fake.to_str().unwrap()))
        .await
        .unwrap();
    let status = ai::status(&db).await.unwrap();
    let gemini = status
        .external
        .iter()
        .find(|r| r.name == "gemini_cli")
        .unwrap();
    assert_eq!(
        (
            gemini.available,
            gemini.reason.as_deref(),
            gemini.path.as_deref()
        ),
        // Off, and the stored path already says why: no search would get past it.
        (false, Some("wrong_path"), None)
    );
}

#[tokio::test]
async fn the_router_lists_an_enabled_api_provider_and_leaves_out_a_missing_endpoint() {
    let (_tmp, db) = db().await;
    ai::set_enabled(&db, "ollama", false).await.unwrap();
    ai::set_enabled(&db, "openai", true).await.unwrap();
    ai::set_enabled(&db, "openrouter", true).await.unwrap();
    ai::set_enabled(&db, "endpoint", true).await.unwrap();

    let router = Router::from_settings(&db).await.unwrap();

    assert_eq!(
        router.provider_ids(),
        [ProviderId::OpenAi, ProviderId::OpenRouter, ProviderId::NoAi]
    );
}
