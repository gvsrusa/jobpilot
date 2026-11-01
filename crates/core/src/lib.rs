//! JobPilot AI core library. All product behaviour lives here.

pub mod audit;
pub mod db;
pub mod documents;
pub mod error;
pub mod paths;
pub mod settings;

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
