//! UVC descriptor parsing, exact Extension Unit queries, and research artifacts.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use link_core::{
    ErrorKind, LinkError,
    paths::AppPaths,
    probe::{ProbeIssue, XuEntityReport, XuSelectorReport},
};
use link_profiles::{AuthorizedControl, SnapshotPolicy, VendorProfile, decode_control};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const USB_DT_INTERFACE: u8 = 0x04;
const USB_DT_INTERFACE_ASSOCIATION: u8 = 0x0b;
const USB_DT_CS_INTERFACE: u8 = 0x24;
const USB_CLASS_VIDEO: u8 = 0x0e;
const UVC_SC_VIDEOCONTROL: u8 = 0x01;
const UVC_VC_HEADER: u8 = 0x01;
const UVC_VC_INPUT_TERMINAL: u8 = 0x02;
const UVC_VC_OUTPUT_TERMINAL: u8 = 0x03;
const UVC_VC_SELECTOR_UNIT: u8 = 0x04;
const UVC_VC_PROCESSING_UNIT: u8 = 0x05;
const UVC_VC_EXTENSION_UNIT: u8 = 0x06;
const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_LEN: u8 = 0x85;
const UVC_GET_INFO: u8 = 0x86;

pub const XU_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// One descriptor retained exactly as observed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawDescriptor {
    pub offset: usize,
    pub descriptor_type: u8,
    pub bytes_hex: String,
}

/// Parsed VideoControl descriptor kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoControlEntityKind {
    Header,
    InputTerminal,
    OutputTerminal,
    SelectorUnit,
    ProcessingUnit,
    ExtensionUnit,
}

/// Normalized fields common to VideoControl entities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoControlEntity {
    pub kind: VideoControlEntityKind,
    pub interface_number: u8,
    pub descriptor_offset: usize,
    pub entity_id: Option<u8>,
    pub source_ids: Vec<u8>,
    pub control_bitmap: Option<String>,
    pub raw_hex: String,
}

/// USB interface-association metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InterfaceAssociation {
    pub descriptor_offset: usize,
    pub first_interface: u8,
    pub interface_count: u8,
    pub function_class: u8,
    pub function_subclass: u8,
    pub function_protocol: u8,
    pub raw_hex: String,
}

/// Parsed inventory with unknown descriptors retained for future use.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DescriptorInventory {
    pub interface_associations: Vec<InterfaceAssociation>,
    pub video_control_entities: Vec<VideoControlEntity>,
    pub extension_units: Vec<XuEntityReport>,
    pub unknown_descriptors: Vec<RawDescriptor>,
}

/// Parse USB descriptors and extract the complete VideoControl entity graph.
pub fn parse_descriptors(bytes: &[u8]) -> Result<DescriptorInventory, LinkError> {
    let mut offset = 0_usize;
    let mut video_control_interface = None;
    let mut interface_associations = Vec::new();
    let mut video_control_entities = Vec::new();
    let mut extension_units = Vec::new();
    let mut unknown_descriptors = Vec::new();

    while offset < bytes.len() {
        let descriptor = descriptor_at(bytes, offset)?;
        let descriptor_type = descriptor[1];
        match descriptor_type {
            USB_DT_INTERFACE => {
                if descriptor.len() < 9 {
                    return Err(descriptor_error(
                        offset,
                        "USB interface descriptor is truncated",
                    ));
                }
                video_control_interface = (descriptor[5] == USB_CLASS_VIDEO
                    && descriptor[6] == UVC_SC_VIDEOCONTROL
                    && descriptor[3] == 0)
                    .then_some(descriptor[2]);
                unknown_descriptors.push(raw_descriptor(offset, descriptor));
            }
            USB_DT_INTERFACE_ASSOCIATION => {
                if descriptor.len() < 8 {
                    return Err(descriptor_error(
                        offset,
                        "interface association descriptor is truncated",
                    ));
                }
                interface_associations.push(InterfaceAssociation {
                    descriptor_offset: offset,
                    first_interface: descriptor[2],
                    interface_count: descriptor[3],
                    function_class: descriptor[4],
                    function_subclass: descriptor[5],
                    function_protocol: descriptor[6],
                    raw_hex: lowercase_hex(descriptor),
                });
            }
            USB_DT_CS_INTERFACE if video_control_interface.is_some() => {
                let interface_number = video_control_interface.expect("checked");
                match descriptor.get(2).copied() {
                    Some(UVC_VC_EXTENSION_UNIT) => {
                        let (entity, report) =
                            parse_extension_unit(descriptor, offset, interface_number)?;
                        video_control_entities.push(entity);
                        extension_units.push(report);
                    }
                    Some(
                        subtype @ (UVC_VC_HEADER
                        | UVC_VC_INPUT_TERMINAL
                        | UVC_VC_OUTPUT_TERMINAL
                        | UVC_VC_SELECTOR_UNIT
                        | UVC_VC_PROCESSING_UNIT),
                    ) => video_control_entities.push(parse_standard_entity(
                        descriptor,
                        offset,
                        interface_number,
                        subtype,
                    )?),
                    _ => unknown_descriptors.push(raw_descriptor(offset, descriptor)),
                }
            }
            _ => unknown_descriptors.push(raw_descriptor(offset, descriptor)),
        }
        offset += descriptor.len();
    }

    Ok(DescriptorInventory {
        interface_associations,
        video_control_entities,
        extension_units,
        unknown_descriptors,
    })
}

