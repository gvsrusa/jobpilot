//! The two HTTP clients and `Retry-After`. Every server is a local fake on 127.0.0.1.

mod fake_http;

use std::time::Duration;

use fake_http::{Canned, Fake};
use jobpilot_core::ai::net::{hosted_client, loopback_client, retry_after};
use jobpilot_core::ai::ollama::{Ollama, OllamaStatus};
use serde_json::json;

#[test]
fn retry_after_only_trusts_small_whole_seconds() {
    assert_eq!(retry_after(Some("2")), Some(Duration::from_secs(2)));
    assert_eq!(retry_after(Some(" 0 ")), Some(Duration::ZERO));
    for bad in [
        "-1",
        "1.5",
        "soon",
        "",
        "Wed, 21 Oct 2026 07:28:00 GMT",
        "99999999999999999999",
    ] {
        assert_eq!(retry_after(Some(bad)), None, "{bad}");
    }
    assert_eq!(retry_after(None), None);
}

#[tokio::test]
async fn https_ollama_address_stays_refused() {
    let error = Ollama::new("https://127.0.0.1:11434", None, None)
        .err()
        .unwrap();
    assert_eq!(error.code(), "refused");
}

#[test]
fn loopback_client_refuses_every_other_address() {
    for bad in [
        "https://127.0.0.1:1",
        "http://example.com",
        "http://10.0.0.1:11434",
        "http://0.0.0.0:11434",
        "not a url",
        "ftp://127.0.0.1",
    ] {
        let error = loopback_client(bad).err().unwrap();
        assert_eq!(error.code(), "refused", "{bad}");
    }
    for good in [
        "http://127.0.0.1:11434",
        "http://[::1]:11434",
        "http://localhost:11434",
    ] {
        assert!(loopback_client(good).is_ok(), "{good}");
    }
}

#[tokio::test]
async fn no_client_follows_a_redirect() {
    let second = Fake::start(vec![("/x", Canned::json(200, &json!({"version": "9"})))]);
    let location = format!("{}/x", second.url);
    let first = Fake::start(vec![
        (
            "/api/version",
            Canned::json(302, &json!({})).with_header("Location", &location),
        ),
        (
            "/y",
            Canned::json(302, &json!({})).with_header("Location", &location),
        ),
    ]);

    let ollama = Ollama::new(&first.url, None, None).unwrap();
    assert!(matches!(
        ollama.status().await,
        OllamaStatus::NotRunning { .. }
    ));
    assert_eq!(first.paths(), ["/api/version"]);

    let (url, client) = loopback_client(&first.url).unwrap();
    let response = client.get(format!("{url}y")).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 302);

    assert!(second.hits("/x").is_empty());
    assert!(second.paths().is_empty());
}

#[tokio::test]
async fn hosted_client_refuses_plain_http() {
    let fake = Fake::start(vec![("/x", Canned::json(200, &json!({})))]);
    let client = hosted_client().unwrap();
    let error = client.get(format!("{}/x", fake.url)).send().await;
    assert!(error.is_err(), "a plain http request must not be sent");
    assert!(fake.paths().is_empty());
}

#[tokio::test]
async fn hosted_client_does_not_follow_a_redirect() {
    // https_only refuses http before any redirect could matter; the policy is still pinned
    // by the builder, and this keeps the 302 from ever being followed to the second fake.
    let second = Fake::start(vec![("/x", Canned::json(200, &json!({})))]);
    let first = Fake::start(vec![(
        "/y",
        Canned::json(302, &json!({})).with_header("Location", &format!("{}/x", second.url)),
    )]);
    let _ = hosted_client()
        .unwrap()
        .get(format!("{}/y", first.url))
        .send()
        .await;
    assert!(second.paths().is_empty());
}

#[test]
fn only_native_tls_is_linked() {
    let lock =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    for banned in ["rustls", "aws-lc-sys", "ring"] {
        let line = format!("name = \"{banned}\"");
        assert!(!lock.contains(&line), "{banned} is in Cargo.lock");
    }
    assert!(lock.contains("name = \"native-tls\""));
}
