//! Shared contracts for the `linkctl` command-line tools and services.

pub mod audio;
pub mod config;
pub mod control;
pub mod device;
pub mod error;
pub mod firmware;
pub mod logging;
pub mod media;
pub mod output;
pub mod paths;
pub mod preset;
pub mod probe;
pub mod safety;
pub mod transaction;

pub use error::{ErrorKind, LinkError, ProcessExit};

/// Current machine-readable output schema.
pub const SCHEMA_VERSION: u32 = 1;