fn descriptor_at(bytes: &[u8], offset: usize) -> Result<&[u8], LinkError> {
    let length = usize::from(bytes[offset]);
    if length == 0 {
        return Err(descriptor_error(offset, "descriptor length is zero"));
    }
    if length < 2 {
        return Err(descriptor_error(
            offset,
            "descriptor is shorter than its header",
        ));
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| descriptor_error(offset, "descriptor length overflows the input"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| descriptor_error(offset, "descriptor extends beyond input"))
}

fn parse_standard_entity(
    descriptor: &[u8],
    offset: usize,
    interface_number: u8,
    subtype: u8,
) -> Result<VideoControlEntity, LinkError> {
    if descriptor.len() < 3 {
        return Err(descriptor_error(
            offset,
            "VideoControl descriptor is truncated",
        ));
    }
    let (kind, entity_id, source_ids, control_bitmap) = match subtype {
        UVC_VC_HEADER => {
            if descriptor.len() < 12 {
                return Err(descriptor_error(offset, "VideoControl header is truncated"));
            }
            let interfaces = usize::from(descriptor[11]);
            if descriptor.get(12..12 + interfaces).is_none() {
                return Err(descriptor_error(
                    offset,
                    "VideoControl interface collection is truncated",
                ));
            }
            (VideoControlEntityKind::Header, None, Vec::new(), None)
        }
        UVC_VC_INPUT_TERMINAL => {
            if descriptor.len() < 8 {
                return Err(descriptor_error(
                    offset,
                    "input terminal descriptor is truncated",
                ));
            }
            let terminal_type = u16::from_le_bytes([descriptor[4], descriptor[5]]);
            let bitmap = if terminal_type == 0x0201 {
                if descriptor.len() < 15 {
                    return Err(descriptor_error(
                        offset,
                        "camera input terminal descriptor is truncated",
                    ));
                }
                let size = usize::from(descriptor[14]);
                Some(lowercase_hex(descriptor.get(15..15 + size).ok_or_else(
                    || descriptor_error(offset, "input terminal control bitmap is truncated"),
                )?))
            } else {
                None
            };
            (
                VideoControlEntityKind::InputTerminal,
                Some(descriptor[3]),
                Vec::new(),
                bitmap,
            )
        }
        UVC_VC_OUTPUT_TERMINAL => {
            if descriptor.len() < 9 {
                return Err(descriptor_error(
                    offset,
                    "output terminal descriptor is truncated",
                ));
            }
            (
                VideoControlEntityKind::OutputTerminal,
                Some(descriptor[3]),
                vec![descriptor[7]],
                None,
            )
        }
        UVC_VC_SELECTOR_UNIT => {
            if descriptor.len() < 6 {
                return Err(descriptor_error(
                    offset,
                    "selector unit descriptor is truncated",
                ));
            }
            let pins = usize::from(descriptor[4]);
            let sources_end = 5 + pins;
            let sources = descriptor
                .get(5..sources_end)
                .ok_or_else(|| descriptor_error(offset, "selector source list is truncated"))?
                .to_vec();
            if descriptor.get(sources_end).is_none() {
                return Err(descriptor_error(
                    offset,
                    "selector string index is truncated",
                ));
            }
            (
                VideoControlEntityKind::SelectorUnit,
                Some(descriptor[3]),
                sources,
                None,
            )
        }
        UVC_VC_PROCESSING_UNIT => {
            if descriptor.len() < 9 {
                return Err(descriptor_error(
                    offset,
                    "processing unit descriptor is truncated",
                ));
            }
            let size = usize::from(descriptor[7]);
            let bitmap = descriptor.get(8..8 + size).ok_or_else(|| {
                descriptor_error(offset, "processing control bitmap is truncated")
            })?;
            if descriptor.get(8 + size).is_none() {
                return Err(descriptor_error(
                    offset,
                    "processing string index is truncated",
                ));
            }
            (
                VideoControlEntityKind::ProcessingUnit,
                Some(descriptor[3]),
                vec![descriptor[4]],
                Some(lowercase_hex(bitmap)),
            )
        }
        _ => unreachable!("caller filters subtypes"),
    };
    Ok(VideoControlEntity {
        kind,
        interface_number,
        descriptor_offset: offset,
        entity_id,
        source_ids,
        control_bitmap,
        raw_hex: lowercase_hex(descriptor),
    })
}

fn parse_extension_unit(
    descriptor: &[u8],
    descriptor_offset: usize,
    interface_number: u8,
) -> Result<(VideoControlEntity, XuEntityReport), LinkError> {
    if descriptor.len() < 24 {
        return Err(descriptor_error(
            descriptor_offset,
            "Extension Unit descriptor is too short",
        ));
    }
    let num_pins = usize::from(descriptor[21]);
    let control_size_offset = 22_usize
        .checked_add(num_pins)
        .ok_or_else(|| descriptor_error(descriptor_offset, "Extension Unit pin count overflows"))?;
    let Some(&control_size) = descriptor.get(control_size_offset) else {
        return Err(descriptor_error(
            descriptor_offset,
            "Extension Unit source-pin list is truncated",
        ));
    };
    let bitmap_start = control_size_offset + 1;
    let bitmap_end = bitmap_start
        .checked_add(usize::from(control_size))
        .ok_or_else(|| descriptor_error(descriptor_offset, "control bitmap length overflows"))?;
    if bitmap_end >= descriptor.len() {
        return Err(descriptor_error(
            descriptor_offset,
            "Extension Unit control bitmap or string index is truncated",
        ));
    }

    let source_ids = descriptor[22..control_size_offset].to_vec();
    let bitmap = &descriptor[bitmap_start..bitmap_end];
    let mut selectors = Vec::new();
    for (byte_index, byte) in bitmap.iter().copied().enumerate() {
        for bit in 0_u8..8 {
            if byte & (1_u8 << bit) != 0 {
                let selector = byte_index
                    .checked_mul(8)
                    .and_then(|base| base.checked_add(usize::from(bit) + 1))
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or_else(|| {
                        descriptor_error(descriptor_offset, "selector number exceeds UVC range")
                    })?;
                selectors.push(XuSelectorReport {
                    selector,
                    length: None,
                    info: None,
                    get_supported: None,
                    set_supported: None,
                    issues: Vec::new(),
                });
            }
        }
    }
    let guid = canonical_guid(&descriptor[4..20]);
    let report = XuEntityReport {
        unit_id: descriptor[3],
        guid,
        num_controls: descriptor[20],
        source_ids: source_ids.clone(),
        control_bitmap: lowercase_hex(bitmap),
        selectors,
        descriptor_offset,
    };
    let entity = VideoControlEntity {
        kind: VideoControlEntityKind::ExtensionUnit,
        interface_number,
        descriptor_offset,
        entity_id: Some(descriptor[3]),
        source_ids,
        control_bitmap: Some(lowercase_hex(bitmap)),
        raw_hex: lowercase_hex(descriptor),
    };
    Ok((entity, report))
}

fn raw_descriptor(offset: usize, descriptor: &[u8]) -> RawDescriptor {
    RawDescriptor {
        offset,
        descriptor_type: descriptor[1],
        bytes_hex: lowercase_hex(descriptor),
    }
}

/// Runtime-resolved XU address.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct XuAddress {
    pub unit: u8,
    pub selector: u8,
}

