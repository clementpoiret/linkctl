//! UVC descriptor parsing and narrowly scoped read-only Extension Unit queries.

use std::{fs::File, io, os::fd::AsFd, path::Path};

use link_core::{
    ErrorKind, LinkError,
    probe::{ProbeIssue, XuEntityReport, XuSelectorReport},
};

const USB_DT_INTERFACE: u8 = 0x04;
const USB_DT_CS_INTERFACE: u8 = 0x24;
const USB_CLASS_VIDEO: u8 = 0x0e;
const UVC_SC_VIDEOCONTROL: u8 = 0x01;
const UVC_VC_EXTENSION_UNIT: u8 = 0x06;
const UVC_GET_LEN: u8 = 0x85;
const UVC_GET_INFO: u8 = 0x86;

/// One descriptor not interpreted by the minimal XU parser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawDescriptor {
    /// Offset in the raw USB descriptor blob.
    pub offset: usize,
    /// USB descriptor type.
    pub descriptor_type: u8,
    /// Descriptor bytes, including length and type.
    pub bytes: Vec<u8>,
}

/// Parsed inventory with uninterpreted descriptors retained for future use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DescriptorInventory {
    /// Parsed Extension Units without query results.
    pub extension_units: Vec<XuEntityReport>,
    /// Every descriptor not interpreted as an Extension Unit.
    pub unknown_descriptors: Vec<RawDescriptor>,
}

/// Parse USB descriptors and extract Extension Units from VideoControl interfaces.
pub fn parse_descriptors(bytes: &[u8]) -> Result<DescriptorInventory, LinkError> {
    let mut offset = 0_usize;
    let mut video_control_interface = false;
    let mut extension_units = Vec::new();
    let mut unknown_descriptors = Vec::new();

    while offset < bytes.len() {
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
        let end = offset.checked_add(length).ok_or_else(|| {
            descriptor_error(offset, "descriptor length overflows the input offset")
        })?;
        if end > bytes.len() {
            return Err(descriptor_error(offset, "descriptor extends beyond input"));
        }
        let descriptor = &bytes[offset..end];
        let descriptor_type = descriptor[1];

        if descriptor_type == USB_DT_INTERFACE {
            if descriptor.len() < 9 {
                return Err(descriptor_error(
                    offset,
                    "USB interface descriptor is truncated",
                ));
            }
            video_control_interface = descriptor[5] == USB_CLASS_VIDEO
                && descriptor[6] == UVC_SC_VIDEOCONTROL
                && descriptor[3] == 0;
            unknown_descriptors.push(RawDescriptor {
                offset,
                descriptor_type,
                bytes: descriptor.to_vec(),
            });
        } else if video_control_interface
            && descriptor_type == USB_DT_CS_INTERFACE
            && descriptor.get(2) == Some(&UVC_VC_EXTENSION_UNIT)
        {
            extension_units.push(parse_extension_unit(descriptor, offset)?);
        } else {
            unknown_descriptors.push(RawDescriptor {
                offset,
                descriptor_type,
                bytes: descriptor.to_vec(),
            });
        }
        offset = end;
    }

    Ok(DescriptorInventory {
        extension_units,
        unknown_descriptors,
    })
}

/// Parse descriptors and issue only `GET_LEN` and `GET_INFO` for advertised selectors.
pub fn inventory(video_node: &Path, descriptors: &[u8]) -> Result<Vec<XuEntityReport>, LinkError> {
    let parsed = parse_descriptors(descriptors)?;
    let file = File::open(video_node).map_err(|error| open_error(video_node, &error))?;
    Ok(inventory_with_transport(
        parsed.extension_units,
        &IoctlTransport { file },
    ))
}

