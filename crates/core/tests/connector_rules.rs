//! The hard rules for connectors, read as source text. A static test cannot prove a program
//! does not misbehave, but it stops the easy ways to break a rule: it fails the moment a file
//! names a client, a process, a solver, or a disguise where the rule says none may be.
//!
//! Comments that state a rule are allowed to use the banned words. Everything else is not.

use std::fs;
use std::path::{Path, PathBuf};

const FETCH: &str = include_str!("../src/connectors/fetch.rs");
const CONNECTORS: &[(&str, &str)] = &[
    ("ashby.rs", include_str!("../src/connectors/ashby.rs")),
    ("assisted.rs", include_str!("../src/connectors/assisted.rs")),
    ("fetch.rs", FETCH),
    (
        "greenhouse.rs",
        include_str!("../src/connectors/greenhouse.rs"),
    ),
    ("lever.rs", include_str!("../src/connectors/lever.rs")),
    ("limit.rs", include_str!("../src/connectors/limit.rs")),
    ("mod.rs", include_str!("../src/connectors/mod.rs")),
    ("remoteok.rs", include_str!("../src/connectors/remoteok.rs")),
    ("wwr.rs", include_str!("../src/connectors/wwr.rs")),
];
const NET: &str = include_str!("../src/ai/net.rs");

/// The code of a source file: every comment removed, so a comment that names a rule does not
/// trip the rule. A comment starts a line, or follows a space; `//` inside `"https://"` has no
/// space before it and stays.
fn code(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line.split(" // ").next().unwrap_or("")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Every `.rs` file of the core, the CLI, and the desktop shell.
fn every_src_file() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    for dir in [
        "crates/core/src",
        "crates/cli/src",
        "apps/desktop/src-tauri/src",
    ] {
        rust_files(&root.join(dir), &mut found);
    }
    assert!(found.len() > 40, "only {} source files found", found.len());
    found
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap();
            // Forward slashes on every system, so a check on a path reads the same on Windows.
            (path.to_string_lossy().replace('\\', "/"), text)
        })
        .collect()
}

/// The list above is kept by hand, so a connector file added to the folder must be added to it,
/// or every test over the list would pass without reading the new file.
#[test]
fn the_connector_list_is_every_file_in_the_folder() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/connectors");
    let mut on_disk: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut listed: Vec<&str> = CONNECTORS.iter().map(|(name, _)| *name).collect();
    listed.sort();
    assert_eq!(on_disk, listed);
}

#[test]
fn only_fetch_rs_touches_the_network() {
    for (name, source) in CONNECTORS.iter().filter(|(name, _)| *name != "fetch.rs") {
        // `reqwest::Url` is a parser for a link and opens nothing: it is the one name allowed.
        let code = code(source).replace("reqwest::Url", "");
        for banned in [
            "reqwest",
            "Client",
            "TcpStream",
            "UdpSocket",
            "TcpListener",
            "hyper",
            "ureq",
            "std::net",
            "tokio::net",
            "hosted_client",
            "loopback_client",
            "net::",
        ] {
            assert!(!code.contains(banned), "{name} names {banned}");
        }
    }
    // The control: the fetcher is where the client is, so the test looks at the right thing.
    assert!(code(FETCH).contains("hosted_client"));
    assert!(code(FETCH).contains("Client"));
}

#[test]
fn no_connector_starts_a_process_or_a_browser() {
    for (name, source) in CONNECTORS {
        let code = code(source).to_lowercase();
        for banned in [
            "process::",
            "std::process",
            "tokio::process",
            "command::new",
            "playwright",
            "webdriver",
            "puppeteer",
            "selenium",
            "headless",
            "chromium",
            "geckodriver",
        ] {
            assert!(!code.contains(banned), "{name} names {banned}");
        }
    }
}

#[test]
fn no_source_holds_a_bypass() {
    let banned = [
        "captcha",
        "2captcha",
        "solver",
        "proxy(",
        "cookie_store",
        "cookie_provider",
        "mozilla/",
        "stealth",
        "x-forwarded-for",
    ];
    let mut hits = Vec::new();
    for (path, source) in every_src_file() {
        // The rule's own words may stand in a comment that names the rule: nowhere else. This
        // file is not in the list, so it may name them all.
        let code = code(&source).to_lowercase();
        for word in banned {
            // `.no_proxy()` is the opposite of `proxy(`: it is the rule being kept.
            let hit = code.replace("no_proxy(", "").contains(word);
            // The app has a CAPTCHA pause state in 0.6 only as a stop for a person: a word in
            // an identifier or a string that says "stop" is still a name for it.
            if hit {
                hits.push(format!("{path}: {word}"));
            }
        }
    }
    assert!(hits.is_empty(), "{hits:#?}");
}

