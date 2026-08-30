//! Project-owned types for read-only hardware discovery and probe fixtures.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::SCHEMA_VERSION;

/// Schema used by standalone probe reports and fixture bundles.
pub const PROBE_SCHEMA_VERSION: u32 = 1;

/// The USB personality in which a device was observed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceMode {
    /// Normal UVC/UAC camera operation.
    Camera,
    /// USB mass-storage maintenance mode.
    UDisk,
    /// A USB device that cannot yet be classified.
    Unknown,
}

/// USB identity used for association and strict profile matching.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UsbIdentity {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// USB device revision (`bcdDevice`).
    pub device_revision: u16,
    /// Manufacturer string, when supplied.
    pub manufacturer: Option<String>,
    /// Product string, when supplied.
    pub product: Option<String>,
    /// Device serial, when supplied and disclosure is enabled.
    pub serial: Option<String>,
    /// Stable USB topology path such as `1-2.1`.
    pub topology: String,
    /// SHA-256 of the raw descriptor blob.
    pub descriptor_sha256: String,
}

impl UsbIdentity {
    /// Produce a non-secret user-facing identifier.
    #[must_use]
    pub fn stable_id(&self) -> String {
        let discriminator = self
            .serial
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.topology);
        let mut hasher = Sha256::new();
        hasher.update(b"linkctl-device-v2\0");
        hasher.update(self.vendor_id.to_be_bytes());
        hasher.update(b"\0");
        hasher.update(discriminator.as_bytes());
        let digest = hasher.finalize();
        let mut suffix = String::with_capacity(16);
        for byte in &digest[..8] {
            use std::fmt::Write as _;
            write!(&mut suffix, "{byte:02x}").expect("writing to String cannot fail");
        }

        let prefix = self.product.as_deref().map_or("usb", |product| {
            if product.to_ascii_lowercase().contains("link 2c pro") {
                "link2cpro"
            } else {
                "usb"
            }
        });
        format!("{prefix}-{suffix}")
    }

    /// Return a copy with the serial removed.
    #[must_use]
    pub fn without_serial(&self) -> Self {
        let mut redacted = self.clone();
        redacted.serial = None;
        redacted
    }
}

/// One recoverable issue encountered while collecting a report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProbeIssue {
    /// Stable subsystem name.
    pub area: String,
    /// Stable short issue code.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

impl ProbeIssue {
    /// Construct a probe issue.
    #[must_use]
    pub fn new(
        area: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            area: area.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

/// One associated device node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeAssociation {
    /// Current kernel device node.
    pub path: String,
    /// Stable `/dev/*/by-id` aliases.
    pub by_id: Vec<String>,
    /// Topology-based `/dev/*/by-path` aliases.
    pub by_path: Vec<String>,
}

/// Read-only filesystem identity for a mass-storage personality.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VolumeReport {
    /// Block device node.
    pub path: String,
    /// Filesystem label, if available.
    pub label: Option<String>,
    /// Filesystem type, if available.
    pub filesystem: Option<String>,
    /// Whether the volume is currently mounted. The mount path is intentionally omitted.
    pub mounted: bool,
}

/// Device information shown by the minimal enumerator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceListEntry {
    /// Non-secret stable identifier.
    pub stable_id: String,
    /// Human-readable device name.
    pub model: String,
    /// Observed USB personality.
    pub mode: DeviceMode,
    /// USB identity without a serial by default.
    pub usb: UsbIdentity,
    /// Associated video nodes.
    pub video_nodes: Vec<NodeAssociation>,
    /// Associated media-controller nodes.
    pub media_nodes: Vec<NodeAssociation>,
    /// Associated ALSA control nodes.
    pub audio_nodes: Vec<NodeAssociation>,
    /// Associated mass-storage volumes.
    pub volumes: Vec<VolumeReport>,
    /// Strictly matched read-only profile, if any.
    pub profile_id: Option<String>,
}