/// Fresh capability metadata returned before every payload query.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct XuCapabilities {
    pub length: u16,
    pub info: u8,
    pub get_supported: bool,
    pub set_supported: bool,
}

/// Exact XU payload with transport and optional profile decoding.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct XuValue {
    pub guid: String,
    pub unit: u8,
    pub selector: u8,
    pub length: u16,
    pub info: u8,
    pub hex: String,
    pub base64: String,
    pub decoded: BTreeMap<String, Value>,
}

trait QueryTransport {
    fn query(&self, unit: u8, selector: u8, query: u8, data: &mut [u8]) -> io::Result<()>;
}

struct IoctlTransport {
    file: File,
}

impl QueryTransport for IoctlTransport {
    fn query(&self, unit: u8, selector: u8, query: u8, data: &mut [u8]) -> io::Result<()> {
        abi::query(self.file.as_fd(), unit, selector, query, data)
    }
}

/// One open file descriptor used for a complete XU transaction.
pub struct XuSession {
    transport: IoctlTransport,
    path: PathBuf,
}

impl XuSession {
    pub fn open_read(path: &Path) -> Result<Self, LinkError> {
        Self::open(path, false)
    }

    pub fn open_write(path: &Path) -> Result<Self, LinkError> {
        Self::open(path, true)
    }

    fn open(path: &Path, writable: bool) -> Result<Self, LinkError> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(writable)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        let file = options
            .open(path)
            .map_err(|error| open_error(path, &error))?;
        Ok(Self {
            transport: IoctlTransport { file },
            path: path.to_owned(),
        })
    }

    pub fn capabilities(&self, address: XuAddress) -> Result<XuCapabilities, LinkError> {
        capabilities_with_transport(&self.transport, address)
    }

    pub fn get_current(
        &self,
        address: XuAddress,
        asserted_length: Option<u16>,
    ) -> Result<(XuCapabilities, Vec<u8>), LinkError> {
        get_current_with_transport(&self.transport, address, asserted_length)
    }

    pub fn set_profiled(
        &self,
        address: XuAddress,
        authorization: AuthorizedControl<'_>,
        payload: &[u8],
    ) -> Result<XuCapabilities, LinkError> {
        let control = authorization.control();
        if address.selector != control.selector {
            return Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "authorized profile control does not match the XU selector",
            ));
        }
        set_current_with_transport(&self.transport, address, control.length, payload)
    }

    #[cfg(feature = "research")]
    pub fn raw_set(
        &self,
        address: XuAddress,
        expected_length: u16,
        payload: &[u8],
    ) -> Result<XuCapabilities, LinkError> {
        set_current_with_transport(&self.transport, address, expected_length, payload)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn capabilities_with_transport<T: QueryTransport>(
    transport: &T,
    address: XuAddress,
) -> Result<XuCapabilities, LinkError> {
    let mut info = [0_u8; 1];
    transport
        .query(address.unit, address.selector, UVC_GET_INFO, &mut info)
        .map_err(|error| query_error("get-info", address, &error))?;
    let mut length = [0_u8; 2];
    transport
        .query(address.unit, address.selector, UVC_GET_LEN, &mut length)
        .map_err(|error| query_error("get-len", address, &error))?;
    let length = u16::from_le_bytes(length);
    if length == 0 {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "XU selector reported a zero payload length",
        ));
    }
    Ok(XuCapabilities {
        length,
        info: info[0],
        get_supported: info[0] & 0x01 != 0,
        set_supported: info[0] & 0x02 != 0,
    })
}

fn get_current_with_transport<T: QueryTransport>(
    transport: &T,
    address: XuAddress,
    asserted_length: Option<u16>,
) -> Result<(XuCapabilities, Vec<u8>), LinkError> {
    let capabilities = capabilities_with_transport(transport, address)?;
    if !capabilities.get_supported {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "XU selector does not advertise GET support",
        ));
    }
    if asserted_length.is_some_and(|value| value != capabilities.length) {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "asserted XU length does not match GET_LEN",
        )
        .with_detail("asserted", u64::from(asserted_length.expect("checked")))
        .with_detail("observed", u64::from(capabilities.length)));
    }
    let mut payload = vec![0_u8; usize::from(capabilities.length)];
    transport
        .query(address.unit, address.selector, UVC_GET_CUR, &mut payload)
        .map_err(|error| query_error("get-current", address, &error))?;
    Ok((capabilities, payload))
}

fn set_current_with_transport<T: QueryTransport>(
    transport: &T,
    address: XuAddress,
    expected_length: u16,
    payload: &[u8],
) -> Result<XuCapabilities, LinkError> {
    let capabilities = capabilities_with_transport(transport, address)?;
    if !capabilities.set_supported {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "XU selector does not advertise SET support",
        ));
    }
    if capabilities.length != expected_length || payload.len() != usize::from(expected_length) {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "XU write payload does not match GET_LEN",
        )
        .with_detail("profile", u64::from(expected_length))
        .with_detail("observed", u64::from(capabilities.length))
        .with_detail("payload", payload.len() as u64));
    }
    let mut copy = payload.to_vec();
    transport
        .query(address.unit, address.selector, UVC_SET_CUR, &mut copy)
        .map_err(|error| query_error("set-current", address, &error))?;
    Ok(capabilities)
}

