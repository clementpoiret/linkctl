//! Logging initialization that never writes command output to stdout.

use tracing_subscriber::{EnvFilter, fmt, util::SubscriberInitExt};

use crate::config::LogLevel;

/// Install the process-wide tracing subscriber if one is not already installed.
pub fn init(level: LogLevel, no_color: bool) {
    let subscriber = fmt()
        .with_env_filter(EnvFilter::new(level.as_str()))
        .with_ansi(!no_color)
        .with_writer(std::io::stderr)
        .finish();

    let _already_initialized = subscriber.try_init();
}
