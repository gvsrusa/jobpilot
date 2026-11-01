//! One test in this binary, so its `set_var` runs with no other test thread.

use std::fs;
use std::path::Path;

use jobpilot_core::Error;
use jobpilot_core::ai::Provider;
use jobpilot_core::ai::claude_code::ClaudeCode;

#[cfg(unix)]
const NAME: &str = "claude";
#[cfg(windows)]
const NAME: &str = "claude.cmd";

fn place(dir: &Path) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(NAME);
    fs::write(&path, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn set(name: &str, value: &std::ffi::OsStr) {
    // SAFETY: the only test in this binary, and it spawns no thread.
    unsafe { std::env::set_var(name, value) };
}

fn located() -> String {
    ClaudeCode::locate().unwrap().declaration().sign_in
}

#[test]
fn locate_takes_the_override_then_absolute_path_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    let empty = tmp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    // A directory with the right name is not a binary.
    fs::create_dir_all(first.join(NAME)).unwrap();
    let found = place(&second);

    // SAFETY: as in `set`.
    unsafe { std::env::remove_var("JOBPILOT_CLAUDE_BIN") };
    let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();

    set("PATH", &path(&[&empty]));
    assert!(matches!(ClaudeCode::locate(), Err(Error::Unavailable)));

    set("PATH", &path(&[&empty, &first, &second]));
    assert!(located().contains(&found.display().to_string()));

    // A relative entry would run whatever sits in the working directory.
    let here = tempfile::tempdir_in(".").unwrap();
    place(here.path());
    let relative = here.path().file_name().unwrap();
    set("PATH", &path(&[Path::new(relative)]));
    assert!(matches!(ClaudeCode::locate(), Err(Error::Unavailable)));

    // The owner's explicit path wins, whether or not it exists yet: `available` reports that.
    let explicit = tmp.path().join("explicit-claude");
    set("JOBPILOT_CLAUDE_BIN", explicit.as_os_str());
    assert!(located().contains(&explicit.display().to_string()));

    // Set but empty means unset.
    set("JOBPILOT_CLAUDE_BIN", std::ffi::OsStr::new(""));
    set("PATH", &path(&[&second]));
    assert!(located().contains(&found.display().to_string()));
}
