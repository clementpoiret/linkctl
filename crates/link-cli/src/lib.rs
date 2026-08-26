//! Parser and shell-facing behavior for the `linkctl` binary.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind as ClapErrorKind};
use link_core::{
    ErrorKind, LinkError, ProcessExit, SCHEMA_VERSION,
    config::{
        Config, ConfigLoader, ConfigOverrides, DaemonMode, DurationValue, LogLevel, OutputFormat,
    },
    logging,
    output::{DeviceSummary, Envelope},
    probe::{DeviceListEntry, DeviceMode, HostReport, ProbeIssue, ProbeReport, VideoNodeKind},
    safety::{Operation, SafetyPolicy},
};
use link_linux::DiscoveredDevice;
use link_profiles::ProfileCatalog;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

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

    /// Read-only device discovery commands.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Implemented top-level commands.
#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Discover and inventory camera hardware.
    Device {
        /// Device operation.
        #[command(subcommand)]
        command: DeviceCommand,
    },
}

/// Read-only device operations.
#[derive(Clone, Debug, Subcommand)]
pub enum DeviceCommand {
    /// List associated camera devices and their Linux nodes.
    List {
        /// Include USB serials in output. Serial values are redacted by default.
        #[arg(long)]
        include_serial: bool,
    },
    /// Capture descriptors, V4L2 controls/formats, XUs, and audio capabilities.
    Probe {
        /// Include the USB serial in the report. Bundles are redacted unless this is explicit.
        #[arg(long)]
        include_serial: bool,

        /// Write a reusable probe bundle into a new directory.
        #[arg(long, value_name = "DIRECTORY")]
        bundle: Option<PathBuf>,
    },
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

    let result = match cli.command {
        Some(Command::Device {
            command: DeviceCommand::List { include_serial },
        }) => run_device_list(&config, include_serial),
        Some(Command::Device {
            command:
                DeviceCommand::Probe {
                    include_serial,
                    bundle,
                },
        }) => run_device_probe(&config, include_serial, bundle.as_deref()),
        None => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "a command is required",
        )),
    };
    match result {
        Ok(()) => ProcessExit::Success.code(),
        Err(error) => emit_link_error(config.output, &error),
    }
}

fn run_device_list(config: &Config, include_serial: bool) -> Result<(), LinkError> {
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let devices = link_linux::enumerate_devices()?
        .into_iter()
        .filter(link_linux::is_listable)
        .collect::<Vec<_>>();
    let include_serial = include_serial || !config.safety.redact_serials;
    let mut entries = Vec::with_capacity(devices.len());
    for device in &devices {
        let profile = catalog.report(&device.identity, device.mode())?;
        entries.push(device.list_entry(include_serial, profile.profile_id));
    }

    match config.output {
        OutputFormat::Human => emit_human_device_list(&entries),
        OutputFormat::Json | OutputFormat::Jsonl => {
            emit_success(config.output, "device.list", None, &entries)?;
        }
    }
    Ok(())
}

fn run_device_probe(
    config: &Config,
    include_serial: bool,
    bundle: Option<&Path>,
) -> Result<(), LinkError> {
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let devices = link_linux::enumerate_devices()?
        .into_iter()
        .filter(link_linux::is_listable)
        .collect::<Vec<_>>();
    let selected = select_one_device(&devices, config.default_device.as_deref())?;
    let include_serial = include_serial || !config.safety.redact_serials;
    let mut report = build_probe(selected, &catalog, include_serial)?;
    report.issues.extend(selected.issues.clone());

    if let Some(destination) = bundle {
        write_probe_bundle(destination, &report, &selected.descriptors)?;
    }

    let summary = DeviceSummary {
        stable_id: report.device.stable_id.clone(),
        model: report.device.model.clone(),
    };
    match config.output {
        OutputFormat::Human => emit_human_probe(&report, bundle),
        OutputFormat::Json | OutputFormat::Jsonl => {
            emit_success(config.output, "device.probe", Some(summary), &report)?;
        }
    }
    Ok(())
}

