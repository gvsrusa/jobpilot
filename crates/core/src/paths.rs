use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(crate) const APP_ID: &str = "io.github.gvsrusa.jobpilot";

/// Every file JobPilot owns lives under one root.
#[derive(Debug, Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `JOBPILOT_HOME` if set, else the platform data directory.
    pub fn from_env() -> Result<Self> {
        root_from(std::env::var_os("JOBPILOT_HOME")).map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("jobpilot.db")
    }

    pub fn documents(&self) -> PathBuf {
        self.root.join("documents")
    }

    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }

    pub fn lock(&self) -> PathBuf {
        self.root.join("jobpilot.lock")
    }

    /// Proves the root is JobPilot's, so a mistaken root is never written or deleted.
    pub fn marker(&self) -> PathBuf {
        self.root.join(".jobpilot-root")
    }
}

fn root_from(home: Option<OsString>) -> Result<PathBuf> {
    match home.filter(|home| !home.is_empty()) {
        Some(home) => Ok(home.into()),
        None => dirs::data_dir()
            .map(|dir| dir.join(APP_ID))
            .ok_or(Error::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_hangs_off_root() {
        let paths = AppPaths::new("base");
        let root = Path::new("base");
        assert_eq!(paths.root(), root);
        assert_eq!(paths.db(), root.join("jobpilot.db"));
        assert_eq!(paths.documents(), root.join("documents"));
        assert_eq!(paths.backups(), root.join("backups"));
        assert_eq!(paths.exports(), root.join("exports"));
        assert_eq!(paths.lock(), root.join("jobpilot.lock"));
        assert_eq!(paths.marker(), root.join(".jobpilot-root"));
    }

    #[test]
    fn home_override_wins_and_empty_means_unset() {
        assert_eq!(
            root_from(Some("custom home".into())).unwrap(),
            PathBuf::from("custom home")
        );
        let default = dirs::data_dir().unwrap().join(APP_ID);
        assert_eq!(root_from(None).unwrap(), default);
        assert_eq!(root_from(Some("".into())).unwrap(), default);
    }
}
