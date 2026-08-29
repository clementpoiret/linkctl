//! Linux USB identity, sysfs, udev, association, and selector support.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs, io,
    os::unix::{
        ffi::OsStringExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use link_core::{
    ErrorKind, LinkError,
    device::DeviceState,
    probe::{DeviceListEntry, DeviceMode, NodeAssociation, ProbeIssue, UsbIdentity, VolumeReport},
};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sha2::{Digest, Sha256};

/// A udev node associated with one physical USB device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredNode {
    /// Public device-node aliases.
    pub association: NodeAssociation,
    /// Sysfs path used only while collecting the report.
    pub syspath: PathBuf,
    /// udev sysname, such as `card3` or `video2`.
    pub sysname: String,
}

/// One physical USB device and every Linux node associated with it.
#[derive(Clone, Debug)]
pub struct DiscoveredDevice {
    /// Full identity, including serial when the device supplies one.
    pub identity: UsbIdentity,
    /// Raw USB descriptors used by the parser and fixture bundle.
    pub descriptors: Vec<u8>,
    /// Root USB device sysfs path.
    pub syspath: PathBuf,
    /// Video4Linux nodes.
    pub video_nodes: Vec<DiscoveredNode>,
    /// Media-controller nodes.
    pub media_nodes: Vec<DiscoveredNode>,
    /// ALSA sound nodes.
    pub sound_nodes: Vec<DiscoveredNode>,
    /// Block volumes for a mass-storage personality.
    pub volumes: Vec<VolumeReport>,
    /// Recoverable enumeration issues.
    pub issues: Vec<ProbeIssue>,
}

impl DiscoveredDevice {
    /// Classify the observed USB personality.
    #[must_use]
    pub fn mode(&self) -> DeviceMode {
        if !self.video_nodes.is_empty() {
            DeviceMode::Camera
        } else if !self.volumes.is_empty() {
            DeviceMode::UDisk
        } else {
            DeviceMode::Unknown
        }
    }

    /// Human-readable product name with a numeric fallback.
    #[must_use]
    pub fn model(&self) -> String {
        self.identity.product.clone().unwrap_or_else(|| {
            format!(
                "USB {:04x}:{:04x}",
                self.identity.vendor_id, self.identity.product_id
            )
        })
    }

    /// Convert to the serializable, privacy-filtered list record.
    #[must_use]
    pub fn list_entry(&self, include_serial: bool, profile_id: Option<String>) -> DeviceListEntry {
        let stable_id = self.identity.stable_id();
        let usb = if include_serial {
            self.identity.clone()
        } else {
            self.identity.without_serial()
        };

        DeviceListEntry {
            stable_id,
            model: self.model(),
            mode: self.mode(),
            usb,
            video_nodes: self
                .video_nodes
                .iter()
                .map(|node| node.association.clone())
                .collect(),
            media_nodes: self
                .media_nodes
                .iter()
                .map(|node| node.association.clone())
                .collect(),
            audio_nodes: self
                .sound_nodes
                .iter()
                .filter(|node| node.sysname.starts_with("controlC"))
                .map(|node| node.association.clone())
                .collect(),
            volumes: self.volumes.clone(),
            profile_id,
        }
    }

    /// ALSA card indexes observed below this USB device.
    #[must_use]
    pub fn alsa_card_indexes(&self) -> Vec<i32> {
        let mut indexes = BTreeSet::new();
        for node in &self.sound_nodes {
            if let Some(value) = node
                .sysname
                .strip_prefix("controlC")
                .or_else(|| node.sysname.strip_prefix("card"))
                && let Ok(index) = value.parse()
            {
                indexes.insert(index);
            }
        }
        indexes.into_iter().collect()
    }

    /// Return true when a selector identifies this device.
    #[must_use]
    pub fn matches_selector(&self, selector: &str) -> bool {
        if selector == self.identity.stable_id() {
            return true;
        }
        if self.identity.serial.as_deref() == Some(selector) {
            return true;
        }
        if selector
            .strip_prefix("usb:")
            .is_some_and(|value| value == self.identity.topology)
        {
            return true;
        }

        self.video_nodes.iter().any(|node| {
            node.association.path == selector
                || node.association.by_id.iter().any(|path| path == selector)
                || node.association.by_path.iter().any(|path| path == selector)
        })
    }

