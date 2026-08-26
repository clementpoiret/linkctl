//! Read-only V4L2 formats, controls, and capability inventory.

use std::fs::File;

use link_core::probe::{
    ControlMenuItem, ControlReport, CurrentFormatReport, FrameInterval, FrameSize, NodeAssociation,
    ProbeIssue, Rational, V4l2Capabilities, VideoFormatReport, VideoNodeKind, VideoNodeReport,
};
use serde_json::Value;
use v4l2r::bindings::{self, v4l2_frmivalenum, v4l2_frmsizeenum, v4l2_query_ext_ctrl};
use v4l2r::ioctl::{
    Capabilities, Capability, FrameIntervalsError, FrameSizeError, FrmIvalTypes, FrmSizeTypes,
    QueryCtrlError, QueryCtrlFlags,
};
use v4l2r::{Format, PixelFormat, QueueType};

/// Inspect one V4L2 node without changing controls, formats, or stream state.
#[must_use]
pub fn probe_node(node: NodeAssociation) -> VideoNodeReport {
    let mut report = VideoNodeReport {
        node,
        kind: VideoNodeKind::Unavailable,
        capabilities: None,
        current_format: None,
        formats: Vec::new(),
        controls: Vec::new(),
        issues: Vec::new(),
    };

    let file = match File::open(&report.node.path) {
        Ok(file) => file,
        Err(error) => {
            report.issues.push(issue(
                "open-failed",
                format!("could not open {} read-only: {error}", report.node.path),
            ));
            return report;
        }
    };

    let capability: Capability = match v4l2r::ioctl::querycap(&file) {
        Ok(capability) => capability,
        Err(error) => {
            report.issues.push(issue(
                "query-capabilities-failed",
                format!("VIDIOC_QUERYCAP failed for {}: {error}", report.node.path),
            ));
            return report;
        }
    };

    let device_caps = capability.device_caps();
    let queue = classify_queue(device_caps);
    report.kind = match queue {
        Some(QueueType::VideoCapture | QueueType::VideoCaptureMplane) => VideoNodeKind::Capture,
        Some(QueueType::MetaCapture) => VideoNodeKind::Metadata,
        _ => VideoNodeKind::Other,
    };
    report.capabilities = Some(capability_report(&capability));

    if let Some(queue) = queue {
        report.formats = enumerate_formats(&file, queue, &mut report.issues);
        if matches!(
            queue,
            QueueType::VideoCapture | QueueType::VideoCaptureMplane
        ) {
            match v4l2r::ioctl::g_fmt::<Format>(&file, queue) {
                Ok(format) => {
                    report.current_format = Some(CurrentFormatReport {
                        width: format.width,
                        height: format.height,
                        fourcc: fourcc(format.pixelformat),
                    });
                }
                Err(error) => report.issues.push(issue(
                    "current-format-failed",
                    format!("VIDIOC_G_FMT failed for {}: {error}", report.node.path),
                )),
            }
        }
    }

    if report.kind == VideoNodeKind::Capture {
        report.controls = enumerate_controls(&file, &mut report.issues);
    }
    report
}

fn issue(code: &str, message: String) -> ProbeIssue {
    ProbeIssue::new("v4l2", code, message)
}

fn classify_queue(capabilities: Capabilities) -> Option<QueueType> {
    if capabilities.contains(Capabilities::VIDEO_CAPTURE) {
        Some(QueueType::VideoCapture)
    } else if capabilities.contains(Capabilities::VIDEO_CAPTURE_MPLANE) {
        Some(QueueType::VideoCaptureMplane)
    } else if capabilities.contains(Capabilities::META_CAPTURE) {
        Some(QueueType::MetaCapture)
    } else {
        None
    }
}

