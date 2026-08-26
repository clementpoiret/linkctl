//! Production device-state and diagnostic result contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Best-effort availability state for a discovered physical device.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceState {
    Ready,
    Busy,
    PermissionDenied,
    Unavailable,
    Maintenance,
    #[default]
    Unknown,
}

/// One normalized hotplug event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DeviceEvent {
    pub sequence: u64,
    pub observed_unix_ms: u128,
    pub kind: String,
    pub stable_id: String,
    pub model: String,
    pub previous: Option<Value>,
    pub current: Option<Value>,
}

/// Severity of one read-only diagnostic check.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorStatus {
    Pass,
    Warning,
    Fail,
}

/// One check returned by `doctor`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
    pub details: Value,
}

/// Complete read-only diagnostic result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DoctorReport {
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}
