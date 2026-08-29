//! Parser and shell-facing behavior for the `linkctl` binary.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "gstreamer")]
use std::io::Write;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, error::ErrorKind as ClapErrorKind};
use clap_complete::Shell;
#[cfg(feature = "gstreamer")]
use link_core::audio::{AudioDirection, AudioProcessing};
#[cfg(feature = "gstreamer")]
use link_core::media::{SnapshotControl, SnapshotMetadata};
use link_core::{
    ErrorKind, LinkError, ProcessExit, SCHEMA_VERSION,
    audio::{AudioBackendKind, AudioControlLayer, AudioEndpoint, AudioInventory},
    config::{
        Config, ConfigLoader, ConfigOverrides, DaemonMode, DurationValue, LogLevel, OutputFormat,
    },
    control::{
        CapabilityConfidence, CapabilityRecord, CapabilitySource, CapabilityState,
        ControlCapabilities, ControlChangeReport, ControlDescriptor, ControlEvent,
        ControlSetReport, ControlValue, RollbackReport, SemanticRange,
    },
    device::{DeviceEvent, DeviceState, DoctorCheck, DoctorReport, DoctorStatus},
    logging,
    media::VideoTuple,
    output::{DeviceSummary, Envelope},
    paths::AppPaths,
    preset::{
        PRESET_SCHEMA_VERSION, Preset, PresetAudio, PresetCategory, PresetRequirements,
        PresetStore, PresetSummary, PresetVideo,
    },
    probe::{
        DeviceListEntry, DeviceMode, HostReport, NodeAssociation, ProbeIssue, ProbeReport,
        Rational, VideoNodeKind,
    },
    safety::{Operation, SafetyPolicy},
    transaction::{
        DeviceLease, JournalStore, RecoveryJournal, TRANSACTION_SCHEMA_VERSION, TransactionOutcome,
        TransactionPlan, TransactionReport, TransactionStepKind, TransactionStepPlan,
        TransactionStepReport, TransactionStepStatus, new_transaction_id,
    },
};
use link_linux::DiscoveredDevice;
use link_profiles::{
    CodecKind, Persistence, ProfileCatalog, RollbackPolicy, SafetyClass, StreamRequirement,
    VerificationMethod,
};
use serde::Serialize;
use serde_json::{Value, json};
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

    /// Acknowledge raw XU access for a research-enabled build.
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
    /// Inspect semantic and raw capabilities.
    Caps {
        #[command(subcommand)]
        command: CapsCommand,
    },
    /// Inspect or change standard V4L2 controls.
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Inspect or change semantic image controls through verified standard controls.
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// Inspect or change camera-native digital zoom.
    Zoom {
        #[command(subcommand)]
        command: ZoomCommand,
    },
    /// Inspect or change verified digital frame translation.
    Frame {
        #[command(subcommand)]
        command: FrameCommand,
    },
    /// Inspect or change camera-native automatic framing.
    AutoFraming {
        #[command(subcommand)]
        command: AutoFramingCommand,
    },
    /// Inspect or change camera-native operating modes.
    Mode {
        #[command(subcommand)]
        command: ModeCommand,
    },
    /// Inspect or configure camera-resident gesture recognition.
    Gesture {
        #[command(subcommand)]
        command: GestureCommand,
    },
    /// Inspect or change native portrait operation.
    Portrait {
        #[command(subcommand)]
        command: PortraitCommand,
    },
    /// Report physical and logical privacy state without actuating the shutter.
    Privacy {
        #[command(subcommand)]
        command: PrivacyCommand,
    },
    /// Inspect verified camera firmware information.
    Firmware {
        #[command(subcommand)]
        command: FirmwareCommand,
    },
    /// Inspect or negotiate exact V4L2 video formats.
    Video {
        #[command(subcommand)]
        command: VideoCommand,
    },
    /// Discover, control, capture, meter, and monitor Linux audio devices.
    Audio {
        #[command(subcommand)]
        command: AudioCommand,
    },
    /// Save, inspect, and transactionally apply local camera presets.
    Preset {
        #[command(subcommand)]
        command: PresetCommand,
    },
    /// Inspect and research UVC Extension Unit controls safely.
    Xu {
        #[command(subcommand)]
        command: XuCommand,
    },
    /// Capture one or more JPEG, PNG, or raw compressed frames.
    #[cfg(feature = "gstreamer")]
    Snapshot(SnapshotArgs),
    /// Copy a raw H.264 or MJPEG stream to standard output.
    #[cfg(feature = "gstreamer")]
    Capture(CaptureArgs),
    /// Record a foreground video stream with optional audio.
    #[cfg(feature = "gstreamer")]
    Record {
        #[command(subcommand)]
        command: RecordCommand,
    },
    /// Restream video through an explicitly enabled network backend.
    #[cfg(feature = "network")]
    Stream {
        #[command(subcommand)]
        command: StreamCommand,
    },
    /// Run read-only configuration, permission, and device diagnostics.
    Doctor(DoctorArgs),
    /// Generate shell completion source for implemented commands.
    Completion {
        /// Target shell.
        #[arg(value_enum)]
        shell: Shell,
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
    /// Show detailed inventory, capabilities, and availability.
    Info {
        /// Include USB serials in output. Serial values are redacted by default.
        #[arg(long)]
        include_serial: bool,
    },
    /// Report camera personality, stream, indicator, and error state.
    State,
    /// Watch add, remove, re-enumeration, and profile changes.
    Watch,
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

/// Implemented capability reports.
#[derive(Clone, Debug, Subcommand)]
pub enum CapsCommand {
    /// Report every implemented, unavailable, and unmapped semantic capability.
    All,
    /// Report semantic image capabilities and all raw V4L2 controls.
    Controls,
}

/// Generic standard-control operations.
#[derive(Clone, Debug, Subcommand)]
pub enum ControlCommand {
    /// List every extended control on the selected capture node.
    List,
    /// Read one control by name or numeric ID.
    Get {
        /// Canonical name, kernel name, decimal ID, or hexadecimal ID.
        control: String,
    },
    /// Set one control or a deterministic batch.
    Set {
        /// Control name or numeric ID for a single write.
        control: Option<String>,
        /// Value for a single write.
        value: Option<String>,
        /// Ordered `CONTROL=VALUE` changes submitted with `VIDIOC_S_EXT_CTRLS`.
        #[arg(long, num_args = 1.., value_name = "CONTROL=VALUE")]
        batch: Vec<String>,
        /// Bypass known automatic/manual prerequisite changes.
        #[arg(long)]
        raw: bool,
        /// Clamp percentages and normalized values rather than rejecting them.
        #[arg(long)]
        clamp: bool,
        /// Fall back to ordered individual writes when the batch ioctl fails.
        #[arg(long)]
        fallback_individual: bool,
    },
    /// Restore one control to a valid driver-advertised default.
    Reset {
        /// Canonical name, kernel name, decimal ID, or hexadecimal ID.
        control: String,
        /// Bypass known automatic/manual prerequisite changes.
        #[arg(long)]
        raw: bool,
    },
    /// Watch selected controls, or every readable scalar when none are named.
    Watch {
        /// Optional control names or IDs.
        controls: Vec<String>,
    },
}

/// Semantic image operations backed only by verified standard controls.
#[derive(Clone, Debug, Subcommand)]
pub enum ImageCommand {
    /// Show every semantic image capability and current value.
    Status,
    /// Select automatic exposure or configure manual exposure fields.
    Exposure {
        #[command(subcommand)]
        command: ExposureCommand,
    },
    /// Set exposure compensation in EV.
    ExposureCompensation { ev: f64 },
    /// Use `auto` or a Kelvin value such as `5000K`.
    WhiteBalance { value: String },
    /// Select automatic or normalized manual focus.
    Focus {
        #[command(subcommand)]
        command: FocusCommand,
    },
    /// Set normalized brightness from 0.0 to 1.0.
    Brightness(ScalarImageValue),
    /// Set normalized contrast from 0.0 to 1.0.
    Contrast(ScalarImageValue),
    /// Set normalized saturation from 0.0 to 1.0.
    Saturation(ScalarImageValue),
    /// Set normalized sharpness from 0.0 to 1.0.
    Sharpness(ScalarImageValue),
    /// Set normalized image gain from 0.0 to 1.0.
    Gain(ScalarImageValue),
    /// Set normalized backlight compensation from 0.0 to 1.0.
    BacklightCompensation(ScalarImageValue),
    /// Set power-line frequency behavior.
    AntiFlicker { value: AntiFlickerChoice },
    /// Set HDR through a standard or verified camera-native control.
    Hdr { value: ToggleChoice },
    /// Set camera-native horizontal mirroring when a verified mapping exists.
    Mirror { value: ToggleChoice },
    /// Set camera-native vertical flipping when a verified mapping exists.
    Flip { value: ToggleChoice },
    /// Reset every present semantic image control with a valid default.
    Reset,
}

/// Camera-native digital zoom operations.
#[derive(Clone, Debug, Subcommand)]
pub enum ZoomCommand {
    /// Read the current camera-native zoom factor.
    Get,
    /// Set an exact zoom factor such as `1.5x`.
    Set { factor: String },
    /// Add a signed zoom delta such as `+0.1x`.
    Step { delta: String },
    /// Move linearly between two factors over a bounded duration.
    Ramp {
        from: String,
        to: String,
        #[arg(long)]
        duration: DurationValue,
    },
    /// Restore the driver-advertised zoom default.
    Reset,
}

/// Digital frame translation operations.
#[derive(Clone, Debug, Subcommand)]
pub enum FrameCommand {
    Status,
    Set {
        #[arg(long)]
        x: f64,
        #[arg(long)]
        y: f64,
    },
    Move {
        #[arg(long, allow_hyphen_values = true)]
        x: f64,
        #[arg(long, allow_hyphen_values = true)]
        y: f64,
    },
    Center,
}

/// Camera-native automatic-framing operations.
#[derive(Clone, Debug, Subcommand)]
pub enum AutoFramingCommand {
    On,
    Off,
    Status,
    Style { style: AutoFramingStyle },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AutoFramingStyle {
    Head,
    HalfBody,
}

/// Camera-native mode operations.
#[derive(Clone, Debug, Subcommand)]
pub enum ModeCommand {
    Normal,
    Whiteboard {
        #[command(subcommand)]
        command: ToggleStatusCommand,
    },
    Compatibility {
        #[command(subcommand)]
        command: CompatibilityCommand,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum CompatibilityCommand {
    Status,
    Set { value: CompatibilityMode },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompatibilityMode {
    Standard,
    LowResolution,
    Yuy2,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ToggleStatusCommand {
    On,
    Off,
    Status,
}

/// Camera-resident gesture configuration.
#[derive(Clone, Debug, Subcommand)]
pub enum GestureCommand {
    Status,
    Enable,
    Disable,
    Set {
        gesture: GestureKind,
        action: GestureAction,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum GestureKind {
    Palm,
    VSign,
    LSign,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum GestureAction {
    Disabled,
    AutoFraming,
    Whiteboard,
    Zoom,
}

/// Native portrait operations. Host portrait is implemented by the daemon later.
#[derive(Clone, Debug, Subcommand)]
pub enum PortraitCommand {
    Status,
    Native {
        #[command(subcommand)]
        command: PortraitNativeCommand,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum PortraitNativeCommand {
    Enable {
        #[arg(long)]
        horizontal_correction: Option<ToggleChoice>,
        #[arg(long)]
        vertical_correction: Option<ToggleChoice>,
    },
    Disable,
}

#[derive(Clone, Debug, Subcommand)]
pub enum PrivacyCommand {
    Status,
}

#[derive(Clone, Debug, Subcommand)]
pub enum FirmwareCommand {
    Info,
}

/// V4L2 video format and direct stream statistics operations.
#[derive(Clone, Debug, Subcommand)]
pub enum VideoCommand {
    /// List kernel-advertised formats, sizes, and frame rates.
    Formats(VideoFilterArgs),
    /// Show the current format, frame rate, and colorimetry.
    Status,
    /// Set and verify one exact format tuple.
    Set {
        /// Canonical four-character pixel format.
        #[arg(long)]
        fourcc: String,
        /// Exact frame size such as 3840x2160.
        #[arg(long)]
        size: String,
        /// Exact FPS as an integer, decimal, or rational.
        #[arg(long)]
        fps: String,
    },
    /// Sample a direct stream and report frame/drop/timestamp statistics.
    Stats {
        #[command(flatten)]
        tuple: VideoTupleArgs,
        /// Measurement duration.
        #[arg(long, default_value = "10s")]
        duration: DurationValue,
    },
}

/// Filters for `video formats`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct VideoFilterArgs {
    /// Filter by canonical FourCC.
    #[arg(long)]
    pub fourcc: Option<String>,
    /// Filter by exact size.
    #[arg(long)]
    pub size: Option<String>,
    /// Filter by exact FPS.
    #[arg(long)]
    pub fps: Option<String>,
}

/// Optional media source tuple fields.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct VideoTupleArgs {
    /// Preferred source FourCC; defaults through configured transport preference.
    #[arg(long)]
    pub fourcc: Option<String>,
    /// Source size; defaults to the current width and height.
    #[arg(long)]
    pub size: Option<String>,
    /// Source frame rate; defaults to the current frame rate.
    #[arg(long)]
    pub fps: Option<String>,
}

/// Standard and host audio operations.
#[derive(Clone, Debug, Subcommand)]
pub enum AudioCommand {
    /// List logical capture and playback endpoints with ALSA/PipeWire routes.
    Devices,
    /// Show hardware and host-session gain/mute state.
    Status {
        /// `camera`, a stable audio ID, or `alsa:`/`pipewire:` selector.
        #[arg(long, default_value = "camera")]
        source: String,
    },
    /// Set normalized capture gain.
    Gain {
        /// Percentage such as `70%`, or normalized value such as `0.7`.
        gain: String,
        /// `camera`, a stable audio ID, or `alsa:`/`pipewire:` selector.
        #[arg(long, default_value = "camera")]
        source: String,
        /// Clamp a value outside the supported normalized range.
        #[arg(long)]
        clamp: bool,
    },
    /// Mute the selected capture source.
    Mute {
        #[arg(long, default_value = "camera")]
        source: String,
    },
    /// Unmute the selected capture source.
    Unmute {
        #[arg(long, default_value = "camera")]
        source: String,
    },
    /// Inspect or change the camera's verified pickup mode.
    Mode {
        #[command(subcommand)]
        command: AudioModeCommand,
    },
    /// Emit periodic peak/RMS and discontinuity events.
    #[cfg(feature = "gstreamer")]
    Meter(AudioMeterArgs),
    /// Capture WAV, FLAC, or headerless signed 16-bit PCM.
    #[cfg(feature = "gstreamer")]
    Capture(AudioCaptureArgs),
    /// Monitor a capture source through a playback sink.
    #[cfg(feature = "gstreamer")]
    Monitor(AudioMonitorArgs),
}

#[derive(Clone, Debug, Subcommand)]
pub enum AudioModeCommand {
    Status,
    Standard,
    Wide,
    Focus,
    Original,
}

/// Local preset operations.
#[derive(Clone, Debug, Subcommand)]
pub enum PresetCommand {
    /// Capture selected live state into a new local preset.
    Save {
        name: String,
        /// Optional human-readable description.
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated state groups to capture instead of all implemented groups.
        #[arg(long, value_enum, value_delimiter = ',')]
        include: Vec<PresetCategoryChoice>,
        /// Comma-separated state groups removed after inclusion.
        #[arg(long, value_enum, value_delimiter = ',')]
        exclude: Vec<PresetCategoryChoice>,
    },
    /// Validate and transactionally apply a named preset.
    Apply { name: String },
    /// List local presets in deterministic name order.
    List,
    /// Show one parsed preset.
    Show { name: String },
    /// Delete one explicitly named preset.
    Delete { name: String },
    /// Export canonical TOML to a new file, or `-` for standard output.
    Export { name: String, output: PathBuf },
    /// Validate and import a preset without replacing an existing name.
    Import { file: PathBuf },
}

/// Safe Extension Unit inventory and research operations.
#[derive(Clone, Debug, Subcommand)]
pub enum XuCommand {
    /// Parse the complete VideoControl descriptor graph and query selector capabilities.
    Inventory {
        /// Include retained raw descriptors and bytes.
        #[arg(long)]
        raw: bool,
    },
    /// Read one advertised selector using its exact device-reported length.
    Get {
        #[arg(long, conflicts_with = "unit")]
        guid: Option<String>,
        #[arg(long, conflicts_with = "guid")]
        unit: Option<u8>,
        #[arg(long)]
        selector: u8,
        /// Assert, but never override, the GET_LEN result.
        #[arg(long)]
        length: Option<u16>,
    },
    /// Capture repeated XU and standard-control state into a new JSON file.
    Snapshot {
        output: PathBuf,
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=64))]
        samples: u8,
        #[arg(long, default_value = "250ms")]
        interval: DurationValue,
        #[arg(long)]
        note: Vec<String>,
        #[arg(long)]
        include_volatile: bool,
    },
    /// Compare two saved XU snapshots without hardware.
    Diff { before: PathBuf, after: PathBuf },
    /// Emit selector changes continuously.
    Watch {
        #[arg(long, default_value = "250ms")]
        interval: DurationValue,
        #[arg(long)]
        include_volatile: bool,
    },
    /// Apply one semantic control from an exact trusted verified profile.
    Set { control: String, value: String },
    /// Apply an exact profile-classified payload in a research-enabled build.
    RawSet {
        #[arg(long)]
        guid: String,
        #[arg(long)]
        selector: u8,
        #[arg(long)]
        hex: String,
    },
    /// Reopen the XU handle and rebuild a bounded no-output probe stream.
    Recover,
}

#[derive(Clone, Debug, clap::Args)]
pub struct DoctorArgs {
    /// Write a redacted, checksummed tar.zst diagnostic archive.
    #[arg(long, value_name = "REPORT.tar.zst")]
    bundle: Option<PathBuf>,
}

/// Implemented preset capture groups.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
pub enum PresetCategoryChoice {
    Video,
    Image,
    Zoom,
    Controls,
    Audio,
}

impl From<PresetCategoryChoice> for PresetCategory {
    fn from(value: PresetCategoryChoice) -> Self {
        match value {
            PresetCategoryChoice::Video => Self::Video,
            PresetCategoryChoice::Image => Self::Image,
            PresetCategoryChoice::Zoom => Self::Zoom,
            PresetCategoryChoice::Controls => Self::Controls,
            PresetCategoryChoice::Audio => Self::Audio,
        }
    }
}

/// Optional host audio processing presets.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, Default, clap::Args)]
pub struct AudioProcessingArgs {
    /// Enable the fixed conservative noise-gate preset.
    #[arg(long)]
    pub gate: bool,
    /// Enable the fixed conservative compressor preset.
    #[arg(long)]
    pub compressor: bool,
    /// Enable the fixed conservative limiter preset.
    #[arg(long)]
    pub limiter: bool,
}

#[cfg(feature = "gstreamer")]
impl From<AudioProcessingArgs> for AudioProcessing {
    fn from(value: AudioProcessingArgs) -> Self {
        Self {
            gate: value.gate,
            compressor: value.compressor,
            limiter: value.limiter,
        }
    }
}

/// Common resolved audio source arguments.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, clap::Args)]
pub struct AudioStreamArgs {
    /// `camera`, a stable audio ID, or `alsa:`/`pipewire:` selector.
    #[arg(long, default_value = "camera")]
    pub source: String,
    /// Output sample rate; defaults to the selected source's native maximum.
    #[arg(long)]
    pub sample_rate: Option<u32>,
    /// Output channel count; defaults to the selected source's native minimum.
    #[arg(long)]
    pub channels: Option<u32>,
    #[command(flatten)]
    pub processing: AudioProcessingArgs,
}

/// Audio meter arguments.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, clap::Args)]
pub struct AudioMeterArgs {
    #[command(flatten)]
    pub stream: AudioStreamArgs,
    /// Stop after this duration; otherwise run until interrupted.
    #[arg(long)]
    pub duration: Option<DurationValue>,
    /// Peak/RMS event interval.
    #[arg(long, default_value = "100ms")]
    pub interval: DurationValue,
}

/// Standalone audio capture arguments.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, clap::Args)]
pub struct AudioCaptureArgs {
    /// Output path. Omit only with `--stdout`.
    pub output: Option<PathBuf>,
    /// Write binary audio to standard output.
    #[arg(long)]
    pub stdout: bool,
    /// Encoding override; required for standard output.
    #[arg(long, value_enum)]
    pub audio_format: Option<AudioFormatChoice>,
    #[command(flatten)]
    pub stream: AudioStreamArgs,
    /// Stop and finalize after this duration.
    #[arg(long)]
    pub duration: Option<DurationValue>,
    /// Replace an existing output file.
    #[arg(long)]
    pub overwrite: bool,
}

/// Live audio monitoring arguments.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, clap::Args)]
pub struct AudioMonitorArgs {
    #[command(flatten)]
    pub stream: AudioStreamArgs,
    /// Playback endpoint ID, `alsa:`/`pipewire:` selector, or `default`.
    #[arg(long, default_value = "default")]
    pub sink: String,
    /// Target buffering latency.
    #[arg(long, default_value = "50ms")]
    pub latency: DurationValue,
    /// Stop after this duration; otherwise run until interrupted.
    #[arg(long)]
    pub duration: Option<DurationValue>,
}

/// Standalone audio output encoding.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AudioFormatChoice {
    Wav,
    Flac,
    Raw,
}

/// Snapshot command arguments.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, clap::Args)]
pub struct SnapshotArgs {
    /// Output image path, or `-` for binary stdout.
    pub output: PathBuf,
    /// Decoded image encoding. Required for stdout unless `--raw-frame` is used.
    #[arg(long, value_enum)]
    pub image_format: Option<ImageFormatChoice>,
    /// Write one source-compressed MJPEG frame or H.264 access unit.
    #[arg(long)]
    pub raw_frame: bool,
    #[command(flatten)]
    pub tuple: VideoTupleArgs,
    /// Number of frames to capture.
    #[arg(long, default_value_t = 1)]
    pub count: u32,
    /// Delay between burst frames.
    #[arg(long, default_value = "0s")]
    pub interval: DurationValue,
    /// Do not write the default adjacent metadata sidecar.
    #[arg(long)]
    pub no_metadata: bool,
    /// Replace existing output and metadata files.
    #[arg(long)]
    pub overwrite: bool,
}

/// Raw stdout capture arguments.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, clap::Args)]
pub struct CaptureArgs {
    /// Encoded transport written to stdout.
    #[arg(long, value_enum)]
    pub video: EncodedVideoChoice,
    /// Confirm binary output on standard output.
    #[arg(long, required = true)]
    pub stdout: bool,
    /// Source size; defaults to the current width and height.
    #[arg(long)]
    pub size: Option<String>,
    /// Source frame rate; defaults to the current frame rate.
    #[arg(long)]
    pub fps: Option<String>,
    /// Stop after this duration; otherwise run until interrupted or the pipe closes.
    #[arg(long)]
    pub duration: Option<DurationValue>,
}

/// Foreground recording operations.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Debug, Subcommand)]
pub enum RecordCommand {
    /// Start a blocking recording.
    Start {
        /// Final output path; segmented output derives numbered siblings.
        output: PathBuf,
        #[command(flatten)]
        tuple: VideoTupleArgs,
        /// Override the container inferred from the extension/configuration.
        #[arg(long, value_enum)]
        container: Option<ContainerChoice>,
        /// Fail unless the encoded camera stream can pass through without decode/re-encode.
        #[arg(long)]
        video_copy: bool,
        /// Stop and finalize after this duration.
        #[arg(long)]
        duration: Option<DurationValue>,
        /// Stop and finalize after total output reaches this size.
        #[arg(long)]
        max_size: Option<String>,
        /// Start a new numbered file after this duration.
        #[arg(long)]
        segment_duration: Option<DurationValue>,
        /// Start a new numbered file after this size.
        #[arg(long)]
        segment_size: Option<String>,
        /// Retain at most this many numbered segments.
        #[arg(long)]
        rolling: Option<u32>,
        /// Override the configured free-space reserve.
        #[arg(long)]
        disk_reserve: Option<String>,
        /// Replace conflicting output files.
        #[arg(long)]
        overwrite: bool,
        /// Opt in to audio with `camera`, a stable audio ID, or an explicit backend selector.
        #[arg(long)]
        audio: Option<String>,
        /// Signed audio timestamp offset, such as `25ms` or `-40ms`.
        #[arg(long, default_value = "0ms")]
        audio_delay: String,
        #[command(flatten)]
        audio_processing: AudioProcessingArgs,
    },
}

/// Feature-gated RTP streaming operations.
#[cfg(feature = "network")]
#[derive(Clone, Debug, Subcommand)]
pub enum StreamCommand {
    /// Start blocking RTP/UDP output to an explicit destination.
    Start {
        /// Destination hostname or IP address.
        #[arg(long)]
        host: String,
        /// Destination UDP port.
        #[arg(long)]
        port: u16,
        #[command(flatten)]
        tuple: VideoTupleArgs,
        /// RTP dynamic payload type; MJPEG defaults to 26 and H.264 to 96.
        #[arg(long)]
        payload_type: Option<u8>,
        /// Stop after this duration; otherwise run until interrupted.
        #[arg(long)]
        duration: Option<DurationValue>,
    },
}

/// Decoded snapshot encoding.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ImageFormatChoice {
    Jpeg,
    Png,
}

/// Encoded pass-through transport.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EncodedVideoChoice {
    H264,
    Mjpeg,
}

/// Recording container.
#[cfg(feature = "gstreamer")]
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ContainerChoice {
    Matroska,
    Mp4,
}

/// Normalized semantic scalar arguments.
#[derive(Clone, Debug, clap::Args)]
pub struct ScalarImageValue {
    /// Normalized value between 0.0 and 1.0.
    pub value: f64,
    /// Clamp an out-of-range value instead of rejecting it.
    #[arg(long)]
    pub clamp: bool,
}

/// Exposure mode and manual values.
#[derive(Clone, Debug, Subcommand)]
pub enum ExposureCommand {
    /// Enable automatic exposure controls that are present.
    Auto,
    /// Disable automatic exposure and set one or both manual fields.
    Manual {
        /// Shutter duration such as `1/120`, `8.3ms`, or `8333us`.
        #[arg(long)]
        shutter: Option<String>,
        /// ISO sensitivity.
        #[arg(long)]
        iso: Option<i64>,
    },
}

/// Focus mode.
#[derive(Clone, Debug, Subcommand)]
pub enum FocusCommand {
    /// Enable continuous autofocus.
    Auto,
    /// Disable autofocus and set normalized manual focus.
    Manual(ScalarImageValue),
}

/// Power-line frequency values.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AntiFlickerChoice {
    Disabled,
    #[value(name = "50hz")]
    FiftyHz,
    #[value(name = "60hz")]
    SixtyHz,
    Auto,
}

/// On/off command value.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ToggleChoice {
    On,
    Off,
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
    let loader = ConfigLoader::from_process(cli.config.clone(), overrides);
    let config = match loader.clone().load() {
        Ok(config) => config,
        Err(error) => return emit_link_error(format_hint, &error),
    };
    let config = match load_selected_device_config(&loader, config, cli.command.as_ref()) {
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

    let command_id = command_identifier(cli.command.as_ref());
    let binary_stdout = uses_binary_stdout(cli.command.as_ref());
    let result = match cli.command {
        Some(Command::Device {
            command: DeviceCommand::List { include_serial },
        }) => run_device_list(&config, include_serial),
        Some(Command::Device {
            command: DeviceCommand::Info { include_serial },
        }) => run_device_info(&config, include_serial),
        Some(Command::Device {
            command: DeviceCommand::State,
        }) => run_device_state(&config),
        Some(Command::Device {
            command: DeviceCommand::Watch,
        }) => run_device_watch(&config),
        Some(Command::Device {
            command:
                DeviceCommand::Probe {
                    include_serial,
                    bundle,
                },
        }) => run_device_probe(&config, include_serial, bundle.as_deref()),
        Some(Command::Caps {
            command: CapsCommand::Controls,
        }) => run_caps_controls(&config, cli.backend),
        Some(Command::Caps {
            command: CapsCommand::All,
        }) => run_caps_all(&config, cli.backend),
        Some(Command::Control { command }) => {
            run_control(&config, cli.backend, command, cli.dry_run, cli.yes)
        }
        Some(Command::Image { command }) => {
            run_image(&config, cli.backend, command, cli.dry_run, cli.yes)
        }
        Some(Command::Zoom { command }) => {
            run_zoom(&config, cli.backend, command, cli.dry_run, cli.yes)
        }
        Some(Command::Frame { command }) => run_frame(&config, command, cli.dry_run),
        Some(Command::AutoFraming { command }) => run_auto_framing(&config, command, cli.dry_run),
        Some(Command::Mode { command }) => run_mode(&config, command, cli.dry_run),
        Some(Command::Gesture { command }) => run_gesture(&config, command, cli.dry_run),
        Some(Command::Portrait { command }) => run_portrait(&config, command, cli.dry_run),
        Some(Command::Privacy { command }) => run_privacy(&config, command),
        Some(Command::Firmware { command }) => run_firmware(&config, command),
        Some(Command::Video { command }) => run_video(&config, cli.backend, command, cli.dry_run),
        Some(Command::Audio { command }) => run_audio(&config, cli.backend, command, cli.dry_run),
        Some(Command::Preset { command }) => run_preset(&config, cli.backend, command, cli.dry_run),
        Some(Command::Xu { command }) => run_xu(&config, command, cli.dry_run, cli.unsafe_xu),
        #[cfg(feature = "gstreamer")]
        Some(Command::Snapshot(arguments)) => {
            run_snapshot(&config, cli.backend, arguments, cli.dry_run)
        }
        #[cfg(feature = "gstreamer")]
        Some(Command::Capture(arguments)) => {
            run_capture(&config, cli.backend, arguments, cli.dry_run)
        }
        #[cfg(feature = "gstreamer")]
        Some(Command::Record { command }) => run_record(&config, cli.backend, command, cli.dry_run),
        #[cfg(feature = "network")]
        Some(Command::Stream { command }) => run_stream(&config, cli.backend, command, cli.dry_run),
        Some(Command::Doctor(arguments)) => run_doctor(&config, arguments.bundle.as_deref()),
        Some(Command::Completion { shell }) => run_completion(&config, shell),
        None => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "a command is required",
        )),
    };
    match result {
        Ok(()) => ProcessExit::Success.code(),
        Err(error) if binary_stdout => {
            emit_command_error_to_stderr(config.output, command_id, None, &error)
        }
        Err(error) => emit_command_error(config.output, command_id, None, &error),
    }
}

fn load_selected_device_config(
    loader: &ConfigLoader,
    base: Config,
    command: Option<&Command>,
) -> Result<Config, LinkError> {
    if !command_uses_device_defaults(command) || base.default_device.as_deref() == Some("all") {
        return Ok(base);
    }
    let devices = discovered_devices()?;
    let selected = if let Some(selector) = base.default_device.as_deref() {
        link_linux::select_devices(&devices, selector)?
    } else if devices.len() == 1 {
        vec![&devices[0]]
    } else {
        return Ok(base);
    };
    match selected.as_slice() {
        [device] => loader
            .clone()
            .with_device(&device.identity.stable_id())?
            .load(),
        _ => Ok(base),
    }
}

fn command_uses_device_defaults(command: Option<&Command>) -> bool {
    match command {
        Some(
            Command::Control { .. }
            | Command::Image { .. }
            | Command::Zoom { .. }
            | Command::Frame { .. }
            | Command::AutoFraming { .. }
            | Command::Mode { .. }
            | Command::Gesture { .. }
            | Command::Portrait { .. }
            | Command::Privacy { .. }
            | Command::Firmware { .. }
            | Command::Video { .. },
        ) => true,
        Some(Command::Xu {
            command: XuCommand::Diff { .. },
        }) => false,
        Some(Command::Xu { .. }) => true,
        Some(Command::Preset {
            command: PresetCommand::Save { .. } | PresetCommand::Apply { .. },
        }) => true,
        Some(Command::Audio { command }) => match command {
            AudioCommand::Devices => false,
            AudioCommand::Status { source }
            | AudioCommand::Gain { source, .. }
            | AudioCommand::Mute { source }
            | AudioCommand::Unmute { source } => source == "camera",
            AudioCommand::Mode { .. } => true,
            #[cfg(feature = "gstreamer")]
            AudioCommand::Meter(arguments) => arguments.stream.source == "camera",
            #[cfg(feature = "gstreamer")]
            AudioCommand::Capture(arguments) => arguments.stream.source == "camera",
            #[cfg(feature = "gstreamer")]
            AudioCommand::Monitor(arguments) => arguments.stream.source == "camera",
        },
        #[cfg(feature = "gstreamer")]
        Some(Command::Snapshot(_) | Command::Capture(_) | Command::Record { .. }) => true,
        #[cfg(feature = "network")]
        Some(Command::Stream { .. }) => true,
        _ => false,
    }
}

