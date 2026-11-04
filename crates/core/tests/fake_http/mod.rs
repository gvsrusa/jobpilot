#![allow(dead_code)]
//! A scripted HTTP server on 127.0.0.1, shared by the network tests. Each test binary uses
//! part of it, and clippy runs with `-D warnings` on all targets, hence the allow above.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

/// One scripted reply.
#[derive(Clone)]
pub struct Canned {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    /// The connection stays open this long after the reply, then closes.
    pub hold: Duration,
    /// Sent as `Content-Length` in place of the body's length.
    pub announced: Option<usize>,
}

impl Canned {
    pub fn json(status: u16, body: &Value) -> Canned {
        Canned {
            status,
            headers: vec![("Content-Type", "application/json".into())],
            body: body.to_string(),
            hold: Duration::ZERO,
            announced: None,
        }
    }

    /// A `200` whose body stops part way and then sits open for `hold`: headers arrive at once,
    /// so only a deadline around the body read can end it.
    pub fn stalled(hold: Duration) -> Canned {
        Canned {
            status: 200,
            headers: Vec::new(),
            body: "{".into(),
            hold,
            announced: Some(1 << 20),
        }
    }

    pub fn with_header(mut self, name: &'static str, value: &str) -> Canned {
        self.headers.push((name, value.into()));
        self
    }
}

/// (path, request head, request body), in arrival order.
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;
/// Path to its queue of replies.
type Queues = Arc<Mutex<Vec<(&'static str, Vec<Canned>)>>>;
/// Builds a reply from the JSON body of the request, for a server that reads what it was sent.
type Answer = Arc<dyn Fn(&Value) -> Canned + Send + Sync>;

pub struct Fake {
    pub url: String,
    seen: Seen,
}

impl Fake {
    /// Routes sharing a path form a queue, served in order. The last reply repeats.
    /// A path with no route answers 404.
    pub fn start(routes: Vec<(&'static str, Canned)>) -> Fake {
        Fake::serving(routes, None, None)
    }

    /// The browser tests' site: `GET /<name>` answers `<dir>/<name>.html`, `GET /done` a fixed
    /// confirmation page, `POST /form` a `303` to `/done`, and any other `POST` a `204`. Every
    /// request is recorded, so a form's body reads back through [`Fake::bodies`].
    pub fn pages(dir: &Path) -> Fake {
        Fake::serving(Vec::new(), None, Some(dir.to_path_buf()))
    }

    /// One path, answered by `answer` from each request's body. Any other path answers 404.
    pub fn answering(
        path: &'static str,
        answer: impl Fn(&Value) -> Canned + Send + Sync + 'static,
    ) -> Fake {
        let unused = Canned::json(500, &Value::Null);
        Fake::serving(vec![(path, unused)], Some(Arc::new(answer)), None)
    }

    fn serving(
        routes: Vec<(&'static str, Canned)>,
        answer: Option<Answer>,
        pages: Option<PathBuf>,
    ) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let queues: Queues = Queues::default();
        for (path, canned) in routes {
            let mut queues = queues.lock().unwrap();
            match queues.iter_mut().find(|(p, _)| *p == path) {
                Some((_, queue)) => queue.push(canned),
                None => queues.push((path, vec![canned])),
            }
        }
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let queues = queues.clone();
                let log = log.clone();
                let answer = answer.clone();
                let pages = pages.clone();
                std::thread::spawn(move || {
                    serve(stream, &queues, &log, answer.as_ref(), pages.as_deref())
                });
            }
        });
        Fake { url, seen }
    }

    /// The JSON bodies sent to `path`, oldest first. A body that is not JSON reads as null.
    pub fn hits(&self, path: &str) -> Vec<Value> {
        let seen = self.seen.lock().unwrap();
        let hits = seen.iter().filter(|(p, _, _)| p == path);
        hits.map(|(_, _, body)| serde_json::from_str(body).unwrap_or(Value::Null))
            .collect()
    }

    /// The raw bodies sent to `path`, oldest first, read as text.
    pub fn bodies(&self, path: &str) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        let hits = seen.iter().filter(|(p, _, _)| p == path);
        hits.map(|(_, _, body)| body.clone()).collect()
    }

    /// Every request header line sent to `path`, lowercased, across all hits.
    pub fn headers(&self, path: &str) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        let heads = seen.iter().filter(|(p, _, _)| p == path);
        heads
            .flat_map(|(_, head, _)| head.lines().skip(1))
            .filter(|line| !line.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    }

    /// Every path requested, in order, repeats included.
    pub fn paths(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|(path, _, _)| path.clone()).collect()
    }
}

const DONE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
    <title>Thank you</title></head><body><main><h1>Thank you</h1>\
    <p>Your application was received.</p></main></body></html>";

fn html(status: u16, body: String) -> Canned {
    Canned {
        status,
        headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
        body,
        hold: Duration::ZERO,
        announced: None,
    }
}

/// The reply of the [`Fake::pages`] site. A name is one plain word, so no path leaves `dir`.
fn page(dir: &Path, method: &str, path: &str) -> Canned {
    let empty = |status: u16| Canned {
        headers: Vec::new(),
        ..html(status, String::new())
    };
    match (method, path) {
        ("POST", "/form") => empty(303).with_header("Location", "/done"),
        ("POST", _) => empty(204),
        ("GET", "/done") => html(200, DONE.into()),
        ("GET", path) => {
            let name = path.trim_start_matches('/');
            let plain =
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            match plain.then(|| std::fs::read_to_string(dir.join(format!("{name}.html")))) {
                Some(Ok(body)) => html(200, body),
                _ => empty(404),
            }
        }
        _ => empty(405),
    }
}

fn serve(
    mut stream: std::net::TcpStream,
    queues: &Queues,
    log: &Seen,
    answer: Option<&Answer>,
    pages: Option<&Path>,
) {
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
    let method = head.split_whitespace().next().unwrap_or("").to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
    let parsed = answer.map(|_| serde_json::from_str(&body).unwrap_or(Value::Null));
    log.lock().unwrap().push((path.clone(), head, body));

    let canned = {
        let mut queues = queues.lock().unwrap();
        queues
            .iter_mut()
            .find(|(p, _)| *p == path)
            .map(|(_, queue)| {
                if queue.len() > 1 {
                    queue.remove(0)
                } else {
                    queue[0].clone()
                }
            })
    };
    let canned = match (canned, answer, parsed) {
        (Some(_), Some(answer), Some(body)) => Some(answer(&body)),
        (canned, ..) => canned,
    };
    let canned = match pages {
        Some(dir) => Some(page(dir, &method, &path)),
        None => canned,
    };
    let Some(canned) = canned else {
        let _ = stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        return;
    };
    let mut reply = format!("HTTP/1.1 {} X\r\n", canned.status);
    for (name, value) in &canned.headers {
        reply.push_str(&format!("{name}: {value}\r\n"));
    }
    reply.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        canned.announced.unwrap_or(canned.body.len()),
        canned.body
    ));
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
    std::thread::sleep(canned.hold);
}