/// Parse descriptors and issue only GET_INFO and GET_LEN for advertised selectors.
pub fn inventory(video_node: &Path, descriptors: &[u8]) -> Result<Vec<XuEntityReport>, LinkError> {
    let parsed = parse_descriptors(descriptors)?;
    let session = XuSession::open_read(video_node)?;
    Ok(inventory_with_transport(
        parsed.extension_units,
        &session.transport,
    ))
}

fn inventory_with_transport<T: QueryTransport>(
    mut entities: Vec<XuEntityReport>,
    transport: &T,
) -> Vec<XuEntityReport> {
    for entity in &mut entities {
        for selector in &mut entity.selectors {
            let address = XuAddress {
                unit: entity.unit_id,
                selector: selector.selector,
            };
            match capabilities_with_transport(transport, address) {
                Ok(capabilities) => {
                    selector.length = Some(capabilities.length);
                    selector.info = Some(capabilities.info);
                    selector.get_supported = Some(capabilities.get_supported);
                    selector.set_supported = Some(capabilities.set_supported);
                }
                Err(error) => selector.issues.push(ProbeIssue::new(
                    "xu",
                    error.kind().code(),
                    error.message(),
                )),
            }
        }
    }
    entities
}

/// Resolve a GUID or explicit unit and prove that the selector is advertised.
pub fn resolve_address(
    inventory: &DescriptorInventory,
    guid: Option<&str>,
    unit: Option<u8>,
    selector: u8,
) -> Result<(String, XuAddress), LinkError> {
    if selector == 0 || guid.is_some() == unit.is_some() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "select exactly one XU GUID or unit and a non-zero selector",
        ));
    }
    let entity = inventory.extension_units.iter().find(|entity| {
        guid.is_some_and(|value| entity.guid.eq_ignore_ascii_case(value))
            || unit == Some(entity.unit_id)
    });
    let entity = entity.ok_or_else(|| {
        LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "requested Extension Unit is not present in the descriptor graph",
        )
    })?;
    if !entity
        .selectors
        .iter()
        .any(|candidate| candidate.selector == selector)
    {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "requested selector is not advertised by the Extension Unit",
        ));
    }
    Ok((
        entity.guid.clone(),
        XuAddress {
            unit: entity.unit_id,
            selector,
        },
    ))
}

/// Read one selector and decode every matching profile control.
pub fn read_value(
    session: &XuSession,
    inventory: &DescriptorInventory,
    guid: Option<&str>,
    unit: Option<u8>,
    selector: u8,
    asserted_length: Option<u16>,
    profile: Option<&VendorProfile>,
) -> Result<XuValue, LinkError> {
    let (guid, address) = resolve_address(inventory, guid, unit, selector)?;
    let (capabilities, payload) = session.get_current(address, asserted_length)?;
    let mut decoded = BTreeMap::new();
    if let Some(profile) = profile {
        for control in profile.controls_for_selector(&guid, selector) {
            if control.readable && control.length == capabilities.length {
                decoded.insert(control.name.clone(), decode_control(control, &payload)?);
            }
        }
    }
    Ok(XuValue {
        guid,
        unit: address.unit,
        selector,
        length: capabilities.length,
        info: capabilities.info,
        hex: lowercase_hex(&payload),
        base64: BASE64.encode(&payload),
        decoded,
    })
}

/// Redacted device identity embedded in research snapshots.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotDevice {
    pub stable_id: String,
    pub model: String,
    pub usb_vid: u16,
    pub usb_pid: u16,
    pub bcd_device: u16,
    pub descriptor_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotProfile {
    pub profile_id: String,
    pub checksum: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSelector {
    pub guid: String,
    pub unit: u8,
    pub selector: u8,
    pub length: Option<u16>,
    pub info: Option<u8>,
    pub samples_base64: Vec<String>,
    pub volatility_mask_base64: Option<String>,
    pub decoded: BTreeMap<String, Value>,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct XuSnapshot {
    pub schema_version: u32,
    pub captured_unix_ms: u128,
    pub application_version: String,
    pub device: SnapshotDevice,
    pub profile: Option<SnapshotProfile>,
    pub stream_state: String,
    pub notes: Vec<String>,
    pub standard_controls: BTreeMap<String, Value>,
    pub selectors: Vec<SnapshotSelector>,
}

pub struct SnapshotRequest<'a> {
    pub captured_unix_ms: u128,
    pub application_version: &'a str,
    pub device: SnapshotDevice,
    pub profile_metadata: Option<SnapshotProfile>,
    pub profile: Option<&'a VendorProfile>,
    pub stream_state: &'a str,
    pub notes: Vec<String>,
    pub standard_controls: BTreeMap<String, Value>,
    pub samples: u8,
    pub interval: Duration,
    pub include_volatile: bool,
}

/// Capture repeated exact payloads and compute per-byte volatility masks.
pub fn capture_snapshot(
    session: &XuSession,
    inventory: &DescriptorInventory,
    request: SnapshotRequest<'_>,
) -> Result<XuSnapshot, LinkError> {
    if !(1..=64).contains(&request.samples) {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "snapshot samples must be between 1 and 64",
        ));
    }
    let mut selectors = Vec::new();
    for entity in &inventory.extension_units {
        for selector in &entity.selectors {
            let profile_controls = request.profile.map_or_else(Vec::new, |profile| {
                profile.controls_for_selector(&entity.guid, selector.selector)
            });
            let policy = profile_controls
                .iter()
                .map(|control| control.snapshot)
                .max_by_key(|policy| match policy {
                    SnapshotPolicy::Include => 0,
                    SnapshotPolicy::Volatile => 1,
                    SnapshotPolicy::Exclude => 2,
                });
            if policy == Some(SnapshotPolicy::Exclude)
                || (!request.include_volatile && policy == Some(SnapshotPolicy::Volatile))
            {
                continue;
            }
            let address = XuAddress {
                unit: entity.unit_id,
                selector: selector.selector,
            };
            let mut samples = Vec::new();
            let mut capabilities = None;
            let mut issue = None;
            for index in 0..request.samples {
                match session.get_current(address, None) {
                    Ok((observed, payload)) => {
                        if capabilities.is_some_and(|value: XuCapabilities| value != observed) {
                            issue = Some("selector capabilities changed during sampling".into());
                            break;
                        }
                        capabilities = Some(observed);
                        samples.push(payload);
                    }
                    Err(error) => {
                        issue = Some(format!("{}: {}", error.kind().code(), error.message()));
                        break;
                    }
                }
                if index + 1 < request.samples {
                    thread::sleep(request.interval);
                }
            }
            let volatility = volatility_mask(&samples);
            let mut decoded = BTreeMap::new();
            if let Some(payload) = samples.last() {
                for control in profile_controls {
                    if control.readable && control.length as usize == payload.len() {
                        decoded.insert(control.name.clone(), decode_control(control, payload)?);
                    }
                }
            }
            selectors.push(SnapshotSelector {
                guid: entity.guid.clone(),
                unit: entity.unit_id,
                selector: selector.selector,
                length: capabilities.map(|value| value.length),
                info: capabilities.map(|value| value.info),
                samples_base64: samples.iter().map(|sample| BASE64.encode(sample)).collect(),
                volatility_mask_base64: volatility.as_ref().map(|mask| BASE64.encode(mask)),
                decoded,
                issue,
            });
        }
    }
    Ok(XuSnapshot {
        schema_version: XU_SNAPSHOT_SCHEMA_VERSION,
        captured_unix_ms: request.captured_unix_ms,
        application_version: request.application_version.to_owned(),
        device: request.device,
        profile: request.profile_metadata,
        stream_state: request.stream_state.to_owned(),
        notes: request.notes,
        standard_controls: request.standard_controls,
        selectors,
    })
}

