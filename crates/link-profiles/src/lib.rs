//! Strictly read-only device-profile loading and matching.

use std::{fs, path::Path};

use link_core::{
    ErrorKind, LinkError,
    probe::{DeviceMode, ProfileReport, UsbIdentity},
};
use serde::{Deserialize, Serialize};

pub use link_core::safety::ProfileState;

const BUILTIN_LINK_2C_PRO: &str =
    include_str!("../../../profiles/read-only/insta360-link-2c-pro.toml");

/// Access level accepted by the current profile boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileAccess {
    /// Profile can identify and annotate a device but cannot authorize writes.
    ReadOnly,
}

/// One exact descriptor-guarded USB personality.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileMatch {
    /// Camera or U-Disk personality.
    pub mode: DeviceMode,
    /// USB vendor ID.
    pub usb_vid: u16,
    /// USB product ID.
    pub usb_pid: u16,
    /// Exact USB device revision.
    pub bcd_device: u16,
    /// Exact lowercase SHA-256 descriptor fingerprint.
    pub descriptor_sha256: String,
}

/// Validated read-only profile document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOnlyProfile {
    /// Profile schema version.
    pub schema_version: u32,
    /// Stable profile identifier.
    pub profile_id: String,
    /// Human-readable model.
    pub model: String,
    /// Non-writable access boundary.
    pub access: ProfileAccess,
    /// Exact known USB personalities.
    #[serde(rename = "match")]
    pub matches: Vec<ProfileMatch>,
}

impl ReadOnlyProfile {
    /// Parse and validate a profile.
    pub fn parse(source: &str, origin: &str) -> Result<Self, LinkError> {
        let profile: Self = toml::from_str(source).map_err(|error| {
            LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "invalid read-only profile",
            )
            .with_detail("origin", origin.to_owned())
            .with_detail("reason", error.to_string())
        })?;
        profile.validate(origin)?;
        Ok(profile)
    }

    fn validate(&self, origin: &str) -> Result<(), LinkError> {
        if self.schema_version != 1 {
            return Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "unsupported profile schema",
            )
            .with_detail("origin", origin.to_owned())
            .with_detail("requested", u64::from(self.schema_version))
            .with_detail("supported", 1_u64));
        }
        if self.profile_id.trim().is_empty() || self.model.trim().is_empty() {
            return Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "profile identity fields cannot be empty",
            )
            .with_detail("origin", origin.to_owned()));
        }
        if self.matches.is_empty() {
            return Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "profile must contain at least one exact match",
            )
            .with_detail("origin", origin.to_owned()));
        }
        for guard in &self.matches {
            if guard.descriptor_sha256.len() != 64
                || !guard
                    .descriptor_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "profile descriptor fingerprint must be lowercase SHA-256",
                )
                .with_detail("origin", origin.to_owned()));
            }
        }
        Ok(())
    }

    /// Return true only for an exact USB, mode, revision, and descriptor match.
    #[must_use]
    pub fn matches(&self, identity: &UsbIdentity, mode: DeviceMode) -> bool {
        self.matches.iter().any(|guard| {
            guard.mode == mode
                && guard.usb_vid == identity.vendor_id
                && guard.usb_pid == identity.product_id
                && guard.bcd_device == identity.device_revision
                && guard.descriptor_sha256 == identity.descriptor_sha256
        })
    }
}

/// Built-in and explicitly supplied read-only profiles.
#[derive(Clone, Debug)]
pub struct ProfileCatalog {
    profiles: Vec<ReadOnlyProfile>,
}

