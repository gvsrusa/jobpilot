//! One test in this binary, so its `set_var` runs with no other test thread.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use jobpilot_core::ai::claude_code::ClaudeCode;
use jobpilot_core::ai::{CancellationToken, Prompt, Provider};

/// What the login needs on macOS, read from the CLI's 2.1.316 source: the keychain account is
/// `$USER`, the service name hangs on the config-dir variables, `security` comes from `PATH`.
const KEPT: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "SystemRoot",
    "ComSpec",
    "LANG",
    "LC_ALL",
    "TERM",
    "XDG_CONFIG_HOME",
    "USER",
    "TMPDIR",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

/// Credentials, app state, and anything that loads code into the child.
const STRIPPED: &[&str] = &[
    "JOBPILOT_SECRET_SERVICE",
    "JOBPILOT_HOME",
    "JOBPILOT_CLAUDE_BIN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "NODE_OPTIONS",
    "BASH_ENV",
];

#[tokio::test]
async fn child_env_is_an_allow_list() {
    for name in STRIPPED {
        // SAFETY: the only test in this binary, before any thread reads the environment.
        unsafe { std::env::set_var(name, "must-not-reach-the-child") };
    }
    for name in ["USER", "CLAUDE_CONFIG_DIR", "HOME", "USERPROFILE"] {
        if std::env::var_os(name).is_none() {
            // SAFETY: as above.
            unsafe { std::env::set_var(name, "/kept") };
        }
    }

    let listed = ClaudeCode::child_env();
    for (name, _) in &listed {
        assert!(KEPT.contains(&name.as_str()), "{name} is not on the list");
    }
    for name in ["PATH", "USER", "CLAUDE_CONFIG_DIR"] {
        assert!(listed.iter().any(|(n, _)| n == name), "{name} was dropped");
    }

    // The same again from inside a real child: the list must be what the process receives.
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("env");
    let bin = wrapper(dir.path(), &dump);
    let claude = ClaudeCode::with_binary(bin, None);
    let prompt = Prompt {
        system: "rules".into(),
        user: "posting".into(),
        schema: "{}",
    };
    claude
        .complete(&prompt, CancellationToken::new())
        .await
        .unwrap();

    let text = fs::read_to_string(&dump).unwrap();
    let got: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
    for name in STRIPPED {
        assert!(!got.contains_key(name), "{name} reached the child");
    }
    assert!(!text.contains("must-not-reach-the-child"));
    assert!(got.contains_key("PATH") || got.contains_key("Path"));
    assert!(got.contains_key("HOME") || got.contains_key("USERPROFILE"));
    assert!(got.contains_key("USER") || cfg!(windows));
    assert!(got.contains_key("CLAUDE_CONFIG_DIR"));
}

#[cfg(unix)]
fn wrapper(dir: &Path, dump: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-claude/claude");
    let script = format!(
        "#!/bin/sh\nexport MOCK_CLAUDE_ENV='{}'\n'{}' \"$@\"\n",
        dump.display(),
        mock.display()
    );
    let path = dir.join("claude-wrapper");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(windows)]
fn wrapper(dir: &Path, dump: &Path) -> std::path::PathBuf {
    let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mock-claude/claude.cmd");
    let script = format!(
        "@echo off\r\nset \"MOCK_CLAUDE_ENV={}\"\r\ncall \"{}\" %*\r\nexit /b %ERRORLEVEL%\r\n",
        dump.display(),
        mock.display()
    );
    let path = dir.join("claude-wrapper.cmd");
    fs::write(&path, script).unwrap();
    path
}