fn parse_extension_unit(
    descriptor: &[u8],
    descriptor_offset: usize,
) -> Result<XuEntityReport, LinkError> {
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
        .ok_or_else(|| {
            descriptor_error(
                descriptor_offset,
                "Extension Unit control bitmap length overflows",
            )
        })?;
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
                    .and_then(|base| base.checked_add(usize::from(bit)))
                    .and_then(|value| value.checked_add(1))
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

    Ok(XuEntityReport {
        unit_id: descriptor[3],
        guid: canonical_guid(&descriptor[4..20]),
        num_controls: descriptor[20],
        source_ids,
        control_bitmap: lowercase_hex(bitmap),
        selectors,
        descriptor_offset,
    })
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

fn inventory_with_transport<T: QueryTransport>(
    mut entities: Vec<XuEntityReport>,
    transport: &T,
) -> Vec<XuEntityReport> {
    for entity in &mut entities {
        for selector in &mut entity.selectors {
            let mut length = [0_u8; 2];
            match transport.query(entity.unit_id, selector.selector, UVC_GET_LEN, &mut length) {
                Ok(()) => selector.length = Some(u16::from_le_bytes(length)),
                Err(error) => selector.issues.push(query_issue("get-len", &error)),
            }

            let mut info = [0_u8; 1];
            match transport.query(entity.unit_id, selector.selector, UVC_GET_INFO, &mut info) {
                Ok(()) => {
                    selector.info = Some(info[0]);
                    selector.get_supported = Some(info[0] & 0x01 != 0);
                    selector.set_supported = Some(info[0] & 0x02 != 0);
                }
                Err(error) => selector.issues.push(query_issue("get-info", &error)),
            }
        }
    }
    entities
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
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
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
        _ => ErrorKind::IoFailure,
    };
    LinkError::new(kind, "failed to open video node for read-only XU inventory")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn query_issue(operation: &'static str, error: &io::Error) -> ProbeIssue {
    let code = match error.raw_os_error() {
        Some(2) => "not-found",
        Some(22) => "invalid-request",
        Some(105) => "incorrect-buffer-size",
        Some(56) => "request-not-supported",
        _ => "io-failure",
    };
    ProbeIssue::new("xu", code, format!("{operation} query failed: {error}"))
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

        // SAFETY: UVCIOC_CTRL_QUERY is `_IOWR('u', 0x21, struct
        // uvc_xu_control_query)`. The repr(C) structure layout is asserted for both
        // supported 64-bit architectures, the borrowed descriptor outlives the call,
        // and `data` remains allocated and exclusively borrowed for exactly `size`
        // writable bytes. Callers expose only GET_LEN and GET_INFO requests.
        let operation = unsafe { Updater::<UVCIOC_CTRL_QUERY, _>::new(&mut request) };
        // SAFETY: The operation and all pointed-to memory satisfy the invariant above.
        unsafe { ioctl::ioctl(fd, operation) }
            .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))
    }

    #[cfg(test)]
    mod tests {
        use super::UvcXuControlQuery;

        #[test]
        fn query_layout_matches_the_linux_abi() {
            #[cfg(target_pointer_width = "64")]
            {
                assert_eq!(std::mem::size_of::<UvcXuControlQuery>(), 16);
                assert_eq!(std::mem::offset_of!(UvcXuControlQuery, size), 4);
                assert_eq!(std::mem::offset_of!(UvcXuControlQuery, data), 8);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, io};

    use super::{
        QueryTransport, UVC_GET_INFO, UVC_GET_LEN, inventory_with_transport, parse_descriptors,
    };

    fn descriptor_blob(bitmap: &[u8]) -> Vec<u8> {
        let length = 25 + bitmap.len();
        let mut blob = vec![
            9,
            4,
            0,
            0,
            1,
            0x0e,
            0x01,
            0,
            0, // VideoControl interface
            u8::try_from(length).expect("fixture length"),
            0x24,
            0x06,
            9, // XU unit
        ];
        blob.extend_from_slice(&[
            0x2d, 0x67, 0xf1, 0xfa, 0x1b, 0xb7, 0x93, 0x47, 0x8c, 0x91, 0x7b, 0x1c, 0x9b, 0x7f,
            0x95, 0xf8,
        ]);
        blob.push(8); // bNumControls
        blob.push(1); // bNrInPins
        blob.push(5); // baSourceID
        blob.push(u8::try_from(bitmap.len()).expect("fixture bitmap"));
        blob.extend_from_slice(bitmap);
        blob.push(0); // iExtension
        blob
    }

    #[test]
    fn parses_guid_sources_and_one_based_selectors() {
        let parsed = parse_descriptors(&descriptor_blob(&[0b1000_0101])).expect("descriptor");
        let xu = &parsed.extension_units[0];
        assert_eq!(xu.unit_id, 9);
        assert_eq!(xu.guid, "faf1672d-b71b-4793-8c91-7b1c9b7f95f8");
        assert_eq!(xu.source_ids, [5]);
        assert_eq!(
            xu.selectors
                .iter()
                .map(|selector| selector.selector)
                .collect::<Vec<_>>(),
            [1, 3, 8]
        );
    }

    #[test]
    fn rejects_zero_length_and_truncated_descriptors() {
        assert!(parse_descriptors(&[0]).is_err());
        assert!(parse_descriptors(&[9, 4, 0]).is_err());

        let mut truncated = descriptor_blob(&[1]);
        truncated.pop();
        assert!(parse_descriptors(&truncated).is_err());
    }

    struct MockTransport {
        requests: RefCell<Vec<(u8, u8, usize)>>,
    }

    impl QueryTransport for MockTransport {
        fn query(&self, _unit: u8, _selector: u8, query: u8, data: &mut [u8]) -> io::Result<()> {
            self.requests
                .borrow_mut()
                .push((query, data.len() as u8, data.len()));
            match query {
                UVC_GET_LEN => data.copy_from_slice(&[61, 0]),
                UVC_GET_INFO => data.copy_from_slice(&[3]),
                _ => panic!("unexpected query"),
            }
            Ok(())
        }
    }

    #[test]
    fn inventory_issues_only_get_len_and_get_info() {
        let entities = parse_descriptors(&descriptor_blob(&[1]))
            .expect("descriptor")
            .extension_units;
        let transport = MockTransport {
            requests: RefCell::new(Vec::new()),
        };
        let inventory = inventory_with_transport(entities, &transport);
        assert_eq!(
            transport.requests.into_inner(),
            [(UVC_GET_LEN, 2, 2), (UVC_GET_INFO, 1, 1)]
        );
        assert_eq!(inventory[0].selectors[0].length, Some(61));
        assert_eq!(inventory[0].selectors[0].get_supported, Some(true));
        assert_eq!(inventory[0].selectors[0].set_supported, Some(true));
    }
}
