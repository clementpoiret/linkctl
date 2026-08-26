//! Parser and shell-facing behavior for the `linkctl` binary.

use std::{ffi::OsString, path::PathBuf};

use clap::{Parser, ValueEnum, error::ErrorKind as ClapErrorKind};
use link_core::{
    ErrorKind, LinkError, ProcessExit, SCHEMA_VERSION,
    config::{ConfigLoader, ConfigOverrides, DaemonMode, DurationValue, LogLevel, OutputFormat},
    logging,
    output::Envelope,
    safety::{Operation, SafetyPolicy},
};
use serde_json::Value;

/// Root command-line parser. Functional subcommands are added only with their implementation.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "linkctl",
    version,
    about = "Safe Linux control foundation for Insta360 Link 2C Pro",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Serial, stable ID, USB path, or video node.
    #[arg(short = 'd', long, global = true, env = "LINKCTL_DEVICE")]
    pub device: Option<String>,

    /// Force a control backend instead of automatic selection.
    #[arg(long, global = true, value_enum, env = "LINKCTL_BACKEND")]
    pub backend: Option<BackendChoice>,

    /// Select whether daemon use is automatic, required, or disabled.
    #[arg(long, global = true, value_enum, env = "LINKCTL_DAEMON")]
    pub daemon: Option<DaemonChoice>,

    /// Select human, JSON, or JSON Lines output.
    #[arg(long, global = true, value_enum, env = "LINKCTL_FORMAT")]
    pub format: Option<FormatChoice>,

    /// Set the operation timeout, for example 500ms or 3s.
    #[arg(long, global = true, env = "LINKCTL_TIMEOUT")]
    pub timeout: Option<DurationValue>,

    /// Use this user configuration file instead of the default user file.
    #[arg(long, global = true, env = "LINKCTL_CONFIG")]
    pub config: Option<PathBuf>,

    /// Add a device-profile directory.
    #[arg(long, global = true, env = "LINKCTL_PROFILE_DIR")]
    pub profile_dir: Option<PathBuf>,

    /// Set application log verbosity.
    #[arg(long, global = true, value_enum, env = "LINKCTL_LOG_LEVEL")]
    pub log_level: Option<LogLevelChoice>,

    /// Disable colored terminal output.
    #[arg(long, global = true, env = "LINKCTL_NO_COLOR")]
    pub no_color: bool,

    /// Resolve and validate a mutation without applying it.
    #[arg(long, global = true, env = "LINKCTL_DRY_RUN")]
    pub dry_run: bool,

    /// Confirm commands that explicitly support non-interactive confirmation.
    #[arg(long, global = true, env = "LINKCTL_YES")]
    pub yes: bool,

    /// Request raw XU access (unavailable in this build).
    #[arg(long, global = true, env = "LINKCTL_UNSAFE_XU")]
    pub unsafe_xu: bool,

    /// Request a machine-output schema major version.
    #[arg(long, global = true, env = "LINKCTL_SCHEMA_VERSION")]
    pub schema_version: Option<u32>,
}

/// Requested semantic backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BackendChoice {
    /// Select the best verified backend.
    Auto,
    /// Use a standard kernel interface.
    Standard,
    /// Use a verified vendor profile.
    Vendor,
    /// Use host-side processing.
    Host,
}

/// Requested daemon policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum DaemonChoice {
    /// Select the daemon when appropriate.
    Auto,
    /// Require daemon use.
    Always,
    /// Disable daemon use.
    Never,
}

impl From<DaemonChoice> for DaemonMode {
    fn from(value: DaemonChoice) -> Self {
        match value {
            DaemonChoice::Auto => Self::Auto,
            DaemonChoice::Always => Self::Always,
            DaemonChoice::Never => Self::Never,
        }
    }
}

/// Requested output format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum FormatChoice {
    /// Concise terminal output.
    Human,
    /// One JSON object.
    Json,
    /// One JSON object per line.
    Jsonl,
}

impl From<FormatChoice> for OutputFormat {
    fn from(value: FormatChoice) -> Self {
        match value {
            FormatChoice::Human => Self::Human,
            FormatChoice::Json => Self::Json,
            FormatChoice::Jsonl => Self::Jsonl,
        }
    }
}

/// Requested tracing verbosity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LogLevelChoice {
    /// Disable logs.
    Off,
    /// Error events.
    Error,
    /// Warning events.
    Warn,
    /// Informational events.
    Info,
    /// Debug events.
    Debug,
    /// Trace events.
    Trace,
}