fn volatility_mask(samples: &[Vec<u8>]) -> Option<Vec<u8>> {
    let first = samples.first()?;
    if samples.iter().any(|sample| sample.len() != first.len()) {
        return None;
    }
    let mut mask = vec![0_u8; first.len()];
    for sample in &samples[1..] {
        for (index, (baseline, observed)) in first.iter().zip(sample).enumerate() {
            mask[index] |= baseline ^ observed;
        }
    }
    Some(mask)
}

/// One changed payload byte and the exact changed bit positions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ByteDiff {
    pub offset: usize,
    pub before: u8,
    pub after: u8,
    pub xor: u8,
    pub changed_bits: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SelectorDiff {
    pub guid: String,
    pub selector: u8,
    pub status: String,
    pub before_length: Option<u16>,
    pub after_length: Option<u16>,
    pub bytes: Vec<ByteDiff>,
    pub decoded_before: BTreeMap<String, Value>,
    pub decoded_after: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct XuSnapshotDiff {
    pub schema_version: u32,
    pub same_device: bool,
    pub descriptor_changed: bool,
    pub profile_changed: bool,
    pub standard_controls_before: BTreeMap<String, Value>,
    pub standard_controls_after: BTreeMap<String, Value>,
    pub selectors: Vec<SelectorDiff>,
}

/// Compare two snapshots offline, aligning selectors by GUID rather than runtime unit ID.
pub fn diff_snapshots(
    before: &XuSnapshot,
    after: &XuSnapshot,
) -> Result<XuSnapshotDiff, LinkError> {
    validate_snapshot(before)?;
    validate_snapshot(after)?;
    let mut keys = BTreeSet::new();
    for selector in before.selectors.iter().chain(&after.selectors) {
        keys.insert((selector.guid.clone(), selector.selector));
    }
    let mut selectors = Vec::new();
    for (guid, selector) in keys {
        let old = before
            .selectors
            .iter()
            .find(|item| item.guid == guid && item.selector == selector);
        let new = after
            .selectors
            .iter()
            .find(|item| item.guid == guid && item.selector == selector);
        let status = match (old, new) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            _ => "changed",
        };
        let old_payload = old.and_then(latest_payload).transpose()?;
        let new_payload = new.and_then(latest_payload).transpose()?;
        let mut bytes = Vec::new();
        if let (Some(old_payload), Some(new_payload)) = (&old_payload, &new_payload) {
            for (offset, (before, after)) in old_payload.iter().zip(new_payload).enumerate() {
                let xor = before ^ after;
                if xor != 0 {
                    bytes.push(ByteDiff {
                        offset,
                        before: *before,
                        after: *after,
                        xor,
                        changed_bits: (0_u8..8).filter(|bit| xor & (1 << bit) != 0).collect(),
                    });
                }
            }
        }
        if status != "changed"
            || old.and_then(|value| value.length) != new.and_then(|value| value.length)
            || !bytes.is_empty()
            || old.map(|value| &value.decoded) != new.map(|value| &value.decoded)
        {
            selectors.push(SelectorDiff {
                guid,
                selector,
                status: status.into(),
                before_length: old.and_then(|value| value.length),
                after_length: new.and_then(|value| value.length),
                bytes,
                decoded_before: old.map_or_else(BTreeMap::new, |value| value.decoded.clone()),
                decoded_after: new.map_or_else(BTreeMap::new, |value| value.decoded.clone()),
            });
        }
    }
    Ok(XuSnapshotDiff {
        schema_version: XU_SNAPSHOT_SCHEMA_VERSION,
        same_device: before.device.stable_id == after.device.stable_id,
        descriptor_changed: before.device.descriptor_sha256 != after.device.descriptor_sha256,
        profile_changed: before.profile != after.profile,
        standard_controls_before: before.standard_controls.clone(),
        standard_controls_after: after.standard_controls.clone(),
        selectors,
    })
}

