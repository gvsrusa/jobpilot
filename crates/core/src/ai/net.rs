//! The two HTTP clients and what both share. A loopback client talks to Ollama or the local
//! endpoint and may reach nothing else (M-21). A hosted client talks to the vendors over
//! HTTPS only. Neither follows a redirect, and neither reads a proxy setting.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use reqwest::{Client, Response, Url};

use crate::error::{Error, Result};

/// Largest reply body read from a hosted provider.
pub(crate) const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;
/// A vendor's error body is read only to match fixed words, so 64 KiB is plenty.
pub(crate) const MAX_ERROR_BODY: u64 = 64 * 1024;
/// The longest `Retry-After` the router will sleep for. A longer one moves on.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// M-21, moved from `Ollama::new`: http only, 127.0.0.1, [::1], or localhost pinned to 127.0.0.1.
/// Returns the origin (`http://host:port/`) and a client that follows no redirect.
pub fn loopback_client(url: &str) -> Result<(Url, Client)> {
    let refused = || Error::Refused("not_loopback".into());
    let parsed = Url::parse(url).map_err(|_| refused())?;
    if parsed.scheme() != "http" {
        return Err(refused());
    }
    let port = parsed.port_or_known_default().ok_or_else(refused)?;
    let mut client = Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .no_proxy()
        // A loopback server must not be able to send us somewhere else.
        .redirect(reqwest::redirect::Policy::none());
    // `host_str` is canonical: the parser has already folded `127.1` and `0x7f.1` to dotted form.
    match parsed.host_str() {
        Some("127.0.0.1" | "[::1]") => {}
        // The name is pinned to 127.0.0.1, so a hosts file cannot point it elsewhere.
        Some("localhost") => {
            let pinned = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            client = client.resolve("localhost", pinned);
        }
        _ => return Err(refused()),
    }
    Ok((parsed, client.build().map_err(net)?))
}

/// HTTPS to the vendors. `pub` only so `tests/net.rs` can test the real client settings.
#[doc(hidden)]
pub fn hosted_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .https_only(true)
        .tls_version_min(reqwest::tls::Version::TLS_1_2)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(net)
}

/// Whole seconds only. A date, a negative, or garbage is `None`.
pub fn retry_after(header: Option<&str>) -> Option<Duration> {
    let text = header?.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok().map(Duration::from_secs)
}

/// Reads at most `limit` bytes of the body, counting chunks as they arrive, so a body with no
/// `Content-Length` is capped too. A longer body is `TooLarge`: a hostile or broken server
/// cannot fill memory.
///
/// This sets no deadline of its own, and `hosted_client` sets no overall timeout. A server that
/// drips bytes is stopped by the router's `bounded` around the whole attempt, so a caller must
/// read inside `bounded` and never outside it.
pub(crate) async fn read_capped(mut response: Response, limit: u64) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|len| len > limit) {
        return Err(Error::TooLarge { limit });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(net)? {
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(Error::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Connection and body errors are `Io`, which the router retries once.
pub(crate) fn net(error: reqwest::Error) -> Error {
    Error::Io(io::Error::other(error.without_url().to_string()))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    async fn get_body(body: &'static str) -> Response {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        });
        let (_, client) = loopback_client(&url).unwrap();
        client.get(url).send().await.unwrap()
    }

    /// Three 3-byte chunks, no `Content-Length`.
    async fn get_chunked() -> Response {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            for _ in 0..3 {
                let _ = stream.write_all(b"3\r\nabc\r\n");
                let _ = stream.flush();
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        });
        let (_, client) = loopback_client(&url).unwrap();
        client.get(url).send().await.unwrap()
    }

    #[tokio::test]
    async fn read_capped_counts_chunks_when_there_is_no_content_length() {
        let response = get_chunked().await;
        assert_eq!(response.content_length(), None);
        let past = read_capped(response, 8).await;
        assert!(matches!(past, Err(Error::TooLarge { limit: 8 })));
        let at = read_capped(get_chunked().await, 9).await.unwrap();
        assert_eq!(at, b"abcabcabc");
    }

    #[tokio::test]
    async fn read_capped_returns_a_body_at_the_limit_and_refuses_one_past_it() {
        let at = read_capped(get_body("12345").await, 5).await.unwrap();
        assert_eq!(at, b"12345");
        let past = read_capped(get_body("123456").await, 5).await;
        assert!(matches!(past, Err(Error::TooLarge { limit: 5 })));
    }
}