fn uses_binary_stdout(command: Option<&Command>) -> bool {
    match command {
        #[cfg(feature = "gstreamer")]
        Some(Command::Capture(_)) => true,
        #[cfg(feature = "gstreamer")]
        Some(Command::Snapshot(arguments)) => arguments.output == Path::new("-"),
        #[cfg(feature = "gstreamer")]
        Some(Command::Audio {
            command: AudioCommand::Capture(arguments),
        }) => arguments.stdout,
        Some(Command::Preset {
            command: PresetCommand::Export { output, .. },
        }) => output == Path::new("-"),
        _ => false,
    }
}

fn command_identifier(command: Option<&Command>) -> &'static str {
    match command {
        Some(Command::Device { command }) => match command {
            DeviceCommand::List { .. } => "device.list",
            DeviceCommand::Info { .. } => "device.info",
            DeviceCommand::State => "device.state",
            DeviceCommand::Watch => "device.watch",
            DeviceCommand::Probe { .. } => "device.probe",
        },
        Some(Command::Caps { command }) => match command {
            CapsCommand::All => "caps.all",
            CapsCommand::Controls => "caps.controls",
        },
        Some(Command::Control { command }) => match command {
            ControlCommand::List => "control.list",
            ControlCommand::Get { .. } => "control.get",
            ControlCommand::Set { .. } => "control.set",
            ControlCommand::Reset { .. } => "control.reset",
            ControlCommand::Watch { .. } => "control.watch",
        },
        Some(Command::Image { command }) => match command {
            ImageCommand::Status => "image.status",
            ImageCommand::Exposure { .. } => "image.exposure",
            ImageCommand::ExposureCompensation { .. } => "image.exposure-compensation",
            ImageCommand::WhiteBalance { .. } => "image.white-balance",
            ImageCommand::Focus { .. } => "image.focus",
            ImageCommand::Brightness(_) => "image.brightness",
            ImageCommand::Contrast(_) => "image.contrast",
            ImageCommand::Saturation(_) => "image.saturation",
            ImageCommand::Sharpness(_) => "image.sharpness",
            ImageCommand::Gain(_) => "image.gain",
            ImageCommand::BacklightCompensation(_) => "image.backlight-compensation",
            ImageCommand::AntiFlicker { .. } => "image.anti-flicker",
            ImageCommand::Hdr { .. } => "image.hdr",
            ImageCommand::Mirror { .. } => "image.mirror",
            ImageCommand::Flip { .. } => "image.flip",
            ImageCommand::Reset => "image.reset",
        },
        Some(Command::Zoom { command }) => match command {
            ZoomCommand::Get => "zoom.get",
            ZoomCommand::Set { .. } => "zoom.set",
            ZoomCommand::Step { .. } => "zoom.step",
            ZoomCommand::Ramp { .. } => "zoom.ramp",
            ZoomCommand::Reset => "zoom.reset",
        },
        Some(Command::Frame { command }) => match command {
            FrameCommand::Status => "frame.status",
            FrameCommand::Set { .. } => "frame.set",
            FrameCommand::Move { .. } => "frame.move",
            FrameCommand::Center => "frame.center",
        },
        Some(Command::AutoFraming { command }) => match command {
            AutoFramingCommand::On => "auto-framing.on",
            AutoFramingCommand::Off => "auto-framing.off",
            AutoFramingCommand::Status => "auto-framing.status",
            AutoFramingCommand::Style { .. } => "auto-framing.style",
        },
        Some(Command::Mode { command }) => match command {
            ModeCommand::Normal => "mode.normal",
            ModeCommand::Whiteboard { .. } => "mode.whiteboard",
            ModeCommand::Compatibility { .. } => "mode.compatibility",
        },
        Some(Command::Gesture { command }) => match command {
            GestureCommand::Status => "gesture.status",
            GestureCommand::Enable => "gesture.enable",
            GestureCommand::Disable => "gesture.disable",
            GestureCommand::Set { .. } => "gesture.set",
        },
        Some(Command::Portrait { command }) => match command {
            PortraitCommand::Status => "portrait.status",
            PortraitCommand::Native { .. } => "portrait.native",
        },
        Some(Command::Privacy { .. }) => "privacy.status",
        Some(Command::Firmware { .. }) => "firmware.info",
        Some(Command::Video { command }) => match command {
            VideoCommand::Formats(_) => "video.formats",
            VideoCommand::Status => "video.status",
            VideoCommand::Set { .. } => "video.set",
            VideoCommand::Stats { .. } => "video.stats",
        },
        Some(Command::Audio { command }) => match command {
            AudioCommand::Devices => "audio.devices",
            AudioCommand::Status { .. } => "audio.status",
            AudioCommand::Gain { .. } => "audio.gain",
            AudioCommand::Mute { .. } => "audio.mute",
            AudioCommand::Unmute { .. } => "audio.unmute",
            AudioCommand::Mode { command } => match command {
                AudioModeCommand::Status => "audio.mode.status",
                AudioModeCommand::Standard => "audio.mode.standard",
                AudioModeCommand::Wide => "audio.mode.wide",
                AudioModeCommand::Focus => "audio.mode.focus",
                AudioModeCommand::Original => "audio.mode.original",
            },
            #[cfg(feature = "gstreamer")]
            AudioCommand::Meter(_) => "audio.meter",
            #[cfg(feature = "gstreamer")]
            AudioCommand::Capture(_) => "audio.capture",
            #[cfg(feature = "gstreamer")]
            AudioCommand::Monitor(_) => "audio.monitor",
        },
        Some(Command::Preset { command }) => match command {
            PresetCommand::Save { .. } => "preset.save",
            PresetCommand::Apply { .. } => "preset.apply",
            PresetCommand::List => "preset.list",
            PresetCommand::Show { .. } => "preset.show",
            PresetCommand::Delete { .. } => "preset.delete",
            PresetCommand::Export { .. } => "preset.export",
            PresetCommand::Import { .. } => "preset.import",
        },
        Some(Command::Xu { command }) => match command {
            XuCommand::Inventory { .. } => "xu.inventory",
            XuCommand::Get { .. } => "xu.get",
            XuCommand::Snapshot { .. } => "xu.snapshot",
            XuCommand::Diff { .. } => "xu.diff",
            XuCommand::Watch { .. } => "xu.watch",
            XuCommand::Set { .. } => "xu.set",
            XuCommand::RawSet { .. } => "xu.raw-set",
            XuCommand::Recover => "xu.recover",
        },
        #[cfg(feature = "gstreamer")]
        Some(Command::Snapshot(_)) => "snapshot",
        #[cfg(feature = "gstreamer")]
        Some(Command::Capture(_)) => "capture",
        #[cfg(feature = "gstreamer")]
        Some(Command::Record { .. }) => "record.start",
        #[cfg(feature = "network")]
        Some(Command::Stream { .. }) => "stream.start",
        Some(Command::Doctor(_)) => "doctor",
        Some(Command::Completion { .. }) => "completion",
        None => "linkctl",
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
        entries.push(DeviceListStatus {
            device: device.list_entry(include_serial, profile.profile_id),
            state: link_linux::availability_state(device),
            owner: None,
        });
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
    let include_serial = include_serial || !config.safety.redact_serials;

    if let Some(destination) = bundle {
        let selected = select_one_device(&devices, config.default_device.as_deref())?;
        let mut report = build_probe(selected, &catalog, include_serial)?;
        report.issues.extend(selected.issues.clone());
        write_probe_bundle(destination, &report, &selected.descriptors)?;
        let summary = DeviceSummary {
            stable_id: report.device.stable_id.clone(),
            model: report.device.model.clone(),
        };
        return match config.output {
            OutputFormat::Human => {
                emit_human_probe(&report, bundle);
                Ok(())
            }
            OutputFormat::Json | OutputFormat::Jsonl => {
                emit_success(config.output, "device.probe", Some(summary), &report)
            }
        };
    }

    let selected = selected_devices(config, true)?;
    let reports = selected
        .iter()
        .map(|device| {
            let mut report = build_probe(device, &catalog, include_serial)?;
            report.issues.extend(device.issues.clone());
            Ok(PerDeviceResult {
                device: device_summary(device),
                result: report,
            })
        })
        .collect::<Result<Vec<_>, LinkError>>()?;
    match config.output {
        OutputFormat::Human => {
            for report in &reports {
                emit_human_probe(&report.result, None);
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            emit_success(config.output, "device.probe", None, &reports)?;
            Ok(())
        }
    }
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
struct DeviceListStatus {
    #[serde(flatten)]
    device: DeviceListEntry,
    state: DeviceState,
    owner: Option<String>,
}

#[derive(Serialize)]
struct DeviceInfoResult {
    inventory: ProbeReport,
    controls: ControlCapabilities,
    state: DeviceState,
    owner: Option<String>,
    daemon_owns_stream: bool,
}

#[derive(Serialize)]
struct PerDeviceResult<T> {
    device: DeviceSummary,
    result: T,
}

#[derive(Serialize)]
struct ControlGetResult {
    control: ControlDescriptor,
    value: ControlValue,
}

fn discovered_devices() -> Result<Vec<DiscoveredDevice>, LinkError> {
    Ok(link_linux::enumerate_devices()?
        .into_iter()
        .filter(link_linux::is_listable)
        .collect())
}

fn selected_devices(config: &Config, allow_all: bool) -> Result<Vec<DiscoveredDevice>, LinkError> {
    let devices = discovered_devices()?;
    if let Some(selector) = config.default_device.as_deref() {
        if selector == "all" && !allow_all {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "this command requires one selected device",
            ));
        }
        return link_linux::select_devices(&devices, selector)
            .map(|selected| selected.into_iter().cloned().collect());
    }
    match devices.as_slice() {
        [] => Err(LinkError::new(
            ErrorKind::DeviceNotFound,
            "no camera device was discovered",
        )),
        [device] => Ok(vec![device.clone()]),
        _ => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "multiple camera devices were discovered; select one with --device",
        )
        .with_detail("matches", devices.len() as u64)),
    }
}

fn control_node(
    device: &DiscoveredDevice,
    selector: Option<&str>,
) -> Result<NodeAssociation, LinkError> {
    if let Some(node) = selector.and_then(|selector| device.selected_video_node(selector)) {
        let report = link_v4l2::probe_node(node.association.clone());
        if report.kind != VideoNodeKind::Capture {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "selected V4L2 node is not a capture/control node",
            )
            .with_detail("path", node.association.path.clone()));
        }
        return Ok(node.association.clone());
    }
    for node in &device.video_nodes {
        let report = link_v4l2::probe_node(node.association.clone());
        if report.kind == VideoNodeKind::Capture {
            return Ok(node.association.clone());
        }
    }
    Err(LinkError::new(
        ErrorKind::CapabilityUnsupported,
        "selected device has no V4L2 capture/control node",
    ))
}

fn device_summary(device: &DiscoveredDevice) -> DeviceSummary {
    DeviceSummary {
        stable_id: device.identity.stable_id(),
        model: device.model(),
    }
}

fn acquire_operation_lease(
    config: &Config,
    key: &str,
    operation: &str,
) -> Result<DeviceLease, LinkError> {
    DeviceLease::acquire(
        &AppPaths::from_process()?,
        key,
        operation,
        config.timeout.get(),
    )
}

fn ensure_standard_backend(
    config: &Config,
    backend: Option<BackendChoice>,
) -> Result<(), LinkError> {
    if config.daemon == DaemonMode::Always {
        return Err(LinkError::new(
            ErrorKind::DaemonUnavailable,
            "the daemon is not implemented in this build",
        ));
    }
    match backend.unwrap_or(BackendChoice::Auto) {
        BackendChoice::Auto | BackendChoice::Standard => Ok(()),
        BackendChoice::Vendor | BackendChoice::Host => Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "the requested backend cannot provide standard controls",
        )
        .with_detail(
            "requested_backend",
            match backend {
                Some(BackendChoice::Vendor) => "vendor",
                Some(BackendChoice::Host) => "host",
                _ => unreachable!(),
            },
        )),
    }
}

fn ensure_direct_video_backend(
    config: &Config,
    backend: Option<BackendChoice>,
) -> Result<(), LinkError> {
    ensure_standard_backend(config, backend)
}

fn selected_video_device(
    config: &Config,
) -> Result<(DiscoveredDevice, NodeAssociation), LinkError> {
    let mut devices = selected_devices(config, false)?;
    let device = devices
        .pop()
        .ok_or_else(|| LinkError::new(ErrorKind::DeviceNotFound, "no camera was selected"))?;
    let node = control_node(&device, config.default_device.as_deref())?;
    Ok((device, node))
}

#[derive(Serialize)]
struct XuInventoryResult {
    descriptor_sha256: String,
    profile_id: Option<String>,
    profile_checksum: Option<String>,
    inventory: link_uvc_xu::DescriptorInventory,
}

#[derive(Serialize)]
struct XuWriteReport {
    control: Option<String>,
    guid: String,
    unit: u8,
    selector: u8,
    length: u16,
    previous: Option<Value>,
    requested: Value,
    observed: Option<Value>,
    verified: bool,
    dry_run: bool,
    rate_limit_wait_ms: u64,
    audit_path: Option<PathBuf>,
}

#[derive(Serialize)]
struct XuSemanticTransactionReport {
    steps: Vec<XuWriteReport>,
    verified: bool,
    dry_run: bool,
}

struct PreparedSemanticWrite<'a> {
    authorization: link_profiles::AuthorizedControl<'a>,
    guid: String,
    address: link_uvc_xu::XuAddress,
    baseline: Vec<u8>,
    previous: Value,
    payload: Vec<u8>,
    requested: Value,
    observed: Option<Value>,
    verified: bool,
    rate_limit_wait_ms: u64,
}

#[derive(Serialize)]
struct XuRecoveryReport {
    node: String,
    handles_reopened: bool,
    selectors_checked: usize,
    pipeline_rebuilt: bool,
    pipeline_note: String,
    dry_run: bool,
}

fn run_xu(
    config: &Config,
    command: XuCommand,
    dry_run: bool,
    unsafe_xu: bool,
) -> Result<(), LinkError> {
    if unsafe_xu && !matches!(command, XuCommand::RawSet { .. }) {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "--unsafe-xu is valid only with xu raw-set",
        ));
    }
    match command {
        XuCommand::Inventory { raw } => run_xu_inventory(config, raw),
        XuCommand::Get {
            guid,
            unit,
            selector,
            length,
        } => run_xu_get(config, guid.as_deref(), unit, selector, length),
        XuCommand::Snapshot {
            output,
            samples,
            interval,
            note,
            include_volatile,
        } => run_xu_snapshot(
            config,
            &output,
            samples,
            interval.get(),
            note,
            include_volatile,
        ),
        XuCommand::Diff { before, after } => run_xu_diff(config, &before, &after),
        XuCommand::Watch {
            interval,
            include_volatile,
        } => run_xu_watch(config, interval.get(), include_volatile),
        XuCommand::Set { control, value } => {
            run_xu_semantic_set(config, "xu.set", &control, &value, dry_run)
        }
        XuCommand::RawSet {
            guid,
            selector,
            hex,
        } => run_xu_raw_set(config, &guid, selector, &hex, dry_run, unsafe_xu),
        XuCommand::Recover => run_xu_recover(config, dry_run),
    }
}

fn run_xu_inventory(config: &Config, raw: bool) -> Result<(), LinkError> {
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let devices = selected_devices(config, true)?;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        let node = control_node(device, config.default_device.as_deref())?;
        let mut inventory = link_uvc_xu::parse_descriptors(&device.descriptors)?;
        inventory.extension_units =
            link_uvc_xu::inventory(Path::new(&node.path), &device.descriptors)?;
        if !raw {
            inventory.unknown_descriptors.clear();
            for entity in &mut inventory.video_control_entities {
                entity.raw_hex.clear();
            }
        }
        let profile = catalog.matching(&device.identity, device.mode(), None)?;
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: XuInventoryResult {
                descriptor_sha256: device.identity.descriptor_sha256.clone(),
                profile_id: profile.map(|entry| entry.profile.profile_id.clone()),
                profile_checksum: profile.map(|entry| entry.checksum.clone()),
                inventory,
            },
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{} ({})", result.device.model, result.device.stable_id);
            for entity in &result.result.inventory.extension_units {
                println!(
                    "  {} unit={} controls={} bitmap={}",
                    entity.guid, entity.unit_id, entity.num_controls, entity.control_bitmap
                );
                for selector in &entity.selectors {
                    println!(
                        "    selector={} length={} get={} set={}",
                        selector.selector,
                        selector
                            .length
                            .map_or_else(|| "?".into(), |value| value.to_string()),
                        selector
                            .get_supported
                            .map_or_else(|| "?".into(), |value| value.to_string()),
                        selector
                            .set_supported
                            .map_or_else(|| "?".into(), |value| value.to_string()),
                    );
                }
            }
        }
        Ok(())
    } else {
        emit_success(config.output, "xu.inventory", None, &results)
    }
}

fn selected_xu_context(
    config: &Config,
    writable: bool,
) -> Result<
    (
        DiscoveredDevice,
        NodeAssociation,
        link_uvc_xu::DescriptorInventory,
        ProfileCatalog,
        link_uvc_xu::XuSession,
    ),
    LinkError,
> {
    let (device, node) = selected_video_device(config)?;
    if device.mode() != DeviceMode::Camera {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "XU commands require the normal camera USB personality",
        ));
    }
    let inventory = link_uvc_xu::parse_descriptors(&device.descriptors)?;
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let session = if writable {
        link_uvc_xu::XuSession::open_write(Path::new(&node.path))?
    } else {
        link_uvc_xu::XuSession::open_read(Path::new(&node.path))?
    };
    Ok((device, node, inventory, catalog, session))
}

fn resolve_firmware_profile<'a>(
    device: &DiscoveredDevice,
    inventory: &link_uvc_xu::DescriptorInventory,
    catalog: &'a ProfileCatalog,
    session: &link_uvc_xu::XuSession,
) -> Result<(Option<&'a link_profiles::CatalogProfile>, Option<String>), LinkError> {
    let bootstrap = catalog.matching(&device.identity, device.mode(), None)?;
    let firmware = bootstrap
        .and_then(|entry| {
            entry
                .profile
                .control("firmware.version")
                .map(|control| (entry, control))
        })
        .filter(|(_, control)| control.readable)
        .map(|(entry, control)| {
            let value = link_uvc_xu::read_value(
                session,
                inventory,
                Some(&control.entity_guid),
                None,
                control.selector,
                Some(control.length),
                Some(&entry.profile),
            )?;
            let decoded = value.decoded.get("firmware.version").ok_or_else(|| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "firmware bootstrap control did not decode a value",
                )
            })?;
            let version = match decoded {
                Value::String(value) => value.clone(),
                value => value.to_string(),
            };
            if version.trim().is_empty() {
                return Err(LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "firmware bootstrap control decoded an empty value",
                ));
            }
            Ok::<String, LinkError>(version)
        })
        .transpose()?;
    let selected = catalog.matching(&device.identity, device.mode(), firmware.as_deref())?;
    Ok((selected, firmware))
}

fn run_xu_get(
    config: &Config,
    guid: Option<&str>,
    unit: Option<u8>,
    selector: u8,
    asserted_length: Option<u16>,
) -> Result<(), LinkError> {
    if selector == 0 || guid.is_some() == unit.is_some() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "select exactly one XU GUID or unit and a non-zero selector",
        ));
    }
    let (device, _, inventory, catalog, session) = selected_xu_context(config, false)?;
    let profile = catalog
        .matching(&device.identity, device.mode(), None)?
        .map(|entry| &entry.profile);
    let value = link_uvc_xu::read_value(
        &session,
        &inventory,
        guid,
        unit,
        selector,
        asserted_length,
        profile,
    )?;
    if config.output == OutputFormat::Human {
        println!("{}", value.hex);
        println!("base64: {}", value.base64);
        for (name, decoded) in &value.decoded {
            println!("{name}: {decoded}");
        }
        Ok(())
    } else {
        emit_success(
            config.output,
            "xu.get",
            Some(device_summary(&device)),
            &value,
        )
    }
}

fn snapshot_standard_controls(
    node: &NodeAssociation,
) -> Result<BTreeMap<String, Value>, LinkError> {
    let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
    Ok(backend
        .controls()?
        .into_iter()
        .filter_map(|control| control.current.map(|value| (control.name, json!(value))))
        .collect())
}

fn snapshot_profile_metadata(
    entry: &link_profiles::CatalogProfile,
) -> link_uvc_xu::SnapshotProfile {
    link_uvc_xu::SnapshotProfile {
        profile_id: entry.profile.profile_id.clone(),
        checksum: entry.checksum.clone(),
        status: match entry.profile.status {
            link_profiles::ProfileStatus::ReadOnly => "read-only",
            link_profiles::ProfileStatus::Experimental => "experimental",
            link_profiles::ProfileStatus::Verified => "verified",
        }
        .into(),
    }
}

fn capture_xu_snapshot(
    config: &Config,
    samples: u8,
    interval: Duration,
    notes: Vec<String>,
    include_volatile: bool,
) -> Result<link_uvc_xu::XuSnapshot, LinkError> {
    let (device, node, inventory, catalog, session) = selected_xu_context(config, false)?;
    let profile = catalog.matching(&device.identity, device.mode(), None)?;
    link_uvc_xu::capture_snapshot(
        &session,
        &inventory,
        link_uvc_xu::SnapshotRequest {
            captured_unix_ms: now_unix_ms()?,
            application_version: env!("CARGO_PKG_VERSION"),
            device: link_uvc_xu::SnapshotDevice {
                stable_id: device.identity.stable_id(),
                model: device.model(),
                usb_vid: device.identity.vendor_id,
                usb_pid: device.identity.product_id,
                bcd_device: device.identity.device_revision,
                descriptor_sha256: device.identity.descriptor_sha256.clone(),
            },
            profile_metadata: profile.map(snapshot_profile_metadata),
            profile: profile.map(|entry| &entry.profile),
            stream_state: "closed-or-external",
            notes,
            standard_controls: snapshot_standard_controls(&node)?,
            samples,
            interval,
            include_volatile,
        },
    )
}

fn run_xu_snapshot(
    config: &Config,
    output: &Path,
    samples: u8,
    interval: Duration,
    notes: Vec<String>,
    include_volatile: bool,
) -> Result<(), LinkError> {
    let snapshot = capture_xu_snapshot(config, samples, interval, notes, include_volatile)?;
    link_uvc_xu::save_snapshot(output, &snapshot)?;
    let result = json!({
        "path": output,
        "selectors": snapshot.selectors.len(),
        "samples": samples,
    });
    if config.output == OutputFormat::Human {
        println!(
            "Saved {} selectors to {}",
            snapshot.selectors.len(),
            output.display()
        );
        Ok(())
    } else {
        emit_success(config.output, "xu.snapshot", None, &result)
    }
}

fn run_xu_diff(config: &Config, before: &Path, after: &Path) -> Result<(), LinkError> {
    let before = link_uvc_xu::load_snapshot(before)?;
    let after = link_uvc_xu::load_snapshot(after)?;
    let diff = link_uvc_xu::diff_snapshots(&before, &after)?;
    if config.output == OutputFormat::Human {
        println!(
            "same_device={} descriptor_changed={} profile_changed={}",
            diff.same_device, diff.descriptor_changed, diff.profile_changed
        );
        for selector in &diff.selectors {
            println!(
                "{} selector={} {} bytes_changed={}",
                selector.guid,
                selector.selector,
                selector.status,
                selector.bytes.len()
            );
            for byte in &selector.bytes {
                println!(
                    "  offset={} {:02x}->{:02x} xor={:02x} bits={:?}",
                    byte.offset, byte.before, byte.after, byte.xor, byte.changed_bits
                );
            }
        }
        Ok(())
    } else {
        emit_success(config.output, "xu.diff", None, &diff)
    }
}

fn run_xu_watch(
    config: &Config,
    interval: Duration,
    include_volatile: bool,
) -> Result<(), LinkError> {
    ensure_watch_format(config.output)?;
    let mut previous =
        capture_xu_snapshot(config, 1, Duration::ZERO, Vec::new(), include_volatile)?;
    loop {
        thread::sleep(interval);
        let current = capture_xu_snapshot(config, 1, Duration::ZERO, Vec::new(), include_volatile)?;
        let diff = link_uvc_xu::diff_snapshots(&previous, &current)?;
        if !diff.selectors.is_empty()
            || diff.standard_controls_before != diff.standard_controls_after
        {
            match config.output {
                OutputFormat::Human => {
                    println!("{} selector change(s)", diff.selectors.len());
                }
                OutputFormat::Jsonl => emit_success(config.output, "xu.watch", None, &diff)?,
                OutputFormat::Json => unreachable!("watch format validated"),
            }
        }
        previous = current;
    }
}

fn authorize_control_safety(class: SafetyClass, policy: &SafetyPolicy) -> Result<(), LinkError> {
    match class {
        SafetyClass::Normal => Ok(()),
        SafetyClass::Firmware | SafetyClass::Boot | SafetyClass::Flash => {
            policy.authorize(Operation::FirmwareWrite)
        }
        SafetyClass::Calibration => policy.authorize(Operation::CalibrationWrite),
        SafetyClass::Motor => policy.authorize(Operation::MotorWrite),
    }
}

fn run_xu_semantic_set(
    config: &Config,
    command: &'static str,
    control_name: &str,
    input: &str,
    dry_run: bool,
) -> Result<(), LinkError> {
    run_xu_semantic_writes(config, command, &[(control_name, input)], dry_run)
}

fn run_xu_semantic_transaction(
    config: &Config,
    command: &'static str,
    writes: &[(&str, &str)],
    dry_run: bool,
) -> Result<(), LinkError> {
    if writes.len() < 2 {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "semantic XU transactions require at least two writes",
        ));
    }
    run_xu_semantic_writes(config, command, writes, dry_run)
}

fn run_xu_semantic_writes(
    config: &Config,
    command: &'static str,
    writes: &[(&str, &str)],
    dry_run: bool,
) -> Result<(), LinkError> {
    if writes.is_empty() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "semantic XU writes cannot be empty",
        ));
    }
    let (device, node, inventory, catalog, session) = selected_xu_context(config, true)?;
    let stable_id = device.identity.stable_id();
    let _lease = acquire_operation_lease(config, &stable_id, command)?;
    #[cfg(feature = "gstreamer")]
    let _media_lease = link_media::MediaLease::acquire(&stable_id, command)?;
    let (entry, firmware) = resolve_firmware_profile(&device, &inventory, &catalog, &session)?;
    let entry = entry.ok_or_else(|| {
        LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "no exact vendor profile matches the selected device",
        )
        .with_detail("firmware", firmware.unwrap_or_else(|| "unknown".into()))
    })?;
    SafetyPolicy::new(config.safety.clone())
        .authorize(Operation::VendorProfileWrite(entry.state()))?;
    if !entry.semantic_write_authorized() {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "semantic XU writes require a compiled-in trusted verified profile",
        ));
    }
    let policy = SafetyPolicy::new(config.safety.clone());
    let mut authorized = Vec::with_capacity(writes.len());
    for (control_name, input) in writes {
        let authorization = entry.authorized_control(control_name).ok_or_else(|| {
            LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "semantic XU control is not a writable authorized profile control",
            )
            .with_detail("control", (*control_name).to_owned())
        })?;
        let control = authorization.control();
        authorize_control_safety(control.safety, &policy)?;
        if control.verification != VerificationMethod::Readback {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "semantic XU writes currently require direct readback verification",
            )
            .with_detail("control", control.name.clone())
            .with_detail("verification", format!("{:?}", control.verification)));
        }
        authorized.push((authorization, *input));
    }

    let stream_control = authorized[0].0.control();
    if authorized.iter().skip(1).any(|(authorization, _)| {
        let control = authorization.control();
        control.stream_requirement != stream_control.stream_requirement
            || control.stream_format != stream_control.stream_format
            || control.stream_warmup_delay_ms != stream_control.stream_warmup_delay_ms
    }) {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "semantic XU transaction controls require incompatible stream conditions",
        ));
    }
    let _stream = prepare_xu_stream(stream_control, &node.path, config.timeout.get(), dry_run)?;
    let limiter = link_uvc_xu::RateLimiter::from_process()?;
    let mut prepared = Vec::with_capacity(authorized.len());
    for (authorization, input) in authorized {
        let control = authorization.control();
        let (guid, address) = link_uvc_xu::resolve_address(
            &inventory,
            Some(&control.entity_guid),
            None,
            control.selector,
        )?;
        let baseline = session.get_current(address, Some(control.length))?.1;
        let previous = link_profiles::decode_control(control, &baseline)?;
        let payload = link_profiles::encode_control(control, input, Some(&baseline))?;
        let requested = link_profiles::decode_control(control, &payload)?;
        let rate_wait = limiter.enforce(
            &format!("{stable_id}:{guid}:{}", control.selector),
            Duration::from_millis(
                control
                    .minimum_write_interval_ms
                    .max(config.safety.minimum_xu_write_interval_ms),
            ),
            config.timeout.get(),
            dry_run,
        )?;
        prepared.push(PreparedSemanticWrite {
            authorization,
            guid,
            address,
            baseline,
            previous,
            payload,
            requested,
            observed: None,
            verified: false,
            rate_limit_wait_ms: rate_wait.as_millis() as u64,
        });
    }

    if !dry_run {
        for index in 0..prepared.len() {
            let write_result = apply_semantic_write(&session, &prepared[index]);
            match write_result {
                Ok(observed) if observed == prepared[index].requested => {
                    prepared[index].observed = Some(observed);
                    prepared[index].verified = true;
                }
                Ok(observed) => {
                    prepared[index].observed = Some(observed);
                    let rollback = rollback_semantic_writes(&session, &prepared[..=index]);
                    return Err(LinkError::new(
                        ErrorKind::ProtocolProfileMismatch,
                        "semantic XU write failed readback verification",
                    )
                    .with_detail(
                        "control",
                        prepared[index].authorization.control().name.clone(),
                    )
                    .with_detail("rollback", rollback));
                }
                Err(error) => {
                    let rollback = rollback_semantic_writes(&session, &prepared[..=index]);
                    return Err(error
                        .with_detail(
                            "control",
                            prepared[index].authorization.control().name.clone(),
                        )
                        .with_detail("rollback", rollback));
                }
            }
        }
        complete_xu_stream_requirement(
            stream_control.stream_requirement,
            &node.path,
            config.timeout.get(),
        )?;
    }
    let reports = prepared
        .into_iter()
        .map(|write| {
            let control = write.authorization.control();
            XuWriteReport {
                control: Some(control.name.clone()),
                guid: write.guid,
                unit: write.address.unit,
                selector: write.address.selector,
                length: control.length,
                previous: Some(write.previous),
                requested: write.requested,
                observed: write.observed,
                verified: write.verified,
                dry_run,
                rate_limit_wait_ms: write.rate_limit_wait_ms,
                audit_path: None,
            }
        })
        .collect::<Vec<_>>();
    if let [report] = reports.as_slice() {
        emit_xu_write_report(config, command, &device, report)
    } else {
        emit_xu_semantic_transaction_report(config, command, &device, reports, dry_run)
    }
}

fn apply_semantic_write(
    session: &link_uvc_xu::XuSession,
    write: &PreparedSemanticWrite<'_>,
) -> Result<Value, LinkError> {
    let control = write.authorization.control();
    session.set_profiled(write.address, write.authorization, &write.payload)?;
    thread::sleep(Duration::from_millis(control.verification_delay_ms));
    let readback = session.get_current(write.address, Some(control.length))?.1;
    link_profiles::decode_control(control, &readback)
}

