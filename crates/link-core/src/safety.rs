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
    /// Valid research input that is never trusted for normal semantic writes.
    Experimental,
    /// A compiled-in profile backed by reviewed evidence.
    Verified,
}

/// Operations classified by the safety boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// A non-mutating operation.
    ReadOnly,
    /// A range-checked standard V4L2 control write.
    StandardControlWrite,
    /// A raw Extension Unit write with build, acknowledgement, and profile gates.
    RawXuWrite {
        feature_enabled: bool,
        acknowledged: bool,
        profile: ProfileState,
    },
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
            Operation::ReadOnly | Operation::StandardControlWrite => Ok(()),
            Operation::VendorProfileWrite(ProfileState::Verified) => Ok(()),
            Operation::VendorProfileWrite(profile) => Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                match profile {
                    ProfileState::Untrusted => "untrusted profiles cannot authorize writes",
                    ProfileState::Invalid => "invalid profiles cannot authorize writes",
                    ProfileState::ReadOnly => "read-only profiles cannot authorize writes",
                    ProfileState::Experimental => {
                        "experimental profiles cannot authorize semantic writes"
                    }
                    ProfileState::Verified => unreachable!("verified profile handled above"),
                },
            )),
            Operation::RawXuWrite {
                feature_enabled,
                acknowledged,
                profile,
            } => {
                if !feature_enabled {
                    return Err(unsafe_denial(
                        "raw XU support requires a research-enabled build",
                    ));
                }
                if !acknowledged {
                    return Err(unsafe_denial("raw XU access requires --unsafe-xu"));
                }
                if !self.config.allow_raw_xu {
                    return Err(unsafe_denial(
                        "raw XU access is disabled by safety configuration",
                    ));
                }
                if matches!(profile, ProfileState::Experimental | ProfileState::Verified) {
                    Ok(())
                } else {
                    Err(LinkError::new(
                        ErrorKind::ProtocolProfileMismatch,
                        "raw XU access requires an exact experimental or verified profile",
                    ))
                }
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
        assert!(policy.authorize(Operation::StandardControlWrite).is_ok());
    }

    #[test]
    fn no_profile_state_can_authorize_a_write() {
        let policy = SafetyPolicy::new(SafetyConfig::default());

        for state in [
            ProfileState::Untrusted,
            ProfileState::Invalid,
            ProfileState::ReadOnly,
            ProfileState::Experimental,
        ] {
            let error = policy
                .authorize(Operation::VendorProfileWrite(state))
                .expect_err("profile writes must be unavailable");
            assert_eq!(error.kind(), ErrorKind::ProtocolProfileMismatch);
        }
        assert!(
            policy
                .authorize(Operation::VendorProfileWrite(ProfileState::Verified))
                .is_ok()
        );
    }

    #[test]
    fn configuration_cannot_enable_raw_writes() {
        let config = SafetyConfig {
            allow_raw_xu: true,
            ..SafetyConfig::default()
        };
        let error = SafetyPolicy::new(config)
            .authorize(Operation::RawXuWrite {
                feature_enabled: false,
                acknowledged: true,
                profile: ProfileState::Experimental,
            })
            .expect_err("raw writes must be unavailable");

        assert_eq!(error.kind(), ErrorKind::UnsafeOperationDenied);
    }

    #[test]
    fn research_raw_writes_require_every_gate() {
        let policy = SafetyPolicy::new(SafetyConfig {
            allow_raw_xu: true,
            ..SafetyConfig::default()
        });
        assert!(
            policy
                .authorize(Operation::RawXuWrite {
                    feature_enabled: true,
                    acknowledged: true,
                    profile: ProfileState::Experimental,
                })
                .is_ok()
        );
        assert!(
            policy
                .authorize(Operation::RawXuWrite {
                    feature_enabled: true,
                    acknowledged: false,
                    profile: ProfileState::Experimental,
                })
                .is_err()
        );
    }
}
