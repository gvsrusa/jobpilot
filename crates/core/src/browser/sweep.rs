//! Ends what a run left behind. Playwright's browser can outlive the server that started it,
//! and the run folder is on its command line, so the folder finds it.

use std::path::Path;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// A run folder is deep: fewer normal components could be a home or temp folder.
const MIN_COMPONENTS: usize = 3;

/// Windows paths differ in separator and case between an argument and its folder.
pub fn windows_fold(text: &str) -> String {
    text.replace('\\', "/").to_lowercase()
}

#[cfg(windows)]
fn fold(text: &str) -> String {
    windows_fold(text)
}

#[cfg(not(windows))]
fn fold(text: &str) -> String {
    text.to_string()
}

/// The marker as a whole path: it ends the argument or is followed by a separator or quote,
/// so a sibling folder that starts with the same letters does not match.
fn holds(arg: &str, marker: &str) -> bool {
    let (arg, marker) = (fold(arg), fold(marker));
    arg.match_indices(&marker).any(|(at, _)| {
        matches!(
            arg[at + marker.len()..].chars().next(),
            None | Some('/' | '\\' | '"' | '\'')
        )
    })
}

/// Kills every process but this one whose arguments hold `marker`. Only an absolute path of at least three parts is a marker; anything else kills nothing. A SIGTERM-proof process
/// is ended too: `Process::kill` is SIGKILL on Unix. Returns how many were signalled.
pub fn kill_marked(marker: &Path) -> usize {
    let normal = marker
        .components()
        .filter(|part| matches!(part, std::path::Component::Normal(_)))
        .count();
    if !marker.is_absolute() || normal < MIN_COMPONENTS {
        return 0;
    }
    let marker = marker.display().to_string();
    let Ok(mut system) = System::new() else {
        return 0;
    };
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let own = sysinfo::get_current_pid().ok();
    let mut killed = 0;
    for (pid, process) in system.processes() {
        if Some(*pid) == own {
            continue;
        }
        let holds = process
            .cmd()
            .iter()
            .any(|arg| holds(&arg.to_string_lossy(), &marker));
        if holds && process.kill() {
            killed += 1;
        }
    }
    killed
}
