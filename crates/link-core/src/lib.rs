//! Shared contracts for the `linkctl` command-line tools and services.

pub mod config;
pub mod error;
pub mod logging;
pub mod output;
pub mod probe;
pub mod safety;

pub use error::{ErrorKind, LinkError, ProcessExit};

/// Current machine-readable output schema.
pub const SCHEMA_VERSION: u32 = 1;