fn rollback_semantic_writes(
    session: &link_uvc_xu::XuSession,
    writes: &[PreparedSemanticWrite<'_>],
) -> Value {
    let mut results = Vec::with_capacity(writes.len());
    for write in writes.iter().rev() {
        let control = write.authorization.control();
        if control.rollback == RollbackPolicy::None {
            results.push(json!({
                "control": control.name,
                "attempted": false,
                "reason": "profile marks rollback unavailable",
            }));
            continue;
        }
        let restored =
            link_profiles::encode_decoded_control(control, &write.previous, Some(&write.baseline))
                .and_then(|payload| {
                    session.set_profiled(write.address, write.authorization, &payload)?;
                    thread::sleep(Duration::from_millis(control.verification_delay_ms));
                    let readback = session.get_current(write.address, Some(control.length))?.1;
                    Ok(link_profiles::decode_control(control, &readback)? == write.previous)
                });
        match restored {
            Ok(restored) => results.push(json!({
                "control": control.name,
                "attempted": true,
                "restored": restored,
            })),
            Err(error) => results.push(json!({
                "control": control.name,
                "attempted": true,
                "restored": false,
                "error": error.message(),
            })),
        }
    }
    Value::Array(results)
}

#[cfg(feature = "gstreamer")]
fn prepare_xu_stream(
    control: &link_profiles::ProfileControl,
    node: &str,
    timeout: Duration,
    dry_run: bool,
) -> Result<Option<link_media::ProbeStream>, LinkError> {
    if dry_run || control.stream_requirement != link_profiles::StreamRequirement::Open {
        return Ok(None);
    }
    let format = control
        .stream_format
        .as_ref()
        .map(link_profiles::StreamFormat::video_tuple);
    let stream = link_media::ProbeStream::open_with_format(node, timeout, format.as_ref())?;
    thread::sleep(Duration::from_millis(control.stream_warmup_delay_ms));
    Ok(Some(stream))
}

#[cfg(not(feature = "gstreamer"))]
fn prepare_xu_stream(
    control: &link_profiles::ProfileControl,
    _node: &str,
    _timeout: Duration,
    _dry_run: bool,
) -> Result<Option<()>, LinkError> {
    match control.stream_requirement {
        link_profiles::StreamRequirement::Either | link_profiles::StreamRequirement::Closed => {
            Ok(None)
        }
        link_profiles::StreamRequirement::Open => Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "profile-required open streams need a GStreamer-enabled build",
        )),
        link_profiles::StreamRequirement::Restart => Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "profile-required pipeline rebuilds need a GStreamer-enabled build",
        )),
    }
}

#[cfg(feature = "gstreamer")]
fn complete_xu_stream_requirement(
    requirement: link_profiles::StreamRequirement,
    node: &str,
    timeout: Duration,
) -> Result<(), LinkError> {
    if requirement == link_profiles::StreamRequirement::Restart {
        link_media::probe_stream(node, timeout)?;
    }
    Ok(())
}

#[cfg(not(feature = "gstreamer"))]
fn complete_xu_stream_requirement(
    _requirement: link_profiles::StreamRequirement,
    _node: &str,
    _timeout: Duration,
) -> Result<(), LinkError> {
    Ok(())
}

fn run_xu_raw_set(
    config: &Config,
    guid: &str,
    selector: u8,
    hex: &str,
    dry_run: bool,
    unsafe_xu: bool,
) -> Result<(), LinkError> {
    SafetyPolicy::new(config.safety.clone()).authorize(Operation::RawXuWrite {
        feature_enabled: cfg!(feature = "research"),
        acknowledged: unsafe_xu,
        profile: link_profiles::ProfileState::Experimental,
    })?;
    let payload = link_uvc_xu::parse_hex(hex)?;
    let (device, node, inventory, catalog, session) = selected_xu_context(config, true)?;
    let stable_id = device.identity.stable_id();
    let _lease = acquire_operation_lease(config, &stable_id, "xu.raw-set")?;
    #[cfg(feature = "gstreamer")]
    let _media_lease = link_media::MediaLease::acquire(&stable_id, "xu.raw-set")?;
    let (entry, firmware) = resolve_firmware_profile(&device, &inventory, &catalog, &session)?;
    let entry = entry.ok_or_else(|| {
        LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "raw XU writes require a known firmware and exact descriptor profile match",
        )
        .with_detail("firmware", firmware.unwrap_or_else(|| "unknown".into()))
    })?;
    SafetyPolicy::new(config.safety.clone()).authorize(Operation::RawXuWrite {
        feature_enabled: cfg!(feature = "research"),
        acknowledged: unsafe_xu,
        profile: entry.state(),
    })?;
    if !entry.research_write_authorized() {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "matching profile does not authorize research writes",
        ));
    }
    let control = entry
        .profile
        .controls_for_selector(guid, selector)
        .into_iter()
        .find(|control| control.writable && usize::from(control.length) == payload.len())
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::UnsafeOperationDenied,
                "raw XU payload is not classified by the exact matching profile",
            )
        })?;
    let policy = SafetyPolicy::new(config.safety.clone());
    authorize_control_safety(control.safety, &policy)?;
    let _stream = prepare_xu_stream(control, &node.path, config.timeout.get(), dry_run)?;
    let (resolved_guid, address) =
        link_uvc_xu::resolve_address(&inventory, Some(guid), None, selector)?;
    let rate_wait = link_uvc_xu::RateLimiter::from_process()?.enforce(
        &format!("{stable_id}:{resolved_guid}:{selector}"),
        Duration::from_millis(
            control
                .minimum_write_interval_ms
                .max(config.safety.minimum_xu_write_interval_ms),
        ),
        config.timeout.get(),
        dry_run,
    )?;
    if !dry_run {
        issue_research_raw_set(&session, address, control.length, &payload)?;
        complete_xu_stream_requirement(
            control.stream_requirement,
            &node.path,
            config.timeout.get(),
        )?;
    }
    let audit = link_uvc_xu::RawWriteAudit::new(
        link_uvc_xu::RawWriteContext {
            stable_id: &stable_id,
            profile_id: &entry.profile.profile_id,
            profile_checksum: &entry.checksum,
            descriptor_sha256: &device.identity.descriptor_sha256,
            guid: &resolved_guid,
            selector,
        },
        &payload,
        dry_run,
        if dry_run { "dry-run" } else { "submitted" },
    );
    let audit_path = link_uvc_xu::append_raw_audit(&audit)?;
    let report = XuWriteReport {
        control: None,
        guid: resolved_guid,
        unit: address.unit,
        selector,
        length: control.length,
        previous: None,
        requested: json!({"sha256": sha256(&payload), "length": payload.len()}),
        observed: None,
        verified: dry_run,
        dry_run,
        rate_limit_wait_ms: rate_wait.as_millis() as u64,
        audit_path: Some(audit_path),
    };
    emit_xu_write_report(config, "xu.raw-set", &device, &report)
}

#[cfg(feature = "research")]
fn issue_research_raw_set(
    session: &link_uvc_xu::XuSession,
    address: link_uvc_xu::XuAddress,
    length: u16,
    payload: &[u8],
) -> Result<(), LinkError> {
    session.raw_set(address, length, payload).map(|_| ())
}

#[cfg(not(feature = "research"))]
fn issue_research_raw_set(
    _session: &link_uvc_xu::XuSession,
    _address: link_uvc_xu::XuAddress,
    _length: u16,
    _payload: &[u8],
) -> Result<(), LinkError> {
    Err(LinkError::new(
        ErrorKind::UnsafeOperationDenied,
        "raw XU support requires a research-enabled build",
    ))
}

fn emit_xu_write_report(
    config: &Config,
    command: &'static str,
    device: &DiscoveredDevice,
    report: &XuWriteReport,
) -> Result<(), LinkError> {
    if config.output == OutputFormat::Human {
        println!(
            "{} {} selector={} length={} verified={}{}",
            if report.dry_run {
                "Would write"
            } else {
                "Wrote"
            },
            report.guid,
            report.selector,
            report.length,
            report.verified,
            report
                .audit_path
                .as_ref()
                .map_or_else(String::new, |path| format!(" audit={}", path.display())),
        );
        Ok(())
    } else {
        emit_success(config.output, command, Some(device_summary(device)), report)
    }
}

fn emit_xu_semantic_transaction_report(
    config: &Config,
    command: &'static str,
    device: &DiscoveredDevice,
    steps: Vec<XuWriteReport>,
    dry_run: bool,
) -> Result<(), LinkError> {
    let report = XuSemanticTransactionReport {
        verified: steps.iter().all(|step| step.verified),
        steps,
        dry_run,
    };
    if config.output == OutputFormat::Human {
        for step in &report.steps {
            println!(
                "{} {}={} verified={}",
                if report.dry_run { "Would set" } else { "Set" },
                step.control.as_deref().unwrap_or("vendor-control"),
                step.requested,
                step.verified,
            );
        }
        Ok(())
    } else {
        emit_success(
            config.output,
            command,
            Some(device_summary(device)),
            &report,
        )
    }
}

fn run_xu_recover(config: &Config, dry_run: bool) -> Result<(), LinkError> {
    let (device, node) = selected_video_device(config)?;
    let stable_id = device.identity.stable_id();
    let _lease = acquire_operation_lease(config, &stable_id, "xu.recover")?;
    #[cfg(feature = "gstreamer")]
    let _media_lease = link_media::MediaLease::acquire(&stable_id, "xu.recover")?;
    let parsed = link_uvc_xu::parse_descriptors(&device.descriptors)?;
    if dry_run {
        let report = XuRecoveryReport {
            node: node.path,
            handles_reopened: false,
            selectors_checked: parsed
                .extension_units
                .iter()
                .map(|entity| entity.selectors.len())
                .sum(),
            pipeline_rebuilt: false,
            pipeline_note: "dry-run: no handle or stream was opened".into(),
            dry_run: true,
        };
        return emit_xu_recovery(config, &device, &report);
    }
    let first = link_uvc_xu::inventory(Path::new(&node.path), &device.descriptors)?;
    let second = link_uvc_xu::inventory(Path::new(&node.path), &device.descriptors)?;
    if first != second {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "XU inventory changed while reopening the control handle",
        ));
    }
    #[cfg(feature = "gstreamer")]
    let (pipeline_rebuilt, pipeline_note) =
        match link_media::probe_stream(&node.path, config.timeout.get()) {
            Ok(()) => (
                true,
                "temporary no-output stream reached Playing and returned to Null".into(),
            ),
            Err(error) => (
                false,
                format!("pipeline rebuild was unavailable: {}", error.message()),
            ),
        };
    #[cfg(not(feature = "gstreamer"))]
    let (pipeline_rebuilt, pipeline_note) =
        (false, "this build has no GStreamer pipeline backend".into());
    let report = XuRecoveryReport {
        node: node.path,
        handles_reopened: true,
        selectors_checked: second.iter().map(|entity| entity.selectors.len()).sum(),
        pipeline_rebuilt,
        pipeline_note,
        dry_run: false,
    };
    emit_xu_recovery(config, &device, &report)
}

fn emit_xu_recovery(
    config: &Config,
    device: &DiscoveredDevice,
    report: &XuRecoveryReport,
) -> Result<(), LinkError> {
    if config.output == OutputFormat::Human {
        println!(
            "Handle reopened: {}; selectors checked: {}; pipeline rebuilt: {} ({})",
            report.handles_reopened,
            report.selectors_checked,
            report.pipeline_rebuilt,
            report.pipeline_note
        );
        Ok(())
    } else {
        emit_success(
            config.output,
            "xu.recover",
            Some(device_summary(device)),
            report,
        )
    }
}

fn run_video(
    config: &Config,
    backend: Option<BackendChoice>,
    command: VideoCommand,
    dry_run: bool,
) -> Result<(), LinkError> {
    ensure_direct_video_backend(config, backend)?;
    let (device, node) = selected_video_device(config)?;
    let summary = device_summary(&device);
    match command {
        VideoCommand::Formats(filters) => {
            let backend = link_v4l2::video::VideoDevice::open_read(&node.path)?;
            let mut inventory = backend.formats()?;
            let size = filters.size.as_deref().map(parse_size).transpose()?;
            let fps = filters.fps.as_deref().map(parse_fps).transpose()?;
            inventory.discrete.retain(|format| {
                filters
                    .fourcc
                    .as_ref()
                    .is_none_or(|fourcc| format.tuple.fourcc.eq_ignore_ascii_case(fourcc))
                    && size.is_none_or(|(width, height)| {
                        format.tuple.width == width && format.tuple.height == height
                    })
                    && fps.is_none_or(|fps| {
                        link_core::media::rational_cmp(format.tuple.fps, fps).is_eq()
                    })
            });
            if config.output == OutputFormat::Human {
                println!("FOURCC\tSIZE\tFPS\tCLASS\tORIENTATION\tREMUX\tEST. BANDWIDTH");
                for format in &inventory.discrete {
                    let bandwidth = format
                        .estimated_bandwidth_bps
                        .map_or_else(|| "-".into(), |value| format!("{value} bps"));
                    println!(
                        "{}\t{}x{}\t{}/{}\t{}\t{}\t{}\t{}",
                        format.tuple.fourcc,
                        format.tuple.width,
                        format.tuple.height,
                        format.tuple.fps.numerator,
                        format.tuple.fps.denominator,
                        if format.compressed {
                            "compressed"
                        } else {
                            "raw"
                        },
                        if format.portrait {
                            "portrait"
                        } else {
                            "landscape"
                        },
                        format.remuxable,
                        bandwidth,
                    );
                }
                Ok(())
            } else {
                emit_success(config.output, "video.formats", Some(summary), &inventory)
            }
        }
        VideoCommand::Status => {
            let status = link_v4l2::video::VideoDevice::open_read(&node.path)?.status()?;
            if config.output == OutputFormat::Human {
                println!(
                    "{}: {} {}x{} at {}/{} fps (colorspace {}, transfer {}, quantization {})",
                    status.node,
                    status.tuple.fourcc,
                    status.tuple.width,
                    status.tuple.height,
                    status.tuple.fps.numerator,
                    status.tuple.fps.denominator,
                    status.colorspace,
                    status.transfer_function,
                    status.quantization,
                );
                Ok(())
            } else {
                emit_success(config.output, "video.status", Some(summary), &status)
            }
        }
        VideoCommand::Set { fourcc, size, fps } => {
            let (width, height) = parse_size(&size)?;
            let requested = VideoTuple {
                fourcc,
                width,
                height,
                fps: parse_fps(&fps)?,
            }
            .normalized();
            let _lease = acquire_operation_lease(config, &summary.stable_id, "video.set")?;
            let report = if dry_run {
                link_v4l2::video::VideoDevice::open_read(&node.path)?.validate(&requested)?
            } else {
                link_v4l2::video::VideoDevice::open_write(&node.path)?.set_format(&requested)?
            };
            if config.output == OutputFormat::Human {
                println!(
                    "{} {}x{} at {}/{} fps: {}",
                    report.requested.fourcc,
                    report.requested.width,
                    report.requested.height,
                    report.requested.fps.numerator,
                    report.requested.fps.denominator,
                    if report.dry_run {
                        "validated (dry-run)"
                    } else {
                        "applied and verified"
                    },
                );
                Ok(())
            } else {
                emit_success(config.output, "video.set", Some(summary), &report)
            }
        }
        VideoCommand::Stats { tuple, duration } => {
            let tuple = resolve_media_tuple(&node, config, &tuple, None)?;
            if dry_run {
                return emit_media_dry_run(config, "video.stats", summary, &node, &tuple);
            }
            #[cfg(feature = "gstreamer")]
            {
                let _transaction_lease =
                    acquire_operation_lease(config, &summary.stable_id, "video.stats")?;
                let _lease = link_media::MediaLease::acquire(&summary.stable_id, "video.stats")?;
                let report = link_media::stats(&link_media::ForegroundRequest {
                    node: PathBuf::from(&node.path),
                    tuple,
                    duration: Some(duration.get()),
                    shutdown_timeout: config.timeout.get(),
                })?;
                emit_media_report(config, "video.stats", summary, &report, false)
            }
            #[cfg(not(feature = "gstreamer"))]
            {
                let _ = duration;
                Err(LinkError::new(
                    ErrorKind::CapabilityUnsupported,
                    "this build does not include direct video statistics",
                ))
            }
        }
    }
}

fn resolve_media_tuple(
    node: &NodeAssociation,
    config: &Config,
    arguments: &VideoTupleArgs,
    forced_fourcc: Option<&str>,
) -> Result<VideoTuple, LinkError> {
    let backend = link_v4l2::video::VideoDevice::open_read(&node.path)?;
    let status = backend.status()?;
    let inventory = backend.formats()?;
    let (width, height) = arguments
        .size
        .as_deref()
        .map(parse_size)
        .transpose()?
        .unwrap_or((status.tuple.width, status.tuple.height));
    let fps = arguments
        .fps
        .as_deref()
        .map(parse_fps)
        .transpose()?
        .unwrap_or(status.tuple.fps);
    let requested_fourcc = forced_fourcc.or(arguments.fourcc.as_deref());
    let fourcc = if let Some(fourcc) = requested_fourcc {
        fourcc.to_ascii_uppercase()
    } else {
        config
            .media
            .preferred_transport
            .iter()
            .find(|candidate| {
                inventory.discrete.iter().any(|format| {
                    format.tuple.fourcc.eq_ignore_ascii_case(candidate)
                        && format.tuple.width == width
                        && format.tuple.height == height
                        && link_core::media::rational_cmp(format.tuple.fps, fps).is_eq()
                })
            })
            .cloned()
            .unwrap_or(status.tuple.fourcc)
    };
    let tuple = VideoTuple {
        fourcc,
        width,
        height,
        fps,
    }
    .normalized();
    link_v4l2::video::validate_tuple(&inventory.formats, &tuple)?;
    Ok(tuple)
}

fn parse_size(input: &str) -> Result<(u32, u32), LinkError> {
    let (width, height) = input
        .split_once(['x', 'X'])
        .ok_or_else(|| invalid_video_value("size", input, "expected WIDTHxHEIGHT"))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| invalid_video_value("size", input, "width is not an integer"))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| invalid_video_value("size", input, "height is not an integer"))?;
    if width == 0 || height == 0 {
        return Err(invalid_video_value(
            "size",
            input,
            "dimensions must be greater than zero",
        ));
    }
    Ok((width, height))
}

fn parse_fps(input: &str) -> Result<Rational, LinkError> {
    let trimmed = input.trim();
    let (numerator, denominator) = if let Some((numerator, denominator)) = trimmed.split_once('/') {
        let numerator = numerator
            .parse::<u32>()
            .map_err(|_| invalid_video_value("fps", input, "invalid rational numerator"))?;
        let denominator = denominator
            .parse::<u32>()
            .map_err(|_| invalid_video_value("fps", input, "invalid rational denominator"))?;
        (numerator, denominator)
    } else if let Some((whole, fraction)) = trimmed.split_once('.') {
        if fraction.is_empty() || fraction.len() > 6 {
            return Err(invalid_video_value(
                "fps",
                input,
                "decimal FPS supports one to six fractional digits",
            ));
        }
        let denominator = 10_u32.pow(fraction.len() as u32);
        let whole = whole
            .parse::<u32>()
            .map_err(|_| invalid_video_value("fps", input, "invalid decimal FPS"))?;
        let fraction = fraction
            .parse::<u32>()
            .map_err(|_| invalid_video_value("fps", input, "invalid decimal FPS"))?;
        (
            whole
                .checked_mul(denominator)
                .and_then(|value| value.checked_add(fraction))
                .ok_or_else(|| invalid_video_value("fps", input, "FPS is too large"))?,
            denominator,
        )
    } else {
        (
            trimmed
                .parse::<u32>()
                .map_err(|_| invalid_video_value("fps", input, "FPS is not a number"))?,
            1,
        )
    };
    if numerator == 0 || denominator == 0 {
        return Err(invalid_video_value(
            "fps",
            input,
            "FPS must be greater than zero",
        ));
    }
    Ok(link_core::media::normalize_rational(Rational {
        numerator,
        denominator,
    }))
}

fn invalid_video_value(field: &'static str, value: &str, reason: &'static str) -> LinkError {
    LinkError::new(ErrorKind::InvalidInvocation, "invalid video tuple value")
        .with_detail("field", field)
        .with_detail("value", value.to_owned())
        .with_detail("reason", reason)
}

fn emit_media_dry_run(
    config: &Config,
    command: &'static str,
    device: DeviceSummary,
    node: &NodeAssociation,
    tuple: &VideoTuple,
) -> Result<(), LinkError> {
    let result = json!({
        "dry_run": true,
        "node": node.path,
        "tuple": tuple,
    });
    if config.output == OutputFormat::Human {
        println!(
            "Dry run: {} {}x{} at {}/{} fps on {}",
            tuple.fourcc,
            tuple.width,
            tuple.height,
            tuple.fps.numerator,
            tuple.fps.denominator,
            node.path,
        );
        Ok(())
    } else {
        emit_success(config.output, command, Some(device), &result)
    }
}

fn discover_audio() -> Result<(AudioInventory, Vec<DiscoveredDevice>), LinkError> {
    let cameras = discovered_devices()?;
    let associations = cameras
        .iter()
        .filter(|device| device.mode() == DeviceMode::Camera)
        .map(|device| link_audio::CameraAudioAssociation {
            stable_id: device.identity.stable_id(),
            usb: device.identity.clone(),
            card_indexes: device.alsa_card_indexes(),
        })
        .collect::<Vec<_>>();
    Ok((link_audio::inventory(&associations)?, cameras))
}

fn selected_audio_source(
    config: &Config,
    selector: &str,
) -> Result<(AudioEndpoint, AudioInventory), LinkError> {
    let (inventory, cameras) = discover_audio()?;
    let camera = if selector == "camera" {
        let selected = if let Some(selector) = config.default_device.as_deref() {
            link_linux::select_devices(&cameras, selector)?
        } else {
            match cameras.as_slice() {
                [camera] => vec![camera],
                [] => {
                    return Err(LinkError::new(
                        ErrorKind::DeviceNotFound,
                        "no camera device was discovered",
                    ));
                }
                _ => {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "multiple cameras were discovered; select one with --device",
                    ));
                }
            }
        };
        match selected.as_slice() {
            [camera] => Some(camera.identity.stable_id()),
            _ => {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "camera audio requires exactly one selected camera",
                ));
            }
        }
    } else {
        None
    };
    let endpoint =
        link_audio::resolve_capture_source(&inventory, Some(selector), camera.as_deref())?;
    Ok((endpoint, inventory))
}

fn run_audio(
    config: &Config,
    backend: Option<BackendChoice>,
    command: AudioCommand,
    dry_run: bool,
) -> Result<(), LinkError> {
    if config.daemon == DaemonMode::Always {
        return Err(LinkError::new(
            ErrorKind::DaemonUnavailable,
            "the daemon is not implemented in this build",
        ));
    }
    match command {
        AudioCommand::Devices => run_audio_devices(config),
        AudioCommand::Status { source } => {
            if backend == Some(BackendChoice::Vendor) {
                return Err(audio_backend_unsupported("vendor"));
            }
            let (endpoint, _) = selected_audio_source(config, &source)?;
            let status = link_audio::status(&endpoint)?;
            if config.output == OutputFormat::Human {
                println!("{} ({})", status.source.name, status.source.id);
                if let Some(hardware) = &status.hardware {
                    println!(
                        "  hardware: gain={} mute={}",
                        hardware
                            .gain
                            .map_or_else(|| "-".into(), |gain| format!("{:.1}%", gain * 100.0)),
                        hardware
                            .muted
                            .map_or_else(|| "-".into(), |muted| muted.to_string())
                    );
                }
                if let Some(host) = &status.host {
                    println!(
                        "  host: gain={} mute={}",
                        host.gain
                            .map_or_else(|| "-".into(), |gain| format!("{:.1}%", gain * 100.0)),
                        host.muted
                            .map_or_else(|| "-".into(), |muted| muted.to_string())
                    );
                }
                Ok(())
            } else {
                emit_success(config.output, "audio.status", None, &status)
            }
        }
        AudioCommand::Gain {
            gain,
            source,
            clamp,
        } => {
            let (endpoint, _) = selected_audio_source(config, &source)?;
            let gain = parse_audio_gain(&gain, clamp)?;
            let layer = select_audio_control_layer(&endpoint, backend, true)?;
            let lease_key = endpoint
                .associated_camera
                .as_deref()
                .unwrap_or(&endpoint.id);
            let _lease = acquire_operation_lease(config, lease_key, "audio.gain")?;
            let report = link_audio::set_gain(&endpoint, layer, gain, dry_run)?;
            emit_audio_set_report(config, "audio.gain", &endpoint, &report)
        }
        AudioCommand::Mute { source } => run_audio_mute(config, backend, &source, true, dry_run),
        AudioCommand::Unmute { source } => run_audio_mute(config, backend, &source, false, dry_run),
        AudioCommand::Mode { command } => match command {
            AudioModeCommand::Status => {
                run_native_status(config, "audio.mode.status", &["audio.pickup-mode"])
            }
            AudioModeCommand::Standard => run_native_vendor_set(
                config,
                "audio.mode.standard",
                "audio.pickup-mode",
                "standard",
                dry_run,
            ),
            AudioModeCommand::Wide => run_native_vendor_set(
                config,
                "audio.mode.wide",
                "audio.pickup-mode",
                "wide",
                dry_run,
            ),
            AudioModeCommand::Focus => run_native_vendor_set(
                config,
                "audio.mode.focus",
                "audio.pickup-mode",
                "focus",
                dry_run,
            ),
            AudioModeCommand::Original => run_native_vendor_set(
                config,
                "audio.mode.original",
                "audio.pickup-mode",
                "original",
                dry_run,
            ),
        },
        #[cfg(feature = "gstreamer")]
        AudioCommand::Meter(arguments) => run_audio_meter(config, arguments, dry_run),
        #[cfg(feature = "gstreamer")]
        AudioCommand::Capture(arguments) => run_audio_capture(config, arguments, dry_run),
        #[cfg(feature = "gstreamer")]
        AudioCommand::Monitor(arguments) => run_audio_monitor(config, arguments, dry_run),
    }
}

fn run_audio_mute(
    config: &Config,
    backend: Option<BackendChoice>,
    source: &str,
    muted: bool,
    dry_run: bool,
) -> Result<(), LinkError> {
    let (endpoint, _) = selected_audio_source(config, source)?;
    let layer = select_audio_control_layer(&endpoint, backend, false)?;
    let lease_key = endpoint
        .associated_camera
        .as_deref()
        .unwrap_or(&endpoint.id);
    let _lease = acquire_operation_lease(
        config,
        lease_key,
        if muted { "audio.mute" } else { "audio.unmute" },
    )?;
    let report = link_audio::set_mute(&endpoint, layer, muted, dry_run)?;
    emit_audio_set_report(
        config,
        if muted { "audio.mute" } else { "audio.unmute" },
        &endpoint,
        &report,
    )
}

fn run_audio_devices(config: &Config) -> Result<(), LinkError> {
    let (mut inventory, cameras) = discover_audio()?;
    if let Some(selector) = config.default_device.as_deref()
        && selector != "all"
    {
        let selected = link_linux::select_devices(&cameras, selector)?;
        let selected_ids = selected
            .iter()
            .map(|device| device.identity.stable_id())
            .collect::<Vec<_>>();
        inventory.endpoints.retain(|endpoint| {
            endpoint
                .associated_camera
                .as_ref()
                .is_some_and(|camera| selected_ids.contains(camera))
        });
        let endpoint_ids = inventory
            .endpoints
            .iter()
            .map(|endpoint| endpoint.id.as_str())
            .collect::<Vec<_>>();
        inventory
            .states
            .retain(|state| endpoint_ids.contains(&state.endpoint_id.as_str()));
    }
    if config.output == OutputFormat::Human {
        println!("ID\tDIRECTION\tNAME\tBACKENDS\tCAMERA\tFORMAT\tROUTE\tSTATE");
        for endpoint in &inventory.endpoints {
            let backends = endpoint
                .transports
                .iter()
                .map(|transport| match transport.backend {
                    AudioBackendKind::Alsa => "alsa",
                    AudioBackendKind::Pipewire => "pipewire",
                })
                .collect::<Vec<_>>()
                .join(",");
            let controls = endpoint
                .mixer_controls
                .iter()
                .map(|control| control.name.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let control_state = inventory
                .states
                .iter()
                .find(|state| state.endpoint_id == endpoint.id)
                .map(|state| {
                    let hardware = state.hardware.as_ref().map_or_else(
                        || "hw=-".into(),
                        |value| {
                            format!(
                                "hw={}/{}",
                                value.gain.map_or_else(
                                    || "-".into(),
                                    |gain| format!("{:.0}%", gain * 100.0)
                                ),
                                value.muted.map_or_else(
                                    || String::from("-"),
                                    |muted| String::from(if muted { "muted" } else { "open" })
                                )
                            )
                        },
                    );
                    let host = state.host.as_ref().map_or_else(
                        || "host=-".into(),
                        |value| {
                            format!(
                                "host={}/{}",
                                value.gain.map_or_else(
                                    || "-".into(),
                                    |gain| format!("{:.0}%", gain * 100.0)
                                ),
                                value.muted.map_or_else(
                                    || String::from("-"),
                                    |muted| String::from(if muted { "muted" } else { "open" })
                                )
                            )
                        },
                    );
                    format!("{hardware};{host};controls={controls}")
                })
                .unwrap_or_else(|| format!("controls={controls}"));
            println!(
                "{}\t{:?}\t{}\t{}\t{}\t{}ch {}Hz {}\t{}\t{}{}",
                endpoint.id,
                endpoint.direction,
                endpoint.name,
                backends,
                endpoint.associated_camera.as_deref().unwrap_or("-"),
                endpoint.channels_max.unwrap_or_default(),
                endpoint.rate_max.unwrap_or_default(),
                endpoint.formats.join(","),
                if endpoint.default { "default" } else { "-" },
                if endpoint.busy { "busy;" } else { "" },
                control_state,
            );
        }
        Ok(())
    } else {
        emit_success(config.output, "audio.devices", None, &inventory)
    }
}

fn run_preset(
    config: &Config,
    backend: Option<BackendChoice>,
    command: PresetCommand,
    dry_run: bool,
) -> Result<(), LinkError> {
    let store = PresetStore::from_process()?;
    match command {
        PresetCommand::Save {
            name,
            description,
            include,
            exclude,
        } => run_preset_save(
            config,
            backend,
            &store,
            name,
            description,
            include,
            exclude,
            dry_run,
        ),
        PresetCommand::Apply { name } => {
            let preset = store.load(&name)?;
            run_preset_apply(config, backend, preset, dry_run)
        }
        PresetCommand::List => {
            let presets = store.list()?;
            if config.output == OutputFormat::Human {
                if presets.is_empty() {
                    println!("No presets found in {}", store.directory().display());
                } else {
                    println!("NAME\tMODEL\tVIDEO\tCONTROLS\tAUDIO\tDESCRIPTION");
                    for preset in &presets {
                        println!(
                            "{}\t{}\t{}\t{}\t{}\t{}",
                            preset.name,
                            preset.model,
                            preset.has_video,
                            preset.standard_control_count,
                            preset.has_audio,
                            preset.description.as_deref().unwrap_or("-"),
                        );
                    }
                }
                Ok(())
            } else {
                emit_success(config.output, "preset.list", None, &presets)
            }
        }
        PresetCommand::Show { name } => {
            let preset = store.load(&name)?;
            if config.output == OutputFormat::Human {
                print!("{}", preset.to_toml()?);
                Ok(())
            } else {
                emit_success(config.output, "preset.show", None, &preset)
            }
        }
        PresetCommand::Delete { name } => {
            let preset = store.load(&name)?;
            let path = store.path_for(&preset.name)?;
            if !dry_run {
                store.delete(&name)?;
            }
            let result = json!({"name": name, "path": path, "dry_run": dry_run});
            if config.output == OutputFormat::Human {
                println!(
                    "{} {}",
                    if dry_run { "Would delete" } else { "Deleted" },
                    path.display()
                );
                Ok(())
            } else {
                emit_success(config.output, "preset.delete", None, &result)
            }
        }
        PresetCommand::Export { name, output } if output == Path::new("-") => {
            let preset = store.load(&name)?;
            print!("{}", preset.to_toml()?);
            Ok(())
        }
        PresetCommand::Export { name, output } => {
            let preset = store.load(&name)?;
            if output.exists() {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "preset export destination already exists",
                )
                .with_detail("path", output.display().to_string()));
            }
            if !dry_run {
                store.export(&name, &output)?;
            }
            let result = json!({"name": preset.name, "path": output, "dry_run": dry_run});
            if config.output == OutputFormat::Human {
                println!(
                    "{} {}",
                    if dry_run {
                        "Would export to"
                    } else {
                        "Exported to"
                    },
                    output.display()
                );
                Ok(())
            } else {
                emit_success(config.output, "preset.export", None, &result)
            }
        }
        PresetCommand::Import { file } => {
            let source = fs::read_to_string(&file).map_err(|error| {
                LinkError::new(ErrorKind::IoFailure, "failed to read preset import")
                    .with_detail("path", file.display().to_string())
                    .with_detail("reason", error.to_string())
            })?;
            let preset = Preset::parse(&source, &file.display().to_string())?;
            let destination = store.path_for(&preset.name)?;
            if destination.exists() {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "preset destination already exists",
                )
                .with_detail("path", destination.display().to_string()));
            }
            let summary = if dry_run {
                PresetSummary {
                    name: preset.name.clone(),
                    description: preset.description.clone(),
                    model: preset.requirements.model.clone(),
                    has_video: preset.video.is_some(),
                    standard_control_count: preset.standard_controls.len(),
                    has_audio: preset.audio.is_some(),
                    path: destination,
                }
            } else {
                store.import(&file)?
            };
            if config.output == OutputFormat::Human {
                println!(
                    "{} {} as {}",
                    if dry_run { "Would import" } else { "Imported" },
                    file.display(),
                    summary.name
                );
                Ok(())
            } else {
                emit_success(config.output, "preset.import", None, &summary)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_preset_save(
    config: &Config,
    backend: Option<BackendChoice>,
    store: &PresetStore,
    name: String,
    description: Option<String>,
    include: Vec<PresetCategoryChoice>,
    exclude: Vec<PresetCategoryChoice>,
    dry_run: bool,
) -> Result<(), LinkError> {
    let mut categories = if include.is_empty() {
        BTreeSet::from([
            PresetCategory::Video,
            PresetCategory::Image,
            PresetCategory::Zoom,
            PresetCategory::Controls,
            PresetCategory::Audio,
        ])
    } else {
        include.into_iter().map(Into::into).collect()
    };
    for category in exclude {
        categories.remove(&category.into());
    }
    if categories.is_empty() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "preset include/exclude selection contains no state groups",
        ));
    }
    if categories
        .iter()
        .any(|category| *category != PresetCategory::Audio)
    {
        ensure_standard_backend(config, backend)?;
    }
    let destination = store.path_for(&name)?;
    if destination.exists() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "preset destination already exists",
        )
        .with_detail("path", destination.display().to_string()));
    }
    let (device, node) = selected_video_device(config)?;
    let summary = device_summary(&device);
    let app_paths = AppPaths::from_process()?;
    let _lease = DeviceLease::acquire(
        &app_paths,
        &summary.stable_id,
        "preset.save",
        config.timeout.get(),
    )?;

    let video = categories
        .contains(&PresetCategory::Video)
        .then(|| link_v4l2::video::VideoDevice::open_read(&node.path)?.status())
        .transpose()?
        .map(|status| PresetVideo::from(status.tuple));

    let mut standard_controls = BTreeMap::new();
    if categories.iter().any(|category| {
        matches!(
            category,
            PresetCategory::Image | PresetCategory::Zoom | PresetCategory::Controls
        )
    }) {
        for control in link_v4l2::production::ControlDevice::open_read(&node.path)?.controls()? {
            let Some(category) = preset_control_category(&control) else {
                continue;
            };
            if categories.contains(&category)
                && control.readable
                && control.writable
                && control.available
                && control.codec_supported
                && !link_v4l2::production::is_movement_control(control.id)
                && let Some(current) = control.current
            {
                link_v4l2::production::validate_raw_value(&control, current)?;
                standard_controls.insert(control.name, current);
            }
        }
    }

    let audio = if categories.contains(&PresetCategory::Audio) {
        let (endpoint, _) = selected_audio_source(config, "camera")?;
        let layer = select_audio_control_layer(&endpoint, backend, true)
            .or_else(|_| select_audio_control_layer(&endpoint, backend, false))?;
        let status = link_audio::status(&endpoint)?;
        let state = match layer {
            AudioControlLayer::Hardware => status.hardware,
            AudioControlLayer::Host => status.host,
        }
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "selected audio control layer has no readable state",
            )
        })?;
        if state.gain.is_none() && state.muted.is_none() {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "selected audio control layer has no preset-compatible state",
            ));
        }
        Some(PresetAudio {
            source: if endpoint.associated_camera.as_deref() == Some(&summary.stable_id) {
                "camera".into()
            } else {
                endpoint.id
            },
            layer,
            gain_percent: state.gain.map(|gain| gain * 100.0),
            mute: state.muted,
        })
    } else {
        None
    };

    let preset = Preset {
        schema_version: PRESET_SCHEMA_VERSION,
        name,
        description: description.filter(|value| !value.trim().is_empty()),
        requirements: PresetRequirements {
            model: device.model(),
            usb_vid: Some(device.identity.vendor_id),
            usb_pid: Some(device.identity.product_id),
            fallback: Default::default(),
        },
        video,
        standard_controls,
        audio,
    };
    preset.validate("captured state")?;
    let saved = if dry_run {
        PresetSummary {
            name: preset.name.clone(),
            description: preset.description.clone(),
            model: preset.requirements.model.clone(),
            has_video: preset.video.is_some(),
            standard_control_count: preset.standard_controls.len(),
            has_audio: preset.audio.is_some(),
            path: destination,
        }
    } else {
        store.save(&preset)?
    };
    if config.output == OutputFormat::Human {
        println!(
            "{} preset {} at {} (video={}, controls={}, audio={})",
            if dry_run { "Would save" } else { "Saved" },
            saved.name,
            saved.path.display(),
            saved.has_video,
            saved.standard_control_count,
            saved.has_audio,
        );
        Ok(())
    } else {
        emit_success(config.output, "preset.save", Some(summary), &saved)
    }
}

