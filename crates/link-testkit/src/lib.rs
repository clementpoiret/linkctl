//! Hardware-free fixtures and backend test support.

use std::{collections::BTreeMap, fs, path::Path};

use link_core::{ErrorKind, LinkError, probe::ProbeReport};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct BundleManifest {
    schema_version: u32,
    files: BTreeMap<String, String>,
    redaction: BundleRedaction,
}

#[derive(Debug, Deserialize)]
struct BundleRedaction {
    serial_included: bool,
    raw_descriptors_contain_string_values: bool,
    omitted: Vec<String>,
}

/// Load a recorded probe bundle and validate its checksums and privacy invariants.
pub fn validate_probe_bundle(path: &Path) -> Result<ProbeReport, LinkError> {
    let probe_bytes = read(path, "probe.json")?;
    let descriptor_bytes = read(path, "usb-descriptors.bin")?;
    let manifest_bytes = read(path, "manifest.json")?;
    let probe: ProbeReport = serde_json::from_slice(&probe_bytes)
        .map_err(|error| invalid_bundle(path, "probe.json is invalid", error.to_string()))?;
    let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| invalid_bundle(path, "manifest.json is invalid", error.to_string()))?;

    if manifest.schema_version != 1 {
        return Err(invalid_bundle(
            path,
            "unsupported bundle schema",
            manifest.schema_version.to_string(),
        ));
    }
    verify_checksum(path, &manifest, "probe.json", &probe_bytes)?;
    verify_checksum(path, &manifest, "usb-descriptors.bin", &descriptor_bytes)?;
    if sha256(&descriptor_bytes) != probe.device.usb.descriptor_sha256 {
        return Err(invalid_bundle(
            path,
            "descriptor fingerprint does not match probe identity",
            probe.device.usb.descriptor_sha256.clone(),
        ));
    }
    if manifest.redaction.serial_included != probe.redaction.serial_included
        || (!manifest.redaction.serial_included && probe.device.usb.serial.is_some())
    {
        return Err(invalid_bundle(
            path,
            "serial redaction metadata is inconsistent",
            "probe and manifest disagree",
        ));
    }
    if manifest.redaction.raw_descriptors_contain_string_values {
        return Err(invalid_bundle(
            path,
            "raw descriptor policy is unsupported",
            "bundle claims that expanded USB string values are present",
        ));
    }
    if manifest.redaction.omitted != probe.redaction.omitted {
        return Err(invalid_bundle(
            path,
            "redaction omission lists differ",
            "probe and manifest disagree",
        ));
    }
    Ok(probe)
}

fn read(path: &Path, name: &str) -> Result<Vec<u8>, LinkError> {
    fs::read(path.join(name)).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to read probe bundle")
            .with_detail("path", path.join(name).display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn verify_checksum(
    path: &Path,
    manifest: &BundleManifest,
    name: &str,
    bytes: &[u8],
) -> Result<(), LinkError> {
    let expected = manifest
        .files
        .get(name)
        .ok_or_else(|| invalid_bundle(path, "manifest entry is missing", name))?;
    if expected != &sha256(bytes) {
        return Err(invalid_bundle(path, "bundle checksum mismatch", name));
    }
    Ok(())
}

fn invalid_bundle(path: &Path, message: impl Into<String>, reason: impl Into<String>) -> LinkError {
    LinkError::new(ErrorKind::ProtocolProfileMismatch, message)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", reason.into())
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use link_core::probe::{DeviceMode, FrameSize, ProbeReport, VideoNodeKind};

    use super::validate_probe_bundle;

    #[test]
    fn recorded_probe_bundles_are_valid_and_redacted() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("fixtures/golden-probe");
        let mut bundles = fs::read_dir(&root)
            .expect("fixture directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        bundles.sort();
        let reports = bundles
            .into_iter()
            .map(|bundle| {
                let name = bundle
                    .file_name()
                    .expect("bundle name")
                    .to_string_lossy()
                    .into_owned();
                let report = validate_probe_bundle(&bundle).expect("valid probe bundle");
                (name, report)
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            reports.keys().map(String::as_str).collect::<Vec<_>>(),
            ["landscape", "portrait", "u-disk"]
        );

        for report in reports.values() {
            assert!(report.device.usb.serial.is_none());
            assert!(!report.redaction.serial_included);
            assert_eq!(
                report.profile.profile_id.as_deref(),
                Some("insta360-link-2c-pro")
            );
            assert!(!report.profile.writable);
            if report.device.mode == DeviceMode::Camera {
                assert!(
                    report
                        .video
                        .iter()
                        .all(|node| node.kind != VideoNodeKind::Unavailable)
                );
                assert!(report.video.iter().any(|node| !node.formats.is_empty()));
                assert!(report.video.iter().any(|node| !node.controls.is_empty()));
                assert!(!report.extension_units.is_empty());
                assert!(!report.audio.alsa.is_empty());
            }
        }

        let landscape = &reports["landscape"];
        let portrait = &reports["portrait"];
        let u_disk = &reports["u-disk"];

        assert_eq!(landscape.device.mode, DeviceMode::Camera);
        assert_eq!(portrait.device.mode, DeviceMode::Camera);
        assert_eq!(u_disk.device.mode, DeviceMode::UDisk);
        assert_eq!(landscape.device.stable_id, portrait.device.stable_id);
        assert_ne!(
            landscape.device.usb.descriptor_sha256,
            portrait.device.usb.descriptor_sha256
        );
        assert!(has_discrete_size(portrait, 1088, 1920));
        assert!(has_discrete_size(portrait, 2176, 3840));
        assert!(!has_discrete_size(landscape, 1088, 1920));

        assert_eq!(
            landscape
                .video
                .iter()
                .map(|node| node.controls.len())
                .sum::<usize>(),
            17
        );
        assert_eq!(landscape.extension_units.len(), 3);
        assert!(landscape.extension_units.iter().all(|unit| {
            unit.selectors.iter().all(|selector| {
                selector.length.is_some() && selector.info.is_some() && selector.issues.is_empty()
            })
        }));
        assert!(landscape.audio.alsa.iter().any(|pcm| {
            pcm.channels_min == 1
                && pcm.channels_max == 1
                && pcm.rate_min == 48_000
                && pcm.rate_max == 48_000
                && pcm.formats.iter().any(|format| format == "S16_LE")
        }));
        assert!(landscape.audio.pipewire_compiled);
        assert!(landscape.audio.pipewire_available);
        assert!(!landscape.audio.pipewire.is_empty());

        assert_eq!(u_disk.device.usb.vendor_id, 0x070a);
        assert_eq!(u_disk.device.usb.product_id, 0x4026);
        assert_eq!(u_disk.device.usb.device_revision, 0x0001);
        assert!(u_disk.video.is_empty());
        assert!(u_disk.extension_units.is_empty());
        assert!(u_disk.audio.alsa.is_empty());
        assert!(u_disk.device.volumes.iter().any(|volume| {
            volume.label.as_deref() == Some("LINK_2C_PRO")
                && volume.filesystem.as_deref() == Some("vfat")
                && !volume.mounted
        }));
    }

    fn has_discrete_size(report: &ProbeReport, width: u32, height: u32) -> bool {
        report
            .video
            .iter()
            .flat_map(|node| &node.formats)
            .any(|format| {
                format.sizes.iter().any(|size| {
                    matches!(
                        size,
                        FrameSize::Discrete {
                            width: observed_width,
                            height: observed_height,
                            ..
                        } if (*observed_width, *observed_height) == (width, height)
                    )
                })
            })
    }
}
