//! Neither client may use a proxy named in the environment. This file holds one test because
//! it sets proxy variables for the whole process, which no sibling test may observe.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use jobpilot_core::ai::net::{hosted_client, loopback_client};

/// A listener that counts connections and drops each one.
fn counting_listener() -> (String, Arc<AtomicU32>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let count = Arc::new(AtomicU32::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            seen.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (address, count)
}

#[tokio::test]
async fn an_environment_proxy_is_never_used() {
    let (proxy, proxied) = counting_listener();
    let (target, hit) = counting_listener();
    let url = format!("http://{proxy}");
    // SAFETY: this binary has this one test, so no other thread reads the environment.
    unsafe {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            std::env::set_var(name, &url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let (_, loopback) = loopback_client(&format!("http://{target}")).unwrap();
    let _ = loopback.get(format!("http://{target}/")).send().await;
    let _ = hosted_client()
        .unwrap()
        .get(format!("https://{target}/"))
        .send()
        .await;

    // Both clients went straight to the target, and neither asked the proxy.
    assert_eq!(hit.load(Ordering::SeqCst), 2);
    assert_eq!(proxied.load(Ordering::SeqCst), 0);
}