fn latest_payload(selector: &SnapshotSelector) -> Option<Result<Vec<u8>, LinkError>> {
    selector.samples_base64.last().map(|value| {
        BASE64.decode(value).map_err(|error| {
            LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "snapshot payload is invalid base64",
            )
            .with_detail("reason", error.to_string())
        })
    })
}

fn validate_snapshot(snapshot: &XuSnapshot) -> Result<(), LinkError> {
    if snapshot.schema_version == XU_SNAPSHOT_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "unsupported XU snapshot schema",
        ))
    }
}

pub fn load_snapshot(path: &Path) -> Result<XuSnapshot, LinkError> {
    let bytes = fs::read(path).map_err(|error| snapshot_io_error(path, &error))?;
    let snapshot: XuSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
        LinkError::new(ErrorKind::ProtocolProfileMismatch, "invalid XU snapshot")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn save_snapshot(path: &Path, snapshot: &XuSnapshot) -> Result<(), LinkError> {
    validate_new_parent(path)?;
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| snapshot_io_error(path, &error))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .and_then(|()| {
            serde_json::to_writer_pretty(&mut temporary, snapshot).map_err(io::Error::other)
        })
        .and_then(|()| temporary.write_all(b"\n"))
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| snapshot_io_error(path, &error))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| snapshot_io_error(path, &error.error))?;
    Ok(())
}

fn validate_new_parent(path: &Path) -> Result<(), LinkError> {
    if path.exists() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "XU snapshot destination already exists",
        )
        .with_detail("path", path.display().to_string()));
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(LinkError::new(
            ErrorKind::IoFailure,
            "XU snapshot parent directory does not exist",
        ));
    }
    Ok(())
}

/// Cross-process write pacing stored below the private runtime directory.
pub struct RateLimiter {
    directory: PathBuf,
}

impl RateLimiter {
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn from_process() -> Result<Self, LinkError> {
        Ok(Self::new(AppPaths::from_process()?.runtime.join("xu-rate")))
    }

    pub fn enforce(
        &self,
        key: &str,
        minimum: Duration,
        timeout: Duration,
        dry_run: bool,
    ) -> Result<Duration, LinkError> {
        AppPaths::ensure_private(&self.directory)?;
        let path = self.path(key);
        let now = unix_millis();
        let previous = match fs::read_to_string(&path) {
            Ok(value) => value.trim().parse::<u128>().map_err(|_| {
                LinkError::new(ErrorKind::IoFailure, "XU rate-limit state is invalid")
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(snapshot_io_error(&path, &error)),
        };
        let next = previous.saturating_add(minimum.as_millis());
        let wait = Duration::from_millis(next.saturating_sub(now).try_into().unwrap_or(u64::MAX));
        if wait > timeout {
            return Err(LinkError::new(
                ErrorKind::Timeout,
                "XU write rate limit exceeds the operation timeout",
            )
            .with_detail("wait_ms", wait.as_millis() as u64));
        }
        if !dry_run {
            thread::sleep(wait);
            let mut options = OpenOptions::new();
            options
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
            let mut file = options
                .open(&path)
                .map_err(|error| snapshot_io_error(&path, &error))?;
            writeln!(file, "{}", unix_millis())
                .and_then(|()| file.sync_data())
                .map_err(|error| snapshot_io_error(&path, &error))?;
        }
        Ok(wait)
    }

    fn path(&self, key: &str) -> PathBuf {
        let hash = Sha256::digest(key.as_bytes());
        self.directory
            .join(format!("{}.timestamp", lowercase_hex(&hash[..12])))
    }
}

/// Redacted audit entry for a raw write attempt.
#[derive(Clone, Debug, Serialize)]
pub struct RawWriteAudit<'a> {
    pub schema_version: u32,
    pub attempted_unix_ms: u128,
    pub stable_id: &'a str,
    pub profile_id: &'a str,
    pub profile_checksum: &'a str,
    pub descriptor_sha256: &'a str,
    pub guid: &'a str,
    pub selector: u8,
    pub payload_length: usize,
    pub payload_sha256: String,
    pub dry_run: bool,
    pub outcome: &'a str,
}

/// Stable, non-payload context recorded for a raw write attempt.
#[derive(Clone, Copy, Debug)]
pub struct RawWriteContext<'a> {
    pub stable_id: &'a str,
    pub profile_id: &'a str,
    pub profile_checksum: &'a str,
    pub descriptor_sha256: &'a str,
    pub guid: &'a str,
    pub selector: u8,
}

impl<'a> RawWriteAudit<'a> {
    #[must_use]
    pub fn new(
        context: RawWriteContext<'a>,
        payload: &[u8],
        dry_run: bool,
        outcome: &'a str,
    ) -> Self {
        Self {
            schema_version: 1,
            attempted_unix_ms: unix_millis(),
            stable_id: context.stable_id,
            profile_id: context.profile_id,
            profile_checksum: context.profile_checksum,
            descriptor_sha256: context.descriptor_sha256,
            guid: context.guid,
            selector: context.selector,
            payload_length: payload.len(),
            payload_sha256: lowercase_hex(&Sha256::digest(payload)),
            dry_run,
            outcome,
        }
    }
}

pub fn append_raw_audit(record: &RawWriteAudit<'_>) -> Result<PathBuf, LinkError> {
    let directory = AppPaths::from_process()?.state.join("audit");
    AppPaths::ensure_private(&directory)?;
    let path = directory.join("xu-writes.jsonl");
    let mut options = OpenOptions::new();
    options
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    let mut file = options
        .open(&path)
        .map_err(|error| snapshot_io_error(&path, &error))?;
    serde_json::to_writer(&mut file, record).map_err(|error| {
        LinkError::new(
            ErrorKind::IoFailure,
            "failed to serialize raw XU audit record",
        )
        .with_detail("reason", error.to_string())
    })?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_data())
        .map_err(|error| snapshot_io_error(&path, &error))?;
    Ok(path)
}