fn preset_control_category(control: &ControlDescriptor) -> Option<PresetCategory> {
    if link_v4l2::production::is_movement_control(control.id) {
        return None;
    }
    if control.name == "zoom_absolute" {
        return Some(PresetCategory::Zoom);
    }
    if matches!(
        control.name.as_str(),
        "brightness"
            | "contrast"
            | "saturation"
            | "sharpness"
            | "backlight_compensation"
            | "exposure_time_absolute"
            | "exposure_compensation"
            | "iso_sensitivity"
            | "gain"
            | "white_balance_temperature"
            | "focus_absolute"
            | "power_line_frequency"
            | "wide_dynamic_range"
            | "hdr_sensor_mode"
            | "exposure_automatic"
            | "iso_sensitivity_automatic"
            | "gain_automatic"
            | "white_balance_automatic"
            | "focus_automatic_continuous"
    ) {
        Some(PresetCategory::Image)
    } else {
        Some(PresetCategory::Controls)
    }
}

#[derive(Clone)]
struct PreparedPresetApply {
    device: DiscoveredDevice,
    node: NodeAssociation,
    plan: TransactionPlan,
    control_requests: Vec<ControlRequest>,
    audio_endpoint: Option<AudioEndpoint>,
}

#[derive(Default)]
struct AppliedPresetState {
    video: Option<link_core::media::FormatSetReport>,
    controls: Option<ControlSetReport>,
    gain: Option<link_core::audio::AudioSetReport>,
    mute: Option<link_core::audio::AudioSetReport>,
}

fn run_preset_apply(
    config: &Config,
    backend: Option<BackendChoice>,
    preset: Preset,
    dry_run: bool,
) -> Result<(), LinkError> {
    let (selected_device, _) = selected_video_device(config)?;
    let summary = device_summary(&selected_device);
    let paths = AppPaths::from_process()?;
    let _lease = DeviceLease::acquire(
        &paths,
        &summary.stable_id,
        "preset.apply",
        config.timeout.get(),
    )?;
    let journals = JournalStore::new(paths.state.join("transactions"));
    if let Some(existing) = journals.existing(&summary.stable_id)? {
        return Err(LinkError::new(
            ErrorKind::PartialSuccess,
            "an incomplete preset transaction requires recovery review",
        )
        .with_detail(
            "journal",
            journals.path(&summary.stable_id).display().to_string(),
        )
        .with_detail(
            "transaction",
            serde_json::to_value(existing).unwrap_or_default(),
        ));
    }
    let prepared = prepare_preset_apply(config, backend, &preset, dry_run)?;
    let mut report = TransactionReport {
        schema_version: TRANSACTION_SCHEMA_VERSION,
        plan: prepared.plan.clone(),
        outcome: if dry_run {
            TransactionOutcome::Planned
        } else {
            TransactionOutcome::InProgress
        },
        steps: prepared
            .plan
            .steps
            .iter()
            .map(|step| TransactionStepReport {
                sequence: step.sequence,
                status: if step.no_op {
                    TransactionStepStatus::Skipped
                } else {
                    TransactionStepStatus::Pending
                },
                observed: step.no_op.then(|| step.previous.clone()),
                error: None,
            })
            .collect(),
        rollback_attempted: false,
        rollback_failures: Vec::new(),
        journal: None,
    };
    if dry_run {
        return emit_preset_transaction(config, summary, &report);
    }
    if prepared.plan.steps.iter().all(|step| step.no_op) {
        report.outcome = TransactionOutcome::Completed;
        return emit_preset_transaction(config, summary, &report);
    }

    report.journal = Some(journals.path(&summary.stable_id));
    write_transaction_journal(&journals, &report)?;
    let mut applied = AppliedPresetState::default();
    let application = apply_prepared_preset(
        config,
        &preset,
        &prepared,
        &mut report,
        &mut applied,
        &journals,
    );
    if let Err((error, sequence)) = application {
        let failed_stage_is_partial = error.kind() == ErrorKind::PartialSuccess;
        set_transaction_step(
            &mut report,
            sequence,
            TransactionStepStatus::Failed,
            None,
            Some(error_value(&error)),
        );
        let rollback_error = rollback_preset(&prepared, &mut report, &applied, &journals);
        if !failed_stage_is_partial
            && rollback_error.is_none()
            && report.rollback_failures.is_empty()
        {
            report.outcome = TransactionOutcome::RolledBack;
            if let Err(journal_error) = journals.remove(&summary.stable_id) {
                report.outcome = TransactionOutcome::Partial;
                let _ = write_transaction_journal(&journals, &report);
                return Err(LinkError::new(
                    ErrorKind::PartialSuccess,
                    "preset transaction rolled back but its recovery journal remains",
                )
                .with_detail("cause", error_value(&error))
                .with_detail("journal_error", error_value(&journal_error))
                .with_detail(
                    "transaction",
                    serde_json::to_value(report).unwrap_or_default(),
                ));
            }
            report.journal = None;
            return Err(LinkError::new(error.kind(), error.message())
                .with_detail("cause", error_value(&error))
                .with_detail(
                    "transaction",
                    serde_json::to_value(report).unwrap_or_default(),
                ));
        }
        report.outcome = TransactionOutcome::Partial;
        if failed_stage_is_partial {
            report.rollback_failures.push("failed-stage-state".into());
        }
        if let Some(rollback_error) = rollback_error {
            report
                .rollback_failures
                .push(rollback_error.message().into());
        }
        let _ = write_transaction_journal(&journals, &report);
        return Err(LinkError::new(
            ErrorKind::PartialSuccess,
            "preset transaction failed and rollback was incomplete",
        )
        .with_detail("cause", error_value(&error))
        .with_detail(
            "transaction",
            serde_json::to_value(report).unwrap_or_default(),
        ));
    }
    report.outcome = TransactionOutcome::Completed;
    if let Err(error) = journals.remove(&summary.stable_id) {
        report.outcome = TransactionOutcome::Partial;
        let _ = write_transaction_journal(&journals, &report);
        return Err(LinkError::new(
            ErrorKind::PartialSuccess,
            "preset was applied but its recovery journal remains",
        )
        .with_detail("journal_error", error_value(&error))
        .with_detail(
            "transaction",
            serde_json::to_value(report).unwrap_or_default(),
        ));
    }
    report.journal = None;
    emit_preset_transaction(config, summary, &report)
}

fn prepare_preset_apply(
    config: &Config,
    backend: Option<BackendChoice>,
    preset: &Preset,
    dry_run: bool,
) -> Result<PreparedPresetApply, LinkError> {
    preset.validate("preset apply")?;
    let (device, node) = selected_video_device(config)?;
    validate_preset_requirements(
        &preset.requirements,
        &device.model(),
        device.identity.vendor_id,
        device.identity.product_id,
    )?;
    if preset.video.is_some() || !preset.standard_controls.is_empty() {
        ensure_standard_backend(config, backend)?;
    }
    let stable_id = device.identity.stable_id();
    let mut steps = Vec::new();
    let mut sequence = 0_u32;
    if let Some(video) = &preset.video {
        let requested = VideoTuple::from(video);
        let reader = link_v4l2::video::VideoDevice::open_read(&node.path)?;
        let previous = reader.status()?.tuple;
        reader.validate(&requested)?;
        steps.push(TransactionStepPlan {
            sequence,
            kind: TransactionStepKind::VideoFormat,
            backend: "v4l2".into(),
            previous: serde_json::to_value(&previous).unwrap_or_default(),
            requested: serde_json::to_value(&requested).unwrap_or_default(),
            reversible: true,
            no_op: previous.equivalent(&requested),
        });
        sequence += 1;
    }

    let control_reader = link_v4l2::production::ControlDevice::open_read(&node.path)?;
    let mut desired_by_id = BTreeMap::new();
    let mut descriptors = BTreeMap::new();
    for (name, value) in &preset.standard_controls {
        let descriptor = control_reader.resolve(name)?;
        link_v4l2::production::validate_raw_value(&descriptor, *value)?;
        desired_by_id.insert(descriptor.id, *value);
        descriptors.insert(descriptor.id, descriptor);
    }
    for descriptor in descriptors.values() {
        for (parent_id, manual_value) in link_v4l2::production::manual_dependencies(descriptor.id) {
            if desired_by_id
                .get(&parent_id)
                .is_some_and(|desired| *desired != manual_value)
            {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "preset contains conflicting automatic/manual control state",
                )
                .with_detail("control", descriptor.name.clone())
                .with_detail("parent_id", u64::from(parent_id)));
            }
        }
    }
    let mut control_requests = Vec::new();
    let mut previous_controls = BTreeMap::new();
    let mut requested_controls = BTreeMap::new();
    for (id, desired) in desired_by_id {
        let descriptor = &descriptors[&id];
        let (_, previous) = control_reader.get(id)?;
        previous_controls.insert(descriptor.name.clone(), previous.raw);
        requested_controls.insert(descriptor.name.clone(), desired);
        if previous.raw != desired {
            control_requests.push(ControlRequest {
                selector: id.to_string(),
                value: RequestedValue::Raw(desired),
            });
        }
    }
    let mut prerequisite_previous = BTreeMap::new();
    let mut prerequisite_requested = BTreeMap::new();
    if !preset.standard_controls.is_empty() {
        if !control_requests.is_empty() {
            let preview = execute_requests(
                &device,
                config,
                control_requests.clone(),
                false,
                false,
                false,
                true,
                true,
            )?;
            for change in preview
                .changes
                .into_iter()
                .filter(|change| change.prerequisite)
            {
                let previous = change.previous.ok_or_else(|| {
                    LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "preset prerequisite cannot be snapshotted for rollback",
                    )
                    .with_detail("control", change.control.name.clone())
                })?;
                prerequisite_previous.insert(change.control.name.clone(), previous.raw);
                prerequisite_requested.insert(change.control.name, change.requested.raw);
            }
        }
        if !prerequisite_requested.is_empty() {
            steps.push(TransactionStepPlan {
                sequence,
                kind: TransactionStepKind::ControlPrerequisite,
                backend: "v4l2".into(),
                previous: serde_json::to_value(prerequisite_previous).unwrap_or_default(),
                requested: serde_json::to_value(prerequisite_requested).unwrap_or_default(),
                reversible: true,
                no_op: false,
            });
            sequence += 1;
        }
        steps.push(TransactionStepPlan {
            sequence,
            kind: TransactionStepKind::StandardControls,
            backend: "v4l2".into(),
            previous: serde_json::to_value(previous_controls).unwrap_or_default(),
            requested: serde_json::to_value(requested_controls).unwrap_or_default(),
            reversible: true,
            no_op: control_requests.is_empty(),
        });
        sequence += 1;
    }

    let mut audio_endpoint = None;
    if let Some(audio) = &preset.audio {
        let (endpoint, _) = selected_audio_source(config, &audio.source)?;
        let requested_layer = match backend.unwrap_or(BackendChoice::Auto) {
            BackendChoice::Auto => audio.layer,
            BackendChoice::Standard => AudioControlLayer::Hardware,
            BackendChoice::Host => AudioControlLayer::Host,
            BackendChoice::Vendor => return Err(audio_backend_unsupported("vendor")),
        };
        if requested_layer != audio.layer {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "forced audio backend does not match the preset control layer",
            ));
        }
        let status = link_audio::status(&endpoint)?;
        let state = match audio.layer {
            AudioControlLayer::Hardware => status.hardware,
            AudioControlLayer::Host => status.host,
        }
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "preset audio control layer is unavailable",
            )
        })?;
        if let Some(gain_percent) = audio.gain_percent {
            let previous = state.gain.ok_or_else(|| {
                LinkError::new(
                    ErrorKind::CapabilityUnsupported,
                    "preset audio gain is unavailable",
                )
            })?;
            let requested = gain_percent / 100.0;
            steps.push(TransactionStepPlan {
                sequence,
                kind: TransactionStepKind::AudioGain,
                backend: format!("{:?}", audio.layer).to_ascii_lowercase(),
                previous: json!(previous),
                requested: json!(requested),
                reversible: true,
                no_op: (previous - requested).abs() <= f64::EPSILON,
            });
            sequence += 1;
        }
        if let Some(muted) = audio.mute {
            let previous = state.muted.ok_or_else(|| {
                LinkError::new(
                    ErrorKind::CapabilityUnsupported,
                    "preset audio mute is unavailable",
                )
            })?;
            steps.push(TransactionStepPlan {
                sequence,
                kind: TransactionStepKind::AudioMute,
                backend: format!("{:?}", audio.layer).to_ascii_lowercase(),
                previous: json!(previous),
                requested: json!(muted),
                reversible: true,
                no_op: previous == muted,
            });
        }
        audio_endpoint = Some(endpoint);
    }
    Ok(PreparedPresetApply {
        device,
        node,
        plan: TransactionPlan {
            schema_version: TRANSACTION_SCHEMA_VERSION,
            transaction_id: new_transaction_id(&stable_id, &preset.name),
            preset: preset.name.clone(),
            stable_id,
            dry_run,
            restart_required: false,
            stream_restart_required: false,
            rollback_feasible: steps.iter().all(|step| step.reversible),
            steps,
        },
        control_requests,
        audio_endpoint,
    })
}

fn validate_preset_requirements(
    requirements: &PresetRequirements,
    model: &str,
    usb_vid: u16,
    usb_pid: u16,
) -> Result<(), LinkError> {
    if requirements.model != model
        || requirements.usb_vid.is_some_and(|value| value != usb_vid)
        || requirements.usb_pid.is_some_and(|value| value != usb_pid)
    {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "preset does not match the selected device",
        )
        .with_detail("preset_model", requirements.model.clone())
        .with_detail("device_model", model.to_owned())
        .with_detail("usb_vid", u64::from(usb_vid))
        .with_detail("usb_pid", u64::from(usb_pid)));
    }
    Ok(())
}

fn apply_prepared_preset(
    config: &Config,
    preset: &Preset,
    prepared: &PreparedPresetApply,
    report: &mut TransactionReport,
    applied: &mut AppliedPresetState,
    journals: &JournalStore,
) -> Result<(), (LinkError, u32)> {
    if let Some(step) = plan_step(&prepared.plan, TransactionStepKind::VideoFormat)
        && !step.no_op
    {
        let requested = VideoTuple::from(preset.video.as_ref().expect("video plan requires video"));
        let result = link_v4l2::video::VideoDevice::open_write(&prepared.node.path)
            .and_then(|mut device| device.set_format(&requested))
            .map_err(|error| (error, step.sequence))?;
        set_transaction_step(
            report,
            step.sequence,
            TransactionStepStatus::Verified,
            Some(serde_json::to_value(&result.applied).unwrap_or_default()),
            None,
        );
        applied.video = Some(result);
        write_transaction_journal(journals, report).map_err(|error| (error, step.sequence))?;
    }
    if let Some(step) = plan_step(&prepared.plan, TransactionStepKind::StandardControls)
        && !step.no_op
    {
        let result = execute_requests(
            &prepared.device,
            config,
            prepared.control_requests.clone(),
            false,
            false,
            false,
            true,
            false,
        )
        .map_err(|error| (error, step.sequence))?;
        if let Some(prerequisite) =
            plan_step(&prepared.plan, TransactionStepKind::ControlPrerequisite)
        {
            let observed = result
                .changes
                .iter()
                .filter(|change| change.prerequisite)
                .collect::<Vec<_>>();
            set_transaction_step(
                report,
                prerequisite.sequence,
                TransactionStepStatus::Verified,
                Some(serde_json::to_value(observed).unwrap_or_default()),
                None,
            );
        }
        set_transaction_step(
            report,
            step.sequence,
            TransactionStepStatus::Verified,
            Some(serde_json::to_value(&result).unwrap_or_default()),
            None,
        );
        applied.controls = Some(result);
        write_transaction_journal(journals, report).map_err(|error| (error, step.sequence))?;
    }
    let endpoint = prepared.audio_endpoint.as_ref();
    if let Some(step) = plan_step(&prepared.plan, TransactionStepKind::AudioGain)
        && !step.no_op
    {
        let audio = preset.audio.as_ref().expect("audio plan requires audio");
        let gain = audio.gain_percent.expect("gain step requires gain") / 100.0;
        let result = link_audio::set_gain(
            endpoint.expect("audio step requires endpoint"),
            audio.layer,
            gain,
            false,
        )
        .map_err(|error| (error, step.sequence))?;
        set_transaction_step(
            report,
            step.sequence,
            TransactionStepStatus::Verified,
            Some(serde_json::to_value(&result.observed).unwrap_or_default()),
            None,
        );
        applied.gain = Some(result);
        write_transaction_journal(journals, report).map_err(|error| (error, step.sequence))?;
    }
    if let Some(step) = plan_step(&prepared.plan, TransactionStepKind::AudioMute)
        && !step.no_op
    {
        let audio = preset.audio.as_ref().expect("audio plan requires audio");
        let result = link_audio::set_mute(
            endpoint.expect("audio step requires endpoint"),
            audio.layer,
            audio.mute.expect("mute step requires mute"),
            false,
        )
        .map_err(|error| (error, step.sequence))?;
        set_transaction_step(
            report,
            step.sequence,
            TransactionStepStatus::Verified,
            Some(serde_json::to_value(&result.observed).unwrap_or_default()),
            None,
        );
        applied.mute = Some(result);
        write_transaction_journal(journals, report).map_err(|error| (error, step.sequence))?;
    }
    Ok(())
}

fn rollback_preset(
    prepared: &PreparedPresetApply,
    report: &mut TransactionReport,
    applied: &AppliedPresetState,
    journals: &JournalStore,
) -> Option<LinkError> {
    report.rollback_attempted = true;
    let mut first_error = None;
    for kind in rollback_order(applied) {
        match kind {
            TransactionStepKind::AudioMute => {
                let applied = applied.mute.as_ref().expect("rollback order requires mute");
                let sequence = plan_step(&prepared.plan, kind)
                    .expect("applied mute has plan")
                    .sequence;
                let previous = applied
                    .previous
                    .muted
                    .expect("mute report has previous mute");
                let result = link_audio::set_mute(
                    prepared.audio_endpoint.as_ref().expect("mute has endpoint"),
                    applied.layer,
                    previous,
                    false,
                );
                record_rollback_result(
                    report,
                    sequence,
                    "audio-mute",
                    result.map(|_| ()),
                    &mut first_error,
                );
            }
            TransactionStepKind::AudioGain => {
                let applied = applied.gain.as_ref().expect("rollback order requires gain");
                let sequence = plan_step(&prepared.plan, kind)
                    .expect("applied gain has plan")
                    .sequence;
                let previous = applied
                    .previous
                    .gain
                    .expect("gain report has previous gain");
                let result = link_audio::set_gain(
                    prepared.audio_endpoint.as_ref().expect("gain has endpoint"),
                    applied.layer,
                    previous,
                    false,
                );
                record_rollback_result(
                    report,
                    sequence,
                    "audio-gain",
                    result.map(|_| ()),
                    &mut first_error,
                );
            }
            TransactionStepKind::StandardControls => {
                let applied = applied
                    .controls
                    .as_ref()
                    .expect("rollback order requires controls");
                let sequence = plan_step(&prepared.plan, kind)
                    .expect("applied controls have plan")
                    .sequence;
                let result = restore_control_report(&prepared.node.path, applied);
                record_rollback_result(
                    report,
                    sequence,
                    "standard-controls",
                    result,
                    &mut first_error,
                );
                if let Some(prerequisite) =
                    plan_step(&prepared.plan, TransactionStepKind::ControlPrerequisite)
                {
                    let standard = report
                        .steps
                        .iter()
                        .find(|step| step.sequence == sequence)
                        .expect("standard control rollback has report");
                    set_transaction_step(
                        report,
                        prerequisite.sequence,
                        standard.status,
                        None,
                        standard.error.clone(),
                    );
                }
            }
            TransactionStepKind::VideoFormat => {
                let applied = applied
                    .video
                    .as_ref()
                    .expect("rollback order requires video");
                let sequence = plan_step(&prepared.plan, kind)
                    .expect("applied video has plan")
                    .sequence;
                let result = link_v4l2::video::VideoDevice::open_write(&prepared.node.path)
                    .and_then(|mut device| device.set_format(&applied.previous))
                    .map(|_| ());
                record_rollback_result(report, sequence, "video-format", result, &mut first_error);
            }
            TransactionStepKind::ControlPrerequisite => unreachable!(
                "control prerequisites are restored with their standard control transaction"
            ),
        }
        if let Err(error) = write_transaction_journal(journals, report) {
            first_error.get_or_insert(error);
        }
    }
    first_error
}

fn rollback_order(applied: &AppliedPresetState) -> Vec<TransactionStepKind> {
    rollback_order_for(
        applied.video.is_some(),
        applied.controls.is_some(),
        applied.gain.is_some(),
        applied.mute.is_some(),
    )
}

fn rollback_order_for(
    video: bool,
    controls: bool,
    gain: bool,
    mute: bool,
) -> Vec<TransactionStepKind> {
    [
        (mute, TransactionStepKind::AudioMute),
        (gain, TransactionStepKind::AudioGain),
        (controls, TransactionStepKind::StandardControls),
        (video, TransactionStepKind::VideoFormat),
    ]
    .into_iter()
    .filter_map(|(applied, kind)| applied.then_some(kind))
    .collect()
}

fn restore_control_report(path: &str, applied: &ControlSetReport) -> Result<(), LinkError> {
    let prepared = applied
        .changes
        .iter()
        .map(|change| PreparedWrite {
            descriptor: change.control.clone(),
            value: change.requested.clone(),
            prerequisite: change.prerequisite,
        })
        .collect::<Vec<_>>();
    let previous = applied
        .changes
        .iter()
        .map(|change| change.previous.clone())
        .collect::<Vec<_>>();
    let writer = link_v4l2::production::ControlDevice::open_write(path)?;
    let rollback = rollback_controls(&writer, &prepared, &previous);
    if rollback.failed.is_empty() {
        Ok(())
    } else {
        Err(LinkError::new(
            ErrorKind::PartialSuccess,
            "failed to restore preset control state",
        )
        .with_detail(
            "failed",
            serde_json::to_value(rollback.failed).unwrap_or_default(),
        ))
    }
}

fn record_rollback_result(
    report: &mut TransactionReport,
    sequence: u32,
    label: &str,
    result: Result<(), LinkError>,
    first_error: &mut Option<LinkError>,
) {
    match result {
        Ok(()) => set_transaction_step(
            report,
            sequence,
            TransactionStepStatus::RolledBack,
            None,
            None,
        ),
        Err(error) => {
            report.rollback_failures.push(label.into());
            set_transaction_step(
                report,
                sequence,
                TransactionStepStatus::RollbackFailed,
                None,
                Some(error_value(&error)),
            );
            first_error.get_or_insert(error);
        }
    }
}

fn plan_step(plan: &TransactionPlan, kind: TransactionStepKind) -> Option<&TransactionStepPlan> {
    plan.steps.iter().find(|step| step.kind == kind)
}

fn set_transaction_step(
    report: &mut TransactionReport,
    sequence: u32,
    status: TransactionStepStatus,
    observed: Option<Value>,
    error: Option<Value>,
) {
    if let Some(step) = report
        .steps
        .iter_mut()
        .find(|step| step.sequence == sequence)
    {
        step.status = status;
        step.observed = observed;
        step.error = error;
    }
}

fn write_transaction_journal(
    journals: &JournalStore,
    report: &TransactionReport,
) -> Result<(), LinkError> {
    journals
        .write(&RecoveryJournal::new(report.clone()))
        .map(|_| ())
}

fn error_value(error: &LinkError) -> Value {
    json!({
        "code": error.kind().code(),
        "message": error.message(),
        "details": error.details(),
    })
}

fn emit_preset_transaction(
    config: &Config,
    device: DeviceSummary,
    report: &TransactionReport,
) -> Result<(), LinkError> {
    if config.output == OutputFormat::Human {
        println!(
            "Preset {} on {}: {:?}",
            report.plan.preset, device.stable_id, report.outcome
        );
        for step in &report.plan.steps {
            let status = report
                .steps
                .iter()
                .find(|value| value.sequence == step.sequence)
                .map_or(TransactionStepStatus::Pending, |value| value.status);
            println!(
                "  {}. {:?} via {}: {:?}{}",
                step.sequence + 1,
                step.kind,
                step.backend,
                status,
                if step.no_op { " (no-op)" } else { "" },
            );
        }
        Ok(())
    } else {
        emit_success(config.output, "preset.apply", Some(device), report)
    }
}

fn select_audio_control_layer(
    endpoint: &AudioEndpoint,
    backend: Option<BackendChoice>,
    gain: bool,
) -> Result<AudioControlLayer, LinkError> {
    match backend.unwrap_or(BackendChoice::Auto) {
        BackendChoice::Standard => Ok(AudioControlLayer::Hardware),
        BackendChoice::Host => Ok(AudioControlLayer::Host),
        BackendChoice::Vendor => Err(audio_backend_unsupported("vendor")),
        BackendChoice::Auto => {
            let status = link_audio::status(endpoint)?;
            if status.hardware.as_ref().is_some_and(|state| {
                if gain {
                    state.gain.is_some()
                } else {
                    state.muted.is_some()
                }
            }) {
                Ok(AudioControlLayer::Hardware)
            } else {
                Ok(AudioControlLayer::Host)
            }
        }
    }
}

fn audio_backend_unsupported(backend: &'static str) -> LinkError {
    LinkError::new(
        ErrorKind::CapabilityUnsupported,
        "the requested backend cannot provide this audio operation",
    )
    .with_detail("requested_backend", backend)
}

fn parse_audio_gain(value: &str, clamp: bool) -> Result<f64, LinkError> {
    let value = value.trim();
    let parsed = if let Some(percent) = value.strip_suffix('%') {
        percent.parse::<f64>().map(|value| value / 100.0)
    } else {
        value.parse::<f64>()
    }
    .map_err(|_| {
        LinkError::new(ErrorKind::InvalidInvocation, "invalid audio gain")
            .with_detail("value", value.to_owned())
            .with_detail("expected", "70% or normalized 0.7")
    })?;
    if !parsed.is_finite() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio gain must be finite",
        ));
    }
    if (0.0..=1.0).contains(&parsed) {
        Ok(parsed)
    } else if clamp {
        Ok(parsed.clamp(0.0, 1.0))
    } else {
        Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio gain must be between 0% and 100%; use --clamp to clamp it",
        ))
    }
}

fn emit_audio_set_report(
    config: &Config,
    command: &'static str,
    endpoint: &AudioEndpoint,
    report: &link_core::audio::AudioSetReport,
) -> Result<(), LinkError> {
    if config.output == OutputFormat::Human {
        println!(
            "{} {}: {}{}",
            endpoint.name,
            report.field,
            if report.dry_run {
                "validated"
            } else {
                "applied and verified"
            },
            if report.dry_run { " (dry run)" } else { "" }
        );
        Ok(())
    } else {
        emit_success(config.output, command, None, report)
    }
}

#[cfg(feature = "gstreamer")]
fn resolved_audio_request(
    config: &Config,
    stream: AudioStreamArgs,
    duration: Option<Duration>,
    delay_ns: i64,
) -> Result<link_media::AudioSourceRequest, LinkError> {
    let (endpoint, _) = selected_audio_source(config, &stream.source)?;
    let transport = if let Some(selector) = stream.source.strip_prefix("alsa:") {
        endpoint.transports.iter().find(|transport| {
            transport.backend == AudioBackendKind::Alsa && transport.selector == selector
        })
    } else if let Some(selector) = stream.source.strip_prefix("pipewire:") {
        endpoint.transports.iter().find(|transport| {
            transport.backend == AudioBackendKind::Pipewire && transport.selector == selector
        })
    } else {
        endpoint
            .transports
            .iter()
            .find(|transport| transport.backend == AudioBackendKind::Pipewire)
            .or_else(|| endpoint.transports.first())
    }
    .ok_or_else(|| {
        LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "selected audio endpoint has no usable capture transport",
        )
    })?;
    if endpoint.busy && transport.backend == AudioBackendKind::Alsa {
        return Err(LinkError::new(
            ErrorKind::DeviceBusy,
            "selected ALSA capture source is busy",
        ));
    }
    let sample_rate = stream.sample_rate.or(endpoint.rate_max).unwrap_or(48_000);
    let channels = stream.channels.or(endpoint.channels_min).unwrap_or(1);
    if !(8_000..=384_000).contains(&sample_rate) || !(1..=32).contains(&channels) {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio sample rate or channel count is outside supported conversion limits",
        )
        .with_detail("sample_rate", u64::from(sample_rate))
        .with_detail("channels", u64::from(channels)));
    }
    Ok(link_media::AudioSourceRequest {
        id: endpoint.id,
        backend: transport.backend,
        selector: transport.selector.clone(),
        sample_rate,
        channels,
        duration,
        shutdown_timeout: config.timeout.get(),
        processing: stream.processing.into(),
        delay_ns,
    })
}