fn capability_report(capability: &Capability) -> V4l2Capabilities {
    let capabilities = capability.capabilities();
    let device_capabilities = capability.device_caps();
    V4l2Capabilities {
        driver: capability.driver.clone(),
        card: capability.card.clone(),
        bus_info: capability.bus_info.clone(),
        version: capability.version,
        capabilities: capabilities.bits(),
        device_capabilities: device_capabilities.bits(),
        capability_names: device_capabilities
            .iter_names()
            .map(|(name, _)| name.to_ascii_lowercase().replace('_', "-"))
            .collect(),
    }
}

fn enumerate_formats(
    file: &File,
    queue: QueueType,
    issues: &mut Vec<ProbeIssue>,
) -> Vec<VideoFormatReport> {
    v4l2r::ioctl::FormatIterator::new(file, queue)
        .map(|format| VideoFormatReport {
            fourcc: fourcc(format.pixelformat),
            description: format.description,
            flags: format.flags.bits(),
            sizes: if queue == QueueType::MetaCapture {
                Vec::new()
            } else {
                enumerate_sizes(file, format.pixelformat, issues)
            },
        })
        .collect()
}

fn enumerate_sizes(
    file: &File,
    pixel_format: PixelFormat,
    issues: &mut Vec<ProbeIssue>,
) -> Vec<FrameSize> {
    let mut sizes = Vec::new();
    for index in 0.. {
        let raw =
            match v4l2r::ioctl::enum_frame_sizes::<v4l2_frmsizeenum>(file, index, pixel_format) {
                Ok(raw) => raw,
                Err(FrameSizeError::IoctlError(v4l2r::nix::errno::Errno::EINVAL)) => break,
                Err(error) => {
                    issues.push(issue(
                        "frame-size-enumeration-failed",
                        format!(
                            "VIDIOC_ENUM_FRAMESIZES failed for {}: {error}",
                            fourcc(pixel_format)
                        ),
                    ));
                    break;
                }
            };

        match raw.size() {
            Some(FrmSizeTypes::Discrete(size)) => sizes.push(FrameSize::Discrete {
                width: size.width,
                height: size.height,
                intervals: enumerate_intervals(file, pixel_format, size.width, size.height, issues),
            }),
            Some(FrmSizeTypes::StepWise(size)) => sizes.push(FrameSize::Stepwise {
                min_width: size.min_width,
                max_width: size.max_width,
                step_width: size.step_width,
                min_height: size.min_height,
                max_height: size.max_height,
                step_height: size.step_height,
            }),
            None => issues.push(issue(
                "unknown-frame-size-type",
                format!(
                    "driver returned an unknown frame-size type for {}",
                    fourcc(pixel_format)
                ),
            )),
        }
    }
    sizes
}

fn enumerate_intervals(
    file: &File,
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    issues: &mut Vec<ProbeIssue>,
) -> Vec<FrameInterval> {
    let mut intervals = Vec::new();
    for index in 0.. {
        let raw = match v4l2r::ioctl::enum_frame_intervals::<v4l2_frmivalenum>(
            file,
            index,
            pixel_format,
            width,
            height,
        ) {
            Ok(raw) => raw,
            Err(FrameIntervalsError::IoctlError(v4l2r::nix::errno::Errno::EINVAL)) => break,
            Err(error) => {
                issues.push(issue(
                    "frame-interval-enumeration-failed",
                    format!(
                        "VIDIOC_ENUM_FRAMEINTERVALS failed for {} {width}x{height}: {error}",
                        fourcc(pixel_format)
                    ),
                ));
                break;
            }
        };

        match raw.intervals() {
            Some(FrmIvalTypes::Discrete(value)) => {
                intervals.push(FrameInterval::Discrete {
                    value: rational(value.numerator, value.denominator),
                });
            }
            Some(FrmIvalTypes::StepWise(value)) => intervals.push(FrameInterval::Stepwise {
                min: rational(value.min.numerator, value.min.denominator),
                max: rational(value.max.numerator, value.max.denominator),
                step: rational(value.step.numerator, value.step.denominator),
            }),
            None => issues.push(issue(
                "unknown-frame-interval-type",
                format!(
                    "driver returned an unknown interval type for {} {width}x{height}",
                    fourcc(pixel_format)
                ),
            )),
        }
    }
    intervals
}

