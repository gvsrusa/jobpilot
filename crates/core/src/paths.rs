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

    /// True when a file at `path` would land under the root, or that cannot be ruled out.
    /// Compares resolved directories, so a symlink, a `..`, or another spelling of the
    /// root's name cannot hide it. A folder that cannot be resolved counts as inside.
    pub fn is_inside_root(&self, path: &Path) -> bool {
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        match (parent.canonicalize(), self.root.canonicalize()) {
            (Ok(parent), Ok(root)) => parent.starts_with(root),
            _ => true,
        }
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
    fn inside_root_is_decided_on_resolved_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data");
        std::fs::create_dir_all(root.join("exports")).unwrap();
        std::fs::create_dir(tmp.path().join("data2")).unwrap();
        let paths = AppPaths::new(&root);

        assert!(paths.is_inside_root(&root.join("key.json")));
        assert!(paths.is_inside_root(&root.join("exports/key.json")));
        assert!(paths.is_inside_root(&tmp.path().join("data2/../data/key.json")));
        assert!(!paths.is_inside_root(&tmp.path().join("key.json")));
        assert!(
            !paths.is_inside_root(&tmp.path().join("data2/key.json")),
            "not a prefix"
        );

        // A bare file name lands in the working directory.
        assert!(AppPaths::new(".").is_inside_root(Path::new("key.json")));
        assert!(paths.is_inside_root(&tmp.path().join("missing/key.json")));
        assert!(AppPaths::new(tmp.path().join("gone")).is_inside_root(&root.join("key.json")));
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