#[cfg(feature = "gstreamer")]
fn run_audio_meter(
    config: &Config,
    arguments: AudioMeterArgs,
    dry_run: bool,
) -> Result<(), LinkError> {
    ensure_watch_format(config.output)?;
    if arguments.interval.get().is_zero() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio meter interval must be greater than zero",
        ));
    }
    let lease_key = audio_operation_lease_key(config, &arguments.stream.source)?;
    let source = resolved_audio_request(
        config,
        arguments.stream,
        arguments.duration.map(DurationValue::get),
        0,
    )?;
    if dry_run {
        return emit_audio_dry_run(config, "audio.meter", &source, None);
    }
    let _lease = acquire_operation_lease(config, &lease_key, "audio.meter")?;
    let report = link_media::audio_meter(
        &link_media::AudioMeterRequest {
            source,
            interval: arguments.interval.get(),
        },
        |event| match config.output {
            OutputFormat::Human => {
                println!(
                    "{:>8} ms  peak {:>7.2} dBFS  rms {:>7.2} dBFS{}",
                    event.elapsed_ms,
                    event.peak_dbfs,
                    event.rms_dbfs,
                    if event.clipped { "  CLIP" } else { "" }
                );
                Ok(())
            }
            OutputFormat::Jsonl => emit_success(config.output, "audio.meter", None, event),
            OutputFormat::Json => unreachable!("meter format is validated"),
        },
    )?;
    if config.output == OutputFormat::Human {
        println!(
            "{} buffers, {} clipping events, {} discontinuities",
            report.stats.buffers,
            report.stats.clipping_events,
            report.stats.timestamp_discontinuities
        );
        Ok(())
    } else {
        emit_success(config.output, "audio.meter", None, &report)
    }
}

#[cfg(feature = "gstreamer")]
fn run_audio_capture(
    config: &Config,
    arguments: AudioCaptureArgs,
    dry_run: bool,
) -> Result<(), LinkError> {
    match (&arguments.output, arguments.stdout) {
        (Some(_), true) => {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "audio capture OUTPUT cannot be combined with --stdout",
            ));
        }
        (None, false) => {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "audio capture requires OUTPUT or --stdout",
            ));
        }
        _ => {}
    }
    let encoding = audio_encoding(arguments.audio_format, arguments.output.as_deref())?;
    let lease_key = audio_operation_lease_key(config, &arguments.stream.source)?;
    let source = resolved_audio_request(
        config,
        arguments.stream,
        arguments.duration.map(DurationValue::get),
        0,
    )?;
    if dry_run {
        return emit_audio_dry_run(
            config,
            "audio.capture",
            &source,
            Some(audio_encoding_label(encoding)),
        );
    }
    let _lease = acquire_operation_lease(config, &lease_key, "audio.capture")?;
    let report = link_media::audio_capture(&link_media::AudioCaptureRequest {
        source,
        output: arguments.output,
        encoding,
        overwrite: arguments.overwrite,
    })?;
    if arguments.stdout {
        emit_success_to_stderr(config.output, "audio.capture", None, &report)
    } else if config.output == OutputFormat::Human {
        for output in &report.outputs {
            println!("Audio capture: {}", output.display());
        }
        Ok(())
    } else {
        emit_success(config.output, "audio.capture", None, &report)
    }
}

#[cfg(feature = "gstreamer")]
fn run_audio_monitor(
    config: &Config,
    arguments: AudioMonitorArgs,
    dry_run: bool,
) -> Result<(), LinkError> {
    let (endpoint, inventory) = selected_audio_source(config, &arguments.stream.source)?;
    let lease_key = endpoint
        .associated_camera
        .clone()
        .unwrap_or_else(|| endpoint.id.clone());
    let source = resolved_audio_request(
        config,
        arguments.stream,
        arguments.duration.map(DurationValue::get),
        0,
    )?;
    let sink = resolve_audio_sink(&inventory, &arguments.sink)?;
    if dry_run {
        return emit_audio_dry_run(config, "audio.monitor", &source, None);
    }
    let _lease = acquire_operation_lease(config, &lease_key, "audio.monitor")?;
    let report = link_media::audio_monitor(&link_media::AudioMonitorRequest {
        source,
        sink,
        latency: arguments.latency.get(),
    })?;
    if config.output == OutputFormat::Human {
        println!(
            "Monitor stopped: {} buffers, {} discontinuities",
            report.stats.buffers, report.stats.timestamp_discontinuities
        );
        Ok(())
    } else {
        emit_success(config.output, "audio.monitor", None, &report)
    }
}

#[cfg(feature = "gstreamer")]
fn audio_operation_lease_key(config: &Config, source: &str) -> Result<String, LinkError> {
    let (endpoint, _) = selected_audio_source(config, source)?;
    Ok(endpoint.associated_camera.unwrap_or(endpoint.id))
}

#[cfg(feature = "gstreamer")]
fn resolve_audio_sink(
    inventory: &AudioInventory,
    selector: &str,
) -> Result<Option<link_media::AudioSinkRequest>, LinkError> {
    if selector == "default" {
        return Ok(None);
    }
    let endpoint = inventory
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.direction == AudioDirection::Playback)
        .find(|endpoint| {
            endpoint.id == selector
                || selector.strip_prefix("alsa:").is_some_and(|value| {
                    endpoint.transports.iter().any(|transport| {
                        transport.backend == AudioBackendKind::Alsa && transport.selector == value
                    })
                })
                || selector.strip_prefix("pipewire:").is_some_and(|value| {
                    endpoint.transports.iter().any(|transport| {
                        transport.backend == AudioBackendKind::Pipewire
                            && transport.selector == value
                    })
                })
        })
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::DeviceNotFound,
                "audio playback sink was not found",
            )
            .with_detail("sink", selector.to_owned())
        })?;
    let transport = if let Some(value) = selector.strip_prefix("alsa:") {
        endpoint.transports.iter().find(|transport| {
            transport.backend == AudioBackendKind::Alsa && transport.selector == value
        })
    } else if let Some(value) = selector.strip_prefix("pipewire:") {
        endpoint.transports.iter().find(|transport| {
            transport.backend == AudioBackendKind::Pipewire && transport.selector == value
        })
    } else {
        endpoint
            .transports
            .iter()
            .find(|transport| transport.backend == AudioBackendKind::Pipewire)
            .or_else(|| endpoint.transports.first())
    }
    .ok_or_else(|| audio_backend_unsupported("audio-sink"))?;
    Ok(Some(link_media::AudioSinkRequest {
        backend: transport.backend,
        selector: transport.selector.clone(),
    }))
}

#[cfg(feature = "gstreamer")]
fn audio_encoding(
    requested: Option<AudioFormatChoice>,
    output: Option<&Path>,
) -> Result<link_media::AudioEncoding, LinkError> {
    match requested {
        Some(AudioFormatChoice::Wav) => Ok(link_media::AudioEncoding::Wav),
        Some(AudioFormatChoice::Flac) => Ok(link_media::AudioEncoding::Flac),
        Some(AudioFormatChoice::Raw) => Ok(link_media::AudioEncoding::Raw),
        None => match output
            .and_then(Path::extension)
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("wav") => Ok(link_media::AudioEncoding::Wav),
            Some("flac") => Ok(link_media::AudioEncoding::Flac),
            Some("raw" | "pcm" | "s16le") => Ok(link_media::AudioEncoding::Raw),
            None => Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "audio capture stdout requires --audio-format",
            )),
            _ => Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "cannot infer audio encoding from output extension",
            )),
        },
    }
}

#[cfg(feature = "gstreamer")]
const fn audio_encoding_label(encoding: link_media::AudioEncoding) -> &'static str {
    match encoding {
        link_media::AudioEncoding::Wav => "wav",
        link_media::AudioEncoding::Flac => "flac",
        link_media::AudioEncoding::Raw => "raw-s16le",
    }
}

#[cfg(feature = "gstreamer")]
fn emit_audio_dry_run(
    config: &Config,
    command: &'static str,
    source: &link_media::AudioSourceRequest,
    encoding: Option<&str>,
) -> Result<(), LinkError> {
    let result = json!({
        "dry_run": true,
        "source_id": source.id,
        "backend": source.backend,
        "selector": source.selector,
        "sample_rate": source.sample_rate,
        "channels": source.channels,
        "processing": source.processing,
        "encoding": encoding,
    });
    if config.output == OutputFormat::Human {
        println!(
            "Dry run: {} via {:?}, {} Hz, {} channel(s)",
            source.id, source.backend, source.sample_rate, source.channels
        );
        Ok(())
    } else {
        emit_success(config.output, command, None, &result)
    }
}

#[cfg(feature = "gstreamer")]
fn run_snapshot(
    config: &Config,
    backend: Option<BackendChoice>,
    arguments: SnapshotArgs,
    dry_run: bool,
) -> Result<(), LinkError> {
    ensure_direct_video_backend(config, backend)?;
    let (device, node) = selected_video_device(config)?;
    let summary = device_summary(&device);
    if arguments.count == 0 {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "snapshot --count must be greater than zero",
        ));
    }
    let stdout = arguments.output == Path::new("-");
    if stdout && arguments.count != 1 {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "snapshot stdout supports exactly one frame",
        ));
    }
    let encoding = snapshot_encoding(&arguments, stdout)?;
    let tuple = resolve_media_tuple(&node, config, &arguments.tuple, None)?;
    if dry_run {
        return emit_media_dry_run(config, "snapshot", summary, &node, &tuple);
    }
    let paths = if stdout {
        Vec::new()
    } else {
        snapshot_paths(&arguments.output, arguments.count)?
    };
    validate_snapshot_outputs(
        &paths,
        !stdout && !arguments.no_metadata,
        arguments.overwrite,
    )?;
    let _transaction_lease = acquire_operation_lease(config, &summary.stable_id, "snapshot")?;
    let _lease = link_media::MediaLease::acquire(&summary.stable_id, "snapshot")?;
    let metadata_template = (!stdout && !arguments.no_metadata)
        .then(|| build_snapshot_metadata(config, &device, &node, &tuple, encoding))
        .transpose()?;
    let frames = link_media::snapshot(&link_media::SnapshotRequest {
        node: PathBuf::from(&node.path),
        tuple: tuple.clone(),
        encoding,
        count: arguments.count,
        interval: arguments.interval.get(),
        timeout: config.timeout.get(),
    })?;
    if stdout {
        std::io::stdout()
            .write_all(&frames[0].bytes)
            .and_then(|()| std::io::stdout().flush())
            .map_err(|error| {
                LinkError::new(ErrorKind::IoFailure, "failed to write snapshot to stdout")
                    .with_detail("reason", error.to_string())
            })?;
        return emit_success_to_stderr(
            config.output,
            "snapshot",
            Some(summary),
            &json!({"tuple": tuple, "encoding": encoding, "bytes": frames[0].bytes.len()}),
        );
    }
    let mut metadata_paths = Vec::new();
    for (frame, path) in frames.iter().zip(&paths) {
        write_atomic(path, &frame.bytes, arguments.overwrite)?;
        if let Some(metadata_template) = &metadata_template {
            let mut metadata = metadata_template.clone();
            metadata.captured_unix_ms = frame.captured_unix_ms;
            let metadata_path = PathBuf::from(format!("{}.json", path.display()));
            let bytes = serde_json::to_vec_pretty(&metadata).map_err(serialization_error)?;
            write_atomic(&metadata_path, &bytes, arguments.overwrite)?;
            metadata_paths.push(metadata_path);
        }
    }
    let result = json!({
        "tuple": tuple,
        "encoding": encoding,
        "outputs": paths,
        "metadata": metadata_paths,
        "count": frames.len(),
    });
    if config.output == OutputFormat::Human {
        for path in &paths {
            println!("Snapshot: {}", path.display());
        }
        Ok(())
    } else {
        emit_success(config.output, "snapshot", Some(summary), &result)
    }
}

#[cfg(feature = "gstreamer")]
fn snapshot_encoding(
    arguments: &SnapshotArgs,
    stdout: bool,
) -> Result<link_media::SnapshotEncoding, LinkError> {
    if arguments.raw_frame {
        if arguments.image_format.is_some() {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "--raw-frame cannot be combined with --image-format",
            ));
        }
        return Ok(link_media::SnapshotEncoding::Raw);
    }
    match arguments.image_format {
        Some(ImageFormatChoice::Jpeg) => Ok(link_media::SnapshotEncoding::Jpeg),
        Some(ImageFormatChoice::Png) => Ok(link_media::SnapshotEncoding::Png),
        None if stdout => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "snapshot stdout requires --image-format or --raw-frame",
        )),
        None => match arguments
            .output
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("jpg" | "jpeg") => Ok(link_media::SnapshotEncoding::Jpeg),
            Some("png") => Ok(link_media::SnapshotEncoding::Png),
            _ => Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "cannot infer snapshot encoding from output extension",
            )),
        },
    }
}

#[cfg(feature = "gstreamer")]
fn build_snapshot_metadata(
    config: &Config,
    device: &DiscoveredDevice,
    node: &NodeAssociation,
    tuple: &VideoTuple,
    encoding: link_media::SnapshotEncoding,
) -> Result<SnapshotMetadata, LinkError> {
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let profile = catalog.report(&device.identity, device.mode())?;
    let controls = link_v4l2::production::ControlDevice::open_read(&node.path)?
        .controls()?
        .into_iter()
        .filter_map(|control| {
            control.current.map(|raw| SnapshotControl {
                name: control.name.clone(),
                value: link_v4l2::production::render_value(&control, raw),
            })
        })
        .collect();
    Ok(SnapshotMetadata {
        schema_version: SCHEMA_VERSION,
        captured_unix_ms: 0,
        stable_id: device.identity.stable_id(),
        model: device.model(),
        tuple: tuple.clone(),
        encoding: match encoding {
            link_media::SnapshotEncoding::Jpeg => "jpeg".into(),
            link_media::SnapshotEncoding::Png => "png".into(),
            link_media::SnapshotEncoding::Raw => tuple.fourcc.clone(),
        },
        profile_id: profile.profile_id,
        controls,
    })
}

#[cfg(feature = "gstreamer")]
fn snapshot_paths(output: &Path, count: u32) -> Result<Vec<PathBuf>, LinkError> {
    if count == 1 {
        return Ok(vec![output.to_owned()]);
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "snapshot output has no file stem",
            )
        })?;
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "burst snapshot output needs an extension",
            )
        })?;
    Ok((0..count)
        .map(|index| parent.join(format!("{stem}-{index:05}.{extension}")))
        .collect())
}

#[cfg(feature = "gstreamer")]
fn validate_snapshot_outputs(
    paths: &[PathBuf],
    metadata: bool,
    overwrite: bool,
) -> Result<(), LinkError> {
    for path in paths.iter().flat_map(|path| {
        let metadata_path = metadata.then(|| PathBuf::from(format!("{}.json", path.display())));
        std::iter::once(path.clone()).chain(metadata_path)
    }) {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(
                LinkError::new(ErrorKind::IoFailure, "output directory does not exist")
                    .with_detail("path", parent.display().to_string()),
            );
        }
        if path.exists() && !overwrite {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "output already exists; use --overwrite to replace it",
            )
            .with_detail("path", path.display().to_string()));
        }
    }
    Ok(())
}

#[cfg(feature = "gstreamer")]
fn write_atomic(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), LinkError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(
            LinkError::new(ErrorKind::IoFailure, "output directory does not exist")
                .with_detail("path", parent.display().to_string()),
        );
    }
    if path.exists() && !overwrite {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "output already exists; use --overwrite to replace it",
        )
        .with_detail("path", path.display().to_string()));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to create temporary output")
            .with_detail("path", parent.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    temporary.write_all(bytes).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to write output")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to synchronize output")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let persisted = if overwrite {
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    };
    persisted.map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to finalize output")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.error.to_string())
    })?;
    Ok(())
}

#[cfg(feature = "gstreamer")]
fn run_capture(
    config: &Config,
    backend: Option<BackendChoice>,
    arguments: CaptureArgs,
    dry_run: bool,
) -> Result<(), LinkError> {
    ensure_direct_video_backend(config, backend)?;
    let (device, node) = selected_video_device(config)?;
    let summary = device_summary(&device);
    let fourcc = match arguments.video {
        EncodedVideoChoice::H264 => "H264",
        EncodedVideoChoice::Mjpeg => "MJPG",
    };
    let tuple_arguments = VideoTupleArgs {
        fourcc: None,
        size: arguments.size,
        fps: arguments.fps,
    };
    let tuple = resolve_media_tuple(&node, config, &tuple_arguments, Some(fourcc))?;
    if dry_run {
        return emit_media_dry_run(config, "capture", summary, &node, &tuple);
    }
    let _transaction_lease = acquire_operation_lease(config, &summary.stable_id, "capture")?;
    let _lease = link_media::MediaLease::acquire(&summary.stable_id, "capture")?;
    let report = link_media::capture_stdout(&link_media::CaptureRequest {
        source: link_media::ForegroundRequest {
            node: PathBuf::from(&node.path),
            tuple,
            duration: arguments.duration.map(DurationValue::get),
            shutdown_timeout: config.timeout.get(),
        },
    })?;
    emit_media_report(config, "capture", summary, &report, true)
}

#[cfg(feature = "gstreamer")]
fn run_record(
    config: &Config,
    backend: Option<BackendChoice>,
    command: RecordCommand,
    dry_run: bool,
) -> Result<(), LinkError> {
    ensure_direct_video_backend(config, backend)?;
    let RecordCommand::Start {
        output,
        tuple: tuple_arguments,
        container,
        video_copy,
        duration,
        max_size,
        segment_duration,
        segment_size,
        rolling,
        disk_reserve,
        overwrite,
        audio,
        audio_delay,
        audio_processing,
    } = command;
    if rolling == Some(0) {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "--rolling must retain at least one segment",
        ));
    }
    if rolling.is_some() && segment_duration.is_none() && segment_size.is_none() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "--rolling requires --segment-duration or --segment-size",
        ));
    }
    let container = record_container(container, &output, &config.media.default_container)?;
    let disk_reserve = link_media::parse_byte_size(
        disk_reserve
            .as_deref()
            .unwrap_or(&config.media.disk_free_minimum),
    )?;
    let max_total_bytes = max_size
        .as_deref()
        .map(link_media::parse_byte_size)
        .transpose()?;
    let segment_bytes = segment_size
        .as_deref()
        .map(link_media::parse_byte_size)
        .transpose()?;
    let (device, node) = selected_video_device(config)?;
    let summary = device_summary(&device);
    let tuple = resolve_media_tuple(&node, config, &tuple_arguments, None)?;
    let delay_ns = parse_signed_duration_ns(&audio_delay)?;
    let processing_enabled =
        audio_processing.gate || audio_processing.compressor || audio_processing.limiter;
    if audio.is_none() && (delay_ns != 0 || processing_enabled) {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio delay and processing options require --audio",
        ));
    }
    let audio = audio
        .map(|source| {
            resolved_audio_request(
                config,
                AudioStreamArgs {
                    source,
                    sample_rate: None,
                    channels: None,
                    processing: audio_processing,
                },
                None,
                delay_ns,
            )
        })
        .transpose()?;
    if dry_run {
        return emit_media_dry_run(config, "record.start", summary, &node, &tuple);
    }
    let _transaction_lease = acquire_operation_lease(config, &summary.stable_id, "record.start")?;
    let _lease = link_media::MediaLease::acquire(&summary.stable_id, "record.start")?;
    let report = link_media::record(&link_media::RecordRequest {
        source: link_media::ForegroundRequest {
            node: PathBuf::from(&node.path),
            tuple,
            duration: duration.map(DurationValue::get),
            shutdown_timeout: config.timeout.get(),
        },
        output,
        container,
        require_video_copy: video_copy,
        max_total_bytes,
        segment_duration: segment_duration.map(DurationValue::get),
        segment_bytes,
        rolling_files: rolling,
        disk_reserve_bytes: disk_reserve,
        overwrite,
        audio,
    })?;
    emit_media_report(config, "record.start", summary, &report, false)
}

#[cfg(feature = "gstreamer")]
fn parse_signed_duration_ns(value: &str) -> Result<i64, LinkError> {
    let value = value.trim();
    let (negative, magnitude) = value
        .strip_prefix('-')
        .map_or((false, value), |magnitude| (true, magnitude));
    let magnitude = humantime::parse_duration(magnitude).map_err(|error| {
        LinkError::new(ErrorKind::InvalidInvocation, "invalid signed audio delay")
            .with_detail("value", value.to_owned())
            .with_detail("reason", error.to_string())
    })?;
    let nanoseconds = i64::try_from(magnitude.as_nanos()).map_err(|_| {
        LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio delay exceeds the supported signed nanosecond range",
        )
    })?;
    Ok(if negative { -nanoseconds } else { nanoseconds })
}

#[cfg(feature = "gstreamer")]
fn record_container(
    requested: Option<ContainerChoice>,
    output: &Path,
    default: &str,
) -> Result<link_media::RecordContainer, LinkError> {
    let value = match requested {
        Some(ContainerChoice::Matroska) => "matroska",
        Some(ContainerChoice::Mp4) => "mp4",
        None => match output.extension().and_then(|value| value.to_str()) {
            Some("mkv" | "mka") => "matroska",
            Some("mp4") => "mp4",
            _ => default,
        },
    };
    match value.to_ascii_lowercase().as_str() {
        "matroska" | "mkv" => Ok(link_media::RecordContainer::Matroska),
        "mp4" => Ok(link_media::RecordContainer::Mp4),
        _ => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "unsupported recording container",
        )
        .with_detail("container", value.to_owned())),
    }
}

#[cfg(feature = "network")]
fn run_stream(
    config: &Config,
    backend: Option<BackendChoice>,
    command: StreamCommand,
    dry_run: bool,
) -> Result<(), LinkError> {
    ensure_direct_video_backend(config, backend)?;
    let StreamCommand::Start {
        host,
        port,
        tuple: tuple_arguments,
        payload_type,
        duration,
    } = command;
    let (device, node) = selected_video_device(config)?;
    let summary = device_summary(&device);
    let tuple = resolve_media_tuple(&node, config, &tuple_arguments, None)?;
    if !matches!(tuple.fourcc.as_str(), "H264" | "MJPG") {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "RTP output requires H.264 or MJPEG input",
        ));
    }
    if dry_run {
        return emit_media_dry_run(config, "stream.start", summary, &node, &tuple);
    }
    let _transaction_lease = acquire_operation_lease(config, &summary.stable_id, "stream.start")?;
    let _lease = link_media::MediaLease::acquire(&summary.stable_id, "stream.start")?;
    let report = link_media::rtp(&link_media::RtpRequest {
        source: link_media::ForegroundRequest {
            node: PathBuf::from(&node.path),
            tuple,
            duration: duration.map(DurationValue::get),
            shutdown_timeout: config.timeout.get(),
        },
        host,
        port,
        payload_type,
    })?;
    emit_media_report(config, "stream.start", summary, &report, false)
}

#[cfg(feature = "gstreamer")]
fn emit_media_report(
    config: &Config,
    command: &'static str,
    device: DeviceSummary,
    report: &link_core::media::MediaRunReport,
    stderr: bool,
) -> Result<(), LinkError> {
    if config.output == OutputFormat::Human {
        let line = format!(
            "Frames: {}, bytes: {}, drops: {}, elapsed: {} ms, stop: {:?}",
            report.stats.frames,
            report.stats.bytes,
            report.stats.sequence_drops + report.stats.qos_drops,
            report.stats.elapsed_ms,
            report.stop_reason,
        );
        if stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
            for output in &report.outputs {
                println!("Output: {}", output.display());
            }
        }
        Ok(())
    } else if stderr {
        emit_success_to_stderr(config.output, command, Some(device), report)
    } else {
        emit_success(config.output, command, Some(device), report)
    }
}

fn now_unix_ms() -> Result<u128, LinkError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            LinkError::new(
                ErrorKind::IoFailure,
                "system clock is before the Unix epoch",
            )
            .with_detail("reason", error.to_string())
        })
}

fn white_balance_status_value(automatic: Option<i64>, kelvin: Option<i64>) -> Option<Value> {
    if automatic.is_none() && kelvin.is_none() {
        return None;
    }
    let mut status = serde_json::Map::new();
    if let Some(automatic) = automatic {
        status.insert(
            "mode".into(),
            json!(if automatic == 0 { "manual" } else { "auto" }),
        );
    }
    if let Some(kelvin) = kelvin {
        status.insert("kelvin".into(), json!(kelvin));
    }
    Some(Value::Object(status))
}

fn control_capabilities(
    device: &DiscoveredDevice,
    controls: Vec<ControlDescriptor>,
) -> Result<ControlCapabilities, LinkError> {
    let verified_at_unix_ms = now_unix_ms()?;
    let model = device.model();
    let groups: [(&str, &[&str]); 14] = [
        (
            "image.exposure",
            &["exposure_time_absolute", "exposure_automatic"],
        ),
        ("image.exposure_compensation", &["exposure_compensation"]),
        (
            "image.white_balance",
            &["white_balance_temperature", "white_balance_automatic"],
        ),
        (
            "image.focus",
            &["focus_absolute", "focus_automatic_continuous"],
        ),
        ("image.brightness", &["brightness"]),
        ("image.contrast", &["contrast"]),
        ("image.saturation", &["saturation"]),
        ("image.sharpness", &["sharpness"]),
        ("image.gain", &["gain"]),
        ("image.backlight_compensation", &["backlight_compensation"]),
        ("image.anti_flicker", &["power_line_frequency"]),
        ("image.hdr", &["wide_dynamic_range", "hdr_sensor_mode"]),
        ("image.mirror", &["horizontal_flip"]),
        ("image.flip", &["vertical_flip"]),
    ];
    let semantic = groups
        .into_iter()
        .map(|(capability, candidates)| {
            let control = candidates
                .iter()
                .find_map(|candidate| controls.iter().find(|control| control.name == *candidate))
                .cloned();
            let white_balance_temperature = (capability == "image.white_balance")
                .then(|| {
                    controls
                        .iter()
                        .find(|control| control.name == "white_balance_temperature")
                })
                .flatten();
            let white_balance_automatic = (capability == "image.white_balance")
                .then(|| {
                    controls
                        .iter()
                        .find(|control| control.name == "white_balance_automatic")
                })
                .flatten();
            let available = control.is_some();
            (
                capability.to_owned(),
                CapabilityRecord {
                    state: if available {
                        CapabilityState::Standard
                    } else {
                        CapabilityState::Unknown
                    },
                    backend: available.then(|| "v4l2".to_owned()),
                    evidence: if available {
                        "live V4L2 extended-control enumeration".into()
                    } else {
                        "no unambiguous standard V4L2 control was enumerated".into()
                    },
                    model: model.clone(),
                    firmware: None,
                    readable: control.as_ref().is_some_and(|control| control.readable),
                    writable: control.as_ref().is_some_and(|control| control.writable),
                    persistent: None,
                    stream_dependent: None,
                    restart_dependent: false,
                    destructive: false,
                    verified_at_unix_ms,
                    confidence: CapabilityConfidence::Verified,
                    source: control.as_ref().map(|control| CapabilitySource::V4l2 {
                        control: control.name.clone(),
                    }),
                    range: white_balance_temperature.map(|control| SemanticRange {
                        minimum: control.minimum as f64,
                        maximum: control.maximum as f64,
                        step: Some(control.step as f64),
                        unit: "K".into(),
                    }),
                    values: if capability == "image.white_balance" {
                        [
                            white_balance_automatic.map(|_| "auto".into()),
                            white_balance_temperature.map(|_| "manual".into()),
                        ]
                        .into_iter()
                        .flatten()
                        .collect()
                    } else {
                        Vec::new()
                    },
                    current: if capability == "image.white_balance" {
                        white_balance_status_value(
                            white_balance_automatic.and_then(|control| control.current),
                            white_balance_temperature.and_then(|control| control.current),
                        )
                    } else {
                        control
                            .as_ref()
                            .and_then(|control| control.current)
                            .map(|value| json!(value))
                    },
                    control,
                },
            )
        })
        .collect();
    Ok(ControlCapabilities {
        semantic,
        raw: controls,
    })
}

const CAMERA_NATIVE_CAPABILITIES: &[(&str, &str)] = &[
    (
        "image.exposure",
        "camera-native exposure controls have not been mapped for this profile",
    ),
    (
        "image.exposure_compensation",
        "camera-native exposure compensation has not been mapped for this profile",
    ),
    (
        "image.white_balance",
        "camera-native white balance has not been mapped for this profile",
    ),
    (
        "image.gain",
        "camera-native ISO/gain has not been mapped for this profile",
    ),
    (
        "image.hdr",
        "camera-native HDR has not been mapped for this profile",
    ),
    (
        "image.mirror",
        "camera-native mirroring has not been mapped for this profile",
    ),
    (
        "image.flip",
        "camera-native vertical flipping has not been mapped for this profile",
    ),
    (
        "frame.position",
        "digital frame translation has not been mapped for this profile",
    ),
    (
        "auto-framing.enabled",
        "camera-native Auto Framing has not been mapped for this profile",
    ),
    (
        "auto-framing.style",
        "Head and Half-body style selection has not been mapped for this profile",
    ),
    (
        "audio.pickup-mode",
        "camera pickup modes have not been mapped for this profile",
    ),
    (
        "mode.normal",
        "the normal-mode transition has not been mapped for this profile",
    ),
    (
        "mode.whiteboard",
        "regular camera Whiteboard Mode has not been mapped for this profile",
    ),
    (
        "gesture.enabled",
        "camera gesture recognition has not been mapped for this profile",
    ),
    (
        "gesture.palm",
        "the palm gesture setting has not been mapped for this profile",
    ),
    (
        "gesture.v-sign",
        "the V-sign gesture setting has not been mapped for this profile",
    ),
    (
        "gesture.l-sign",
        "the L-sign gesture setting has not been mapped for this profile",
    ),
    (
        "portrait.native",
        "native portrait mode has not been mapped for this profile",
    ),
    (
        "portrait.horizontal-correction",
        "portrait horizontal correction has not been mapped for this profile",
    ),
    (
        "portrait.vertical-correction",
        "portrait vertical correction has not been mapped for this profile",
    ),
    (
        "mode.compatibility",
        "low-resolution/YUY2 compatibility has not been mapped for this profile",
    ),
    (
        "firmware.version",
        "no verified firmware-version source is available",
    ),
    (
        "hardware.version",
        "no verified hardware-version source is available",
    ),
    (
        "device.mode-state",
        "no verified vendor mode-state source is available",
    ),
    (
        "device.error-state",
        "no verified vendor error-state source is available",
    ),
    (
        "device.indicator-state",
        "no verified indicator readback is available",
    ),
];

#[derive(Serialize)]
struct AllCapabilities {
    semantic: BTreeMap<String, CapabilityRecord>,
    raw_controls: Vec<ControlDescriptor>,
    profile_id: Option<String>,
    profile_checksum: Option<String>,
}

fn native_capabilities(
    config: &Config,
    device: &DiscoveredDevice,
) -> Result<AllCapabilities, LinkError> {
    native_capabilities_for(config, device, None)
}