const fn rational(numerator: u32, denominator: u32) -> Rational {
    Rational {
        numerator,
        denominator,
    }
}

fn enumerate_controls(file: &File, issues: &mut Vec<ProbeIssue>) -> Vec<ControlReport> {
    let mut controls = Vec::new();
    let mut id = 0;

    loop {
        let control_id = v4l2r::ioctl::CtrlId::new(id).expect("queried control IDs are masked");
        let raw = match v4l2r::ioctl::query_ext_ctrl::<v4l2_query_ext_ctrl>(
            file,
            control_id,
            QueryCtrlFlags::NEXT | QueryCtrlFlags::COMPOUND,
        ) {
            Ok(raw) => raw,
            Err(QueryCtrlError::IoctlError(v4l2r::nix::errno::Errno::EINVAL)) => break,
            Err(error) => {
                issues.push(issue(
                    "control-enumeration-failed",
                    format!("VIDIOC_QUERY_EXT_CTRL failed after control 0x{id:08x}: {error}"),
                ));
                break;
            }
        };
        id = raw.id;
        controls.push(control_report(file, raw));
    }

    controls
}

fn control_report(file: &File, raw: v4l2_query_ext_ctrl) -> ControlReport {
    let mut control_issue = None;
    let scalar_type = matches!(
        raw.type_,
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BOOLEAN
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BITMASK
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU
    );
    let readable =
        raw.flags & (bindings::V4L2_CTRL_FLAG_WRITE_ONLY | bindings::V4L2_CTRL_FLAG_DISABLED) == 0;
    let current = if scalar_type && readable {
        match v4l2r::ioctl::g_ctrl(file, raw.id) {
            Ok(value) => Some(Value::from(value)),
            Err(error) => {
                control_issue = Some(issue(
                    "control-read-failed",
                    format!("could not read control 0x{:08x}: {error}", raw.id),
                ));
                None
            }
        }
    } else {
        None
    };
    let current_in_range = current
        .as_ref()
        .and_then(Value::as_i64)
        .map(|value| value >= raw.minimum && value <= raw.maximum);

    let menu = if matches!(
        raw.type_,
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU
    ) {
        enumerate_menu(file, &raw, &mut control_issue)
    } else {
        Vec::new()
    };

    ControlReport {
        id: raw.id,
        name: c_char_string(&raw.name),
        control_type: raw.type_,
        flags: raw.flags,
        flag_names: control_flag_names(raw.flags),
        minimum: raw.minimum,
        maximum: raw.maximum,
        step: raw.step,
        default: raw.default_value,
        element_size: raw.elem_size,
        elements: raw.elems,
        dimensions: raw.dims[..usize::try_from(raw.nr_of_dims)
            .unwrap_or(0)
            .min(raw.dims.len())]
            .to_vec(),
        current,
        current_in_range,
        menu,
        issue: control_issue,
    }
}

fn enumerate_menu(
    file: &File,
    control: &v4l2_query_ext_ctrl,
    issue_slot: &mut Option<ProbeIssue>,
) -> Vec<ControlMenuItem> {
    let Ok(minimum) = u32::try_from(control.minimum) else {
        *issue_slot = Some(issue(
            "invalid-menu-range",
            format!("control 0x{:08x} has a negative menu minimum", control.id),
        ));
        return Vec::new();
    };
    let Ok(maximum) = u32::try_from(control.maximum) else {
        *issue_slot = Some(issue(
            "invalid-menu-range",
            format!("control 0x{:08x} has an invalid menu maximum", control.id),
        ));
        return Vec::new();
    };
    if maximum.saturating_sub(minimum) > 4096 {
        *issue_slot = Some(issue(
            "menu-range-too-large",
            format!(
                "control 0x{:08x} advertises more than 4097 menu entries",
                control.id
            ),
        ));
        return Vec::new();
    }

    (minimum..=maximum)
        .filter_map(
            |index| match abi::query_menu(file, control.id, index, control.type_) {
                Ok(item) => Some(ControlMenuItem { index, value: item }),
                Err(v4l2r::ioctl::QueryMenuError::InvalidIdOrIndex) => None,
                Err(error) => {
                    if issue_slot.is_none() {
                        *issue_slot = Some(issue(
                            "menu-read-failed",
                            format!(
                                "could not read menu index {index} for control 0x{:08x}: {error}",
                                control.id
                            ),
                        ));
                    }
                    None
                }
            },
        )
        .collect()
}

