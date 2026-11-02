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
        "+5",
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

/// A local check of the redirect policy needs an https server, and a test has no certificate
/// the OS trusts, so the client's own description is read instead. `https_only` is pinned by
/// the test above. `tls_version_min` has no local check: its description is not exposed and a
/// handshake needs a trusted certificate, so only Task 11's real handshake covers it.
#[test]
fn both_clients_are_built_to_follow_no_redirect() {
    let hosted = format!("{:?}", hosted_client().unwrap());
    let (_, loopback) = loopback_client("http://127.0.0.1:1").unwrap();
    for described in [hosted, format!("{loopback:?}")] {
        assert!(
            described.contains("redirect_policy: \"Policy(None)\""),
            "{described}"
        );
    }
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

/// Proves the TLS handshake with each vendor host: an unauthenticated `GET` with no body, and
/// any HTTP status counts. It carries no key and no data, so it costs nothing. On Windows it
/// is the Schannel proof. Run only by CI and by Task 11, with `JOBPILOT_REAL_TLS=1`.
#[tokio::test]
#[ignore = "opens a real connection to four vendor hosts"]
async fn real_tls_handshake() {
    if std::env::var_os("JOBPILOT_REAL_TLS").is_none_or(|value| value != "1") {
        eprintln!("skipped: JOBPILOT_REAL_TLS is not 1");
        return;
    }
    let client = hosted_client().unwrap();
    for host in [
        "https://api.openai.com/",
        "https://api.anthropic.com/",
        "https://generativelanguage.googleapis.com/",
        "https://openrouter.ai/",
    ] {
        let response = client
            .get(host)
            .send()
            .await
            .unwrap_or_else(|error| panic!("no TLS handshake with {host}: {error}"));
        eprintln!("{host} answered {}", response.status());
    }
}