fn select_one_device<'a>(
    devices: &'a [DiscoveredDevice],
    selector: Option<&str>,
) -> Result<&'a DiscoveredDevice, LinkError> {
    if let Some(selector) = selector {
        let selected = link_linux::select_devices(devices, selector)?;
        return match selected.as_slice() {
            [device] => Ok(device),
            _ => Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "device probe requires exactly one device",
            )
            .with_detail("matches", selected.len() as u64)),
        };
    }
    match devices {
        [] => Err(LinkError::new(
            ErrorKind::DeviceNotFound,
            "no camera device was discovered",
        )),
        [device] => Ok(device),
        _ => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "multiple camera devices were discovered; select one with --device",
        )
        .with_detail("matches", devices.len() as u64)),
    }
}

fn build_probe(
    device: &DiscoveredDevice,
    catalog: &ProfileCatalog,
    include_serial: bool,
) -> Result<ProbeReport, LinkError> {
    let mode = device.mode();
    let profile = catalog.report(&device.identity, mode)?;
    let profile_id = profile.profile_id.clone();
    let captured_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            LinkError::new(
                ErrorKind::IoFailure,
                "system clock is before the Unix epoch",
            )
            .with_detail("reason", error.to_string())
        })?
        .as_millis();
    let mut report = ProbeReport::new(
        captured_unix_ms,
        env!("CARGO_PKG_VERSION"),
        HostReport {
            kernel_release: link_linux::kernel_release(),
            architecture: std::env::consts::ARCH.into(),
        },
        device.list_entry(include_serial, profile_id),
        profile,
        include_serial,
    );

    for node in &device.video_nodes {
        report
            .video
            .push(link_v4l2::probe_node(node.association.clone()));
    }

    if mode == DeviceMode::Camera {
        if let Some(capture) = report
            .video
            .iter()
            .find(|node| node.kind == VideoNodeKind::Capture)
        {
            match link_uvc_xu::inventory(Path::new(&capture.node.path), &device.descriptors) {
                Ok(extension_units) => report.extension_units = extension_units,
                Err(error) => {
                    report
                        .issues
                        .push(ProbeIssue::new("xu", error.kind().code(), error.message()))
                }
            }
        } else {
            report.issues.push(ProbeIssue::new(
                "xu",
                "no-capture-node",
                "no capture node was available for safe UVC Extension Unit queries",
            ));
        }
        report.audio = link_audio::probe(&device.alsa_card_indexes(), &device.identity);
    }
    Ok(report)
}

#[derive(Serialize)]
struct BundleManifest {
    schema_version: u32,
    files: BTreeMap<String, String>,
    redaction: BundleRedaction,
}

#[derive(Serialize)]
struct BundleRedaction {
    serial_included: bool,
    raw_descriptors_contain_string_values: bool,
    omitted: Vec<String>,
}

fn write_probe_bundle(
    destination: &Path,
    report: &ProbeReport,
    descriptors: &[u8],
) -> Result<(), LinkError> {
    link_linux::validate_bundle_parent(destination)?;
    fs::create_dir(destination).map_err(|error| bundle_io_error(destination, &error))?;
    let result = write_probe_bundle_contents(destination, report, descriptors);
    if result.is_err() {
        let _cleanup_result = fs::remove_dir_all(destination);
    }
    result
}

