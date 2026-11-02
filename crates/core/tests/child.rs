//! Rules every provider program shares: names, folders, environment, and the pipe run. The
//! pure functions take `windows` as an argument so one macOS run covers both sides.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use jobpilot_core::ai::child::{
    CODEX, ChildRun, GEMINI, accepts_name, bin_names, child_env, child_env_from, child_path,
    cli_dirs, run_piped, spec_for,
};

fn no_env(_: &str) -> Option<OsString> {
    None
}

#[test]
fn windows_prefers_the_exe_then_the_shim() {
    assert_eq!(bin_names("gemini", true), ["gemini.exe", "gemini.cmd"]);
    assert_eq!(bin_names("gemini", false), ["gemini"]);
}

#[test]
fn windows_looks_in_the_npm_and_manager_folders() {
    let env = |name: &str| -> Option<OsString> {
        match name {
            "APPDATA" => Some(r"C:\U\AppData\Roaming".into()),
            "LOCALAPPDATA" => Some(r"C:\U\AppData\Local".into()),
            "ProgramFiles" => Some(r"C:\Program Files".into()),
            _ => None,
        }
    };
    let dirs = cli_dirs(Some(r"C:\U".into()), &env, true);
    for wanted in ["npm", "Volta", "shims", "nodejs"] {
        assert!(
            dirs.iter()
                .any(|dir| dir.to_string_lossy().contains(wanted)),
            "{wanted}"
        );
    }
}

#[test]
fn nvm_folders_are_searched_newest_first() {
    let home = tempfile::tempdir().unwrap();
    let node = home.path().join(".nvm").join("versions").join("node");
    // v9 sorts after v22 as text, and before it as a version.
    for version in ["v18.20.0", "v22.1.0", "v24.0.0", "v9.11.2"] {
        fs::create_dir_all(node.join(version).join("bin")).unwrap();
    }
    let dirs = cli_dirs(Some(home.path().to_path_buf()), &no_env, false);
    let at = |version: &str| {
        let wanted = node.join(version).join("bin");
        dirs.iter().position(|dir| *dir == wanted).expect(version)
    };
    assert!(at("v24.0.0") < at("v22.1.0"));
    assert!(at("v22.1.0") < at("v18.20.0"));
    assert!(at("v18.20.0") < at("v9.11.2"));
}

#[test]
fn child_path_starts_with_the_programs_folder() {
    let path = child_path(
        Path::new("/h/.nvm/versions/node/v24/bin/gemini"),
        &GEMINI,
        OsStr::new("/usr/bin"),
    );
    assert_eq!(
        std::env::split_paths(&path).next().unwrap(),
        Path::new("/h/.nvm/versions/node/v24/bin")
    );
}

#[test]
fn npm_entry_file_is_accepted_and_finds_its_node() {
    let entry =
        Path::new("/h/.nvm/versions/node/v24/lib/node_modules/@google/gemini-cli/bundle/gemini.js");
    assert!(accepts_name(entry, &GEMINI, false));
    let path = child_path(entry, &GEMINI, OsStr::new("/usr/bin"));
    assert!(
        std::env::split_paths(&path).any(|dir| dir == Path::new("/h/.nvm/versions/node/v24/bin"))
    );
    assert!(!accepts_name(Path::new("/tmp/gemini.js"), &GEMINI, false));
    // Another package's file of that name is not Gemini's.
    let other = Path::new("/h/lib/node_modules/@evil/gemini-cli/bundle/gemini.js");
    assert!(!accepts_name(other, &GEMINI, false));
    assert!(!accepts_name(entry, &CODEX, false));
}

#[test]
fn only_the_named_program_is_accepted() {
    for bad in ["/bin/sh", "/x/gemini-evil", "/x/Gemini.sh"] {
        assert!(!accepts_name(Path::new(bad), &GEMINI, false), "{bad}");
    }
    assert!(accepts_name(Path::new("/x/gemini"), &GEMINI, false));
    assert!(!accepts_name(Path::new("/x/codex"), &GEMINI, false));
    // Windows names ignore case and take the shim.
    for good in ["/n/Gemini.CMD", "/n/gemini.exe", "/n/gemini"] {
        assert!(accepts_name(Path::new(good), &GEMINI, true), "{good}");
    }
    assert!(!accepts_name(Path::new("/n/gemini.bat"), &GEMINI, true));
}

#[test]
fn the_two_programs_have_their_own_names() {
    assert_eq!(spec_for("gemini_cli").unwrap().program, "gemini");
    assert_eq!(spec_for("codex_cli").unwrap().setting, "ai.codex_path");
    assert_eq!(CODEX.env_bin, "JOBPILOT_CODEX_BIN");
    assert!(spec_for("claude_code").is_none());
    assert!(spec_for("openai").is_none());
}