    /// Return the exact video node named by a path or stable alias selector.
    #[must_use]
    pub fn selected_video_node(&self, selector: &str) -> Option<&DiscoveredNode> {
        self.video_nodes.iter().find(|node| {
            node.association.path == selector
                || node.association.by_id.iter().any(|path| path == selector)
                || node.association.by_path.iter().any(|path| path == selector)
        })
    }
}

/// Nonblocking udev monitor used by production hotplug commands.
pub struct HotplugMonitor {
    socket: udev::MonitorSocket,
}

impl HotplugMonitor {
    /// Monitor USB device events. Higher layers rescan and diff normalized snapshots.
    pub fn new() -> Result<Self, LinkError> {
        let socket = udev::MonitorBuilder::new()
            .and_then(|builder| builder.match_subsystem("usb"))
            .and_then(udev::MonitorBuilder::listen)
            .map_err(udev_error)?;
        Ok(Self { socket })
    }

    /// Wait for at least one USB event, draining a burst before returning.
    pub fn wait(&self, timeout: Duration) -> Result<bool, LinkError> {
        let timeout = Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&self.socket, PollFlags::IN)];
        let ready = poll(&mut fds, Some(&timeout)).map_err(|error| {
            LinkError::new(ErrorKind::IoFailure, "failed while waiting for udev events")
                .with_detail("reason", error.to_string())
        })?;
        if ready == 0 {
            return Ok(false);
        }
        let mut observed = false;
        for _event in self.socket.iter() {
            observed = true;
        }
        Ok(observed)
    }
}

#[derive(Default)]
struct DeviceBuilder {
    identity: Option<UsbIdentity>,
    descriptors: Vec<u8>,
    syspath: PathBuf,
    video_nodes: Vec<DiscoveredNode>,
    media_nodes: Vec<DiscoveredNode>,
    sound_nodes: Vec<DiscoveredNode>,
    volumes: Vec<VolumeReport>,
    issues: Vec<ProbeIssue>,
}

/// Enumerate USB devices and group their video, media, sound, and block nodes.
pub fn enumerate_devices() -> Result<Vec<DiscoveredDevice>, LinkError> {
    let mut builders = enumerate_usb_roots()?;
    associate_nodes(&mut builders, "video4linux", NodeKind::Video)?;
    associate_nodes(&mut builders, "media", NodeKind::Media)?;
    associate_nodes(&mut builders, "sound", NodeKind::Sound)?;
    associate_nodes(&mut builders, "block", NodeKind::Block)?;

    let mut devices: Vec<_> = builders
        .into_values()
        .filter_map(|builder| {
            Some(DiscoveredDevice {
                identity: builder.identity?,
                descriptors: builder.descriptors,
                syspath: builder.syspath,
                video_nodes: sorted_nodes(builder.video_nodes),
                media_nodes: sorted_nodes(builder.media_nodes),
                sound_nodes: sorted_nodes(builder.sound_nodes),
                volumes: builder.volumes,
                issues: builder.issues,
            })
        })
        .collect();
    devices.sort_by(|left, right| {
        left.identity
            .topology
            .cmp(&right.identity.topology)
            .then_with(|| left.identity.product_id.cmp(&right.identity.product_id))
    });
    Ok(devices)
}

/// Resolve a selector against an already enumerated device set.
pub fn select_devices<'a>(
    devices: &'a [DiscoveredDevice],
    selector: &str,
) -> Result<Vec<&'a DiscoveredDevice>, LinkError> {
    if selector == "all" {
        return Ok(devices.iter().collect());
    }
    if selector.chars().all(|character| character.is_ascii_digit()) {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "bare numeric device indexes are prohibited; use index:<n>",
        ));
    }
    if let Some(index) = selector.strip_prefix("index:") {
        let index: usize = index.parse().map_err(|_| {
            LinkError::new(ErrorKind::InvalidInvocation, "invalid device index")
                .with_detail("selector", selector.to_owned())
        })?;
        return devices
            .get(index)
            .map(|device| vec![device])
            .ok_or_else(|| {
                LinkError::new(ErrorKind::DeviceNotFound, "device index does not exist")
                    .with_detail("selector", selector.to_owned())
            });
    }

    let matches: Vec<_> = devices
        .iter()
        .filter(|device| device.matches_selector(selector))
        .collect();
    match matches.len() {
        0 => Err(
            LinkError::new(ErrorKind::DeviceNotFound, "device selector did not match")
                .with_detail("selector", selector.to_owned()),
        ),
        1 => Ok(matches),
        count => Err(
            LinkError::new(ErrorKind::InvalidInvocation, "device selector is ambiguous")
                .with_detail("selector", selector.to_owned())
                .with_detail("matches", count as u64),
        ),
    }
}

