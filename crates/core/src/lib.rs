//! JobPilot AI core library. All product behaviour lives here.

pub mod audit;
pub mod backup;
pub mod crypto;
pub mod db;
pub mod documents;
pub mod drafts;
pub mod error;
pub mod knowledge;
pub mod parse;
pub mod paths;
pub mod profile;
pub mod recovery;
pub mod render;
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