/// How a V4L2 node is used.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoNodeKind {
    /// Pixel or compressed-frame capture.
    Capture,
    /// UVC payload metadata capture.
    Metadata,
    /// A node with another or unrecognized capability set.
    Other,
    /// The node could not be opened or queried.
    Unavailable,
}

/// Raw and decoded V4L2 capability information.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct V4l2Capabilities {
    /// Kernel driver name.
    pub driver: String,
    /// Device card name.
    pub card: String,
    /// Kernel bus identity.
    pub bus_info: String,
    /// Kernel media API version.
    pub version: u32,
    /// Physical-device capability bits.
    pub capabilities: u32,
    /// Opened-node capability bits.
    pub device_capabilities: u32,
    /// Decoded names for the opened-node capability bits.
    pub capability_names: Vec<String>,
}

/// A rational number such as a frame interval.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Rational {
    /// Numerator.
    pub numerator: u32,
    /// Denominator.
    pub denominator: u32,
}

/// Enumerated frame interval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FrameInterval {
    /// One exact interval.
    Discrete { value: Rational },
    /// A continuous or stepwise interval range.
    Stepwise {
        /// Minimum interval.
        min: Rational,
        /// Maximum interval.
        max: Rational,
        /// Increment between supported intervals.
        step: Rational,
    },
}

/// Enumerated frame size and its supported intervals.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FrameSize {
    /// One exact width and height.
    Discrete {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Supported frame intervals.
        intervals: Vec<FrameInterval>,
    },
    /// A continuous or stepwise width/height range.
    Stepwise {
        /// Minimum width.
        min_width: u32,
        /// Maximum width.
        max_width: u32,
        /// Width increment.
        step_width: u32,
        /// Minimum height.
        min_height: u32,
        /// Maximum height.
        max_height: u32,
        /// Height increment.
        step_height: u32,
    },
}

/// One V4L2 pixel or metadata format.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoFormatReport {
    /// Canonical four-character code.
    pub fourcc: String,
    /// Kernel-supplied description.
    pub description: String,
    /// Raw format flags.
    pub flags: u32,
    /// Enumerated sizes and intervals.
    pub sizes: Vec<FrameSize>,
}

/// Current V4L2 format, where readable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CurrentFormatReport {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Canonical four-character code.
    pub fourcc: String,
}

/// One V4L2 menu item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlMenuItem {
    /// Raw menu index.
    pub index: u32,
    /// Text label or integer-menu value.
    pub value: Value,
}

/// One standard or driver-provided V4L2 control.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlReport {
    /// Numeric V4L2 control ID.
    pub id: u32,
    /// Kernel control name.
    pub name: String,
    /// Kernel control type number.
    pub control_type: u32,
    /// Raw flags.
    pub flags: u32,
    /// Decoded flag names.
    pub flag_names: Vec<String>,
    /// Minimum raw value.
    pub minimum: i64,
    /// Maximum raw value.
    pub maximum: i64,
    /// Raw step.
    pub step: u64,
    /// Raw default.
    pub default: i64,
    /// Element size for compound controls.
    pub element_size: u32,
    /// Number of elements.
    pub elements: u32,
    /// Dimensions for array controls.
    pub dimensions: Vec<u32>,
    /// Read current value when the type is safely supported by the inventory backend.
    pub current: Option<Value>,
    /// Whether the observed scalar is within the reported range.
    pub current_in_range: Option<bool>,
    /// Menu entries.
    pub menu: Vec<ControlMenuItem>,
    /// Per-control read issue, if any.
    pub issue: Option<ProbeIssue>,
}

/// Full report for one V4L2 node.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VideoNodeReport {
    /// Node association information.
    pub node: NodeAssociation,
    /// Classified role.
    pub kind: VideoNodeKind,
    /// Queried capabilities.
    pub capabilities: Option<V4l2Capabilities>,
    /// Current format.
    pub current_format: Option<CurrentFormatReport>,
    /// Advertised formats.
    pub formats: Vec<VideoFormatReport>,
    /// Advertised controls.
    pub controls: Vec<ControlReport>,
    /// Recoverable node-level issues.
    pub issues: Vec<ProbeIssue>,
}

