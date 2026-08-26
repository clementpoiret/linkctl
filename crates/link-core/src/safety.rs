//! Non-bypassable safety decisions shared by every future backend.

use crate::{ErrorKind, LinkError, config::SafetyConfig};

/// Profile states admitted by the current profile boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileState {
    /// Raw input that has not been validated.
    Untrusted,
    /// Input that failed validation or device guards.
    Invalid,
    /// Valid input that is deliberately read-only.
    ReadOnly,
}

/// Operations classified by the safety boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// A non-mutating operation.
    ReadOnly,
    /// A raw Extension Unit write.
    RawXuWrite,
    /// A write to a selector with unknown semantics.
    UnknownXuWrite,
    /// A vendor write proposed by a profile.
    VendorProfileWrite(ProfileState),
    /// Detach the kernel video driver.
    DriverDetach,
    /// Reset a USB device.
    UsbReset,
    /// Enter a boot mode or write firmware/flash.
    FirmwareWrite,
    /// Write factory calibration data.
    CalibrationWrite,
    /// Operate a motor or mechanical positioning interface.
    MotorWrite,
}

/// Central authorization policy. Configuration can only narrow this policy.
#[derive(Clone, Debug)]
pub struct SafetyPolicy {
    config: SafetyConfig,
}

impl SafetyPolicy {
    /// Create a policy from effective configuration.
    #[must_use]
    pub const fn new(config: SafetyConfig) -> Self {
        Self { config }
    }

    /// Authorize an operation or return a stable denial.
    pub fn authorize(&self, operation: Operation) -> Result<(), LinkError> {
        match operation {
            Operation::ReadOnly => Ok(()),
            Operation::VendorProfileWrite(profile) => Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                match profile {
                    ProfileState::Untrusted => "untrusted profiles cannot authorize writes",
                    ProfileState::Invalid => "invalid profiles cannot authorize writes",
                    ProfileState::ReadOnly => "read-only profiles cannot authorize writes",
                },
            )),
            Operation::RawXuWrite => {
                let configured = self.config.allow_raw_xu;
                Err(LinkError::new(
                    ErrorKind::UnsafeOperationDenied,
                    "raw XU support is not available in this build",
                )
                .with_detail("configured", configured))
            }
            Operation::UnknownXuWrite => Err(unsafe_denial("unknown XU writes are prohibited")),
            Operation::DriverDetach => Err(unsafe_denial("detaching uvcvideo is prohibited")),
            Operation::UsbReset => Err(unsafe_denial("USB reset support is not available")),
            Operation::FirmwareWrite => Err(unsafe_denial(
                "firmware, boot, and flash writes are prohibited",
            )),
            Operation::CalibrationWrite => Err(unsafe_denial("calibration writes are prohibited")),
            Operation::MotorWrite => Err(unsafe_denial(
                "mechanical and motor operations are not supported",
            )),
        }
    }
}

fn unsafe_denial(message: &'static str) -> LinkError {
    LinkError::new(ErrorKind::UnsafeOperationDenied, message)
}

#[cfg(test)]
mod tests {
    use super::{Operation, ProfileState, SafetyPolicy};
    use crate::{ErrorKind, config::SafetyConfig};

    #[test]
    fn read_only_operations_are_allowed() {
        let policy = SafetyPolicy::new(SafetyConfig::default());
        assert!(policy.authorize(Operation::ReadOnly).is_ok());
    }

    #[test]
    fn no_profile_state_can_authorize_a_write() {
        let policy = SafetyPolicy::new(SafetyConfig::default());

        for state in [
            ProfileState::Untrusted,
            ProfileState::Invalid,
            ProfileState::ReadOnly,
        ] {
            let error = policy
                .authorize(Operation::VendorProfileWrite(state))
                .expect_err("profile writes must be unavailable");
            assert_eq!(error.kind(), ErrorKind::ProtocolProfileMismatch);
        }
    }

    #[test]
    fn configuration_cannot_enable_raw_writes() {
        let config = SafetyConfig {
            allow_raw_xu: true,
            ..SafetyConfig::default()
        };
        let error = SafetyPolicy::new(config)
            .authorize(Operation::RawXuWrite)
            .expect_err("raw writes must be unavailable");

        assert_eq!(error.kind(), ErrorKind::UnsafeOperationDenied);
        assert_eq!(error.details()["configured"], true);
    }
}