impl From<LogLevelChoice> for LogLevel {
    fn from(value: LogLevelChoice) -> Self {
        match value {
            LogLevelChoice::Off => Self::Off,
            LogLevelChoice::Error => Self::Error,
            LogLevelChoice::Warn => Self::Warn,
            LogLevelChoice::Info => Self::Info,
            LogLevelChoice::Debug => Self::Debug,
            LogLevelChoice::Trace => Self::Trace,
        }
    }
}

/// Run using the process argument vector.
#[must_use]
pub fn run_from_env() -> u8 {
    run(std::env::args_os().collect())
}

/// Run from an explicit argument vector.
#[must_use]
pub fn run(arguments: Vec<OsString>) -> u8 {
    let format_hint = output_format_hint(&arguments);
    let cli = match Cli::try_parse_from(&arguments) {
        Ok(cli) => cli,
        Err(error) => return emit_clap_error(error, format_hint),
    };

    let overrides = ConfigOverrides {
        default_device: cli.device.clone(),
        daemon: cli.daemon.map(Into::into),
        output: cli.format.map(Into::into),
        timeout: cli.timeout,
        profile_dir: cli.profile_dir.clone(),
        log_level: cli.log_level.map(Into::into),
        no_color: cli.no_color.then_some(true),
    };
    let config = match ConfigLoader::from_process(cli.config.clone(), overrides).load() {
        Ok(config) => config,
        Err(error) => return emit_link_error(format_hint, &error),
    };
    logging::init(config.log_level, config.no_color);

    if let Some(requested) = cli.schema_version
        && requested != SCHEMA_VERSION
    {
        let error = LinkError::new(
            ErrorKind::InvalidInvocation,
            "unsupported machine-output schema version",
        )
        .with_detail("requested", u64::from(requested))
        .with_detail("supported", u64::from(SCHEMA_VERSION));
        return emit_link_error(config.output, &error);
    }

    if cli.unsafe_xu {
        let policy = SafetyPolicy::new(config.safety);
        let error = policy
            .authorize(Operation::RawXuWrite)
            .expect_err("raw XU access is unavailable by contract");
        return emit_link_error(config.output, &error);
    }

    let error = LinkError::new(
        ErrorKind::InvalidInvocation,
        "a command is required; no functional commands are available in this build",
    );
    emit_link_error(config.output, &error)
}

fn emit_clap_error(error: clap::Error, format: OutputFormat) -> u8 {
    if matches!(
        error.kind(),
        ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion
    ) {
        print!("{error}");
        return ProcessExit::Success.code();
    }

    if format == OutputFormat::Human {
        eprint!("{error}");
        return ProcessExit::InvalidInvocation.code();
    }

    let link_error = LinkError::new(ErrorKind::InvalidInvocation, "invalid command invocation")
        .with_detail("parser", error.to_string());
    emit_link_error(format, &link_error)
}

fn emit_link_error(format: OutputFormat, error: &LinkError) -> u8 {
    match format {
        OutputFormat::Human => eprintln!("error: {}", error.message()),
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope: Envelope<Value> = Envelope::failure("linkctl", None, error);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => println!("{serialized}"),
                Err(serialization_error) => {
                    eprintln!("error: failed to serialize error output: {serialization_error}");
                }
            }
        }
    }
    error.process_exit().code()
}

fn output_format_hint(arguments: &[OsString]) -> OutputFormat {
    let mut iterator = arguments.iter().skip(1);
    while let Some(argument) = iterator.next() {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if let Some(value) = argument.strip_prefix("--format=") {
            return parse_format_hint(value);
        }
        if argument == "--format"
            && let Some(value) = iterator.next().and_then(|value| value.to_str())
        {
            return parse_format_hint(value);
        }
    }

    std::env::var("LINKCTL_FORMAT")
        .ok()
        .map_or(OutputFormat::Human, |value| parse_format_hint(&value))
}

fn parse_format_hint(value: &str) -> OutputFormat {
    match value.to_ascii_lowercase().as_str() {
        "json" => OutputFormat::Json,
        "jsonl" => OutputFormat::Jsonl,
        _ => OutputFormat::Human,
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn command_graph_excludes_mechanical_movement_commands() {
        let prohibited = ["pan", "tilt", "gimbal", "center-gimbal", "motor"];
        assert_command_names_are_absent(&Cli::command(), &prohibited);
    }

    fn assert_command_names_are_absent(command: &clap::Command, prohibited: &[&str]) {
        for subcommand in command.get_subcommands() {
            assert!(
                !prohibited.contains(&subcommand.get_name()),
                "prohibited semantic command: {}",
                subcommand.get_name()
            );
            assert_command_names_are_absent(subcommand, prohibited);
        }
    }
}