fn native_capabilities_for(
    config: &Config,
    device: &DiscoveredDevice,
    current_names: Option<&[&str]>,
) -> Result<AllCapabilities, LinkError> {
    let verified_at_unix_ms = now_unix_ms()?;
    let model = device.model();
    if device.mode() != DeviceMode::Camera {
        let mut capabilities = BTreeMap::new();
        capabilities.insert(
            "zoom".into(),
            unmapped_capability(
                &model,
                verified_at_unix_ms,
                "standard and vendor camera controls are unavailable in U-disk mode",
            ),
        );
        for (name, _) in CAMERA_NATIVE_CAPABILITIES {
            capabilities.insert(
                (*name).into(),
                unmapped_capability(
                    &model,
                    verified_at_unix_ms,
                    "camera-native controls are unavailable in U-disk mode",
                ),
            );
        }
        insert_physical_shutter(&mut capabilities, &model, verified_at_unix_ms);
        return Ok(AllCapabilities {
            semantic: capabilities,
            raw_controls: Vec::new(),
            profile_id: None,
            profile_checksum: None,
        });
    }

    let node = control_node(device, config.default_device.as_deref())?;
    let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
    let controls = backend.controls()?;
    drop(backend);
    let mut capabilities = control_capabilities(device, controls.clone())?.semantic;

    if let Some(control) = controls
        .iter()
        .find(|control| control.name == "zoom_absolute")
        .cloned()
    {
        let range = SemanticRange {
            minimum: control.minimum as f64 / 100.0,
            maximum: control.maximum as f64 / 100.0,
            step: Some(control.step as f64 / 100.0),
            unit: "x".into(),
        };
        capabilities.insert(
            "zoom".into(),
            CapabilityRecord {
                state: CapabilityState::Standard,
                backend: Some("v4l2".into()),
                evidence: "live V4L2 zoom_absolute enumeration".into(),
                model: model.clone(),
                firmware: None,
                readable: control.readable,
                writable: control.writable,
                persistent: None,
                stream_dependent: Some(false),
                restart_dependent: false,
                destructive: false,
                verified_at_unix_ms,
                confidence: CapabilityConfidence::Verified,
                source: Some(CapabilitySource::V4l2 {
                    control: control.name.clone(),
                }),
                range: Some(range),
                values: Vec::new(),
                current: control.current.map(|raw| json!(raw as f64 / 100.0)),
                control: Some(control),
            },
        );
    } else {
        capabilities.insert(
            "zoom".into(),
            unmapped_capability(
                &model,
                verified_at_unix_ms,
                "no standard zoom control or verified vendor mapping is available",
            ),
        );
    }

    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let inventory = link_uvc_xu::parse_descriptors(&device.descriptors)?;
    let session = link_uvc_xu::XuSession::open_read(Path::new(&node.path))?;
    let (profile, firmware) = resolve_firmware_profile(device, &inventory, &catalog, &session)?;
    drop(session);
    let read_requirement = profile
        .map(|entry| profile_read_stream_requirement(&entry.profile, current_names))
        .transpose()?
        .flatten();
    #[cfg(feature = "gstreamer")]
    let _media_lease = if read_requirement == Some(StreamRequirement::Open) {
        Some(link_media::MediaLease::acquire(
            &device.identity.stable_id(),
            "native-status-read",
        )?)
    } else {
        None
    };
    let _read_stream = read_requirement
        .map(|requirement| prepare_xu_read_stream(requirement, &node.path, config.timeout.get()))
        .transpose()?
        .flatten();
    let session = link_uvc_xu::XuSession::open_read(Path::new(&node.path))?;
    for (name, evidence) in CAMERA_NATIVE_CAPABILITIES {
        if capabilities
            .get(*name)
            .is_some_and(|capability| capability.state == CapabilityState::Standard)
        {
            continue;
        }
        let record = profile
            .and_then(|entry| entry.profile.control(name).map(|control| (entry, control)))
            .map_or_else(
                || unmapped_capability(&model, verified_at_unix_ms, evidence),
                |(entry, control)| {
                    let read_verified = entry.semantic_read_verified();
                    CapabilityRecord {
                        state: if read_verified {
                            CapabilityState::VendorProfile
                        } else {
                            CapabilityState::DiscoveredUnmapped
                        },
                        backend: Some("uvc-xu".into()),
                        evidence: entry.profile.provenance.join("; "),
                        model: model.clone(),
                        firmware: firmware.clone(),
                        readable: control.readable,
                        writable: entry.authorized_control(name).is_some(),
                        persistent: Some(!matches!(control.persistence, Persistence::Volatile)),
                        stream_dependent: Some(!matches!(
                            control.stream_requirement,
                            StreamRequirement::Either
                        )),
                        restart_dependent: control.stream_requirement == StreamRequirement::Restart,
                        destructive: false,
                        verified_at_unix_ms,
                        confidence: if read_verified {
                            CapabilityConfidence::Verified
                        } else {
                            CapabilityConfidence::Experimental
                        },
                        source: Some(CapabilitySource::UvcXu {
                            profile_id: entry.profile.profile_id.clone(),
                            profile_checksum: entry.checksum.clone(),
                            guid: control.entity_guid.clone(),
                            selector: control.selector,
                            length: control.length,
                        }),
                        range: None,
                        values: profile_values(control),
                        current: if control.readable
                            && current_names.is_none_or(|names| names.contains(name))
                        {
                            link_uvc_xu::read_value(
                                &session,
                                &inventory,
                                Some(&control.entity_guid),
                                None,
                                control.selector,
                                Some(control.length),
                                Some(&entry.profile),
                            )
                            .ok()
                            .and_then(|value| value.decoded.get(*name).cloned())
                        } else {
                            None
                        },
                        control: None,
                    }
                },
            );
        capabilities.insert((*name).into(), record);
    }
    insert_physical_shutter(&mut capabilities, &model, verified_at_unix_ms);
    Ok(AllCapabilities {
        semantic: capabilities,
        raw_controls: controls,
        profile_id: profile.map(|entry| entry.profile.profile_id.clone()),
        profile_checksum: profile.map(|entry| entry.checksum.clone()),
    })
}

fn profile_read_stream_requirement(
    profile: &link_profiles::VendorProfile,
    current_names: Option<&[&str]>,
) -> Result<Option<StreamRequirement>, LinkError> {
    let mut requirement = None;
    for control in profile.controls.iter().filter(|control| {
        control.readable
            && current_names.is_none_or(|names| names.contains(&control.name.as_str()))
            && control.stream_requirement != StreamRequirement::Either
    }) {
        if requirement.is_some_and(|current| current != control.stream_requirement) {
            return Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "requested vendor reads require incompatible stream states",
            )
            .with_detail("control", control.name.clone()));
        }
        requirement = Some(control.stream_requirement);
    }
    Ok(requirement)
}

#[cfg(feature = "gstreamer")]
fn prepare_xu_read_stream(
    requirement: StreamRequirement,
    node: &str,
    timeout: Duration,
) -> Result<Option<link_media::ProbeStream>, LinkError> {
    match requirement {
        StreamRequirement::Either | StreamRequirement::Closed => Ok(None),
        StreamRequirement::Open => {
            let current = link_v4l2::video::VideoDevice::open_read(node)?
                .status()?
                .tuple;
            let stream = link_media::ProbeStream::open_with_format(node, timeout, Some(&current))?;
            // Link 2C Pro selector 2 exposes its active value after the first frames arrive.
            thread::sleep(Duration::from_millis(1_000));
            Ok(Some(stream))
        }
        StreamRequirement::Restart => Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "profile-required restart reads are not supported",
        )),
    }
}

#[cfg(not(feature = "gstreamer"))]
fn prepare_xu_read_stream(
    requirement: StreamRequirement,
    _node: &str,
    _timeout: Duration,
) -> Result<Option<()>, LinkError> {
    match requirement {
        StreamRequirement::Either | StreamRequirement::Closed => Ok(None),
        StreamRequirement::Open => Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "profile-required open-stream reads need a GStreamer-enabled build",
        )),
        StreamRequirement::Restart => Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "profile-required restart reads are not supported",
        )),
    }
}

fn insert_physical_shutter(
    capabilities: &mut BTreeMap<String, CapabilityRecord>,
    model: &str,
    verified_at_unix_ms: u128,
) {
    capabilities.insert(
        "privacy.physical-shutter".into(),
        CapabilityRecord {
            state: CapabilityState::HardwareOnly,
            backend: None,
            evidence: "the lens shutter is physically actuated and has no verified readback".into(),
            model: model.into(),
            firmware: None,
            readable: false,
            writable: false,
            persistent: None,
            stream_dependent: Some(false),
            restart_dependent: false,
            destructive: false,
            verified_at_unix_ms,
            confidence: CapabilityConfidence::Verified,
            source: Some(CapabilitySource::Hardware {
                component: "lens-shutter".into(),
            }),
            range: None,
            values: vec!["open".into(), "closed".into(), "unknown".into()],
            current: Some(json!("unknown")),
            control: None,
        },
    );
}

fn unmapped_capability(model: &str, verified_at_unix_ms: u128, evidence: &str) -> CapabilityRecord {
    CapabilityRecord {
        state: CapabilityState::DiscoveredUnmapped,
        backend: None,
        evidence: evidence.into(),
        model: model.into(),
        firmware: None,
        readable: false,
        writable: false,
        persistent: None,
        stream_dependent: None,
        restart_dependent: false,
        destructive: false,
        verified_at_unix_ms,
        confidence: CapabilityConfidence::Experimental,
        source: None,
        range: None,
        values: Vec::new(),
        current: None,
        control: None,
    }
}

fn profile_values(control: &link_profiles::ProfileControl) -> Vec<String> {
    match control.codec {
        CodecKind::Enum | CodecKind::Bitmask => control.values.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

fn run_caps_all(config: &Config, _backend_choice: Option<BackendChoice>) -> Result<(), LinkError> {
    let devices = selected_devices(config, true)?;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: native_capabilities(config, device)?,
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{} ({})", result.device.model, result.device.stable_id);
            for (name, capability) in &result.result.semantic {
                println!(
                    "{name}\t{:?}\t{}",
                    capability.state,
                    capability.backend.as_deref().unwrap_or("-")
                );
            }
        }
        Ok(())
    } else {
        emit_success(config.output, "caps.all", None, &results)
    }
}

#[derive(Serialize)]
struct ZoomStatusResult {
    factor: f64,
    raw: i64,
    minimum: f64,
    maximum: f64,
    step: f64,
    backend: &'static str,
}

#[derive(Serialize)]
struct ZoomRampResult {
    previous: ZoomStatusResult,
    requested_from: f64,
    requested_to: f64,
    observed: Option<ZoomStatusResult>,
    writes: usize,
    duration_ms: u64,
    verified: bool,
    dry_run: bool,
    rollback: RollbackReport,
}

fn run_zoom(
    config: &Config,
    backend_choice: Option<BackendChoice>,
    command: ZoomCommand,
    dry_run: bool,
    yes: bool,
) -> Result<(), LinkError> {
    ensure_standard_backend(config, backend_choice)?;
    match command {
        ZoomCommand::Get => run_zoom_get(config),
        ZoomCommand::Set { factor } => {
            let factor = parse_zoom_factor(&factor, false)?;
            run_semantic_builder(config, dry_run, yes, "zoom.set", move |backend| {
                let control = backend.resolve("zoom_absolute")?;
                let raw = zoom_factor_to_raw(&control, factor)?;
                Ok(vec![raw_request(&control.id.to_string(), raw)])
            })
        }
        ZoomCommand::Step { delta } => {
            let delta = parse_zoom_factor(&delta, true)?;
            run_semantic_builder(config, dry_run, yes, "zoom.step", move |backend| {
                let control = backend.resolve("zoom_absolute")?;
                let current = backend.get(control.id)?.1.raw;
                let factor = current as f64 / 100.0 + delta;
                let raw = zoom_factor_to_raw(&control, factor)?;
                Ok(vec![raw_request(&control.id.to_string(), raw)])
            })
        }
        ZoomCommand::Reset => {
            run_semantic_builder(config, dry_run, yes, "zoom.reset", move |backend| {
                let control = backend.resolve("zoom_absolute")?;
                if !control.default_is_valid {
                    return Err(LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "the zoom control has no valid driver default",
                    ));
                }
                Ok(vec![raw_request(&control.id.to_string(), control.default)])
            })
        }
        ZoomCommand::Ramp { from, to, duration } => {
            let from = parse_zoom_factor(&from, false)?;
            let to = parse_zoom_factor(&to, false)?;
            run_zoom_ramp(config, from, to, duration.get(), dry_run, yes)
        }
    }
}

fn run_zoom_get(config: &Config) -> Result<(), LinkError> {
    let devices = selected_devices(config, true)?;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        let node = control_node(device, config.default_device.as_deref())?;
        let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
        let control = backend.resolve("zoom_absolute")?;
        let (_, value) = backend.get(control.id)?;
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: zoom_status(&control, value.raw),
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{:.2}x", result.result.factor);
        }
        Ok(())
    } else {
        emit_success(config.output, "zoom.get", None, &results)
    }
}

fn run_zoom_ramp(
    config: &Config,
    from: f64,
    to: f64,
    duration: Duration,
    dry_run: bool,
    yes: bool,
) -> Result<(), LinkError> {
    const TICK: Duration = Duration::from_millis(50);
    const MAX_DURATION: Duration = Duration::from_secs(60);
    if duration < TICK || duration > MAX_DURATION {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "zoom ramp duration must be between 50ms and 60s",
        ));
    }
    require_all_confirmation(config, yes)?;
    let devices = selected_devices(config, true)?;
    let ticks_u128 = duration.as_millis().div_ceil(TICK.as_millis());
    let ticks = usize::try_from(ticks_u128).map_err(|_| {
        LinkError::new(
            ErrorKind::InvalidInvocation,
            "zoom ramp duration is too large",
        )
    })?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for device in &devices {
        let outcome = (|| {
            let stable_id = device.identity.stable_id();
            let _lease = acquire_operation_lease(config, &stable_id, "zoom.ramp")?;
            let node = control_node(device, config.default_device.as_deref())?;
            let read_backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
            let control = read_backend.resolve("zoom_absolute")?;
            let previous_raw = read_backend.get(control.id)?.1.raw;
            let previous = zoom_status(&control, previous_raw);
            let from_raw = zoom_factor_to_raw(&control, from)?;
            let to_raw = zoom_factor_to_raw(&control, to)?;
            let raw_at_tick = |tick: usize| {
                let fraction = tick as f64 / ticks as f64;
                let interpolated = from_raw as f64 + (to_raw - from_raw) as f64 * fraction;
                let step = i64::try_from(control.step.max(1)).unwrap_or(i64::MAX);
                control.minimum + ((interpolated.round() as i64 - control.minimum) / step) * step
            };
            let mut planned_writes = 0_usize;
            let mut last_raw = Some(previous_raw);
            for tick in 0..=ticks {
                let raw = raw_at_tick(tick);
                if last_raw != Some(raw) {
                    planned_writes += 1;
                    last_raw = Some(raw);
                }
            }
            if dry_run {
                return Ok(ZoomRampResult {
                    previous,
                    requested_from: from,
                    requested_to: to,
                    observed: None,
                    writes: planned_writes,
                    duration_ms: duration.as_millis() as u64,
                    verified: false,
                    dry_run: true,
                    rollback: RollbackReport::default(),
                });
            }
            let backend = link_v4l2::production::ControlDevice::open_write(&node.path)?;
            let mut writes = 0_usize;
            let result = (|| {
                let started = Instant::now();
                let mut last_raw = Some(previous_raw);
                for tick in 0..=ticks {
                    let raw = raw_at_tick(tick);
                    if last_raw != Some(raw) {
                        backend.set(&control, raw)?;
                        writes += 1;
                        last_raw = Some(raw);
                    }
                    if tick < ticks {
                        let target_elapsed = duration.mul_f64((tick + 1) as f64 / ticks as f64);
                        if let Some(remaining) = target_elapsed.checked_sub(started.elapsed()) {
                            thread::sleep(remaining);
                        }
                    }
                }
                let (_, observed_value) = backend.get(control.id)?;
                if observed_value.raw != to_raw {
                    return Err(LinkError::new(
                        ErrorKind::IoFailure,
                        "zoom ramp failed final readback verification",
                    )
                    .with_detail("requested", to_raw)
                    .with_detail("observed", observed_value.raw));
                }
                Ok(zoom_status(&control, observed_value.raw))
            })();
            match result {
                Ok(observed) => Ok(ZoomRampResult {
                    previous,
                    requested_from: from,
                    requested_to: to,
                    observed: Some(observed),
                    writes,
                    duration_ms: duration.as_millis() as u64,
                    verified: true,
                    dry_run: false,
                    rollback: RollbackReport::default(),
                }),
                Err(error) => {
                    let mut rollback = RollbackReport {
                        attempted: true,
                        ..RollbackReport::default()
                    };
                    if backend.set(&control, previous_raw).is_ok()
                        && backend
                            .get(control.id)
                            .is_ok_and(|(_, value)| value.raw == previous_raw)
                    {
                        rollback.restored.push("zoom".into());
                    } else {
                        rollback.failed.push("zoom".into());
                    }
                    Err(error.with_detail("rollback", json!(rollback)))
                }
            }
        })();
        match outcome {
            Ok(result) => results.push(PerDeviceResult {
                device: device_summary(device),
                result,
            }),
            Err(error) => failures.push(device_failure(device, &error)),
        }
    }
    if failures.is_empty() {
        if config.output == OutputFormat::Human {
            for result in &results {
                println!(
                    "{:.2}x -> {:.2}x ({} write(s){})",
                    result.result.requested_from,
                    result.result.requested_to,
                    result.result.writes,
                    if result.result.dry_run {
                        ", dry-run"
                    } else {
                        ""
                    }
                );
            }
            Ok(())
        } else {
            emit_success(config.output, "zoom.ramp", None, &results)
        }
    } else {
        let kind = if results.is_empty()
            && failures
                .iter()
                .all(|failure| failure.kind == failures[0].kind)
        {
            failures[0].kind
        } else {
            ErrorKind::PartialSuccess
        };
        Err(LinkError::new(kind, "one or more zoom ramps failed")
            .with_detail(
                "successes",
                serde_json::to_value(&results).unwrap_or_default(),
            )
            .with_detail(
                "failures",
                Value::Array(failures.into_iter().map(|failure| failure.value).collect()),
            ))
    }
}

fn zoom_status(control: &ControlDescriptor, raw: i64) -> ZoomStatusResult {
    ZoomStatusResult {
        factor: raw as f64 / 100.0,
        raw,
        minimum: control.minimum as f64 / 100.0,
        maximum: control.maximum as f64 / 100.0,
        step: control.step as f64 / 100.0,
        backend: "v4l2",
    }
}

fn parse_zoom_factor(input: &str, signed: bool) -> Result<f64, LinkError> {
    let value = input
        .trim()
        .trim_end_matches(['x', 'X'])
        .parse::<f64>()
        .map_err(|_| invalid_zoom_value(input))?;
    if !value.is_finite() || (!signed && value <= 0.0) {
        return Err(invalid_zoom_value(input));
    }
    Ok(value)
}

fn zoom_factor_to_raw(control: &ControlDescriptor, factor: f64) -> Result<i64, LinkError> {
    let raw = (factor * 100.0).round() as i64;
    validate_semantic_raw(control, raw)?;
    let step = i64::try_from(control.step.max(1)).unwrap_or(i64::MAX);
    if (raw - control.minimum) % step != 0 {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "zoom factor does not align with the advertised device step",
        )
        .with_detail("factor", factor)
        .with_detail("step", control.step));
    }
    Ok(raw)
}

fn invalid_zoom_value(input: &str) -> LinkError {
    LinkError::new(
        ErrorKind::InvalidInvocation,
        "zoom values must be finite factors such as 1.5x",
    )
    .with_detail("value", input.to_owned())
}

#[derive(Serialize)]
struct NativeStatusResult {
    capabilities: BTreeMap<String, CapabilityRecord>,
}

fn run_native_status(
    config: &Config,
    command: &'static str,
    names: &[&str],
) -> Result<(), LinkError> {
    let devices = selected_devices(config, true)?;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        let all = native_capabilities_for(config, device, Some(names))?;
        let capabilities = names
            .iter()
            .filter_map(|name| {
                all.semantic
                    .get(*name)
                    .cloned()
                    .map(|capability| ((*name).to_owned(), capability))
            })
            .collect();
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: NativeStatusResult { capabilities },
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{} ({})", result.device.model, result.device.stable_id);
            for (name, capability) in &result.result.capabilities {
                println!(
                    "{name}\t{:?}\t{}",
                    capability.state,
                    capability
                        .current
                        .as_ref()
                        .map_or_else(|| "-".into(), Value::to_string)
                );
            }
        }
        Ok(())
    } else {
        emit_success(config.output, command, None, &results)
    }
}

fn native_profile_control<'a>(
    catalog: &'a ProfileCatalog,
    device: &DiscoveredDevice,
    firmware: Option<&str>,
    name: &str,
) -> Result<
    (
        &'a link_profiles::CatalogProfile,
        &'a link_profiles::ProfileControl,
    ),
    LinkError,
> {
    let entry = catalog
        .matching(&device.identity, device.mode(), firmware)?
        .ok_or_else(|| native_unmapped_error(name, "no exact profile matches the device"))?;
    let control = entry.profile.control(name).ok_or_else(|| {
        native_unmapped_error(
            name,
            "the matching profile has no verified semantic mapping",
        )
    })?;
    Ok((entry, control))
}

fn run_native_vendor_set(
    config: &Config,
    command: &'static str,
    control_name: &str,
    value: &str,
    dry_run: bool,
) -> Result<(), LinkError> {
    let (device, _, inventory, catalog, session) = selected_xu_context(config, false)?;
    let (entry, firmware) = resolve_firmware_profile(&device, &inventory, &catalog, &session)?;
    let entry = entry.ok_or_else(|| {
        native_unmapped_error(
            control_name,
            &format!(
                "no verified profile matches firmware {}",
                firmware.as_deref().unwrap_or("unknown")
            ),
        )
    })?;
    let _ = entry.profile.control(control_name).ok_or_else(|| {
        native_unmapped_error(
            control_name,
            "the matching profile has no verified semantic mapping",
        )
    })?;
    if !entry.semantic_write_authorized() {
        return Err(native_unmapped_error(
            control_name,
            "the mapping is not a trusted built-in verified profile control",
        ));
    }
    run_xu_semantic_set(config, command, control_name, value, dry_run)
}

fn run_native_vendor_transaction(
    config: &Config,
    command: &'static str,
    writes: &[(&str, &str)],
    dry_run: bool,
) -> Result<(), LinkError> {
    let (device, _, inventory, catalog, session) = selected_xu_context(config, false)?;
    let (entry, firmware) = resolve_firmware_profile(&device, &inventory, &catalog, &session)?;
    let entry = entry.ok_or_else(|| {
        native_unmapped_error(
            writes.first().map_or("vendor-control", |(name, _)| *name),
            &format!(
                "no verified profile matches firmware {}",
                firmware.as_deref().unwrap_or("unknown")
            ),
        )
    })?;
    for (control_name, _) in writes {
        let _ = entry.profile.control(control_name).ok_or_else(|| {
            native_unmapped_error(
                control_name,
                "the matching profile has no verified semantic mapping",
            )
        })?;
    }
    if !entry.semantic_write_authorized() {
        return Err(native_unmapped_error(
            writes.first().map_or("vendor-control", |(name, _)| *name),
            "the mapping is not a trusted built-in verified profile control",
        ));
    }
    run_xu_semantic_transaction(config, command, writes, dry_run)
}

fn read_native_vendor_value(config: &Config, control_name: &str) -> Result<Value, LinkError> {
    let (device, node, inventory, catalog, session) = selected_xu_context(config, false)?;
    let (_, firmware) = resolve_firmware_profile(&device, &inventory, &catalog, &session)?;
    let (entry, control) =
        native_profile_control(&catalog, &device, firmware.as_deref(), control_name)?;
    if !control.readable {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "the verified semantic control is not readable",
        )
        .with_detail("capability", control_name.to_owned()));
    }
    drop(session);
    #[cfg(feature = "gstreamer")]
    let _media_lease = if control.stream_requirement == StreamRequirement::Open {
        Some(link_media::MediaLease::acquire(
            &device.identity.stable_id(),
            "native-value-read",
        )?)
    } else {
        None
    };
    let _read_stream =
        prepare_xu_read_stream(control.stream_requirement, &node.path, config.timeout.get())?;
    let session = link_uvc_xu::XuSession::open_read(Path::new(&node.path))?;
    let value = link_uvc_xu::read_value(
        &session,
        &inventory,
        Some(&control.entity_guid),
        None,
        control.selector,
        Some(control.length),
        Some(&entry.profile),
    )?;
    value.decoded.get(control_name).cloned().ok_or_else(|| {
        LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "the matched profile did not decode its semantic control",
        )
        .with_detail("capability", control_name.to_owned())
    })
}

fn native_unmapped_error(capability: &str, evidence: &str) -> LinkError {
    LinkError::new(
        ErrorKind::CapabilityUnsupported,
        "camera-native capability is not verified for the selected device",
    )
    .with_detail("capability", capability.to_owned())
    .with_detail("state", "discovered-unmapped")
    .with_detail("evidence", evidence.to_owned())
}

fn validate_normalized_coordinate(name: &'static str, value: f64) -> Result<(), LinkError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "frame coordinates must be finite values from 0.0 to 1.0",
        )
        .with_detail("coordinate", name)
        .with_detail("value", value))
    }
}

fn run_frame(config: &Config, command: FrameCommand, dry_run: bool) -> Result<(), LinkError> {
    match command {
        FrameCommand::Status => run_native_status(config, "frame.status", &["frame.position"]),
        FrameCommand::Set { x, y } => {
            validate_normalized_coordinate("x", x)?;
            validate_normalized_coordinate("y", y)?;
            run_native_vendor_set(
                config,
                "frame.set",
                "frame.position",
                &format!("x={x},y={y}"),
                dry_run,
            )
        }
        FrameCommand::Move { x, y } => {
            if !x.is_finite() || !y.is_finite() {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "frame deltas must be finite",
                ));
            }
            let current = read_native_vendor_value(config, "frame.position")?;
            let current_x = current.get("x").and_then(Value::as_f64).ok_or_else(|| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "frame profile did not decode an x coordinate",
                )
            })?;
            let current_y = current.get("y").and_then(Value::as_f64).ok_or_else(|| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "frame profile did not decode a y coordinate",
                )
            })?;
            let target_x = current_x + x;
            let target_y = current_y + y;
            validate_normalized_coordinate("x", target_x)?;
            validate_normalized_coordinate("y", target_y)?;
            run_native_vendor_set(
                config,
                "frame.move",
                "frame.position",
                &format!("x={target_x},y={target_y}"),
                dry_run,
            )
        }
        FrameCommand::Center => run_native_vendor_set(
            config,
            "frame.center",
            "frame.position",
            "x=0.5,y=0.5",
            dry_run,
        ),
    }
}

fn run_auto_framing(
    config: &Config,
    command: AutoFramingCommand,
    dry_run: bool,
) -> Result<(), LinkError> {
    match command {
        AutoFramingCommand::Status => run_native_status(
            config,
            "auto-framing.status",
            &["auto-framing.enabled", "auto-framing.style"],
        ),
        AutoFramingCommand::On => run_native_vendor_set(
            config,
            "auto-framing.on",
            "auto-framing.enabled",
            "on",
            dry_run,
        ),
        AutoFramingCommand::Off => run_native_vendor_set(
            config,
            "auto-framing.off",
            "auto-framing.enabled",
            "off",
            dry_run,
        ),
        AutoFramingCommand::Style { style } => run_native_vendor_transaction(
            config,
            "auto-framing.style",
            &[
                ("auto-framing.smart-composition", "on"),
                (
                    "auto-framing.style",
                    match style {
                        AutoFramingStyle::Head => "head",
                        AutoFramingStyle::HalfBody => "half-body",
                    },
                ),
            ],
            dry_run,
        ),
    }
}

fn run_mode(config: &Config, command: ModeCommand, dry_run: bool) -> Result<(), LinkError> {
    match command {
        ModeCommand::Normal => {
            run_native_vendor_set(config, "mode.normal", "mode.normal", "on", dry_run)
        }
        ModeCommand::Whiteboard { command } => match command {
            ToggleStatusCommand::Status => {
                run_native_status(config, "mode.whiteboard", &["mode.whiteboard"])
            }
            ToggleStatusCommand::On => {
                run_native_vendor_set(config, "mode.whiteboard", "mode.whiteboard", "on", dry_run)
            }
            ToggleStatusCommand::Off => {
                run_native_vendor_set(config, "mode.whiteboard", "mode.whiteboard", "off", dry_run)
            }
        },
        ModeCommand::Compatibility { command } => match command {
            CompatibilityCommand::Status => {
                run_native_status(config, "mode.compatibility", &["mode.compatibility"])
            }
            CompatibilityCommand::Set { value } => run_native_vendor_set(
                config,
                "mode.compatibility",
                "mode.compatibility",
                match value {
                    CompatibilityMode::Standard => "standard",
                    CompatibilityMode::LowResolution => "low-resolution",
                    CompatibilityMode::Yuy2 => "yuy2",
                },
                dry_run,
            ),
        },
    }
}

fn run_gesture(config: &Config, command: GestureCommand, dry_run: bool) -> Result<(), LinkError> {
    match command {
        GestureCommand::Status => run_native_status(
            config,
            "gesture.status",
            &[
                "gesture.enabled",
                "gesture.palm",
                "gesture.v-sign",
                "gesture.l-sign",
            ],
        ),
        GestureCommand::Enable => {
            run_native_vendor_set(config, "gesture.enable", "gesture.enabled", "on", dry_run)
        }
        GestureCommand::Disable => {
            run_native_vendor_set(config, "gesture.disable", "gesture.enabled", "off", dry_run)
        }
        GestureCommand::Set { gesture, action } => {
            let (control, expected) = match gesture {
                GestureKind::Palm => ("gesture.palm", GestureAction::AutoFraming),
                GestureKind::VSign => ("gesture.v-sign", GestureAction::Whiteboard),
                GestureKind::LSign => ("gesture.l-sign", GestureAction::Zoom),
            };
            let value = if matches!(action, GestureAction::Disabled) {
                "off"
            } else if std::mem::discriminant(&action) == std::mem::discriminant(&expected) {
                "on"
            } else {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "camera gestures cannot be rebound to a different action",
                ));
            };
            run_native_vendor_set(config, "gesture.set", control, value, dry_run)
        }
    }
}

fn run_portrait(config: &Config, command: PortraitCommand, dry_run: bool) -> Result<(), LinkError> {
    match command {
        PortraitCommand::Status => run_native_status(
            config,
            "portrait.status",
            &[
                "portrait.native",
                "portrait.horizontal-correction",
                "portrait.vertical-correction",
            ],
        ),
        PortraitCommand::Native { command } => match command {
            PortraitNativeCommand::Disable => {
                run_native_vendor_set(config, "portrait.native", "portrait.native", "off", dry_run)
            }
            PortraitNativeCommand::Enable {
                horizontal_correction,
                vertical_correction,
            } => {
                if horizontal_correction.is_some() || vertical_correction.is_some() {
                    return Err(LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "portrait correction options require verified correction mappings",
                    )
                    .with_detail("state", "discovered-unmapped"));
                }
                run_native_vendor_set(config, "portrait.native", "portrait.native", "on", dry_run)
            }
        },
    }
}

#[derive(Serialize)]
struct PrivacyStatusResult {
    physical_shutter: &'static str,
    video_streaming: bool,
    audio_hardware_muted: Option<bool>,
    audio_host_muted: Option<bool>,
    logical_privacy: bool,
    physical_status_source: &'static str,
}

fn run_privacy(config: &Config, command: PrivacyCommand) -> Result<(), LinkError> {
    match command {
        PrivacyCommand::Status => {
            let (device, _) = selected_video_device(config)?;
            let (audio, _) = discover_audio()?;
            let endpoint = audio.endpoints.iter().find(|endpoint| {
                endpoint.associated_camera.as_deref() == Some(&device.identity.stable_id())
                    && endpoint.direction == link_core::audio::AudioDirection::Capture
            });
            let state = endpoint.and_then(|endpoint| {
                audio
                    .states
                    .iter()
                    .find(|state| state.endpoint_id == endpoint.id)
            });
            let result = PrivacyStatusResult {
                physical_shutter: "unknown",
                video_streaming: false,
                audio_hardware_muted: state
                    .and_then(|state| state.hardware.as_ref())
                    .and_then(|state| state.muted),
                audio_host_muted: state
                    .and_then(|state| state.host.as_ref())
                    .and_then(|state| state.muted),
                logical_privacy: false,
                physical_status_source: "no verified hardware readback",
            };
            if config.output == OutputFormat::Human {
                println!("physical shutter: unknown (manual)");
                println!("video streaming: false (no daemon ownership)");
                println!(
                    "audio hardware muted: {}",
                    result
                        .audio_hardware_muted
                        .map_or_else(|| "unknown".into(), |value| value.to_string())
                );
                Ok(())
            } else {
                emit_success(
                    config.output,
                    "privacy.status",
                    Some(device_summary(&device)),
                    &result,
                )
            }
        }
    }
}

fn run_firmware(config: &Config, command: FirmwareCommand) -> Result<(), LinkError> {
    match command {
        FirmwareCommand::Info => run_native_status(
            config,
            "firmware.info",
            &["firmware.version", "hardware.version"],
        ),
    }
}

#[derive(Serialize)]
struct DeviceStateResult {
    availability: DeviceState,
    usb_mode: DeviceMode,
    daemon_owns_stream: bool,
    mode: CapabilityRecord,
    error: CapabilityRecord,
    indicator: CapabilityRecord,
}

