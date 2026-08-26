//! Stable errors and process exit codes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Process exit codes defined by the public CLI contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProcessExit {
    /// Successful command.
    Success = 0,
    /// The invocation or configuration is invalid.
    InvalidInvocation = 2,
    /// The selected device does not exist.
    DeviceNotFound = 3,
    /// The requested capability is unsupported or unmapped.
    CapabilityUnsupported = 4,
    /// The device or another resource is busy.
    DeviceBusy = 5,
    /// The operation lacks permission.
    PermissionDenied = 6,
    /// A device or filesystem operation failed.
    IoFailure = 7,
    /// A protocol or profile guard did not match.
    ProtocolProfileMismatch = 8,
    /// A prohibited operation was requested.
    UnsafeOperationDenied = 9,
    /// A multi-operation transaction completed only partially.
    PartialSuccess = 10,
    /// The operation exceeded its deadline.
    Timeout = 11,
    /// The daemon is unavailable or incompatible.
    DaemonUnavailable = 12,
    /// A media pipeline failed.
    MediaPipelineFailure = 13,
    /// Firmware staging validation failed.
    FirmwareValidationFailure = 14,
}

impl ProcessExit {
    /// Return the code suitable for `std::process::ExitCode`.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// Stable, serializable error categories.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    /// Invalid CLI arguments or configuration.
    InvalidInvocation,
    /// No matching device.
    DeviceNotFound,
    /// Unsupported or unmapped capability.
    CapabilityUnsupported,
    /// Busy device or resource.
    DeviceBusy,
    /// Insufficient permissions.
    PermissionDenied,
    /// Device or filesystem I/O failure.
    IoFailure,
    /// Profile or protocol guard mismatch.
    ProtocolProfileMismatch,
    /// Safety policy denied the operation.
    UnsafeOperationDenied,
    /// Partially applied transaction.
    PartialSuccess,
    /// Operation timeout.
    Timeout,
    /// Missing or incompatible daemon.
    DaemonUnavailable,
    /// Media pipeline failure.
    MediaPipelineFailure,
    /// Invalid firmware staging request.
    FirmwareValidationFailure,
}

impl ErrorKind {
    /// Return the process exit code associated with this error.
    #[must_use]
    pub const fn process_exit(self) -> ProcessExit {
        match self {
            Self::InvalidInvocation => ProcessExit::InvalidInvocation,
            Self::DeviceNotFound => ProcessExit::DeviceNotFound,
            Self::CapabilityUnsupported => ProcessExit::CapabilityUnsupported,
            Self::DeviceBusy => ProcessExit::DeviceBusy,
            Self::PermissionDenied => ProcessExit::PermissionDenied,
            Self::IoFailure => ProcessExit::IoFailure,
            Self::ProtocolProfileMismatch => ProcessExit::ProtocolProfileMismatch,
            Self::UnsafeOperationDenied => ProcessExit::UnsafeOperationDenied,
            Self::PartialSuccess => ProcessExit::PartialSuccess,
            Self::Timeout => ProcessExit::Timeout,
            Self::DaemonUnavailable => ProcessExit::DaemonUnavailable,
            Self::MediaPipelineFailure => ProcessExit::MediaPipelineFailure,
            Self::FirmwareValidationFailure => ProcessExit::FirmwareValidationFailure,
        }
    }

    /// Stable string used in machine-readable errors.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInvocation => "invalid-invocation",
            Self::DeviceNotFound => "device-not-found",
            Self::CapabilityUnsupported => "capability-unsupported",
            Self::DeviceBusy => "device-busy",
            Self::PermissionDenied => "permission-denied",
            Self::IoFailure => "io-failure",
            Self::ProtocolProfileMismatch => "protocol-profile-mismatch",
            Self::UnsafeOperationDenied => "unsafe-operation-denied",
            Self::PartialSuccess => "partial-success",
            Self::Timeout => "timeout",
            Self::DaemonUnavailable => "daemon-unavailable",
            Self::MediaPipelineFailure => "media-pipeline-failure",
            Self::FirmwareValidationFailure => "firmware-validation-failure",
        }
    }
}

/// Application error with stable classification and structured details.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct LinkError {
    kind: ErrorKind,
    message: String,
    details: Map<String, Value>,
}

impl LinkError {
    /// Construct a typed error.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Map::new(),
        }
    }

    /// Attach one structured detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Return the stable error kind.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Return the human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Return structured error details.
    #[must_use]
    pub fn details(&self) -> &Map<String, Value> {
        &self.details
    }

    /// Return the process exit code for this error.
    #[must_use]
    pub const fn process_exit(&self) -> ProcessExit {
        self.kind.process_exit()
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorKind, ProcessExit};

    #[test]
    fn error_kinds_map_to_the_contract_exit_codes() {
        let cases = [
            (ErrorKind::InvalidInvocation, 2),
            (ErrorKind::DeviceNotFound, 3),
            (ErrorKind::CapabilityUnsupported, 4),
            (ErrorKind::DeviceBusy, 5),
            (ErrorKind::PermissionDenied, 6),
            (ErrorKind::IoFailure, 7),
            (ErrorKind::ProtocolProfileMismatch, 8),
            (ErrorKind::UnsafeOperationDenied, 9),
            (ErrorKind::PartialSuccess, 10),
            (ErrorKind::Timeout, 11),
            (ErrorKind::DaemonUnavailable, 12),
            (ErrorKind::MediaPipelineFailure, 13),
            (ErrorKind::FirmwareValidationFailure, 14),
        ];

        for (kind, expected) in cases {
            assert_eq!(kind.process_exit().code(), expected);
        }
        assert_eq!(ProcessExit::Success.code(), 0);
    }
}
