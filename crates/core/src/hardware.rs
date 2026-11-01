//! What this machine can run. Read once per call; nothing is cached or stored.

use std::path::Path;

use serde::Serialize;
use sysinfo::{Disks, System};

use crate::ai::ollama::Fit;

#[derive(Debug, Clone, Serialize)]
pub struct Hardware {
    pub total_memory: u64,
    pub cpus: usize,
    pub gpu: String,
    /// Free bytes on the disk that holds the home folder, where Ollama keeps its models.
    pub free_disk: u64,
}

pub fn detect() -> Hardware {
    // Unreadable memory reports 0, which `fit` calls too large: no model is promised to fit.
    let total_memory = System::new().map_or(0, |mut system| {
        system.refresh_memory();
        system.total_memory()
    });
    Hardware {
        total_memory,
        cpus: std::thread::available_parallelism().map_or(1, usize::from),
        gpu: gpu(),
        free_disk: free_disk(),
    }
}

// ponytail: only Apple Silicon is named. Other GPUs are not probed (no vendor SDKs), so a
// machine with a discrete GPU still reports "no GPU found"; the memory tiers ignore VRAM.
fn gpu() -> String {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "Apple Silicon (unified memory)".into()
    } else {
        "no GPU found".into()
    }
}

fn free_disk() -> u64 {
    let Ok(disks) = Disks::new_with_refreshed_list() else {
        return 0;
    };
    let home = dirs::home_dir();
    // The disk with the longest mount point that is a prefix of home, else the roomiest.
    let holding = |mount: &Path| home.as_deref().is_some_and(|h| h.starts_with(mount));
    disks
        .iter()
        .filter(|d| holding(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .or_else(|| disks.iter().max_by_key(|d| d.available_space()))
        .map_or(0, |d| d.available_space())
}

/// A model needs its size plus 20% headroom. Within 75% of memory it fits; within all of
/// it, it may be slow; beyond that it is too large.
pub fn fit(model_size: u64, hw: &Hardware) -> Fit {
    // Integer math, exact at the edges: size*6/5 against total*3/4 and total.
    let need = u128::from(model_size) * 6;
    let total = u128::from(hw.total_memory) * 5;
    if need * 4 <= total * 3 {
        Fit::Fits
    } else if need <= total {
        Fit::MaySlow
    } else {
        Fit::TooLarge
    }
}