#[test]
fn sign_in_variables_pass_and_vertex_and_other_keys_do_not() {
    let set = [
        ("GEMINI_API_KEY", "g"),
        ("GOOGLE_API_KEY", "g2"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
        ("OPENAI_API_KEY", "o"),
        ("ANTHROPIC_API_KEY", "a"),
        ("NODE_OPTIONS", "--require x"),
        ("CODEX_HOME", "/c"),
        ("HOME", "/h"),
    ];
    let get = |name: &str| {
        set.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    };
    let gemini: Vec<String> = child_env_from(GEMINI.extra_env, &get)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(gemini, ["HOME", "GEMINI_API_KEY"]);
    let codex: Vec<String> = child_env_from(CODEX.extra_env, &get)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(codex, ["HOME", "CODEX_HOME"]);
    // Nothing a Claude login needs rides along.
    assert!(
        child_env(&[])
            .iter()
            .all(|(name, _)| !name.starts_with("CLAUDE"))
    );
}

fn test_binary() -> PathBuf {
    std::env::current_exe().unwrap()
}

/// The test binary is the child that exists on both systems: with `CHILD_HELPER` set, this
/// "test" is the program that writes a lot, or waits.
#[test]
fn child_helper() {
    match std::env::var("CHILD_HELPER").as_deref() {
        Ok("big") => {
            use std::io::Write;
            let block = [b'x'; 8192];
            let mut out = std::io::stdout().lock();
            for _ in 0..64 {
                let _ = out.write_all(&block);
            }
        }
        Ok("echo") => {
            let mut input = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).unwrap();
            print!("<<{input}>>");
        }
        _ => {}
    }
}

fn helper_run<'a>(cwd: &'a Path, mode: &str, stdin: &'a [u8], limit: u64) -> ChildRun<'a> {
    let mut env = child_env(&[]);
    env.push(("CHILD_HELPER".into(), mode.into()));
    ChildRun {
        bin: Box::leak(test_binary().into_boxed_path()),
        args: ["child_helper", "--exact", "--nocapture", "--test-threads=1"]
            .map(OsString::from)
            .into(),
        env,
        cwd,
        stdin,
        limit,
    }
}

#[tokio::test]
async fn run_piped_caps_the_output() {
    let dir = tempfile::tempdir().unwrap();
    let err = run_piped(helper_run(dir.path(), "big", b"", 1000))
        .await
        .unwrap_err();
    assert!(
        matches!(err, jobpilot_core::Error::TooLarge { limit: 1000 }),
        "{err:?}"
    );
    let ok = run_piped(helper_run(dir.path(), "big", b"", 10_000_000))
        .await
        .unwrap();
    assert!(ok.status.success());
    assert!(ok.stdout.len() >= 64 * 8192);
}

#[tokio::test]
async fn run_piped_hands_over_stdin_and_the_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_piped(helper_run(
        dir.path(),
        "echo",
        "prompt é".as_bytes(),
        1 << 20,
    ))
    .await
    .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("<<prompt é>>"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn run_piped_kills_the_tree_when_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = format!("sleep 30 & echo $! > '{}'; wait", pidfile.display());
    let run = ChildRun {
        bin: Path::new("/bin/sh"),
        args: vec!["-c".into(), script.into()],
        env: child_env(&[]),
        cwd: dir.path(),
        stdin: b"",
        limit: 1 << 20,
    };
    let waited = tokio::time::timeout(std::time::Duration::from_millis(600), run_piped(run)).await;
    assert!(waited.is_err(), "the child should still be running");
    let pid: i32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = || unsafe { libc::kill(pid, 0) == 0 };
    for _ in 0..50 {
        if !alive() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the grandchild {pid} outlived the run");
}

/// A `.cmd` in a folder with a space: stdin arrives, the exit code comes back, and an argument
/// with a space and a percent sign is either passed whole or refused as `cli_error`, never
/// handed to cmd.exe to reinterpret.
#[cfg(windows)]
#[tokio::test]
async fn cmd_shim_takes_spaces_stdin_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("a b");
    fs::create_dir_all(&folder).unwrap();
    let shim = folder.join("gemini.cmd");
    fs::write(
        &shim,
        "@echo off\r\necho ARGS:%*\r\nfindstr \"^\"\r\nexit /b 3\r\n",
    )
    .unwrap();
    let run = ChildRun {
        bin: &shim,
        args: vec!["has space 100%".into()],
        env: child_env(&[]),
        cwd: tmp.path(),
        stdin: b"from stdin\r\n",
        limit: 1 << 20,
    };
    match run_piped(run).await {
        Ok(out) => {
            assert_eq!(out.status.code(), Some(3));
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains("from stdin"), "{text}");
            assert!(text.contains("has space 100%"), "{text}");
        }
        Err(err) => assert!(
            matches!(&err, jobpilot_core::Error::Failed(why) if why == "cli_error"),
            "{err:?}"
        ),
    }
}