fn enumerate_usb_roots() -> Result<BTreeMap<PathBuf, DeviceBuilder>, LinkError> {
    let mut enumerator = udev::Enumerator::new().map_err(udev_error)?;
    enumerator.match_subsystem("usb").map_err(udev_error)?;
    let mut builders = BTreeMap::new();

    for device in enumerator.scan_devices().map_err(udev_error)? {
        if device.devtype() != Some(OsStr::new("usb_device")) {
            continue;
        }
        let Some(vendor_id) = parse_hex_attribute(&device, "idVendor") else {
            continue;
        };
        let Some(product_id) = parse_hex_attribute(&device, "idProduct") else {
            continue;
        };
        let device_revision = parse_hex_attribute(&device, "bcdDevice").unwrap_or_default();
        let syspath = device.syspath().to_path_buf();
        let topology = device.sysname().to_string_lossy().into_owned();
        let mut issues = Vec::new();
        let descriptors = match fs::read(syspath.join("descriptors")) {
            Ok(bytes) => bytes,
            Err(error) => {
                issues.push(ProbeIssue::new(
                    "usb",
                    io_error_code(&error),
                    format!("failed to read USB descriptors: {error}"),
                ));
                Vec::new()
            }
        };
        let descriptor_sha256 = sha256(&descriptors);
        let identity = UsbIdentity {
            vendor_id,
            product_id,
            device_revision,
            manufacturer: text_attribute(&device, "manufacturer"),
            product: text_attribute(&device, "product"),
            serial: text_attribute(&device, "serial").filter(|value| !value.is_empty()),
            topology,
            descriptor_sha256,
        };
        builders.insert(
            syspath.clone(),
            DeviceBuilder {
                identity: Some(identity),
                descriptors,
                syspath,
                issues,
                ..DeviceBuilder::default()
            },
        );
    }
    Ok(builders)
}

#[derive(Clone, Copy)]
enum NodeKind {
    Video,
    Media,
    Sound,
    Block,
}

fn associate_nodes(
    builders: &mut BTreeMap<PathBuf, DeviceBuilder>,
    subsystem: &str,
    kind: NodeKind,
) -> Result<(), LinkError> {
    let mut enumerator = udev::Enumerator::new().map_err(udev_error)?;
    enumerator.match_subsystem(subsystem).map_err(udev_error)?;
    for device in enumerator.scan_devices().map_err(udev_error)? {
        let Some(parent) = device
            .parent_with_subsystem_devtype("usb", "usb_device")
            .map_err(udev_error)?
        else {
            continue;
        };
        let Some(builder) = builders.get_mut(parent.syspath()) else {
            continue;
        };
        match kind {
            NodeKind::Block => {
                if let Some(volume) = volume_report(&device) {
                    builder.volumes.push(volume);
                }
            }
            NodeKind::Video | NodeKind::Media | NodeKind::Sound => {
                if let Some(node) = discovered_node(&device) {
                    match kind {
                        NodeKind::Video => builder.video_nodes.push(node),
                        NodeKind::Media => builder.media_nodes.push(node),
                        NodeKind::Sound => builder.sound_nodes.push(node),
                        NodeKind::Block => unreachable!(),
                    }
                }
            }
        }
    }
    Ok(())
}