fn c_char_string(bytes: &[std::ffi::c_char]) -> String {
    let bytes = bytes
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn fourcc(format: PixelFormat) -> String {
    String::from_utf8_lossy(&format.to_fourcc()).into_owned()
}

fn control_flag_names(flags: u32) -> Vec<String> {
    [
        (bindings::V4L2_CTRL_FLAG_DISABLED, "disabled"),
        (bindings::V4L2_CTRL_FLAG_GRABBED, "grabbed"),
        (bindings::V4L2_CTRL_FLAG_READ_ONLY, "read-only"),
        (bindings::V4L2_CTRL_FLAG_UPDATE, "update"),
        (bindings::V4L2_CTRL_FLAG_INACTIVE, "inactive"),
        (bindings::V4L2_CTRL_FLAG_SLIDER, "slider"),
        (bindings::V4L2_CTRL_FLAG_WRITE_ONLY, "write-only"),
        (bindings::V4L2_CTRL_FLAG_VOLATILE, "volatile"),
        (bindings::V4L2_CTRL_FLAG_HAS_PAYLOAD, "has-payload"),
        (
            bindings::V4L2_CTRL_FLAG_EXECUTE_ON_WRITE,
            "execute-on-write",
        ),
        (bindings::V4L2_CTRL_FLAG_MODIFY_LAYOUT, "modify-layout"),
        (bindings::V4L2_CTRL_FLAG_DYNAMIC_ARRAY, "dynamic-array"),
        (
            bindings::V4L2_CTRL_FLAG_HAS_WHICH_MIN_MAX,
            "has-which-min-max",
        ),
    ]
    .into_iter()
    .filter(|(bit, _)| flags & bit != 0)
    .map(|(_, name)| name.to_owned())
    .collect()
}

#[allow(unsafe_code)]
mod abi {
    use std::fs::File;

    use serde_json::Value;
    use v4l2r::bindings::{self, v4l2_querymenu};
    use v4l2r::ioctl::QueryMenuError;

    pub(super) fn query_menu(
        file: &File,
        id: u32,
        index: u32,
        control_type: u32,
    ) -> Result<Value, QueryMenuError> {
        let raw = v4l2r::ioctl::querymenu::<v4l2_querymenu>(file, id, index)?;
        if control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU {
            // SAFETY: V4L2 defines the `value` union member for INTEGER_MENU controls.
            Ok(Value::from(unsafe { raw.__bindgen_anon_1.value }))
        } else {
            // SAFETY: V4L2 defines the `name` union member for MENU controls. Copying the
            // fixed array by value avoids taking a possibly unaligned reference to the packed ABI.
            let name = unsafe { raw.__bindgen_anon_1.name };
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(name.len());
            Ok(Value::from(
                String::from_utf8_lossy(&name[..end]).into_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{control_flag_names, fourcc};
    use v4l2r::{PixelFormat, bindings};

    #[test]
    fn fourcc_preserves_kernel_byte_order() {
        assert_eq!(fourcc(PixelFormat::from_fourcc(b"MJPG")), "MJPG");
    }

    #[test]
    fn control_flags_are_stable_and_readable() {
        assert_eq!(
            control_flag_names(
                bindings::V4L2_CTRL_FLAG_READ_ONLY | bindings::V4L2_CTRL_FLAG_VOLATILE
            ),
            ["read-only", "volatile"]
        );
    }
}
