//! JobPilot AI core library. All product behaviour lives here.

pub mod agents;
pub mod ai;
pub mod analytics;
pub mod applications;
pub mod apply;
pub mod audit;
pub mod backup;
pub mod browser;
pub mod connectors;
pub mod crypto;
pub mod db;
pub mod documents;
pub mod drafts;
pub mod embed;
pub mod error;
pub mod forms;
pub mod generate;
pub mod hardware;
pub mod jobs;
pub mod knowledge;
pub mod memory;
pub mod parse;
pub mod paths;
pub mod pause;
pub mod permissions;
pub mod profile;
pub mod recovery;
pub mod render;
pub mod runner;
pub mod secrets;
pub mod settings;
pub mod transfer;

pub use db::{Db, MIGRATOR, app_schema_version};
pub use error::{Error, Result};
pub use paths::AppPaths;

/// Crate version, shown by the CLI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
