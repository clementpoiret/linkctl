//! Project-owned contracts for standard controls and semantic capabilities.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::probe::ProbeIssue;

/// One menu entry advertised for a control.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlMenuEntry {
    /// Raw menu index.
    pub index: i64,
    /// Kernel-supplied label or integer-menu value.
    pub label: String,
}

/// Complete live metadata for one V4L2 control.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlDescriptor {
    /// Numeric V4L2 control identifier.
    pub id: u32,
    /// Stable hexadecimal rendering of [`Self::id`].
    pub id_hex: String,
    /// Stable snake-case name accepted by the CLI.
    pub name: String,
    /// Name supplied by the kernel.
    pub kernel_name: String,
    /// Numeric V4L2 control type.
    pub control_type: u32,
    /// Stable descriptive control type.
    pub control_type_name: String,
    /// Numeric V4L2 control class.
    pub control_class: u32,
    /// Stable descriptive control class.
    pub control_class_name: String,
    /// Raw flags returned by the driver.
    pub flags: u32,
    /// Decoded flag names.
    pub flag_names: Vec<String>,
    /// Minimum raw scalar value.
    pub minimum: i64,
    /// Maximum raw scalar value.
    pub maximum: i64,
    /// Raw scalar increment.
    pub step: u64,
    /// Driver-advertised default value.
    pub default: i64,
    /// Current scalar value, when readable.
    pub current: Option<i64>,
    /// Whether the current value is inside the advertised range.
    pub current_in_range: Option<bool>,
    /// Whether the default can be safely submitted to the driver.
    pub default_is_valid: bool,
    /// Enumerated menu values.
    pub menu: Vec<ControlMenuEntry>,
    /// The kernel declares the control readable.
    pub readable: bool,
    /// The kernel declares the control writable.
    pub writable: bool,
    /// The control is currently active and not grabbed or disabled.
    pub available: bool,
    /// The production backend supports the control's value codec.
    pub codec_supported: bool,
    /// Known parent controls that gate this control.
    pub dependencies: Vec<String>,
    /// Recoverable metadata or read issue.
    pub issue: Option<ProbeIssue>,
}

/// A scalar control value with optional semantic rendering.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlValue {
    /// Raw kernel value.
    pub raw: i64,
    /// Reversible 0.0–1.0 rendering, where meaningful.
    pub normalized: Option<f64>,
    /// Menu or boolean label, where meaningful.
    pub label: Option<String>,
}

/// One control change included in a report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlChangeReport {
    /// Control metadata after dependency changes were applied.
    pub control: ControlDescriptor,
    /// Value observed before the operation.
    pub previous: Option<ControlValue>,
    /// Requested value after parsing and range translation.
    pub requested: ControlValue,
    /// Value returned by the write ioctl, when supplied.
    pub applied: Option<ControlValue>,
    /// Value read after the write.
    pub observed: Option<ControlValue>,
    /// Whether readback matched the requested value.
    pub verified: bool,
    /// Whether this entry was an automatically inserted prerequisite.
    pub prerequisite: bool,
}

/// Rollback outcome after a failed or partially applied operation.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RollbackReport {
    /// Whether rollback was required.
    pub attempted: bool,
    /// Controls successfully restored.
    pub restored: Vec<String>,
    /// Controls that could not be restored.
    pub failed: Vec<String>,
}

/// Result of one or more standard-control writes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlSetReport {
    /// Ordered changes, including prerequisites.
    pub changes: Vec<ControlChangeReport>,
    /// Whether no device write was issued.
    pub dry_run: bool,
    /// Whether a batch ioctl was used.
    pub batched: bool,
    /// Whether explicitly requested individual fallback was used.
    pub individual_fallback_used: bool,
    /// Failing batch index returned by the kernel.
    pub error_index: Option<u32>,
    /// Best-effort rollback result.
    pub rollback: RollbackReport,
}

/// Capability implementation state from the public specification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityState {
    Standard,
    VendorProfile,
    Host,
    DiscoveredUnmapped,
    HardwareOnly,
    Unsupported,
    Unknown,
    UnsafeDisabled,
}

/// Confidence assigned to a capability mapping.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityConfidence {
    Verified,
    Probable,
    Experimental,
}

/// Concrete source implementing one semantic capability.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CapabilitySource {
    V4l2 {
        control: String,
    },
    UvcXu {
        profile_id: String,
        profile_checksum: String,
        guid: String,
        selector: u8,
        length: u16,
    },
    Hardware {
        component: String,
    },
}

/// Public semantic range independent of a backend's raw units.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SemanticRange {
    pub minimum: f64,
    pub maximum: f64,
    pub step: Option<f64>,
    pub unit: String,
}

/// Machine-readable status for one semantic capability.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapabilityRecord {
    pub state: CapabilityState,
    pub backend: Option<String>,
    pub evidence: String,
    pub model: String,
    pub firmware: Option<String>,
    pub readable: bool,
    pub writable: bool,
    pub persistent: Option<bool>,
    pub stream_dependent: Option<bool>,
    pub restart_dependent: bool,
    pub destructive: bool,
    pub verified_at_unix_ms: u128,
    pub confidence: CapabilityConfidence,
    pub source: Option<CapabilitySource>,
    pub range: Option<SemanticRange>,
    pub values: Vec<String>,
    pub current: Option<Value>,
    /// Standard control descriptor when this capability uses V4L2.
    pub control: Option<ControlDescriptor>,
}

/// Capabilities and raw controls returned by `caps controls`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlCapabilities {
    pub semantic: BTreeMap<String, CapabilityRecord>,
    pub raw: Vec<ControlDescriptor>,
}

/// One event emitted by `control watch`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlEvent {
    pub sequence: u64,
    pub observed_unix_ms: u128,
    pub kind: String,
    pub control: Option<ControlDescriptor>,
    pub previous: Option<ControlValue>,
    pub current: Option<ControlValue>,
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::{CapabilityState, ControlValue};

    #[test]
    fn public_enums_use_specification_spelling() {
        assert_eq!(
            serde_json::to_value(CapabilityState::UnsafeDisabled).unwrap(),
            "unsafe-disabled"
        );
    }

    #[test]
    fn control_values_keep_raw_and_semantic_forms() {
        let value = ControlValue {
            raw: 50,
            normalized: Some(0.5),
            label: None,
        };
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json["raw"], 50);
        assert_eq!(json["normalized"], 0.5);
    }
}