impl ProfileCatalog {
    /// Load the built-in profile and optional additional `.toml` profiles.
    pub fn load(additional_directory: Option<&Path>) -> Result<Self, LinkError> {
        let mut profiles = vec![ReadOnlyProfile::parse(
            BUILTIN_LINK_2C_PRO,
            "builtin:insta360-link-2c-pro",
        )?];
        if let Some(directory) = additional_directory {
            let mut paths = fs::read_dir(directory)
                .map_err(|error| profile_io_error(directory, &error))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|value| value == "toml"))
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let source =
                    fs::read_to_string(&path).map_err(|error| profile_io_error(&path, &error))?;
                profiles.push(ReadOnlyProfile::parse(
                    &source,
                    &path.display().to_string(),
                )?);
            }
        }
        Ok(Self { profiles })
    }

    /// Return the unique matching profile, refusing ambiguous catalogs.
    pub fn matching_profile(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
    ) -> Result<Option<&ReadOnlyProfile>, LinkError> {
        let matches = self
            .profiles
            .iter()
            .filter(|profile| profile.matches(identity, mode))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Ok(None),
            [profile] => Ok(Some(profile)),
            _ => Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "multiple read-only profiles match the same device",
            )
            .with_detail("matches", matches.len() as u64)),
        }
    }

    /// Produce the public profile report with exact mismatch reasons.
    pub fn report(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
    ) -> Result<ProfileReport, LinkError> {
        if let Some(profile) = self.matching_profile(identity, mode)? {
            return Ok(ProfileReport {
                profile_id: Some(profile.profile_id.clone()),
                reasons: vec!["exact USB revision and descriptor fingerprint match".into()],
                writable: false,
            });
        }

        let related = self.profiles.iter().find(|profile| {
            profile.matches.iter().any(|guard| {
                guard.usb_vid == identity.vendor_id && guard.usb_pid == identity.product_id
            })
        });
        let reason = if related.is_some() {
            "USB model recognized, but revision, mode, or descriptor fingerprint mismatched"
        } else {
            "no read-only profile recognizes this USB model"
        };
        Ok(ProfileReport {
            profile_id: None,
            reasons: vec![reason.into()],
            writable: false,
        })
    }
}

fn profile_io_error(path: &Path, error: &std::io::Error) -> LinkError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, "failed to read profile input")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use link_core::probe::{DeviceMode, UsbIdentity};

    use super::{ProfileCatalog, ReadOnlyProfile};

    fn identity(hash: &str) -> UsbIdentity {
        UsbIdentity {
            vendor_id: 0x2e1a,
            product_id: 0x4c05,
            device_revision: 0x0200,
            manufacturer: Some("Insta360".into()),
            product: Some("Insta360 Link 2C Pro".into()),
            serial: None,
            topology: "1-2.1".into(),
            descriptor_sha256: hash.into(),
        }
    }

    #[test]
    fn builtin_profile_is_read_only_and_exact() {
        let catalog = ProfileCatalog::load(None).expect("built-in profile");
        for hash in [
            "1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c",
            "7a60c8dd0f5e3d83e6c1c1fb245d96e02cc4ea6fdea8c10cc5a2e3b1094a2cc8",
        ] {
            let observed = identity(hash);
            let matched = catalog
                .matching_profile(&observed, DeviceMode::Camera)
                .expect("profile lookup")
                .expect("exact match");
            assert_eq!(matched.profile_id, "insta360-link-2c-pro");
        }

        let mut u_disk =
            identity("8c9226df8b126f700d738b42f38c0163549a37a19753832527ce27742d3d7f2e");
        u_disk.vendor_id = 0x070a;
        u_disk.product_id = 0x4026;
        u_disk.device_revision = 0x0001;
        let matched = catalog
            .matching_profile(&u_disk, DeviceMode::UDisk)
            .expect("profile lookup")
            .expect("exact U-Disk match");
        assert_eq!(matched.profile_id, "insta360-link-2c-pro");

        let mut mismatched =
            identity("1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c");
        mismatched.descriptor_sha256.replace_range(0..1, "0");
        assert!(
            catalog
                .matching_profile(&mismatched, DeviceMode::Camera)
                .expect("profile lookup")
                .is_none()
        );
    }

    #[test]
    fn writable_or_unknown_profile_fields_are_rejected() {
        let source = r#"
schema_version = 1
profile_id = "bad"
model = "bad"
access = "read-only"
writable = true

[[match]]
mode = "camera"
usb_vid = 1
usb_pid = 2
bcd_device = 3
descriptor_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#;
        assert!(ReadOnlyProfile::parse(source, "test").is_err());
    }
}