/// Outcome of the safe XU information queries for one selector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct XuSelectorReport {
    /// One-based UVC selector number.
    pub selector: u8,
    /// `GET_LEN` result decoded as little-endian.
    pub length: Option<u16>,
    /// Raw `GET_INFO` byte.
    pub info: Option<u8>,
    /// Whether `GET_CUR` is advertised as supported.
    pub get_supported: Option<bool>,
    /// Whether `SET_CUR` is advertised as supported. No set is issued.
    pub set_supported: Option<bool>,
    /// Query failures recorded independently.
    pub issues: Vec<ProbeIssue>,
}

/// Parsed UVC Extension Unit inventory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct XuEntityReport {
    /// Runtime UVC unit ID.
    pub unit_id: u8,
    /// Canonical Extension Unit GUID.
    pub guid: String,
    /// Declared number of controls.
    pub num_controls: u8,
    /// Upstream source unit IDs.
    pub source_ids: Vec<u8>,
    /// Raw control bitmap in lowercase hexadecimal.
    pub control_bitmap: String,
    /// Every selector advertised by the control bitmap.
    pub selectors: Vec<XuSelectorReport>,
    /// Byte offset within the descriptor blob.
    pub descriptor_offset: usize,
}

/// ALSA capture PCM capabilities associated with the camera.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AlsaPcmReport {
    /// ALSA card index.
    pub card: i32,
    /// ALSA PCM device index.
    pub device: i32,
    /// ALSA device string.
    pub name: String,
    /// Human-readable PCM name.
    pub description: String,
    /// Minimum channel count.
    pub channels_min: u32,
    /// Maximum channel count.
    pub channels_max: u32,
    /// Minimum sample rate.
    pub rate_min: u32,
    /// Maximum sample rate.
    pub rate_max: u32,
    /// Supported sample formats from the backend's known format set.
    pub formats: Vec<String>,
    /// Supported access modes.
    pub access_modes: Vec<String>,
}

/// PipeWire object associated with the camera.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PipeWireObjectReport {
    /// PipeWire global ID.
    pub id: u32,
    /// PipeWire interface type.
    pub object_type: String,
    /// Stable, selected properties useful for association and format inspection.
    pub properties: BTreeMap<String, String>,
}

/// Audio discovery from ALSA and optionally PipeWire.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioReport {
    /// Associated ALSA capture PCMs.
    pub alsa: Vec<AlsaPcmReport>,
    /// Associated PipeWire devices/nodes. Empty when unavailable or not compiled.
    pub pipewire: Vec<PipeWireObjectReport>,
    /// Whether PipeWire support was compiled.
    pub pipewire_compiled: bool,
    /// Whether a PipeWire registry was reached.
    pub pipewire_available: bool,
    /// Recoverable audio-discovery issues.
    pub issues: Vec<ProbeIssue>,
}

/// Firmware discovery result without inferring firmware from USB revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirmwareReport {
    /// Reported firmware version, when obtained from a verified source.
    pub version: Option<String>,
    /// Read-only sources that were attempted.
    pub attempted_sources: Vec<String>,
    /// Explanation when no version is available.
    pub note: String,
}

/// Strict read-only profile match result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfileReport {
    /// Matching profile identifier.
    pub profile_id: Option<String>,
    /// Why the profile did or did not match.
    pub reasons: Vec<String>,
    /// Profiles in this implementation can never authorize writes.
    pub writable: bool,
}

/// Host metadata needed to reproduce an inventory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostReport {
    /// Kernel release.
    pub kernel_release: String,
    /// Rust target architecture at build time.
    pub architecture: String,
    /// Source revision embedded by the producing build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

/// Manifest of privacy-sensitive fields omitted from a report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RedactionManifest {
    /// Whether a USB serial was deliberately included.
    pub serial_included: bool,
    /// Classes of data always omitted from the bundle.
    pub omitted: Vec<String>,
}