pub fn parse_hex(value: &str) -> Result<Vec<u8>, LinkError> {
    let normalized = value.trim().replace([' ', ':', '-'], "");
    if normalized.is_empty()
        || !normalized.len().is_multiple_of(2)
        || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "XU payload must contain complete hexadecimal bytes",
        ));
    }
    normalized
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16)
                .map_err(|_| LinkError::new(ErrorKind::InvalidInvocation, "invalid XU hex payload"))
        })
        .collect()
}

fn canonical_guid(bytes: &[u8]) -> String {
    debug_assert_eq!(bytes.len(), 16);
    let first = u32::from_le_bytes(bytes[0..4].try_into().expect("GUID field"));
    let second = u16::from_le_bytes(bytes[4..6].try_into().expect("GUID field"));
    let third = u16::from_le_bytes(bytes[6..8].try_into().expect("GUID field"));
    format!(
        "{first:08x}-{second:04x}-{third:04x}-{:02x}{:02x}-{}",
        bytes[8],
        bytes[9],
        lowercase_hex(&bytes[10..16])
    )
}

fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn descriptor_error(offset: usize, reason: &'static str) -> LinkError {
    LinkError::new(
        ErrorKind::ProtocolProfileMismatch,
        "malformed USB descriptor data",
    )
    .with_detail("offset", offset as u64)
    .with_detail("reason", reason)
}

