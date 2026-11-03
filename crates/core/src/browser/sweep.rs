//! Ends what a run left behind. Playwright's browser can outlive the server that started it,
//! and the run folder is on its command line, so the folder finds it.

use std::path::Path;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// A shorter marker could match unrelated programs, and an empty one matches all of them.
const MIN_MARKER: usize = 8;

/// Kills every process but this one whose arguments hold `marker`. A SIGTERM-proof process
/// is ended too: `Process::kill` is SIGKILL on Unix. Returns how many were signalled.
pub fn kill_marked(marker: &Path) -> usize {
    let marker = marker.display().to_string();
    if marker.len() < MIN_MARKER {
        return 0;
    }
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
            .any(|arg| arg.to_string_lossy().contains(&marker));
        if holds && process.kill() {
            killed += 1;
        }
    }
    killed
}