/// Self-contained, normalized device probe report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProbeReport {
    /// Probe format schema.
    pub schema_version: u32,
    /// Machine-output envelope schema used by the producing build.
    pub output_schema_version: u32,
    /// Capture time as Unix milliseconds.
    pub captured_unix_ms: u128,
    /// `linkctl` package version.
    pub application_version: String,
    /// Host metadata.
    pub host: HostReport,
    /// Enumerated device identity and associations.
    pub device: DeviceListEntry,
    /// Per-node V4L2 inventory.
    pub video: Vec<VideoNodeReport>,
    /// Parsed and safely queried XU inventory.
    pub extension_units: Vec<XuEntityReport>,
    /// Audio inventory.
    pub audio: AudioReport,
    /// Firmware discovery attempt.
    pub firmware: FirmwareReport,
    /// Strict profile match.
    pub profile: ProfileReport,
    /// Privacy redaction manifest.
    pub redaction: RedactionManifest,
    /// Recoverable cross-subsystem issues.
    pub issues: Vec<ProbeIssue>,
}

impl ProbeReport {
    /// Construct the invariant fields of a report.
    #[must_use]
    pub fn new(
        captured_unix_ms: u128,
        application_version: impl Into<String>,
        host: HostReport,
        device: DeviceListEntry,
        profile: ProfileReport,
        serial_included: bool,
    ) -> Self {
        Self {
            schema_version: PROBE_SCHEMA_VERSION,
            output_schema_version: SCHEMA_VERSION,
            captured_unix_ms,
            application_version: application_version.into(),
            host,
            device,
            video: Vec::new(),
            extension_units: Vec::new(),
            audio: AudioReport::default(),
            firmware: FirmwareReport {
                version: None,
                attempted_sources: vec!["usb-descriptors".into(), "read-only-profile".into()],
                note: "no verified firmware-version source is available".into(),
            },
            profile,
            redaction: RedactionManifest {
                serial_included,
                omitted: vec![
                    "usernames".into(),
                    "home-paths".into(),
                    "mount-paths".into(),
                    "credentials".into(),
                    "media".into(),
                ],
            },
            issues: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HostReport, UsbIdentity};

    fn identity(serial: Option<&str>, topology: &str) -> UsbIdentity {
        UsbIdentity {
            vendor_id: 0x2e1a,
            product_id: 0x4c05,
            device_revision: 0x0200,
            manufacturer: Some("Insta360".into()),
            product: Some("Insta360 Link 2C Pro".into()),
            serial: serial.map(str::to_owned),
            topology: topology.into(),
            descriptor_sha256: "00".repeat(32),
        }
    }

    #[test]
    fn serial_devices_keep_their_id_when_moved() {
        assert_eq!(
            identity(Some("serial"), "1-1").stable_id(),
            identity(Some("serial"), "2-7").stable_id()
        );
    }

    #[test]
    fn serialless_devices_use_topology_to_avoid_collisions() {
        assert_ne!(
            identity(None, "1-1").stable_id(),
            identity(None, "2-7").stable_id()
        );
    }

    #[test]
    fn serialless_devices_keep_their_id_across_usb_personalities() {
        let camera = identity(None, "1-1");
        let mut alternate = camera.clone();
        alternate.product_id = 0x4c06;
        alternate.device_revision = 0x0201;
        alternate.descriptor_sha256 = "ff".repeat(32);

        assert_eq!(camera.stable_id(), alternate.stable_id());
    }

    #[test]
    fn redaction_removes_the_serial_without_changing_other_fields() {
        let source = identity(Some("private"), "1-1");
        let redacted = source.without_serial();
        assert!(redacted.serial.is_none());
        assert_eq!(redacted.product_id, source.product_id);
    }

    #[test]
    fn host_report_accepts_pre_provenance_json() {
        let report: HostReport = serde_json::from_value(serde_json::json!({
            "kernel_release": "6.12.0",
            "architecture": "x86_64"
        }))
        .unwrap();

        assert!(report.source_revision.is_none());
        let serialized = serde_json::to_value(report).unwrap();
        assert!(serialized.get("source_revision").is_none());
    }
}
