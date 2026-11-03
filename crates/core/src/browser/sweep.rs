//! Ends what a run left behind. Playwright's browser can outlive the server that started it,
//! and the run folder is on its command line, so the folder finds it.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

/// A run folder is deep: fewer normal components could be a home or temp folder.
const MIN_COMPONENTS: usize = 3;
/// How long a sweep keeps looking for a process that a killed one started on its way out:
/// Chromium starts a helper when it loses its server's pipe.
const SETTLE: Duration = Duration::from_millis(500);
/// The wait between two looks.
const BETWEEN: Duration = Duration::from_millis(100);

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

/// Kills every process but this one whose arguments hold `marker`. Only an absolute path of at
/// least three parts is a marker; anything else kills nothing. A SIGTERM-proof process is ended
/// too: `Process::kill` is SIGKILL on Unix. After a pass that killed something it looks again,
/// so a process started meanwhile is killed too, until a pass finds nothing new or `SETTLE`
/// has passed. It blocks for about that long at most: `SETTLE` plus one look at the process
/// table. Returns how many processes were signalled.
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
    let mut signalled = HashSet::new();
    let begun = Instant::now();
    while kill_pass(&mut system, &marker, &mut signalled) > 0 && begun.elapsed() + BETWEEN <= SETTLE
    {
        std::thread::sleep(BETWEEN);
    }
    signalled.len()
}

/// One look at the process table. A process already signalled, or already dead and waiting
/// for its parent, is not counted again. Returns how many were signalled in this pass.
fn kill_pass(system: &mut System, marker: &str, signalled: &mut HashSet<Pid>) -> usize {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let own = sysinfo::get_current_pid().ok();
    let mut killed = 0;
    for (pid, process) in system.processes() {
        if Some(*pid) == own || signalled.contains(pid) || process.status() == ProcessStatus::Zombie
        {
            continue;
        }
        let holds = process
            .cmd()
            .iter()
            .any(|arg| holds(&arg.to_string_lossy(), marker));
        if holds && process.kill() {
            signalled.insert(*pid);
            killed += 1;
        }
    }
    killed
}