fn discovered_node(device: &udev::Device) -> Option<DiscoveredNode> {
    let path = device.devnode()?.to_string_lossy().into_owned();
    let mut by_id = Vec::new();
    let mut by_path = Vec::new();
    if let Some(links) = device.property_value("DEVLINKS") {
        for link in links.to_string_lossy().split_ascii_whitespace() {
            if link.contains("/by-id/") {
                by_id.push(link.to_owned());
            } else if link.contains("/by-path/") {
                by_path.push(link.to_owned());
            }
        }
    }
    by_id.sort();
    by_path.sort();
    Some(DiscoveredNode {
        association: NodeAssociation {
            path,
            by_id,
            by_path,
        },
        syspath: device.syspath().to_path_buf(),
        sysname: device.sysname().to_string_lossy().into_owned(),
    })
}

fn volume_report(device: &udev::Device) -> Option<VolumeReport> {
    let path = device.devnode()?.to_string_lossy().into_owned();
    let mounted = mounted_volume_path(Path::new(&path))
        .ok()
        .flatten()
        .is_some();
    Some(VolumeReport {
        path,
        label: property_text(device, "ID_FS_LABEL"),
        filesystem: property_text(device, "ID_FS_TYPE"),
        mounted,
    })
}

/// Resolve the current mount point for a block device using its kernel device number.
pub fn mounted_volume_path(block_device: &Path) -> Result<Option<PathBuf>, LinkError> {
    let metadata = match fs::metadata(block_device) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                LinkError::new(ErrorKind::IoFailure, "failed to inspect block device")
                    .with_detail("path", block_device.display().to_string())
                    .with_detail("reason", error.to_string()),
            );
        }
    };
    if !metadata.file_type().is_block_device() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "volume path is not a block device",
        )
        .with_detail("path", block_device.display().to_string()));
    }
    let major = rustix::fs::major(metadata.rdev());
    let minor = rustix::fs::minor(metadata.rdev());
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to read process mount table")
            .with_detail("reason", error.to_string())
    })?;
    Ok(parse_mountinfo(&mountinfo, major, minor))
}

fn parse_mountinfo(mountinfo: &str, expected_major: u32, expected_minor: u32) -> Option<PathBuf> {
    for line in mountinfo.lines() {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 {
            continue;
        }
        let Some((major, minor)) = fields[2].split_once(':') else {
            continue;
        };
        if fields[3] != "/"
            || major.parse::<u32>().ok() != Some(expected_major)
            || minor.parse::<u32>().ok() != Some(expected_minor)
        {
            continue;
        }
        return decode_mount_field(fields[4]);
    }
    None
}

fn decode_mount_field(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let digits = &bytes[index + 1..index + 4];
            if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                let value = u16::from(digits[0] - b'0') * 64
                    + u16::from(digits[1] - b'0') * 8
                    + u16::from(digits[2] - b'0');
                if let Ok(byte) = u8::try_from(value) {
                    decoded.push(byte);
                    index += 4;
                    continue;
                }
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
}

fn parse_hex_attribute(device: &udev::Device, name: &str) -> Option<u16> {
    let value = device.attribute_value(name)?.to_str()?;
    u16::from_str_radix(value.trim(), 16).ok()
}

fn text_attribute(device: &udev::Device, name: &str) -> Option<String> {
    device
        .attribute_value(name)
        .map(|value| value.to_string_lossy().trim().to_owned())
}

fn property_text(device: &udev::Device, name: &str) -> Option<String> {
    device
        .property_value(name)
        .map(|value| value.to_string_lossy().trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn sorted_nodes(mut nodes: Vec<DiscoveredNode>) -> Vec<DiscoveredNode> {
    nodes.sort_by(|left, right| left.association.path.cmp(&right.association.path));
    nodes.dedup_by(|left, right| left.association.path == right.association.path);
    nodes
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn udev_error(error: io::Error) -> LinkError {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, "failed to enumerate Linux devices")
        .with_detail("reason", error.to_string())
}

fn io_error_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission-denied",
        io::ErrorKind::NotFound => "not-found",
        _ => "io-failure",
    }
}

/// Return the kernel release without invoking an external command.
#[must_use]
pub fn kernel_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|_| "unknown".into())
}