fn open_error(path: &Path, error: &io::Error) -> LinkError {
    let kind = match error.kind() {
        io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
        io::ErrorKind::NotFound => ErrorKind::DeviceNotFound,
        _ if error.raw_os_error() == Some(16) => ErrorKind::DeviceBusy,
        _ => ErrorKind::IoFailure,
    };
    LinkError::new(kind, "failed to open video node for XU access")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn query_error(operation: &'static str, address: XuAddress, error: &io::Error) -> LinkError {
    let (kind, message) = match error.raw_os_error() {
        Some(2) => (
            ErrorKind::CapabilityUnsupported,
            "XU entity or selector was not found",
        ),
        Some(105) => (
            ErrorKind::ProtocolProfileMismatch,
            "XU query buffer length is incorrect",
        ),
        Some(22) => (
            ErrorKind::ProtocolProfileMismatch,
            "XU query code is invalid",
        ),
        Some(56) => (
            ErrorKind::CapabilityUnsupported,
            "XU query is not supported",
        ),
        Some(13) => (
            ErrorKind::PermissionDenied,
            "XU query permission was denied",
        ),
        Some(16) => (ErrorKind::DeviceBusy, "XU device is busy"),
        _ => (ErrorKind::IoFailure, "XU query failed"),
    };
    LinkError::new(kind, message)
        .with_detail("operation", operation)
        .with_detail("unit", u64::from(address.unit))
        .with_detail("selector", u64::from(address.selector))
        .with_detail("reason", error.to_string())
}

fn snapshot_io_error(path: &Path, error: &io::Error) -> LinkError {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, "failed to access XU research artifact")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

#[allow(unsafe_code)]
mod abi {
    use std::{io, os::fd::BorrowedFd};

    use rustix::ioctl::{self, Updater, opcode};

    #[repr(C)]
    struct UvcXuControlQuery {
        unit: u8,
        selector: u8,
        query: u8,
        size: u16,
        data: *mut u8,
    }

    const UVCIOC_CTRL_QUERY: ioctl::Opcode = opcode::read_write::<UvcXuControlQuery>(b'u', 0x21);

    #[cfg(target_pointer_width = "64")]
    const _: () = {
        assert!(std::mem::size_of::<UvcXuControlQuery>() == 16);
        assert!(std::mem::align_of::<UvcXuControlQuery>() == 8);
        assert!(std::mem::offset_of!(UvcXuControlQuery, unit) == 0);
        assert!(std::mem::offset_of!(UvcXuControlQuery, selector) == 1);
        assert!(std::mem::offset_of!(UvcXuControlQuery, query) == 2);
        assert!(std::mem::offset_of!(UvcXuControlQuery, size) == 4);
        assert!(std::mem::offset_of!(UvcXuControlQuery, data) == 8);
    };

    pub(super) fn query(
        fd: BorrowedFd<'_>,
        unit: u8,
        selector: u8,
        query: u8,
        data: &mut [u8],
    ) -> io::Result<()> {
        let size = u16::try_from(data.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "XU query buffer is too large")
        })?;
        let mut request = UvcXuControlQuery {
            unit,
            selector,
            query,
            size,
            data: data.as_mut_ptr(),
        };
        // SAFETY: this is the kernel's `_IOWR('u', 0x21, struct
        // uvc_xu_control_query)` ABI. The repr(C) layout is asserted on both
        // supported architectures, the descriptor outlives the call, and the
        // uniquely borrowed buffer remains valid for exactly `size` bytes.
        let operation = unsafe { Updater::<UVCIOC_CTRL_QUERY, _>::new(&mut request) };
        // SAFETY: the operation and pointed-to buffer satisfy the invariant above.
        unsafe { ioctl::ioctl(fd, operation) }
            .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))
    }

    #[cfg(test)]
    mod tests {
        use super::UvcXuControlQuery;

        #[test]
        fn query_layout_matches_the_linux_abi() {
            assert_eq!(std::mem::size_of::<UvcXuControlQuery>(), 16);
            assert_eq!(std::mem::align_of::<UvcXuControlQuery>(), 8);
            assert_eq!(std::mem::offset_of!(UvcXuControlQuery, size), 4);
            assert_eq!(std::mem::offset_of!(UvcXuControlQuery, data), 8);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, io, time::Duration};

    use super::{
        QueryTransport, RateLimiter, UVC_GET_CUR, UVC_GET_INFO, UVC_GET_LEN, UVC_SET_CUR,
        XuAddress, diff_snapshots, get_current_with_transport, parse_descriptors,
        set_current_with_transport,
    };

    fn descriptor_blob(bitmap: &[u8]) -> Vec<u8> {
        let length = 25 + bitmap.len();
        let mut blob = vec![
            8,
            0x0b,
            0,
            2,
            0x0e,
            0x03,
            0,
            0, // Interface association.
            9,
            4,
            0,
            0,
            1,
            0x0e,
            0x01,
            0,
            0, // VideoControl interface.
            13,
            0x24,
            0x01,
            0x10,
            0x01,
            0,
            0,
            0x00,
            0x6c,
            0xdc,
            0x02,
            1,
            1, // Header.
            16,
            0x24,
            0x02,
            1,
            0x01,
            0x02,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            1, // Input.
            7,
            0x24,
            0x04,
            3,
            1,
            1,
            0, // Selector unit.
            12,
            0x24,
            0x05,
            4,
            1,
            0,
            0,
            1,
            0x03,
            0,
            0,
            0, // Processing unit.
            9,
            0x24,
            0x03,
            6,
            0x01,
            0x01,
            0,
            4,
            0, // Output terminal.
            u8::try_from(length).expect("fixture length"),
            0x24,
            0x06,
            9,
        ];
        blob.extend_from_slice(&[
            0x2d, 0x67, 0xf1, 0xfa, 0x1b, 0xb7, 0x93, 0x47, 0x8c, 0x91, 0x7b, 0x1c, 0x9b, 0x7f,
            0x95, 0xf8,
        ]);
        blob.push(8);
        blob.push(1);
        blob.push(4);
        blob.push(u8::try_from(bitmap.len()).expect("fixture bitmap"));
        blob.extend_from_slice(bitmap);
        blob.push(0);
        blob
    }

    #[test]
    fn parses_complete_graph_and_one_based_selectors() {
        let parsed = parse_descriptors(&descriptor_blob(&[0b1000_0101])).unwrap();
        assert_eq!(parsed.interface_associations.len(), 1);
        assert_eq!(parsed.video_control_entities.len(), 6);
        let xu = &parsed.extension_units[0];
        assert_eq!(xu.unit_id, 9);
        assert_eq!(xu.guid, "faf1672d-b71b-4793-8c91-7b1c9b7f95f8");
        assert_eq!(
            xu.selectors
                .iter()
                .map(|selector| selector.selector)
                .collect::<Vec<_>>(),
            [1, 3, 8]
        );
    }

    #[test]
    fn truncated_prefixes_never_panic_and_partial_descriptors_fail() {
        let fixture = descriptor_blob(&[1]);
        for length in 0..fixture.len() {
            let _ = parse_descriptors(&fixture[..length]);
        }
        assert!(parse_descriptors(&fixture[..fixture.len() - 1]).is_err());
        assert!(parse_descriptors(&fixture).is_ok());
    }

    struct MockTransport {
        requests: RefCell<Vec<(u8, usize)>>,
        length: u16,
        info: u8,
    }

    impl QueryTransport for MockTransport {
        fn query(&self, _unit: u8, _selector: u8, query: u8, data: &mut [u8]) -> io::Result<()> {
            self.requests.borrow_mut().push((query, data.len()));
            match query {
                UVC_GET_INFO => data.copy_from_slice(&[self.info]),
                UVC_GET_LEN => data.copy_from_slice(&self.length.to_le_bytes()),
                UVC_GET_CUR => data.fill(0x55),
                UVC_SET_CUR => {}
                _ => panic!("unexpected query"),
            }
            Ok(())
        }
    }

    #[test]
    fn safe_get_uses_info_len_and_exact_current_buffer() {
        let transport = MockTransport {
            requests: RefCell::new(Vec::new()),
            length: 61,
            info: 3,
        };
        let (_, payload) = get_current_with_transport(
            &transport,
            XuAddress {
                unit: 9,
                selector: 1,
            },
            Some(61),
        )
        .unwrap();
        assert_eq!(payload.len(), 61);
        assert_eq!(
            transport.requests.into_inner(),
            [(UVC_GET_INFO, 1), (UVC_GET_LEN, 2), (UVC_GET_CUR, 61)]
        );
    }

    #[test]
    fn incorrect_lengths_never_reach_get_or_set() {
        let transport = MockTransport {
            requests: RefCell::new(Vec::new()),
            length: 61,
            info: 3,
        };
        assert!(
            get_current_with_transport(
                &transport,
                XuAddress {
                    unit: 9,
                    selector: 1
                },
                Some(52),
            )
            .is_err()
        );
        assert!(
            set_current_with_transport(
                &transport,
                XuAddress {
                    unit: 9,
                    selector: 1
                },
                52,
                &[0; 52],
            )
            .is_err()
        );
        assert!(
            transport
                .requests
                .borrow()
                .iter()
                .all(|(query, _)| !matches!(*query, UVC_GET_CUR | UVC_SET_CUR))
        );
    }

    #[test]
    fn rate_limiter_rejects_a_write_beyond_the_timeout() {
        let root = tempfile::tempdir().unwrap();
        let limiter = RateLimiter::new(root.path().join("rate"));
        assert_eq!(
            limiter
                .enforce("device:guid:1", Duration::ZERO, Duration::ZERO, false)
                .unwrap(),
            Duration::ZERO
        );
        let error = limiter
            .enforce(
                "device:guid:1",
                Duration::from_secs(1),
                Duration::ZERO,
                false,
            )
            .unwrap_err();
        assert_eq!(error.kind(), link_core::ErrorKind::Timeout);
    }

    #[test]
    fn empty_snapshot_diff_is_stable() {
        let snapshot = super::XuSnapshot {
            schema_version: 1,
            captured_unix_ms: 0,
            application_version: "test".into(),
            device: super::SnapshotDevice {
                stable_id: "device".into(),
                model: "model".into(),
                usb_vid: 1,
                usb_pid: 2,
                bcd_device: 3,
                descriptor_sha256: "1".repeat(64),
            },
            profile: None,
            stream_state: "closed".into(),
            notes: Vec::new(),
            standard_controls: Default::default(),
            selectors: Vec::new(),
        };
        assert!(
            diff_snapshots(&snapshot, &snapshot)
                .unwrap()
                .selectors
                .is_empty()
        );
    }
}