fn run_device_state(config: &Config) -> Result<(), LinkError> {
    const NAMES: &[&str] = &[
        "device.mode-state",
        "device.error-state",
        "device.indicator-state",
    ];
    let devices = selected_devices(config, true)?;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        let mut capabilities = native_capabilities_for(config, device, Some(NAMES))?.semantic;
        let mode = capabilities
            .remove("device.mode-state")
            .expect("declared capability");
        let error = capabilities
            .remove("device.error-state")
            .expect("declared capability");
        let indicator = capabilities
            .remove("device.indicator-state")
            .expect("declared capability");
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: DeviceStateResult {
                availability: link_linux::availability_state(device),
                usb_mode: device.mode(),
                daemon_owns_stream: false,
                mode,
                error,
                indicator,
            },
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!(
                "{}\t{:?}\t{:?}",
                result.device.stable_id, result.result.availability, result.result.usb_mode
            );
        }
        Ok(())
    } else {
        emit_success(config.output, "device.state", None, &results)
    }
}

fn run_device_info(config: &Config, include_serial: bool) -> Result<(), LinkError> {
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let devices = selected_devices(config, true)?;
    let include_serial = include_serial || !config.safety.redact_serials;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        let mut inventory = build_probe(device, &catalog, include_serial)?;
        inventory.issues.extend(device.issues.clone());
        let controls = if device.mode() == DeviceMode::Camera {
            let node = control_node(device, config.default_device.as_deref())?;
            let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
            control_capabilities(device, backend.controls()?)?
        } else {
            control_capabilities(device, Vec::new())?
        };
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: DeviceInfoResult {
                inventory,
                controls,
                state: link_linux::availability_state(device),
                owner: None,
                daemon_owns_stream: false,
            },
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!(
                "Device: {} ({})",
                result.device.model, result.device.stable_id
            );
            println!("State: {:?}", result.result.state);
            println!("Capture nodes: {}", result.result.inventory.video.len());
            println!("Raw controls: {}", result.result.controls.raw.len());
            println!(
                "Profile: {}",
                result
                    .result
                    .inventory
                    .profile
                    .profile_id
                    .as_deref()
                    .unwrap_or("unmatched")
            );
        }
    } else {
        emit_success(config.output, "device.info", None, &results)?;
    }
    Ok(())
}

fn run_caps_controls(
    config: &Config,
    backend_choice: Option<BackendChoice>,
) -> Result<(), LinkError> {
    ensure_standard_backend(config, backend_choice)?;
    let devices = selected_devices(config, true)?;
    let mut results = Vec::with_capacity(devices.len());
    for device in &devices {
        let node = control_node(device, config.default_device.as_deref())?;
        let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: control_capabilities(device, backend.controls()?)?,
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{} ({})", result.device.model, result.device.stable_id);
            for (name, capability) in &result.result.semantic {
                println!(
                    "{name}\t{:?}\t{}",
                    capability.state,
                    capability.backend.as_deref().unwrap_or("-")
                );
            }
        }
    } else {
        emit_success(config.output, "caps.controls", None, &results)?;
    }
    Ok(())
}

#[derive(Clone)]
enum RequestedValue {
    Generic(String),
    Raw(i64),
}

#[derive(Clone)]
struct ControlRequest {
    selector: String,
    value: RequestedValue,
}

#[derive(Clone)]
struct PreparedWrite {
    descriptor: ControlDescriptor,
    value: ControlValue,
    prerequisite: bool,
}

fn run_control(
    config: &Config,
    backend_choice: Option<BackendChoice>,
    command: ControlCommand,
    dry_run: bool,
    yes: bool,
) -> Result<(), LinkError> {
    ensure_standard_backend(config, backend_choice)?;
    match command {
        ControlCommand::List => run_control_list(config),
        ControlCommand::Get { control } => run_control_get(config, &control),
        ControlCommand::Set {
            control,
            value,
            batch,
            raw,
            clamp,
            fallback_individual,
        } => {
            let (requests, batched) = parse_control_requests(control, value, batch)?;
            run_control_mutation(
                config,
                requests,
                raw,
                clamp,
                fallback_individual,
                batched,
                dry_run,
                yes,
                "control.set",
            )
        }
        ControlCommand::Reset { control, raw } => {
            let devices = selected_devices(config, true)?;
            require_all_confirmation(config, yes)?;
            let mut results = Vec::new();
            let mut failures = Vec::new();
            for device in &devices {
                let outcome = (|| {
                    let stable_id = device.identity.stable_id();
                    let _lease = acquire_operation_lease(config, &stable_id, "control.reset")?;
                    let node = control_node(device, config.default_device.as_deref())?;
                    let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
                    let descriptor = backend.resolve(&control)?;
                    if !descriptor.default_is_valid {
                        return Err(LinkError::new(
                            ErrorKind::CapabilityUnsupported,
                            "driver-advertised default is invalid and will not be written",
                        )
                        .with_detail("control", descriptor.name)
                        .with_detail("default", descriptor.default)
                        .with_detail("minimum", descriptor.minimum)
                        .with_detail("maximum", descriptor.maximum));
                    }
                    let request = ControlRequest {
                        selector: descriptor.id.to_string(),
                        value: RequestedValue::Raw(descriptor.default),
                    };
                    execute_requests(
                        device,
                        config,
                        vec![request],
                        raw,
                        false,
                        false,
                        false,
                        dry_run,
                    )
                })();
                match outcome {
                    Ok(report) => results.push(PerDeviceResult {
                        device: device_summary(device),
                        result: report,
                    }),
                    Err(error) => failures.push(device_failure(device, &error)),
                }
            }
            finish_mutation(config, "control.reset", results, failures)
        }
        ControlCommand::Watch { controls } => run_control_watch(config, controls),
    }
}

fn run_control_list(config: &Config) -> Result<(), LinkError> {
    let devices = selected_devices(config, true)?;
    let mut results = Vec::new();
    for device in &devices {
        let node = control_node(device, config.default_device.as_deref())?;
        let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: backend.controls()?,
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{} ({})", result.device.model, result.device.stable_id);
            println!("ID\tNAME\tTYPE\tCURRENT\tDEFAULT\tRANGE\tFLAGS");
            for control in &result.result {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}..{} step {}\t{}",
                    control.id_hex,
                    control.name,
                    control.control_type_name,
                    control
                        .current
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    control.default,
                    control.minimum,
                    control.maximum,
                    control.step,
                    control.flag_names.join(",")
                );
            }
        }
    } else {
        emit_success(config.output, "control.list", None, &results)?;
    }
    Ok(())
}

fn run_control_get(config: &Config, selector: &str) -> Result<(), LinkError> {
    let devices = selected_devices(config, true)?;
    let mut results = Vec::new();
    for device in &devices {
        let node = control_node(device, config.default_device.as_deref())?;
        let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
        let descriptor = backend.resolve(selector)?;
        let (control, value) = backend.get(descriptor.id)?;
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: ControlGetResult { control, value },
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            let value = &result.result.value;
            let semantic = value.label.clone().or_else(|| {
                value
                    .normalized
                    .map(|normalized| format!("{:.1}%", normalized * 100.0))
            });
            println!(
                "{}: {} = {}{}",
                result.device.stable_id,
                result.result.control.name,
                value.raw,
                semantic.map_or_else(String::new, |value| format!(" ({value})"))
            );
        }
    } else {
        emit_success(config.output, "control.get", None, &results)?;
    }
    Ok(())
}

fn parse_control_requests(
    control: Option<String>,
    value: Option<String>,
    batch: Vec<String>,
) -> Result<(Vec<ControlRequest>, bool), LinkError> {
    if !batch.is_empty() {
        if control.is_some() || value.is_some() {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "single-control arguments cannot be combined with --batch",
            ));
        }
        let requests = batch
            .into_iter()
            .map(|entry| {
                let (selector, value) = entry.split_once('=').ok_or_else(|| {
                    LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "batch entries must use CONTROL=VALUE",
                    )
                    .with_detail("entry", entry.clone())
                })?;
                if selector.is_empty() || value.is_empty() {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "batch control and value must not be empty",
                    )
                    .with_detail("entry", entry));
                }
                Ok(ControlRequest {
                    selector: selector.to_owned(),
                    value: RequestedValue::Generic(value.to_owned()),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((requests, true));
    }
    match (control, value) {
        (Some(selector), Some(value)) => Ok((
            vec![ControlRequest {
                selector,
                value: RequestedValue::Generic(value),
            }],
            false,
        )),
        _ => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "control set requires CONTROL VALUE or --batch CONTROL=VALUE...",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_control_mutation(
    config: &Config,
    requests: Vec<ControlRequest>,
    raw: bool,
    clamp: bool,
    fallback_individual: bool,
    batched: bool,
    dry_run: bool,
    yes: bool,
    command: &'static str,
) -> Result<(), LinkError> {
    require_all_confirmation(config, yes)?;
    let devices = selected_devices(config, true)?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for device in &devices {
        let outcome = (|| {
            let stable_id = device.identity.stable_id();
            let _lease = acquire_operation_lease(config, &stable_id, command)?;
            execute_requests(
                device,
                config,
                requests.clone(),
                raw,
                clamp,
                fallback_individual,
                batched,
                dry_run,
            )
        })();
        match outcome {
            Ok(report) => results.push(PerDeviceResult {
                device: device_summary(device),
                result: report,
            }),
            Err(error) => failures.push(device_failure(device, &error)),
        }
    }
    finish_mutation(config, command, results, failures)
}

fn require_all_confirmation(config: &Config, yes: bool) -> Result<(), LinkError> {
    if config.default_device.as_deref() == Some("all") && !yes {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "mutating --device all operations require --yes",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_requests(
    device: &DiscoveredDevice,
    config: &Config,
    requests: Vec<ControlRequest>,
    raw: bool,
    clamp: bool,
    fallback_individual: bool,
    batched: bool,
    dry_run: bool,
) -> Result<ControlSetReport, LinkError> {
    SafetyPolicy::new(config.safety.clone()).authorize(Operation::StandardControlWrite)?;
    let node = control_node(device, config.default_device.as_deref())?;
    let reader = link_v4l2::production::ControlDevice::open_read(&node.path)?;
    let mut prepared = Vec::new();
    let mut prerequisite_ids = Vec::new();
    for request in requests {
        let descriptor = reader.resolve(&request.selector)?;
        if !raw {
            for (parent_id, manual_value) in
                link_v4l2::production::manual_dependencies(descriptor.id)
            {
                if prerequisite_ids.contains(&parent_id) {
                    continue;
                }
                let Ok(parent) = reader.query(parent_id) else {
                    continue;
                };
                let current = parent.current;
                if current != Some(manual_value) {
                    prepared.push(PreparedWrite {
                        value: link_v4l2::production::render_value(&parent, manual_value),
                        descriptor: parent,
                        prerequisite: true,
                    });
                }
                prerequisite_ids.push(parent_id);
            }
        }
        let value = match request.value {
            RequestedValue::Generic(input) => {
                link_v4l2::production::parse_value(&descriptor, &input, clamp)?
            }
            RequestedValue::Raw(raw) => {
                link_v4l2::production::validate_raw_value(&descriptor, raw)?;
                link_v4l2::production::render_value(&descriptor, raw)
            }
        };
        link_v4l2::production::validate_raw_value(&descriptor, value.raw)?;
        prepared.push(PreparedWrite {
            descriptor,
            value,
            prerequisite: false,
        });
    }
    execute_prepared(&node.path, prepared, fallback_individual, batched, dry_run)
}

fn execute_prepared(
    path: &str,
    mut prepared: Vec<PreparedWrite>,
    fallback_individual: bool,
    batched: bool,
    dry_run: bool,
) -> Result<ControlSetReport, LinkError> {
    if prepared.is_empty() {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "no writable standard controls matched the request",
        ));
    }
    let reader = link_v4l2::production::ControlDevice::open_read(path)?;
    let mut previous = Vec::with_capacity(prepared.len());
    for write in &prepared {
        previous.push(reader.get(write.descriptor.id).ok().map(|(_, value)| value));
    }
    if dry_run {
        return Ok(ControlSetReport {
            changes: prepared
                .into_iter()
                .zip(previous)
                .map(|(write, previous)| ControlChangeReport {
                    control: write.descriptor,
                    previous: previous.clone(),
                    requested: write.value,
                    applied: None,
                    observed: previous,
                    verified: false,
                    prerequisite: write.prerequisite,
                })
                .collect(),
            dry_run: true,
            batched,
            individual_fallback_used: false,
            error_index: None,
            rollback: RollbackReport::default(),
        });
    }
    let writer = link_v4l2::production::ControlDevice::open_write(path)?;
    for write in prepared.iter().filter(|write| write.prerequisite) {
        if let Err(error) = writer.set(&write.descriptor, write.value.raw) {
            let rollback = rollback_controls(&writer, &prepared, &previous);
            return Err(write_error_with_rollback(error, None, &rollback));
        }
    }
    for write in prepared.iter_mut().filter(|write| !write.prerequisite) {
        match writer.query(write.descriptor.id) {
            Ok(descriptor) if descriptor.available => write.descriptor = descriptor,
            Ok(descriptor) => {
                let error = LinkError::new(
                    ErrorKind::CapabilityUnsupported,
                    "V4L2 control remained unavailable after changing its prerequisite",
                )
                .with_detail("control", descriptor.name);
                let rollback = rollback_controls(&writer, &prepared, &previous);
                return Err(write_error_with_rollback(error, None, &rollback));
            }
            Err(error) => {
                let rollback = rollback_controls(&writer, &prepared, &previous);
                return Err(write_error_with_rollback(error, None, &rollback));
            }
        }
    }
    let raw_writes = prepared
        .iter()
        .filter(|write| !write.prerequisite)
        .map(|write| link_v4l2::production::RawControlWrite {
            descriptor: write.descriptor.clone(),
            value: write.value.raw,
        })
        .collect::<Vec<_>>();
    let mut fallback_used = false;
    let mut error_index = None;
    let write_result = if batched || raw_writes.len() > 1 {
        match writer.set_batch(&raw_writes) {
            Ok(()) => Ok(()),
            Err(batch_error) if fallback_individual => {
                error_index = Some(batch_error.error_index);
                let rollback = rollback_controls(&writer, &prepared, &previous);
                if !rollback.failed.is_empty() {
                    return Err(partial_write_error(
                        "batch failed and rollback was incomplete",
                        Some(batch_error.error_index),
                        &rollback,
                    ));
                }
                for write in prepared.iter().filter(|write| write.prerequisite) {
                    if let Err(error) = writer.set(&write.descriptor, write.value.raw) {
                        let rollback = rollback_controls(&writer, &prepared, &previous);
                        return Err(write_error_with_rollback(error, error_index, &rollback));
                    }
                }
                fallback_used = true;
                raw_writes
                    .iter()
                    .try_for_each(|write| writer.set(&write.descriptor, write.value).map(|_| ()))
            }
            Err(batch_error) => {
                let rollback = rollback_controls(&writer, &prepared, &previous);
                return Err(write_error_with_rollback(
                    batch_error.error,
                    Some(batch_error.error_index),
                    &rollback,
                ));
            }
        }
    } else {
        writer
            .set(&raw_writes[0].descriptor, raw_writes[0].value)
            .map(|_| ())
    };
    if let Err(error) = write_result {
        let rollback = rollback_controls(&writer, &prepared, &previous);
        return Err(write_error_with_rollback(error, error_index, &rollback));
    }

    let verifier = link_v4l2::production::ControlDevice::open_read(path)?;
    let mut changes = Vec::new();
    let mut verified = true;
    for (write, previous) in prepared.iter().zip(previous.iter()) {
        let observed = verifier
            .get(write.descriptor.id)
            .ok()
            .map(|(_, value)| value);
        let matches = observed
            .as_ref()
            .is_some_and(|value| value.raw == write.value.raw)
            || !write.descriptor.readable;
        verified &= matches;
        changes.push(ControlChangeReport {
            control: verifier
                .query(write.descriptor.id)
                .unwrap_or_else(|_| write.descriptor.clone()),
            previous: previous.clone(),
            requested: write.value.clone(),
            applied: Some(write.value.clone()),
            observed,
            verified: matches,
            prerequisite: write.prerequisite,
        });
    }
    if !verified {
        let rollback = rollback_controls(&writer, &prepared, &previous);
        return Err(partial_write_error(
            "V4L2 control readback did not match the requested value",
            error_index,
            &rollback,
        ));
    }
    Ok(ControlSetReport {
        changes,
        dry_run: false,
        batched: batched || raw_writes.len() > 1,
        individual_fallback_used: fallback_used,
        error_index,
        rollback: RollbackReport::default(),
    })
}

fn rollback_controls(
    writer: &link_v4l2::production::ControlDevice,
    prepared: &[PreparedWrite],
    previous: &[Option<ControlValue>],
) -> RollbackReport {
    let mut report = RollbackReport {
        attempted: true,
        ..RollbackReport::default()
    };
    let prerequisite_ids = prepared
        .iter()
        .filter(|write| write.prerequisite)
        .map(|write| write.descriptor.id)
        .collect::<Vec<_>>();
    let mut restored_ids = Vec::new();
    for (write, previous) in prepared
        .iter()
        .zip(previous)
        .rev()
        .filter(|(write, _)| !prerequisite_ids.contains(&write.descriptor.id))
        .chain(
            prepared
                .iter()
                .zip(previous)
                .rev()
                .filter(|(write, _)| prerequisite_ids.contains(&write.descriptor.id)),
        )
    {
        if restored_ids.contains(&write.descriptor.id) {
            continue;
        }
        restored_ids.push(write.descriptor.id);
        let Some(previous) = previous else {
            report.failed.push(write.descriptor.name.clone());
            continue;
        };
        if writer
            .get(write.descriptor.id)
            .is_ok_and(|(_, current)| current.raw == previous.raw)
        {
            report.restored.push(write.descriptor.name.clone());
            continue;
        }
        match writer.set(&write.descriptor, previous.raw) {
            Ok(_) => report.restored.push(write.descriptor.name.clone()),
            Err(_) => report.failed.push(write.descriptor.name.clone()),
        }
    }
    report
}

fn write_error_with_rollback(
    error: LinkError,
    error_index: Option<u32>,
    rollback: &RollbackReport,
) -> LinkError {
    let kind = if rollback.failed.is_empty() {
        error.kind()
    } else {
        ErrorKind::PartialSuccess
    };
    let mut result = LinkError::new(kind, error.message()).with_detail(
        "rollback",
        serde_json::to_value(rollback).unwrap_or_default(),
    );
    if let Some(error_index) = error_index {
        result = result.with_detail("error_index", u64::from(error_index));
    }
    result
}

fn partial_write_error(
    message: &'static str,
    error_index: Option<u32>,
    rollback: &RollbackReport,
) -> LinkError {
    let mut error = LinkError::new(ErrorKind::PartialSuccess, message).with_detail(
        "rollback",
        serde_json::to_value(rollback).unwrap_or_default(),
    );
    if let Some(error_index) = error_index {
        error = error.with_detail("error_index", u64::from(error_index));
    }
    error
}

struct DeviceFailure {
    kind: ErrorKind,
    value: Value,
}

fn device_failure(device: &DiscoveredDevice, error: &LinkError) -> DeviceFailure {
    DeviceFailure {
        kind: error.kind(),
        value: json!({
            "device": device_summary(device),
            "error": {
                "code": error.kind().code(),
                "message": error.message(),
                "details": error.details(),
            }
        }),
    }
}

fn finish_mutation(
    config: &Config,
    command: &'static str,
    results: Vec<PerDeviceResult<ControlSetReport>>,
    failures: Vec<DeviceFailure>,
) -> Result<(), LinkError> {
    if !failures.is_empty() {
        let kind = if results.is_empty()
            && failures
                .iter()
                .all(|failure| failure.kind == failures[0].kind)
        {
            failures[0].kind
        } else {
            ErrorKind::PartialSuccess
        };
        return Err(
            LinkError::new(kind, "one or more control operations failed")
                .with_detail(
                    "successes",
                    serde_json::to_value(&results).unwrap_or_default(),
                )
                .with_detail(
                    "failures",
                    Value::Array(failures.into_iter().map(|failure| failure.value).collect()),
                ),
        );
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{}:", result.device.stable_id);
            for change in &result.result.changes {
                println!(
                    "  {}: {} -> {}{}",
                    change.control.name,
                    change
                        .previous
                        .as_ref()
                        .map_or_else(|| "?".into(), |value| value.raw.to_string()),
                    change.requested.raw,
                    if result.result.dry_run {
                        " (dry run)"
                    } else {
                        ""
                    }
                );
            }
        }
    } else {
        emit_success(config.output, command, None, &results)?;
    }
    Ok(())
}

fn run_device_watch(config: &Config) -> Result<(), LinkError> {
    ensure_watch_format(config.output)?;
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let initial =
        if config.default_device.as_deref() == Some("all") || config.default_device.is_none() {
            discovered_devices()?
        } else {
            selected_devices(config, true)?
        };
    let watched_ids = initial
        .iter()
        .map(|device| device.identity.stable_id())
        .collect::<Vec<_>>();
    let watch_all = config
        .default_device
        .as_deref()
        .is_none_or(|value| value == "all");
    let mut previous = device_snapshots(&initial, &catalog)?;
    let monitor = link_linux::HotplugMonitor::new()?;
    let mut sequence = 0_u64;
    loop {
        if !monitor.wait(Duration::from_secs(1))? {
            continue;
        }
        thread::sleep(Duration::from_millis(250));
        let devices = discovered_devices()?
            .into_iter()
            .filter(|device| watch_all || watched_ids.contains(&device.identity.stable_id()))
            .collect::<Vec<_>>();
        let current = device_snapshots(&devices, &catalog)?;
        for (stable_id, old) in &previous {
            if !current.contains_key(stable_id) {
                sequence += 1;
                emit_device_event(
                    config.output,
                    &DeviceEvent {
                        sequence,
                        observed_unix_ms: now_unix_ms()?,
                        kind: "remove".into(),
                        stable_id: stable_id.clone(),
                        model: old["model"].as_str().unwrap_or("USB camera").to_owned(),
                        previous: Some(old.clone()),
                        current: None,
                    },
                )?;
            }
        }
        for (stable_id, new) in &current {
            match previous.get(stable_id) {
                None => {
                    sequence += 1;
                    emit_device_event(
                        config.output,
                        &DeviceEvent {
                            sequence,
                            observed_unix_ms: now_unix_ms()?,
                            kind: "add".into(),
                            stable_id: stable_id.clone(),
                            model: new["model"].as_str().unwrap_or("USB camera").to_owned(),
                            previous: None,
                            current: Some(new.clone()),
                        },
                    )?;
                }
                Some(old) if old != new => {
                    sequence += 1;
                    let kind = if old["profile_id"] != new["profile_id"] {
                        "profile-change"
                    } else {
                        "re-enumerate"
                    };
                    emit_device_event(
                        config.output,
                        &DeviceEvent {
                            sequence,
                            observed_unix_ms: now_unix_ms()?,
                            kind: kind.into(),
                            stable_id: stable_id.clone(),
                            model: new["model"].as_str().unwrap_or("USB camera").to_owned(),
                            previous: Some(old.clone()),
                            current: Some(new.clone()),
                        },
                    )?;
                }
                Some(_) => {}
            }
        }
        previous = current;
    }
}

fn device_snapshots(
    devices: &[DiscoveredDevice],
    catalog: &ProfileCatalog,
) -> Result<BTreeMap<String, Value>, LinkError> {
    let mut snapshots = BTreeMap::new();
    for device in devices {
        let profile = catalog.report(&device.identity, device.mode())?;
        let stable_id = device.identity.stable_id();
        snapshots.insert(
            stable_id.clone(),
            json!({
                "stable_id": stable_id,
                "model": device.model(),
                "mode": device.mode(),
                "profile_id": profile.profile_id,
                "descriptor_sha256": device.identity.descriptor_sha256,
                "video_nodes": device.video_nodes.iter().map(|node| &node.association).collect::<Vec<_>>(),
                "audio_nodes": device.sound_nodes.iter().map(|node| &node.association).collect::<Vec<_>>(),
            }),
        );
    }
    Ok(snapshots)
}

fn emit_device_event(format: OutputFormat, event: &DeviceEvent) -> Result<(), LinkError> {
    match format {
        OutputFormat::Human => {
            println!("{}\t{}\t{}", event.sequence, event.kind, event.stable_id);
            Ok(())
        }
        OutputFormat::Jsonl => emit_success(format, "device.watch", None, event),
        OutputFormat::Json => unreachable!("watch format is validated"),
    }
}

fn run_control_watch(config: &Config, selectors: Vec<String>) -> Result<(), LinkError> {
    ensure_watch_format(config.output)?;
    let devices = selected_devices(config, false)?;
    let device = &devices[0];
    let stable_id = device.identity.stable_id();
    let summary = device_summary(device);
    let node = control_node(device, config.default_device.as_deref())?;
    let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
    let selected = if selectors.is_empty() {
        backend
            .controls()?
            .into_iter()
            .filter(|control| control.readable && control.codec_supported)
            .map(|control| control.name)
            .collect::<Vec<_>>()
    } else {
        selectors
    };
    let mut previous = BTreeMap::new();
    for selector in &selected {
        let descriptor = backend.resolve(selector)?;
        let (_, value) = backend.get(descriptor.id)?;
        previous.insert(descriptor.id, (descriptor, value));
    }
    let control_ids = previous.keys().copied().collect::<Vec<_>>();
    let mut event_monitor =
        link_v4l2::production::ControlEventMonitor::new(&node.path, &control_ids).ok();
    let mut sequence = 0_u64;
    let mut missing = false;
    loop {
        let source = if let Some(monitor) = &event_monitor {
            match monitor.wait(Duration::from_secs(1)) {
                Ok(events) if events.is_empty() => continue,
                Ok(_) => "v4l2-event",
                Err(_) => {
                    event_monitor = None;
                    "poll"
                }
            }
        } else {
            thread::sleep(Duration::from_millis(250));
            "poll"
        };
        let discovered = discovered_devices()?;
        let Some(current_device) = discovered
            .iter()
            .find(|device| device.identity.stable_id() == stable_id)
        else {
            if !missing {
                missing = true;
                sequence += 1;
                emit_control_event(
                    config.output,
                    Some(summary.clone()),
                    &ControlEvent {
                        sequence,
                        observed_unix_ms: now_unix_ms()?,
                        kind: "device-removed".into(),
                        control: None,
                        previous: None,
                        current: None,
                        source: "udev-rescan".into(),
                    },
                )?;
            }
            continue;
        };
        if missing {
            missing = false;
            let reconnected_node = control_node(current_device, None)?;
            event_monitor = link_v4l2::production::ControlEventMonitor::new(
                &reconnected_node.path,
                &control_ids,
            )
            .ok();
            sequence += 1;
            emit_control_event(
                config.output,
                Some(summary.clone()),
                &ControlEvent {
                    sequence,
                    observed_unix_ms: now_unix_ms()?,
                    kind: "device-reconnected".into(),
                    control: None,
                    previous: None,
                    current: None,
                    source: "udev-rescan".into(),
                },
            )?;
        }
        let node = control_node(current_device, None)?;
        let Ok(backend) = link_v4l2::production::ControlDevice::open_read(&node.path) else {
            continue;
        };
        for selector in &selected {
            let Ok(descriptor) = backend.resolve(selector) else {
                continue;
            };
            let Ok((descriptor, current)) = backend.get(descriptor.id) else {
                continue;
            };
            let old = previous.get(&descriptor.id).map(|(_, value)| value.clone());
            if old.as_ref() != Some(&current) {
                sequence += 1;
                emit_control_event(
                    config.output,
                    Some(summary.clone()),
                    &ControlEvent {
                        sequence,
                        observed_unix_ms: now_unix_ms()?,
                        kind: "change".into(),
                        control: Some(descriptor.clone()),
                        previous: old,
                        current: Some(current.clone()),
                        source: source.into(),
                    },
                )?;
                previous.insert(descriptor.id, (descriptor, current));
            }
        }
    }
}

fn emit_control_event(
    format: OutputFormat,
    device: Option<DeviceSummary>,
    event: &ControlEvent,
) -> Result<(), LinkError> {
    match format {
        OutputFormat::Human => {
            println!(
                "{}\t{}\t{}\t{}",
                event.sequence,
                event.kind,
                event
                    .control
                    .as_ref()
                    .map_or("-", |control| control.name.as_str()),
                event
                    .current
                    .as_ref()
                    .map_or_else(|| "-".into(), |value| value.raw.to_string())
            );
            Ok(())
        }
        OutputFormat::Jsonl => emit_success(format, "control.watch", device, event),
        OutputFormat::Json => unreachable!("watch format is validated"),
    }
}

fn ensure_watch_format(format: OutputFormat) -> Result<(), LinkError> {
    if format == OutputFormat::Json {
        Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "watch commands require human or JSON Lines output",
        ))
    } else {
        Ok(())
    }
}

#[derive(Serialize)]
struct ImageStatusResult {
    capabilities: BTreeMap<String, CapabilityRecord>,
    values: BTreeMap<String, Option<Value>>,
    raw_controls: Vec<ControlDescriptor>,
}