/// Inspect whether the current process can open an associated video node for controls.
#[must_use]
pub fn availability_state(device: &DiscoveredDevice) -> DeviceState {
    if device.mode() == DeviceMode::UDisk {
        return DeviceState::Maintenance;
    }
    let Some(node) = device.video_nodes.first() else {
        return DeviceState::Unavailable;
    };
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&node.association.path)
    {
        Ok(_) => DeviceState::Ready,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            DeviceState::PermissionDenied
        }
        Err(error) if error.raw_os_error() == Some(16) => DeviceState::Busy,
        Err(_) => DeviceState::Unavailable,
    }
}

/// Return true for UVC devices and the camera's known USB personalities.
#[must_use]
pub fn is_listable(device: &DiscoveredDevice) -> bool {
    !device.video_nodes.is_empty()
        || device.identity.vendor_id == 0x2e1a
        || (device.identity.vendor_id, device.identity.product_id) == (0x070a, 0x4026)
}

/// Validate that a path names a newly creatable bundle directory parent.
pub fn validate_bundle_parent(path: &Path) -> Result<&Path, LinkError> {
    if path.exists() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "probe bundle destination already exists",
        )
        .with_detail("path", path.display().to_string()));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(LinkError::new(
            ErrorKind::IoFailure,
            "probe bundle parent directory does not exist",
        )
        .with_detail("path", parent.display().to_string()));
    }
    Ok(parent)
}

#[cfg(test)]
mod tests {
    use link_core::probe::UsbIdentity;

    use super::{DiscoveredDevice, is_listable, parse_mountinfo};

    fn device() -> DiscoveredDevice {
        DiscoveredDevice {
            identity: UsbIdentity {
                vendor_id: 0x2e1a,
                product_id: 0x4c05,
                device_revision: 0x0200,
                manufacturer: Some("Insta360".into()),
                product: Some("Insta360 Link 2C Pro".into()),
                serial: Some("secret".into()),
                topology: "1-2.1".into(),
                descriptor_sha256: "00".repeat(32),
            },
            descriptors: Vec::new(),
            syspath: "/sys/example".into(),
            video_nodes: Vec::new(),
            media_nodes: Vec::new(),
            sound_nodes: Vec::new(),
            volumes: Vec::new(),
            issues: Vec::new(),
        }
    }

    #[test]
    fn selectors_accept_stable_serial_and_topology_forms() {
        let device = device();
        assert!(device.matches_selector(&device.identity.stable_id()));
        assert!(device.matches_selector("secret"));
        assert!(device.matches_selector("usb:1-2.1"));
        assert!(!device.matches_selector("1-2.1"));
    }

    #[test]
    fn exact_video_selectors_retain_the_selected_node() {
        let mut device = device();
        device.video_nodes.push(super::DiscoveredNode {
            association: link_core::probe::NodeAssociation {
                path: "/dev/video8".into(),
                by_id: vec!["/dev/v4l/by-id/example".into()],
                by_path: Vec::new(),
            },
            syspath: "/sys/example/video8".into(),
            sysname: "video8".into(),
        });
        assert_eq!(
            device
                .selected_video_node("/dev/v4l/by-id/example")
                .unwrap()
                .association
                .path,
            "/dev/video8"
        );
    }

    #[test]
    fn public_entry_redacts_serial_by_default() {
        let entry = device().list_entry(false, None);
        assert!(entry.usb.serial.is_none());
    }

    #[test]
    fn known_u_disk_personality_is_listable_without_video_nodes() {
        let mut device = device();
        device.identity.vendor_id = 0x070a;
        device.identity.product_id = 0x4026;
        assert!(is_listable(&device));
    }

    #[test]
    fn mountinfo_resolution_uses_device_numbers_and_decodes_paths() {
        let mountinfo = concat!(
            "33 25 8:1 / /run/media/test/LINK\\0402C rw,nosuid - vfat /dev/sda1 rw\n",
            "34 25 8:2 / /run/media/test/other rw,nosuid - vfat /dev/sda2 rw\n",
        );
        assert_eq!(
            parse_mountinfo(mountinfo, 8, 1).unwrap(),
            std::path::PathBuf::from("/run/media/test/LINK 2C")
        );
        assert!(parse_mountinfo(mountinfo, 8, 3).is_none());
    }
}