fn write_probe_bundle_contents(
    destination: &Path,
    report: &ProbeReport,
    descriptors: &[u8],
) -> Result<(), LinkError> {
    let mut probe_json = serde_json::to_vec_pretty(report).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to serialize probe report")
            .with_detail("reason", error.to_string())
    })?;
    probe_json.push(b'\n');
    let descriptor_name = "usb-descriptors.bin";
    let probe_name = "probe.json";
    fs::write(destination.join(probe_name), &probe_json)
        .map_err(|error| bundle_io_error(destination, &error))?;
    fs::write(destination.join(descriptor_name), descriptors)
        .map_err(|error| bundle_io_error(destination, &error))?;

    let manifest = BundleManifest {
        schema_version: 1,
        files: BTreeMap::from([
            (probe_name.into(), sha256(&probe_json)),
            (descriptor_name.into(), sha256(descriptors)),
        ]),
        redaction: BundleRedaction {
            serial_included: report.redaction.serial_included,
            raw_descriptors_contain_string_values: false,
            omitted: report.redaction.omitted.clone(),
        },
    };
    let mut manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to serialize probe manifest")
            .with_detail("reason", error.to_string())
    })?;
    manifest_json.push(b'\n');
    fs::write(destination.join("manifest.json"), manifest_json)
        .map_err(|error| bundle_io_error(destination, &error))?;
    Ok(())
}

fn bundle_io_error(path: &Path, error: &std::io::Error) -> LinkError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, "failed to write probe bundle")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn emit_human_device_list(entries: &[DeviceListEntry]) {
    if entries.is_empty() {
        println!("No camera devices found.");
        return;
    }
    println!("STABLE ID\tMODEL\tMODE\tVIDEO\tAUDIO");
    for entry in entries {
        let video = entry
            .video_nodes
            .iter()
            .map(|node| node.path.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let audio = entry
            .audio_nodes
            .iter()
            .map(|node| node.path.as_str())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{}\t{}\t{:?}\t{}\t{}",
            entry.stable_id, entry.model, entry.mode, video, audio
        );
    }
}

fn emit_human_probe(report: &ProbeReport, bundle: Option<&Path>) {
    println!(
        "Device: {} ({})",
        report.device.model, report.device.stable_id
    );
    println!(
        "USB: {:04x}:{:04x} revision {:04x}, mode {:?}",
        report.device.usb.vendor_id,
        report.device.usb.product_id,
        report.device.usb.device_revision,
        report.device.mode
    );
    println!(
        "Inventory: {} video nodes, {} controls, {} formats, {} extension units, {} ALSA PCMs, {} PipeWire objects",
        report.video.len(),
        report
            .video
            .iter()
            .map(|node| node.controls.len())
            .sum::<usize>(),
        report
            .video
            .iter()
            .map(|node| node.formats.len())
            .sum::<usize>(),
        report.extension_units.len(),
        report.audio.alsa.len(),
        report.audio.pipewire.len(),
    );
    println!(
        "Profile: {} (read-only)",
        report.profile.profile_id.as_deref().unwrap_or("unmatched")
    );
    if let Some(bundle) = bundle {
        println!("Bundle: {}", bundle.display());
    }
    let issue_count = report.issues.len()
        + report.audio.issues.len()
        + report
            .video
            .iter()
            .map(|node| node.issues.len())
            .sum::<usize>();
    if issue_count > 0 {
        println!("Recoverable issues: {issue_count} (use --format json for details)");
    }
}

fn emit_success<T: Serialize>(
    format: OutputFormat,
    command: &str,
    device: Option<DeviceSummary>,
    result: &T,
) -> Result<(), LinkError> {
    let value = serde_json::to_value(result).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to serialize command output")
            .with_detail("reason", error.to_string())
    })?;
    let envelope = Envelope::success(command, device, value);
    match format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&envelope).map_err(serialization_error)?
        ),
        OutputFormat::Jsonl => println!(
            "{}",
            serde_json::to_string(&envelope).map_err(serialization_error)?
        ),
        OutputFormat::Human => unreachable!("human output has dedicated renderers"),
    }
    Ok(())
}

fn serialization_error(error: serde_json::Error) -> LinkError {
    LinkError::new(ErrorKind::IoFailure, "failed to serialize command output")
        .with_detail("reason", error.to_string())
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