fn run_image(
    config: &Config,
    backend_choice: Option<BackendChoice>,
    command: ImageCommand,
    dry_run: bool,
    yes: bool,
) -> Result<(), LinkError> {
    if backend_choice == Some(BackendChoice::Host) {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "host image controls require the daemon and effects pipeline",
        ));
    }
    if backend_choice == Some(BackendChoice::Vendor) {
        return match command {
            ImageCommand::Status => run_native_status(
                config,
                "image.status",
                &[
                    "image.exposure",
                    "image.exposure_compensation",
                    "image.white_balance",
                    "image.gain",
                    "image.hdr",
                    "image.mirror",
                    "image.flip",
                ],
            ),
            ImageCommand::Hdr { value } => run_native_vendor_set(
                config,
                "image.hdr",
                "image.hdr",
                if matches!(value, ToggleChoice::On) {
                    "on"
                } else {
                    "off"
                },
                dry_run,
            ),
            ImageCommand::Mirror { value } => run_native_vendor_set(
                config,
                "image.mirror",
                "image.mirror",
                if matches!(value, ToggleChoice::On) {
                    "on"
                } else {
                    "off"
                },
                dry_run,
            ),
            ImageCommand::Flip { value } => run_native_vendor_set(
                config,
                "image.flip",
                "image.flip",
                if matches!(value, ToggleChoice::On) {
                    "on"
                } else {
                    "off"
                },
                dry_run,
            ),
            _ => Err(native_unmapped_error(
                "image",
                "the requested vendor image adapter is not verified",
            )),
        };
    }
    ensure_standard_backend(config, backend_choice)?;
    match command {
        ImageCommand::Status => run_image_status(config),
        ImageCommand::Exposure { command } => match command {
            ExposureCommand::Auto => {
                run_semantic_builder(config, dry_run, yes, "image.exposure", |backend| {
                    let mut requests = Vec::new();
                    for (name, value) in [
                        ("exposure_automatic", 0),
                        ("iso_sensitivity_automatic", 1),
                        ("gain_automatic", 1),
                    ] {
                        if backend.resolve(name).is_ok() {
                            requests.push(ControlRequest {
                                selector: name.into(),
                                value: RequestedValue::Raw(value),
                            });
                        }
                    }
                    require_semantic_requests("image.exposure", requests)
                })
            }
            ExposureCommand::Manual { shutter, iso } => {
                if shutter.is_none() && iso.is_none() {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "manual exposure requires --shutter, --iso, or both",
                    ));
                }
                run_semantic_builder(config, dry_run, yes, "image.exposure", move |backend| {
                    let mut requests = Vec::new();
                    if let Some(shutter) = &shutter {
                        let descriptor = backend.resolve("exposure_time_absolute")?;
                        let raw = shutter_to_v4l2(shutter)?;
                        validate_semantic_raw(&descriptor, raw)?;
                        requests.push(ControlRequest {
                            selector: descriptor.id.to_string(),
                            value: RequestedValue::Raw(raw),
                        });
                    }
                    if let Some(iso) = iso {
                        let descriptor = backend.resolve("iso_sensitivity")?;
                        validate_semantic_raw(&descriptor, iso)?;
                        requests.push(ControlRequest {
                            selector: descriptor.id.to_string(),
                            value: RequestedValue::Raw(iso),
                        });
                    }
                    Ok(requests)
                })
            }
        },
        ImageCommand::ExposureCompensation { ev } => run_semantic_builder(
            config,
            dry_run,
            yes,
            "image.exposure-compensation",
            move |backend| {
                let descriptor = backend.resolve("exposure_compensation")?;
                let thousandths = (ev * 1000.0).round() as i64;
                let raw = if descriptor.menu.is_empty() {
                    thousandths
                } else {
                    descriptor
                        .menu
                        .iter()
                        .find(|entry| entry.label.parse::<i64>().ok() == Some(thousandths))
                        .map(|entry| entry.index)
                        .ok_or_else(|| {
                            LinkError::new(
                                ErrorKind::InvalidInvocation,
                                "requested exposure compensation is not advertised",
                            )
                            .with_detail("ev", ev)
                        })?
                };
                Ok(vec![ControlRequest {
                    selector: descriptor.id.to_string(),
                    value: RequestedValue::Raw(raw),
                }])
            },
        ),
        ImageCommand::WhiteBalance { value } => {
            if value.eq_ignore_ascii_case("auto") {
                run_semantic_requests(
                    config,
                    dry_run,
                    yes,
                    "image.white-balance",
                    vec![raw_request("white_balance_automatic", 1)],
                )
            } else {
                let kelvin = value
                    .trim()
                    .trim_end_matches(['K', 'k'])
                    .parse::<i64>()
                    .map_err(|_| {
                        LinkError::new(
                            ErrorKind::InvalidInvocation,
                            "white balance must be `auto` or a Kelvin value",
                        )
                        .with_detail("value", value.clone())
                    })?;
                run_semantic_requests(
                    config,
                    dry_run,
                    yes,
                    "image.white-balance",
                    vec![raw_request("white_balance_temperature", kelvin)],
                )
            }
        }
        ImageCommand::Focus { command } => match command {
            FocusCommand::Auto => run_semantic_requests(
                config,
                dry_run,
                yes,
                "image.focus",
                vec![raw_request("focus_automatic_continuous", 1)],
            ),
            FocusCommand::Manual(value) => {
                run_semantic_scalar(config, "image.focus", "focus_absolute", value, dry_run, yes)
            }
        },
        ImageCommand::Brightness(value) => run_semantic_scalar(
            config,
            "image.brightness",
            "brightness",
            value,
            dry_run,
            yes,
        ),
        ImageCommand::Contrast(value) => {
            run_semantic_scalar(config, "image.contrast", "contrast", value, dry_run, yes)
        }
        ImageCommand::Saturation(value) => run_semantic_scalar(
            config,
            "image.saturation",
            "saturation",
            value,
            dry_run,
            yes,
        ),
        ImageCommand::Sharpness(value) => {
            run_semantic_scalar(config, "image.sharpness", "sharpness", value, dry_run, yes)
        }
        ImageCommand::Gain(value) => {
            run_semantic_scalar(config, "image.gain", "gain", value, dry_run, yes)
        }
        ImageCommand::BacklightCompensation(value) => run_semantic_scalar(
            config,
            "image.backlight-compensation",
            "backlight_compensation",
            value,
            dry_run,
            yes,
        ),
        ImageCommand::AntiFlicker { value } => {
            let raw = match value {
                AntiFlickerChoice::Disabled => 0,
                AntiFlickerChoice::FiftyHz => 1,
                AntiFlickerChoice::SixtyHz => 2,
                AntiFlickerChoice::Auto => 3,
            };
            run_semantic_builder(config, dry_run, yes, "image.anti-flicker", move |backend| {
                let descriptor = backend.resolve("power_line_frequency")?;
                if !descriptor.menu.iter().any(|entry| entry.index == raw) {
                    return Err(LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "the requested anti-flicker mode is not advertised by the device",
                    )
                    .with_detail("requested", raw));
                }
                Ok(vec![raw_request(&descriptor.id.to_string(), raw)])
            })
        }
        ImageCommand::Hdr { value } => {
            let vendor_value = if matches!(value, ToggleChoice::On) {
                "on"
            } else {
                "off"
            };
            let standard =
                run_semantic_builder(config, dry_run, yes, "image.hdr", move |backend| {
                    let descriptor = backend.resolve("wide_dynamic_range").or_else(|error| {
                        if error.kind() == ErrorKind::CapabilityUnsupported {
                            backend.resolve("hdr_sensor_mode")
                        } else {
                            Err(error)
                        }
                    })?;
                    Ok(vec![raw_request(
                        &descriptor.id.to_string(),
                        i64::from(matches!(value, ToggleChoice::On)),
                    )])
                });
            match standard {
                Err(error)
                    if error.kind() == ErrorKind::CapabilityUnsupported
                        && matches!(backend_choice, None | Some(BackendChoice::Auto)) =>
                {
                    run_native_vendor_set(config, "image.hdr", "image.hdr", vendor_value, dry_run)
                }
                result => result,
            }
        }
        ImageCommand::Mirror { value } => {
            let vendor_value = if matches!(value, ToggleChoice::On) {
                "on"
            } else {
                "off"
            };
            let standard = run_semantic_requests(
                config,
                dry_run,
                yes,
                "image.mirror",
                vec![raw_request(
                    "horizontal_flip",
                    i64::from(matches!(value, ToggleChoice::On)),
                )],
            );
            match standard {
                Err(error)
                    if error.kind() == ErrorKind::CapabilityUnsupported
                        && matches!(backend_choice, None | Some(BackendChoice::Auto)) =>
                {
                    run_native_vendor_set(
                        config,
                        "image.mirror",
                        "image.mirror",
                        vendor_value,
                        dry_run,
                    )
                }
                result => result,
            }
        }
        ImageCommand::Flip { value } => {
            let vendor_value = if matches!(value, ToggleChoice::On) {
                "on"
            } else {
                "off"
            };
            let standard = run_semantic_requests(
                config,
                dry_run,
                yes,
                "image.flip",
                vec![raw_request(
                    "vertical_flip",
                    i64::from(matches!(value, ToggleChoice::On)),
                )],
            );
            match standard {
                Err(error)
                    if error.kind() == ErrorKind::CapabilityUnsupported
                        && matches!(backend_choice, None | Some(BackendChoice::Auto)) =>
                {
                    run_native_vendor_set(config, "image.flip", "image.flip", vendor_value, dry_run)
                }
                result => result,
            }
        }
        ImageCommand::Reset => run_image_reset(config, dry_run, yes),
    }
}

fn run_image_status(config: &Config) -> Result<(), LinkError> {
    let names = CAMERA_NATIVE_CAPABILITIES
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with("image."))
        .collect::<Vec<_>>();
    let devices = selected_devices(config, true)?;
    let mut results = Vec::new();
    for device in &devices {
        let all = native_capabilities_for(config, device, Some(&names))?;
        let capabilities = all
            .semantic
            .into_iter()
            .filter(|(name, _)| name.starts_with("image."))
            .collect::<BTreeMap<_, _>>();
        let values = capabilities
            .iter()
            .map(|(name, capability)| (name.clone(), capability.current.clone()))
            .collect();
        results.push(PerDeviceResult {
            device: device_summary(device),
            result: ImageStatusResult {
                capabilities,
                values,
                raw_controls: all.raw_controls,
            },
        });
    }
    if config.output == OutputFormat::Human {
        for result in &results {
            println!("{} ({})", result.device.model, result.device.stable_id);
            for (name, capability) in &result.result.capabilities {
                let value = result
                    .result
                    .values
                    .get(name)
                    .and_then(Option::as_ref)
                    .map_or_else(|| "-".into(), Value::to_string);
                println!("{name}\t{:?}\t{value}", capability.state);
            }
        }
    } else {
        emit_success(config.output, "image.status", None, &results)?;
    }
    Ok(())
}

fn run_semantic_scalar(
    config: &Config,
    command: &'static str,
    selector: &'static str,
    value: ScalarImageValue,
    dry_run: bool,
    yes: bool,
) -> Result<(), LinkError> {
    run_semantic_builder(config, dry_run, yes, command, move |backend| {
        let descriptor = backend.resolve(selector)?;
        let raw = link_v4l2::production::normalized_to_raw(&descriptor, value.value, value.clamp)?;
        Ok(vec![ControlRequest {
            selector: descriptor.id.to_string(),
            value: RequestedValue::Raw(raw),
        }])
    })
}

fn raw_request(selector: &str, value: i64) -> ControlRequest {
    ControlRequest {
        selector: selector.into(),
        value: RequestedValue::Raw(value),
    }
}

fn run_semantic_requests(
    config: &Config,
    dry_run: bool,
    yes: bool,
    command: &'static str,
    requests: Vec<ControlRequest>,
) -> Result<(), LinkError> {
    run_semantic_builder(config, dry_run, yes, command, move |_| Ok(requests.clone()))
}

fn run_semantic_builder<F>(
    config: &Config,
    dry_run: bool,
    yes: bool,
    command: &'static str,
    builder: F,
) -> Result<(), LinkError>
where
    F: Fn(&link_v4l2::production::ControlDevice) -> Result<Vec<ControlRequest>, LinkError>,
{
    require_all_confirmation(config, yes)?;
    let devices = selected_devices(config, true)?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for device in &devices {
        let outcome = (|| {
            let stable_id = device.identity.stable_id();
            let _lease = acquire_operation_lease(config, &stable_id, command)?;
            let node = control_node(device, config.default_device.as_deref())?;
            let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
            let requests = builder(&backend)?;
            execute_requests(device, config, requests, false, false, false, true, dry_run)
        })();
        match outcome {
            Ok(report) => results.push(PerDeviceResult {
                device: device_summary(device),
                result: report,
            }),
            Err(error) => failures.push(device_failure(
                device,
                &semantic_error(device, command, error),
            )),
        }
    }
    finish_mutation(config, command, results, failures)
}

fn require_semantic_requests(
    capability: &str,
    requests: Vec<ControlRequest>,
) -> Result<Vec<ControlRequest>, LinkError> {
    if requests.is_empty() {
        Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "no standard V4L2 control implements the semantic capability",
        )
        .with_detail("capability", capability))
    } else {
        Ok(requests)
    }
}

fn semantic_error(device: &DiscoveredDevice, capability: &str, error: LinkError) -> LinkError {
    if error.kind() != ErrorKind::CapabilityUnsupported {
        return error;
    }
    LinkError::new(error.kind(), error.message())
        .with_detail("capability", capability)
        .with_detail("state", "unknown")
        .with_detail("backend", "v4l2")
        .with_detail("model", device.model())
        .with_detail(
            "evidence",
            "no unambiguous standard V4L2 control was enumerated",
        )
}

fn validate_semantic_raw(control: &ControlDescriptor, raw: i64) -> Result<(), LinkError> {
    if raw < control.minimum || raw > control.maximum {
        Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "semantic value is outside the device range",
        )
        .with_detail("control", control.name.clone())
        .with_detail("value", raw)
        .with_detail("minimum", control.minimum)
        .with_detail("maximum", control.maximum))
    } else {
        Ok(())
    }
}

fn shutter_to_v4l2(input: &str) -> Result<i64, LinkError> {
    let seconds = if let Some((numerator, denominator)) = input.split_once('/') {
        let numerator = numerator
            .trim()
            .parse::<f64>()
            .map_err(|_| invalid_shutter(input))?;
        let denominator = denominator
            .trim()
            .parse::<f64>()
            .map_err(|_| invalid_shutter(input))?;
        if denominator == 0.0 {
            return Err(invalid_shutter(input));
        }
        numerator / denominator
    } else {
        humantime::parse_duration(input)
            .map_err(|_| invalid_shutter(input))?
            .as_secs_f64()
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(invalid_shutter(input));
    }
    Ok((seconds / 0.0001).round() as i64)
}

fn invalid_shutter(input: &str) -> LinkError {
    LinkError::new(
        ErrorKind::InvalidInvocation,
        "invalid shutter duration; use a fraction or duration",
    )
    .with_detail("value", input.to_owned())
}

fn run_image_reset(config: &Config, dry_run: bool, yes: bool) -> Result<(), LinkError> {
    require_all_confirmation(config, yes)?;
    let devices = selected_devices(config, true)?;
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let semantic_names = [
        "brightness",
        "contrast",
        "saturation",
        "sharpness",
        "backlight_compensation",
        "exposure_time_absolute",
        "iso_sensitivity",
        "gain",
        "white_balance_temperature",
        "focus_absolute",
        "power_line_frequency",
        "wide_dynamic_range",
        "exposure_automatic",
        "iso_sensitivity_automatic",
        "gain_automatic",
        "white_balance_automatic",
        "focus_automatic_continuous",
    ];
    for device in &devices {
        let outcome = (|| {
            let stable_id = device.identity.stable_id();
            let _lease = acquire_operation_lease(config, &stable_id, "image.reset")?;
            let node = control_node(device, config.default_device.as_deref())?;
            let backend = link_v4l2::production::ControlDevice::open_read(&node.path)?;
            let mut requests = Vec::new();
            let mut invalid = Vec::new();
            for name in semantic_names {
                let Ok(control) = backend.resolve(name) else {
                    continue;
                };
                if control.default_is_valid {
                    requests.push(ControlRequest {
                        selector: control.id.to_string(),
                        value: RequestedValue::Raw(control.default),
                    });
                } else {
                    invalid.push(control.name);
                }
            }
            let report =
                execute_requests(device, config, requests, false, false, false, true, dry_run)?;
            Ok((report, invalid))
        })();
        match outcome {
            Ok((report, invalid)) => {
                results.push(PerDeviceResult {
                    device: device_summary(device),
                    result: report,
                });
                if !invalid.is_empty() {
                    failures.push(DeviceFailure {
                        kind: ErrorKind::PartialSuccess,
                        value: json!({
                            "device": device_summary(device),
                            "error": {
                                "code": "invalid-driver-default",
                                "message": "one or more driver defaults were invalid and skipped",
                                "details": {"controls": invalid}
                            }
                        }),
                    });
                }
            }
            Err(error) => failures.push(device_failure(device, &error)),
        }
    }
    finish_mutation(config, "image.reset", results, failures)
}

fn run_doctor(config: &Config, bundle: Option<&Path>) -> Result<(), LinkError> {
    let mut checks = vec![DoctorCheck {
        name: "configuration".into(),
        status: DoctorStatus::Pass,
        message: "configuration loaded and validated".into(),
        details: json!({"schema_version": config.schema_version}),
    }];
    let catalog = ProfileCatalog::load(config.profile_dir.as_deref())?;
    let devices = if config.default_device.is_some() {
        selected_devices(config, true)?
    } else {
        discovered_devices()?
    };
    if devices.is_empty() {
        checks.push(DoctorCheck {
            name: "device-discovery".into(),
            status: DoctorStatus::Warning,
            message: "no UVC camera or known maintenance-mode device was discovered".into(),
            details: Value::Null,
        });
    } else {
        checks.push(DoctorCheck {
            name: "device-discovery".into(),
            status: DoctorStatus::Pass,
            message: format!("discovered {} camera device(s)", devices.len()),
            details: json!({"count": devices.len()}),
        });
    }
    for device in &devices {
        let stable_id = device.identity.stable_id();
        let state = link_linux::availability_state(device);
        checks.push(DoctorCheck {
            name: format!("permissions.{stable_id}"),
            status: match state {
                DeviceState::Ready | DeviceState::Maintenance => DoctorStatus::Pass,
                DeviceState::PermissionDenied | DeviceState::Unavailable => DoctorStatus::Fail,
                DeviceState::Busy | DeviceState::Unknown => DoctorStatus::Warning,
            },
            message: format!("device access state is {state:?}"),
            details: json!({
                "state": state,
                "video_nodes": device.video_nodes.iter().map(|node| &node.association.path).collect::<Vec<_>>()
            }),
        });
        let profile = catalog.report(&device.identity, device.mode())?;
        checks.push(DoctorCheck {
            name: format!("profile.{stable_id}"),
            status: if profile.profile_id.is_some() {
                DoctorStatus::Pass
            } else {
                DoctorStatus::Warning
            },
            message: profile.profile_id.as_ref().map_or_else(
                || "no exact vendor profile matched".into(),
                |profile| format!("matched vendor profile {profile}"),
            ),
            details: serde_json::to_value(profile).unwrap_or_default(),
        });
        if device.mode() == DeviceMode::Camera {
            match control_node(device, None).and_then(|node| {
                link_v4l2::production::ControlDevice::open_read(&node.path)
                    .and_then(|backend| backend.controls())
                    .map(|controls| (node.path, controls.len()))
            }) {
                Ok((path, count)) => checks.push(DoctorCheck {
                    name: format!("controls.{stable_id}"),
                    status: DoctorStatus::Pass,
                    message: format!("enumerated {count} V4L2 controls"),
                    details: json!({"path": path, "count": count}),
                }),
                Err(error) => checks.push(DoctorCheck {
                    name: format!("controls.{stable_id}"),
                    status: DoctorStatus::Fail,
                    message: error.message().into(),
                    details: json!({
                        "code": error.kind().code(),
                        "details": error.details(),
                    }),
                }),
            }
        }
    }
    let recovery_journals = JournalStore::from_process()?.paths()?;
    checks.push(DoctorCheck {
        name: "preset-transactions".into(),
        status: if recovery_journals.is_empty() {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Warning
        },
        message: if recovery_journals.is_empty() {
            "no incomplete preset transactions were found".into()
        } else {
            format!(
                "found {} incomplete preset transaction journal(s)",
                recovery_journals.len()
            )
        },
        details: json!({"journals": recovery_journals}),
    });
    let healthy = checks
        .iter()
        .all(|check| check.status != DoctorStatus::Fail);
    let report = DoctorReport { healthy, checks };
    if let Some(destination) = bundle {
        write_doctor_bundle(destination, &report, &devices, &catalog)?;
    }
    if config.output == OutputFormat::Human {
        for check in &report.checks {
            println!("{:?}\t{}\t{}", check.status, check.name, check.message);
        }
        println!("Healthy: {}", report.healthy);
        if let Some(destination) = bundle {
            println!("Bundle: {}", destination.display());
        }
    } else {
        emit_success(config.output, "doctor", None, &report)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct DoctorBundleManifest {
    schema_version: u32,
    files: BTreeMap<String, String>,
    redaction: BundleRedaction,
}

fn write_doctor_bundle(
    destination: &Path,
    report: &DoctorReport,
    devices: &[DiscoveredDevice],
    catalog: &ProfileCatalog,
) -> Result<(), LinkError> {
    if !destination
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".tar.zst"))
    {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "doctor bundle path must end in .tar.zst",
        ));
    }
    let parent = link_linux::validate_bundle_parent(destination)?;
    let staging = tempfile::tempdir_in(parent).map_err(|error| {
        doctor_bundle_error(
            "failed to create diagnostic staging directory",
            parent,
            &error,
        )
    })?;
    let mut files = BTreeMap::new();

    let mut redacted_report = report.clone();
    if let Some(check) = redacted_report
        .checks
        .iter_mut()
        .find(|check| check.name == "preset-transactions")
    {
        let count = check
            .details
            .get("journals")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        check.details = json!({"count": count});
    }
    doctor_write_json(staging.path(), "doctor.json", &redacted_report, &mut files)?;
    doctor_write_json(
        staging.path(),
        "host.json",
        &json!({
            "application_version": env!("CARGO_PKG_VERSION"),
            "architecture": std::env::consts::ARCH,
            "kernel_release": link_linux::kernel_release(),
            "module_versions": {
                "uvcvideo": fs::read_to_string("/sys/module/uvcvideo/version")
                    .ok()
                    .map(|value| value.trim().to_owned()),
            },
        }),
        &mut files,
    )?;
    doctor_write_json(
        staging.path(),
        "runtime.json",
        &json!({
            "cli_version": env!("CARGO_PKG_VERSION"),
            "daemon_version": null,
            "media_pipeline_graph": null,
            "test_results": null,
        }),
        &mut files,
    )?;
    let profiles = catalog
        .profiles()
        .iter()
        .map(|entry| {
            json!({
                "profile_id": entry.profile.profile_id,
                "checksum": entry.checksum,
                "status": entry.profile.status,
                "trust": match entry.trust {
                    link_profiles::ProfileTrust::BuiltIn => "built-in",
                    link_profiles::ProfileTrust::External => "external",
                },
            })
        })
        .collect::<Vec<_>>();
    doctor_write_json(staging.path(), "profiles.json", &profiles, &mut files)?;

    for (index, device) in devices.iter().enumerate() {
        let probe = build_probe(device, catalog, false)
            .and_then(|probe| serde_json::to_value(probe).map_err(doctor_serialization_error))
            .unwrap_or_else(|error| {
                json!({
                    "error": {
                        "code": error.kind().code(),
                        "message": error.message(),
                        "details": error.details(),
                    }
                })
            });
        doctor_write_json(
            staging.path(),
            &format!("probes/device-{index}.json"),
            &probe,
            &mut files,
        )?;
    }

    let audit = AppPaths::from_process()?
        .state
        .join("audit/xu-writes.jsonl");
    let mut omitted = vec![
        "USB serial numbers".into(),
        "USB string-descriptor values".into(),
        "raw XU payload bytes".into(),
        "kernel log (may contain unrelated host identifiers)".into(),
        "general recent logs (the direct CLI has no persistent general log)".into(),
        "daemon version (no daemon connection was established)".into(),
        "media pipeline graph (no persistent daemon pipeline was available)".into(),
        "test results (not executed by a diagnostic command)".into(),
    ];
    if audit.is_file() {
        let metadata = fs::symlink_metadata(&audit).map_err(|error| {
            doctor_bundle_error("failed to inspect XU audit log", &audit, &error)
        })?;
        if metadata.file_type().is_symlink() {
            omitted.push("XU audit log (symbolic links are not followed)".into());
        } else if metadata.len() > 1024 * 1024 {
            omitted.push("XU audit log (larger than 1 MiB)".into());
        } else {
            let bytes = fs::read(&audit).map_err(|error| {
                doctor_bundle_error("failed to read XU audit log", &audit, &error)
            })?;
            doctor_write_file(staging.path(), "audit/xu-writes.jsonl", &bytes, &mut files)?;
        }
    } else {
        omitted.push("XU write audit log (no audit log exists)".into());
    }

    let manifest = DoctorBundleManifest {
        schema_version: 1,
        files,
        redaction: BundleRedaction {
            serial_included: false,
            raw_descriptors_contain_string_values: false,
            omitted,
        },
    };
    doctor_write_json(
        staging.path(),
        "manifest.json",
        &manifest,
        &mut BTreeMap::new(),
    )?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        doctor_bundle_error("failed to create diagnostic archive", parent, &error)
    })?;
    temporary
        .as_file_mut()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            doctor_bundle_error("failed to secure diagnostic archive", destination, &error)
        })?;
    {
        let encoder = zstd::Encoder::new(temporary.as_file_mut(), 3).map_err(|error| {
            doctor_bundle_error(
                "failed to initialize diagnostic compression",
                destination,
                &error,
            )
        })?;
        let mut archive = tar::Builder::new(encoder);
        archive
            .append_dir_all(".", staging.path())
            .map_err(|error| {
                doctor_bundle_error("failed to assemble diagnostic archive", destination, &error)
            })?;
        let encoder = archive.into_inner().map_err(|error| {
            doctor_bundle_error("failed to finalize diagnostic archive", destination, &error)
        })?;
        encoder.finish().map_err(|error| {
            doctor_bundle_error(
                "failed to finish diagnostic compression",
                destination,
                &error,
            )
        })?;
    }
    temporary.as_file().sync_all().map_err(|error| {
        doctor_bundle_error(
            "failed to synchronize diagnostic archive",
            destination,
            &error,
        )
    })?;
    temporary.persist_noclobber(destination).map_err(|error| {
        doctor_bundle_error(
            "failed to finalize diagnostic archive",
            destination,
            &error.error,
        )
    })?;
    Ok(())
}

fn doctor_write_json<T: ?Sized + Serialize>(
    root: &Path,
    relative: &str,
    value: &T,
    files: &mut BTreeMap<String, String>,
) -> Result<(), LinkError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(doctor_serialization_error)?;
    bytes.push(b'\n');
    doctor_write_file(root, relative, &bytes, files)
}

fn doctor_write_file(
    root: &Path,
    relative: &str,
    bytes: &[u8],
    files: &mut BTreeMap<String, String>,
) -> Result<(), LinkError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            doctor_bundle_error(
                "failed to create diagnostic archive directory",
                parent,
                &error,
            )
        })?;
    }
    fs::write(&path, bytes).map_err(|error| {
        doctor_bundle_error("failed to write diagnostic artifact", &path, &error)
    })?;
    files.insert(relative.into(), sha256(bytes));
    Ok(())
}

fn doctor_serialization_error(error: serde_json::Error) -> LinkError {
    LinkError::new(
        ErrorKind::IoFailure,
        "failed to serialize diagnostic artifact",
    )
    .with_detail("reason", error.to_string())
}

fn doctor_bundle_error(message: &'static str, path: &Path, error: &std::io::Error) -> LinkError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, message)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn run_completion(config: &Config, shell: Shell) -> Result<(), LinkError> {
    if config.output != OutputFormat::Human {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "completion output is a raw shell script and requires --format human",
        ));
    }
    let mut command = Cli::command();
    clap_complete::generate(shell, &mut command, "linkctl", &mut std::io::stdout());
    Ok(())
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

fn emit_human_device_list(entries: &[DeviceListStatus]) {
    if entries.is_empty() {
        println!("No camera devices found.");
        return;
    }
    println!("STABLE ID\tMODEL\tVIDEO\tAUDIO\tSTATE");
    for entry in entries {
        let video = entry
            .device
            .video_nodes
            .iter()
            .map(|node| node.path.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let audio = entry
            .device
            .audio_nodes
            .iter()
            .map(|node| node.path.as_str())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{}\t{}\t{}\t{}\t{:?}",
            entry.device.stable_id, entry.device.model, video, audio, entry.state
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

#[cfg(feature = "gstreamer")]
fn emit_success_to_stderr<T: Serialize>(
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
        OutputFormat::Json => eprintln!(
            "{}",
            serde_json::to_string_pretty(&envelope).map_err(serialization_error)?
        ),
        OutputFormat::Jsonl => eprintln!(
            "{}",
            serde_json::to_string(&envelope).map_err(serialization_error)?
        ),
        OutputFormat::Human => eprintln!("{command} completed"),
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
    emit_command_error(format, "linkctl", None, error)
}

fn emit_command_error(
    format: OutputFormat,
    command: &str,
    device: Option<DeviceSummary>,
    error: &LinkError,
) -> u8 {
    match format {
        OutputFormat::Human => eprintln!("error: {}", error.message()),
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope: Envelope<Value> = Envelope::failure(command, device, error);
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

fn emit_command_error_to_stderr(
    format: OutputFormat,
    command: &str,
    device: Option<DeviceSummary>,
    error: &LinkError,
) -> u8 {
    match format {
        OutputFormat::Human => eprintln!("error: {}", error.message()),
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope: Envelope<Value> = Envelope::failure(command, device, error);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => eprintln!("{serialized}"),
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
    use link_core::preset::PresetRequirements;
    use link_profiles::{ProfileCatalog, StreamRequirement};
    use serde_json::Value;

    use super::{
        Cli, ErrorKind, LinkError, TRANSACTION_SCHEMA_VERSION, TransactionOutcome, TransactionPlan,
        TransactionReport, TransactionStepKind, TransactionStepPlan, TransactionStepReport,
        TransactionStepStatus, parse_fps, parse_size, parse_zoom_factor,
        profile_read_stream_requirement, record_rollback_result, rollback_order_for,
        shutter_to_v4l2, validate_preset_requirements, white_balance_status_value,
    };

    #[test]
    fn command_graph_excludes_mechanical_movement_commands() {
        let prohibited = ["pan", "tilt", "gimbal", "center-gimbal", "motor"];
        assert_command_names_are_absent(&Cli::command(), &prohibited);
    }

    #[test]
    fn shutter_fractions_and_durations_use_v4l2_units() {
        assert_eq!(shutter_to_v4l2("1/100").unwrap(), 100);
        assert_eq!(shutter_to_v4l2("10ms").unwrap(), 100);
        assert!(shutter_to_v4l2("1/0").is_err());
    }

    #[test]
    fn white_balance_status_reports_mode_and_kelvin() {
        assert_eq!(
            white_balance_status_value(Some(1), Some(4_800)),
            Some(serde_json::json!({"mode": "auto", "kelvin": 4_800}))
        );
        assert_eq!(
            white_balance_status_value(Some(0), Some(2_000)),
            Some(serde_json::json!({"mode": "manual", "kelvin": 2_000}))
        );
    }

    #[test]
    fn video_sizes_and_frame_rates_are_exact() {
        assert_eq!(parse_size("3840x2160").unwrap(), (3840, 2160));
        assert!(parse_size("3840").is_err());
        assert_eq!(parse_fps("30000/1001").unwrap().numerator, 30000);
        assert_eq!(parse_fps("29.97").unwrap().denominator, 100);
        assert!(parse_fps("0").is_err());
    }

    #[test]
    fn zoom_factors_accept_explicit_units_and_signed_steps() {
        assert_eq!(parse_zoom_factor("1.5x", false).unwrap(), 1.5);
        assert_eq!(parse_zoom_factor("+0.1x", true).unwrap(), 0.1);
        assert_eq!(parse_zoom_factor("-0.1", true).unwrap(), -0.1);
        assert!(parse_zoom_factor("0x", false).is_err());
        assert!(parse_zoom_factor("NaN", true).is_err());
    }

    #[test]
    fn incompatible_preset_requirements_fail_as_a_preflight_error() {
        let requirements = PresetRequirements {
            model: "Insta360 Link 2C Pro".into(),
            usb_vid: Some(0x2e1a),
            usb_pid: Some(0x4c05),
            fallback: Default::default(),
        };
        validate_preset_requirements(&requirements, "Insta360 Link 2C Pro", 0x2e1a, 0x4c05)
            .unwrap();
        let error = validate_preset_requirements(&requirements, "another model", 0x2e1a, 0x4c05)
            .expect_err("model mismatch must fail");
        assert_eq!(error.kind(), ErrorKind::ProtocolProfileMismatch);
    }

    #[test]
    fn auto_framing_status_selects_the_profile_open_stream_requirement() {
        let catalog = ProfileCatalog::load(None).unwrap();
        let profile = &catalog
            .profiles()
            .iter()
            .find(|entry| entry.profile.profile_id == "insta360-link-2c-pro-v0.2.9.8-build3")
            .unwrap()
            .profile;
        assert_eq!(
            profile_read_stream_requirement(
                profile,
                Some(&["auto-framing.enabled", "auto-framing.style"]),
            )
            .unwrap(),
            Some(StreamRequirement::Open)
        );
        assert_eq!(
            profile_read_stream_requirement(profile, Some(&["firmware.version"])).unwrap(),
            None
        );
        assert_eq!(
            profile_read_stream_requirement(profile, Some(&["image.hdr"])).unwrap(),
            Some(StreamRequirement::Open)
        );
    }

    #[test]
    fn rollback_is_reverse_order_and_continues_after_an_injected_failure() {
        let kinds = [
            TransactionStepKind::VideoFormat,
            TransactionStepKind::StandardControls,
            TransactionStepKind::AudioGain,
            TransactionStepKind::AudioMute,
        ];
        let plan_steps = kinds
            .into_iter()
            .enumerate()
            .map(|(sequence, kind)| TransactionStepPlan {
                sequence: sequence as u32,
                kind,
                backend: "test".into(),
                previous: Value::Null,
                requested: Value::Null,
                reversible: true,
                no_op: false,
            })
            .collect::<Vec<_>>();
        let mut report = TransactionReport {
            schema_version: TRANSACTION_SCHEMA_VERSION,
            plan: TransactionPlan {
                schema_version: TRANSACTION_SCHEMA_VERSION,
                transaction_id: "tx-test".into(),
                preset: "test".into(),
                stable_id: "test-device".into(),
                dry_run: false,
                restart_required: false,
                stream_restart_required: false,
                rollback_feasible: true,
                steps: plan_steps.clone(),
            },
            outcome: TransactionOutcome::Partial,
            steps: plan_steps
                .iter()
                .map(|step| TransactionStepReport {
                    sequence: step.sequence,
                    status: TransactionStepStatus::Verified,
                    observed: None,
                    error: None,
                })
                .collect(),
            rollback_attempted: true,
            rollback_failures: Vec::new(),
            journal: None,
        };
        let order = rollback_order_for(true, true, true, true);
        assert_eq!(
            order,
            [
                TransactionStepKind::AudioMute,
                TransactionStepKind::AudioGain,
                TransactionStepKind::StandardControls,
                TransactionStepKind::VideoFormat,
            ]
        );

        let mut first_error = None;
        for kind in order {
            let step = plan_steps.iter().find(|step| step.kind == kind).unwrap();
            let result = if kind == TransactionStepKind::StandardControls {
                Err(LinkError::new(
                    ErrorKind::IoFailure,
                    "injected rollback failure",
                ))
            } else {
                Ok(())
            };
            record_rollback_result(
                &mut report,
                step.sequence,
                "test-step",
                result,
                &mut first_error,
            );
        }

        assert!(first_error.is_some());
        assert_eq!(report.rollback_failures, ["test-step"]);
        assert_eq!(
            report
                .steps
                .iter()
                .map(|step| step.status)
                .collect::<Vec<_>>(),
            [
                TransactionStepStatus::RolledBack,
                TransactionStepStatus::RollbackFailed,
                TransactionStepStatus::RolledBack,
                TransactionStepStatus::RolledBack,
            ]
        );
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