/// The text between `hosts: &[` and the closing `]` of each declaration.
fn declared_hosts(source: &str) -> Vec<String> {
    let code = code(source);
    let mut found = Vec::new();
    let mut rest = code.as_str();
    while let Some((_, after)) = rest.split_once("hosts: &[") {
        let (list, tail) = after.split_once(']').unwrap();
        // A parameter typed `hosts: &[&str]` is not a declaration.
        if !list.contains("str") {
            found.push(list.to_owned());
        }
        rest = tail;
    }
    found
}

#[test]
fn every_host_is_fixed_and_https_only_comes_from_the_client() {
    let mut read_connectors = 0;
    for (name, source) in CONNECTORS {
        for list in declared_hosts(source) {
            for item in list
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
            {
                let host = item
                    .strip_prefix('"')
                    .and_then(|item| item.strip_suffix('"'))
                    .unwrap_or_else(|| panic!("{name}: host {item} is not a literal"));
                assert!(
                    host.chars().all(|c| c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || c == '.'
                        || c == '-'),
                    "{name}: {host} is not a bare lower-case host"
                );
                assert!(host.contains('.'), "{name}: {host}");
            }
        }
        // A request names its host by a literal, never by a value built at run time. A field
        // typed `&str` or `&'static str` is a declaration, not a request.
        let text = code(source);
        for (at, _) in text.match_indices("host:") {
            let after = text[at + "host:".len()..].trim_start();
            if after.starts_with('&') || after.starts_with("String") {
                continue;
            }
            assert!(
                after.starts_with('"') || after.starts_with("if "),
                "{name}: a request host is not a literal: {}",
                &after[..after.len().min(40)]
            );
        }
        if !declared_hosts(source).is_empty() {
            read_connectors += 1;
        }
    }
    assert!(
        read_connectors >= 6,
        "{read_connectors} connectors declare hosts"
    );
    // Nothing builds a link with a scheme but https. RemoteOK reads an `http://` prefix only to
    // rewrite that link to https, so a plain link is only ever built by `format!`.
    for (name, source) in CONNECTORS {
        let code = code(source);
        assert!(
            !code.contains("format!(\"http://"),
            "{name} builds a plain http link"
        );
    }
    assert!(!code(FETCH).contains("http://"));
    let fetch = code(FETCH);
    assert!(fetch.contains("format!(\"https://{}{}\""));
    assert!(fetch.contains("hosted_client()"));
    assert!(fetch.contains("decl.hosts.contains"));
}

#[test]
fn hosted_client_keeps_no_proxy_and_no_redirect() {
    let net = code(NET);
    let hosted = net
        .split_once("pub fn hosted_client()")
        .and_then(|(_, rest)| rest.split_once("\n}\n"))
        .map(|(body, _)| body.to_owned())
        .expect("hosted_client is in net.rs");
    assert!(hosted.contains(".no_proxy()"), "{hosted}");
    assert!(hosted.contains("redirect::Policy::none()"), "{hosted}");
    assert!(hosted.contains(".https_only(true)"), "{hosted}");
    // The fetcher builds on that client and holds none of its own.
    let fetch = code(FETCH);
    assert!(!fetch.contains("Client::builder"));
    assert!(!fetch.contains(".proxy("));
    assert!(!fetch.contains("cookie"));
    // One honest User-Agent, named for the app, and no browser's name.
    let agent = FETCH.split_once("pub const USER_AGENT").unwrap().1;
    let agent = agent.split_once(';').unwrap().0;
    assert!(agent.to_lowercase().contains("jobpilot"), "{agent}");
    assert!(!agent.to_lowercase().contains("mozilla"), "{agent}");
}

#[test]
fn only_jobs_makes_a_job_text_so_every_text_has_a_job() {
    // Parked from Task 1: after migration 0013 a text that `knowledge::import_job_text` stores
    // alone has no `jobs` row, so the Jobs list would miss it. The product never stores one
    // alone: the one path that does is `jobs`, which makes the job in the same step (and
    // deletes the text if the job fails). This pins that: a new caller must come through here.
    let mut callers = Vec::new();
    for (path, source) in every_src_file() {
        if code(&source).contains("import_job_text(") && !path.ends_with("knowledge.rs") {
            callers.push(path);
        }
    }
    callers.sort();
    assert_eq!(callers.len(), 1, "{callers:?}");
    assert!(callers[0].ends_with("jobs/mod.rs"), "{callers:?}");
}
