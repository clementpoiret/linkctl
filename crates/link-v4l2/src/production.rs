//! Production standard-control access through V4L2.

use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::Duration,
};

use link_core::{
    ErrorKind, LinkError,
    control::{ControlDescriptor, ControlMenuEntry, ControlValue},
    probe::ProbeIssue,
};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use v4l2r::{
    bindings::{self, v4l2_query_ext_ctrl},
    ioctl::{
        CtrlWhich, GCtrlError, QueryCtrlError, QueryCtrlFlags, g_ctrl, g_ext_ctrls, query_ext_ctrl,
        s_ctrl, s_ext_ctrls,
    },
};

/// One raw scalar write prepared for the backend.
#[derive(Clone, Debug)]
pub struct RawControlWrite {
    pub descriptor: ControlDescriptor,
    pub value: i64,
}

/// A failed extended-control batch.
#[derive(Debug)]
pub struct BatchWriteError {
    pub error: LinkError,
    pub error_index: u32,
}

/// One kernel V4L2 control event.
#[derive(Clone, Copy, Debug)]
pub struct KernelControlEvent {
    pub id: u32,
    pub value: i64,
}

/// Event subscription for one capture/control node.
pub struct ControlEventMonitor {
    file: File,
    path: String,
}

impl ControlEventMonitor {
    /// Subscribe to each requested control ID.
    pub fn new(path: impl AsRef<Path>, control_ids: &[u32]) -> Result<Self, LinkError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(v4l2r::nix::libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| open_error(path, &error, false))?;
        for id in control_ids {
            v4l2r::ioctl::subscribe_event(
                &file,
                v4l2r::ioctl::EventType::Ctrl(*id),
                v4l2r::ioctl::SubscribeEventFlags::empty(),
            )
            .map_err(|error| {
                errno_error(
                    error.into(),
                    "driver does not support V4L2 control events",
                    &path.display().to_string(),
                )
            })?;
        }
        Ok(Self {
            file,
            path: path.display().to_string(),
        })
    }

    /// Wait for a burst of control events.
    pub fn wait(&self, timeout: Duration) -> Result<Vec<KernelControlEvent>, LinkError> {
        let timeout = Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(
            &self.file,
            PollFlags::PRI | PollFlags::IN | PollFlags::HUP | PollFlags::ERR,
        )];
        let ready = poll(&mut fds, Some(&timeout)).map_err(|error| {
            LinkError::new(
                ErrorKind::IoFailure,
                "failed while waiting for control events",
            )
            .with_detail("path", self.path.clone())
            .with_detail("reason", error.to_string())
        })?;
        if ready == 0 {
            return Ok(Vec::new());
        }
        if fds[0].revents().intersects(PollFlags::HUP | PollFlags::ERR) {
            return Err(LinkError::new(
                ErrorKind::DeviceNotFound,
                "V4L2 control-event node was removed",
            )
            .with_detail("path", self.path.clone()));
        }
        let mut events = Vec::new();
        loop {
            match v4l2r::ioctl::dqevent::<abi::RawControlEvent>(&self.file) {
                Ok(event) => events.push(KernelControlEvent {
                    id: event.id,
                    value: event.value,
                }),
                Err(v4l2r::ioctl::DqEventError::NotReady) => break,
                Err(error) => {
                    return Err(LinkError::new(
                        ErrorKind::IoFailure,
                        "failed to dequeue V4L2 control event",
                    )
                    .with_detail("path", self.path.clone())
                    .with_detail("reason", error.to_string()));
                }
            }
        }
        Ok(events)
    }
}

/// Open V4L2 node used for a bounded control transaction.
pub struct ControlDevice {
    file: File,
    path: String,
    writable: bool,
}

impl ControlDevice {
    /// Open a node for inspection only.
    pub fn open_read(path: impl AsRef<Path>) -> Result<Self, LinkError> {
        Self::open(path.as_ref(), false)
    }

    /// Open a node for control writes and readback.
    pub fn open_write(path: impl AsRef<Path>) -> Result<Self, LinkError> {
        Self::open(path.as_ref(), true)
    }

    fn open(path: &Path, writable: bool) -> Result<Self, LinkError> {
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        let file = options
            .open(path)
            .map_err(|error| open_error(path, &error, writable))?;
        Ok(Self {
            file,
            path: path.display().to_string(),
            writable,
        })
    }

    /// Enumerate every extended control and read supported scalar values.
    pub fn controls(&self) -> Result<Vec<ControlDescriptor>, LinkError> {
        let mut controls = Vec::new();
        let mut id = 0;
        loop {
            let control_id = v4l2r::ioctl::CtrlId::new(id).map_err(|error| {
                LinkError::new(ErrorKind::IoFailure, "invalid V4L2 control identifier")
                    .with_detail("reason", error.to_string())
            })?;
            let raw = match query_ext_ctrl::<v4l2_query_ext_ctrl>(
                &self.file,
                control_id,
                QueryCtrlFlags::NEXT | QueryCtrlFlags::COMPOUND,
            ) {
                Ok(raw) => raw,
                Err(QueryCtrlError::IoctlError(v4l2r::nix::errno::Errno::EINVAL)) => break,
                Err(QueryCtrlError::IoctlError(error)) => {
                    return Err(errno_error(
                        error,
                        "failed to enumerate V4L2 controls",
                        &self.path,
                    ));
                }
            };
            id = raw.id;
            controls.push(descriptor(&self.file, raw));
        }
        disambiguate_names(&mut controls);
        Ok(controls)
    }

    /// Re-query one exact control.
    pub fn query(&self, id: u32) -> Result<ControlDescriptor, LinkError> {
        let control_id = v4l2r::ioctl::CtrlId::new(id).map_err(|_| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "invalid V4L2 control identifier",
            )
            .with_detail("control_id", u64::from(id))
        })?;
        let raw =
            query_ext_ctrl::<v4l2_query_ext_ctrl>(&self.file, control_id, QueryCtrlFlags::empty())
                .map_err(|error| match error {
                    QueryCtrlError::IoctlError(v4l2r::nix::errno::Errno::EINVAL) => LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "V4L2 control is not exposed by the selected node",
                    )
                    .with_detail("control_id", u64::from(id)),
                    QueryCtrlError::IoctlError(errno) => {
                        errno_error(errno, "failed to query V4L2 control", &self.path)
                    }
                })?;
        Ok(descriptor(&self.file, raw))
    }

    /// Resolve a canonical name, kernel name, decimal ID, or hexadecimal ID.
    pub fn resolve(&self, selector: &str) -> Result<ControlDescriptor, LinkError> {
        if let Some(id) = parse_control_id(selector)? {
            return self.query(id);
        }
        let controls = self.controls()?;
        let normalized = canonicalize(selector);
        let matches = controls
            .into_iter()
            .filter(|control| {
                control.name == normalized
                    || control.kernel_name.eq_ignore_ascii_case(selector)
                    || canonicalize(&control.kernel_name) == normalized
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [control] => Ok(control.clone()),
            [] => Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "V4L2 control is not exposed by the selected node",
            )
            .with_detail("control", selector.to_owned())),
            _ => Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "V4L2 control name is ambiguous; use its numeric ID",
            )
            .with_detail("control", selector.to_owned())
            .with_detail("matches", matches.len() as u64)),
        }
    }

    /// Read one scalar control value using fresh metadata.
    pub fn get(&self, id: u32) -> Result<(ControlDescriptor, ControlValue), LinkError> {
        let control = self.query(id)?;
        let raw = read_scalar(&self.file, &control).map_err(|error| {
            ioctl_context(error, "failed to read V4L2 control", &self.path, &control)
        })?;
        let mut observed = control.clone();
        observed.current = Some(raw);
        observed.current_in_range = Some(raw >= observed.minimum && raw <= observed.maximum);
        Ok((observed.clone(), render_value(&observed, raw)))
    }

    /// Write one validated scalar and return the value supplied by the driver.
    pub fn set(&self, control: &ControlDescriptor, value: i64) -> Result<i64, LinkError> {
        self.ensure_write(control, value)?;
        if control.control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64 {
            let mut raw = abi::scalar64(control.id, value);
            s_ext_ctrls(
                &self.file,
                CtrlWhich::Current,
                std::slice::from_mut(&mut raw),
            )
            .map_err(|error| {
                errno_error(
                    error.error.into(),
                    "failed to write V4L2 control",
                    &self.path,
                )
                .with_detail("control", control.name.clone())
            })?;
            Ok(abi::value64(&raw))
        } else {
            let value = i32::try_from(value).map_err(|_| {
                LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "control value does not fit i32",
                )
                .with_detail("control", control.name.clone())
                .with_detail("value", value)
            })?;
            s_ctrl(&self.file, control.id, value)
                .map(i64::from)
                .map_err(|error| {
                    ioctl_context(
                        g_ctrl_error(error),
                        "failed to write V4L2 control",
                        &self.path,
                        control,
                    )
                })
        }
    }

    /// Submit a deterministic scalar batch using `VIDIOC_S_EXT_CTRLS`.
    pub fn set_batch(&self, writes: &[RawControlWrite]) -> Result<(), BatchWriteError> {
        for write in writes {
            self.ensure_write(&write.descriptor, write.value)
                .map_err(|error| BatchWriteError {
                    error,
                    error_index: u32::try_from(
                        writes
                            .iter()
                            .position(|candidate| candidate.descriptor.id == write.descriptor.id)
                            .unwrap_or_default(),
                    )
                    .unwrap_or(u32::MAX),
                })?;
        }
        let mut raw = writes
            .iter()
            .map(|write| abi::scalar(&write.descriptor, write.value))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| BatchWriteError {
                error,
                error_index: 0,
            })?;
        s_ext_ctrls(&self.file, CtrlWhich::Current, raw.as_mut_slice()).map_err(|error| {
            BatchWriteError {
                error: errno_error(
                    error.error.into(),
                    "failed to write V4L2 control batch",
                    &self.path,
                )
                .with_detail("error_index", u64::from(error.error_idx)),
                error_index: error.error_idx,
            }
        })
    }

    fn ensure_write(&self, control: &ControlDescriptor, value: i64) -> Result<(), LinkError> {
        if !self.writable {
            return Err(LinkError::new(
                ErrorKind::PermissionDenied,
                "V4L2 node was not opened for writes",
            ));
        }
        if is_movement_control(control.id) {
            return Err(LinkError::new(
                ErrorKind::UnsafeOperationDenied,
                "pan and tilt control writes are disabled for the fixed-mount camera",
            )
            .with_detail("control", control.name.clone())
            .with_detail("control_id", u64::from(control.id)));
        }
        if !control.writable || !control.codec_supported {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "V4L2 control is not writable by this backend",
            )
            .with_detail("control", control.name.clone()));
        }
        validate_value(control, value)
    }
}

/// Return true for mechanical movement IDs that the fixed camera must never write.
#[must_use]
pub const fn is_movement_control(id: u32) -> bool {
    matches!(
        id,
        bindings::V4L2_CID_PAN_ABSOLUTE
            | bindings::V4L2_CID_TILT_ABSOLUTE
            | bindings::V4L2_CID_PAN_RELATIVE
            | bindings::V4L2_CID_TILT_RELATIVE
            | bindings::V4L2_CID_PAN_RESET
            | bindings::V4L2_CID_TILT_RESET
            | bindings::V4L2_CID_PAN_SPEED
            | bindings::V4L2_CID_TILT_SPEED
    )
}

/// Known parent dependencies and raw values that enable manual operation.
#[must_use]
pub fn manual_dependencies(id: u32) -> Vec<(u32, i64)> {
    match id {
        bindings::V4L2_CID_WHITE_BALANCE_TEMPERATURE => {
            vec![(bindings::V4L2_CID_AUTO_WHITE_BALANCE, 0)]
        }
        bindings::V4L2_CID_FOCUS_ABSOLUTE => vec![(bindings::V4L2_CID_FOCUS_AUTO, 0)],
        bindings::V4L2_CID_EXPOSURE_ABSOLUTE => vec![(
            bindings::V4L2_CID_EXPOSURE_AUTO,
            i64::from(bindings::v4l2_exposure_auto_type_V4L2_EXPOSURE_MANUAL),
        )],
        bindings::V4L2_CID_ISO_SENSITIVITY => vec![
            (
                bindings::V4L2_CID_EXPOSURE_AUTO,
                i64::from(bindings::v4l2_exposure_auto_type_V4L2_EXPOSURE_MANUAL),
            ),
            (
                bindings::V4L2_CID_ISO_SENSITIVITY_AUTO,
                i64::from(bindings::v4l2_iso_sensitivity_auto_type_V4L2_ISO_SENSITIVITY_MANUAL),
            ),
        ],
        bindings::V4L2_CID_GAIN => vec![(bindings::V4L2_CID_AUTOGAIN, 0)],
        _ => Vec::new(),
    }
}

/// Parse and validate a user-facing generic value.
pub fn parse_value(
    control: &ControlDescriptor,
    input: &str,
    clamp: bool,
) -> Result<ControlValue, LinkError> {
    let lower = input.trim().to_ascii_lowercase();
    let raw = if control.control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BOOLEAN {
        match lower.as_str() {
            "1" | "true" | "yes" | "on" | "enable" | "enabled" => 1,
            "0" | "false" | "no" | "off" | "disable" | "disabled" => 0,
            _ => parse_integer(input)?,
        }
    } else if matches!(
        control.control_type,
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU
    ) {
        if let Some(entry) = control
            .menu
            .iter()
            .find(|entry| canonicalize(&entry.label) == canonicalize(input))
        {
            entry.index
        } else {
            parse_integer(input)?
        }
    } else if let Some(percent) = lower.strip_suffix('%') {
        let percent = percent
            .trim()
            .parse::<f64>()
            .map_err(|_| invalid_value(control, input, "normalized percentage is not a number"))?;
        normalized_to_raw(control, percent / 100.0, clamp)?
    } else {
        parse_integer(input)?
    };
    validate_value_with_clamp(control, raw, clamp).map(|raw| render_value(control, raw))
}

/// Translate a semantic 0.0–1.0 scalar to a raw value.
pub fn normalized_to_raw(
    control: &ControlDescriptor,
    normalized: f64,
    clamp: bool,
) -> Result<i64, LinkError> {
    if !normalized.is_finite() {
        return Err(invalid_value(control, "non-finite", "value must be finite"));
    }
    let normalized = if clamp {
        normalized.clamp(0.0, 1.0)
    } else if !(0.0..=1.0).contains(&normalized) {
        return Err(invalid_value(
            control,
            normalized.to_string(),
            "normalized value must be between 0.0 and 1.0",
        ));
    } else {
        normalized
    };
    let step = i64::try_from(control.step.max(1)).unwrap_or(i64::MAX);
    let span = control.maximum.saturating_sub(control.minimum);
    let steps = span / step;
    let selected = (normalized * steps as f64).round() as i64;
    Ok(control
        .minimum
        .saturating_add(selected.saturating_mul(step)))
}

/// Render a raw value using the descriptor's semantic metadata.
#[must_use]
pub fn render_value(control: &ControlDescriptor, raw: i64) -> ControlValue {
    let normalized = if control.maximum > control.minimum
        && matches!(
            control.control_type,
            bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER
                | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64
        ) {
        Some((raw - control.minimum) as f64 / (control.maximum - control.minimum) as f64)
    } else {
        None
    };
    let label = if control.control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BOOLEAN {
        Some(if raw == 0 { "off" } else { "on" }.to_owned())
    } else {
        control
            .menu
            .iter()
            .find(|entry| entry.index == raw)
            .map(|entry| entry.label.clone())
    };
    ControlValue {
        raw,
        normalized,
        label,
    }
}

fn descriptor(file: &File, raw: v4l2_query_ext_ctrl) -> ControlDescriptor {
    let scalar = scalar_type(raw.type_);
    let readable = scalar
        && raw.type_ != bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BUTTON
        && raw.flags & (bindings::V4L2_CTRL_FLAG_WRITE_ONLY | bindings::V4L2_CTRL_FLAG_DISABLED)
            == 0;
    let codec_supported = scalar;
    let writable = codec_supported
        && raw.type_ != bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_CTRL_CLASS
        && raw.flags & (bindings::V4L2_CTRL_FLAG_READ_ONLY | bindings::V4L2_CTRL_FLAG_DISABLED)
            == 0;
    let available = raw.flags
        & (bindings::V4L2_CTRL_FLAG_DISABLED
            | bindings::V4L2_CTRL_FLAG_GRABBED
            | bindings::V4L2_CTRL_FLAG_INACTIVE)
        == 0;
    let kernel_name = c_char_string(&raw.name);
    let name = known_name(raw.id)
        .map(str::to_owned)
        .unwrap_or_else(|| canonicalize(&kernel_name));
    let mut issue = None;
    let current = if readable {
        match read_raw(file, raw.id, raw.type_) {
            Ok(value) => Some(value),
            Err(error) => {
                issue = Some(ProbeIssue::new(
                    "v4l2",
                    error.kind().code(),
                    error.message(),
                ));
                None
            }
        }
    } else {
        None
    };
    let menu = if matches!(
        raw.type_,
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU
    ) {
        enumerate_menu(file, &raw, &mut issue)
    } else {
        Vec::new()
    };
    let control_class = raw.id & 0x0fff_0000;
    let mut descriptor = ControlDescriptor {
        id: raw.id,
        id_hex: format!("0x{:08x}", raw.id),
        name,
        kernel_name,
        control_type: raw.type_,
        control_type_name: control_type_name(raw.type_).to_owned(),
        control_class,
        control_class_name: control_class_name(control_class).to_owned(),
        flags: raw.flags,
        flag_names: control_flag_names(raw.flags),
        minimum: raw.minimum,
        maximum: raw.maximum,
        step: raw.step,
        default: raw.default_value,
        current,
        current_in_range: current.map(|value| value >= raw.minimum && value <= raw.maximum),
        default_is_valid: false,
        menu,
        readable,
        writable,
        available,
        codec_supported,
        dependencies: dependency_names(raw.id),
        issue,
    };
    descriptor.default_is_valid = validate_value(&descriptor, descriptor.default).is_ok();
    descriptor
}

fn read_raw(file: &File, id: u32, control_type: u32) -> Result<i64, LinkError> {
    if control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64 {
        let mut raw = abi::scalar64(id, 0);
        g_ext_ctrls(file, CtrlWhich::Current, std::slice::from_mut(&mut raw))
            .map_err(|error| errno_error(error.error.into(), "failed to read V4L2 control", ""))?;
        Ok(abi::value64(&raw))
    } else {
        g_ctrl(file, id).map(i64::from).map_err(g_ctrl_error)
    }
}

fn read_scalar(file: &File, control: &ControlDescriptor) -> Result<i64, LinkError> {
    if !control.readable || !control.codec_supported {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "V4L2 control is not readable by this backend",
        )
        .with_detail("control", control.name.clone()));
    }
    read_raw(file, control.id, control.control_type)
}

fn validate_value(control: &ControlDescriptor, value: i64) -> Result<(), LinkError> {
    validate_value_with_clamp(control, value, false).map(|_| ())
}

/// Validate a raw value without writing it.
pub fn validate_raw_value(control: &ControlDescriptor, value: i64) -> Result<(), LinkError> {
    if is_movement_control(control.id) {
        return Err(LinkError::new(
            ErrorKind::UnsafeOperationDenied,
            "pan and tilt control writes are disabled for the fixed-mount camera",
        )
        .with_detail("control", control.name.clone())
        .with_detail("control_id", u64::from(control.id)));
    }
    if !control.writable || !control.codec_supported {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "V4L2 control is not writable by this backend",
        )
        .with_detail("control", control.name.clone()));
    }
    validate_value(control, value)
}

fn validate_value_with_clamp(
    control: &ControlDescriptor,
    value: i64,
    clamp: bool,
) -> Result<i64, LinkError> {
    if control.control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BUTTON {
        return Ok(0);
    }
    let value = if clamp {
        value.clamp(control.minimum, control.maximum)
    } else if value < control.minimum || value > control.maximum {
        return Err(invalid_value(
            control,
            value.to_string(),
            "value is outside the range",
        ));
    } else {
        value
    };
    if matches!(
        control.control_type,
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU
    ) && !control.menu.iter().any(|entry| entry.index == value)
    {
        return Err(invalid_value(
            control,
            value.to_string(),
            "menu value is not advertised",
        ));
    }
    let step = i64::try_from(control.step).unwrap_or(i64::MAX);
    if step > 1 && (value - control.minimum).rem_euclid(step) != 0 {
        return Err(invalid_value(
            control,
            value.to_string(),
            "value does not match the step",
        ));
    }
    Ok(value)
}

fn invalid_value(control: &ControlDescriptor, value: impl Into<String>, reason: &str) -> LinkError {
    LinkError::new(ErrorKind::InvalidInvocation, "invalid V4L2 control value")
        .with_detail("control", control.name.clone())
        .with_detail("value", value.into())
        .with_detail("reason", reason)
        .with_detail("minimum", control.minimum)
        .with_detail("maximum", control.maximum)
        .with_detail("step", control.step)
}

fn parse_integer(input: &str) -> Result<i64, LinkError> {
    let trimmed = input.trim();
    if let Some(value) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        i64::from_str_radix(value, 16)
    } else {
        trimmed.parse()
    }
    .map_err(|_| {
        LinkError::new(
            ErrorKind::InvalidInvocation,
            "control value is not an integer",
        )
        .with_detail("value", input.to_owned())
    })
}

fn parse_control_id(input: &str) -> Result<Option<u32>, LinkError> {
    let parsed = if let Some(value) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        Some(u32::from_str_radix(value, 16))
    } else if input.chars().all(|character| character.is_ascii_digit()) {
        Some(input.parse())
    } else {
        None
    };
    parsed
        .transpose()
        .map_err(|_| LinkError::new(ErrorKind::InvalidInvocation, "invalid control identifier"))
}

fn scalar_type(control_type: u32) -> bool {
    matches!(
        control_type,
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BOOLEAN
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BUTTON
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_CTRL_CLASS
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BITMASK
            | bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU
    )
}

fn known_name(id: u32) -> Option<&'static str> {
    Some(match id {
        bindings::V4L2_CID_BRIGHTNESS => "brightness",
        bindings::V4L2_CID_CONTRAST => "contrast",
        bindings::V4L2_CID_SATURATION => "saturation",
        bindings::V4L2_CID_HUE => "hue",
        bindings::V4L2_CID_AUTO_WHITE_BALANCE => "white_balance_automatic",
        bindings::V4L2_CID_POWER_LINE_FREQUENCY => "power_line_frequency",
        bindings::V4L2_CID_WHITE_BALANCE_TEMPERATURE => "white_balance_temperature",
        bindings::V4L2_CID_SHARPNESS => "sharpness",
        bindings::V4L2_CID_BACKLIGHT_COMPENSATION => "backlight_compensation",
        bindings::V4L2_CID_AUTOGAIN => "gain_automatic",
        bindings::V4L2_CID_GAIN => "gain",
        bindings::V4L2_CID_EXPOSURE_AUTO => "exposure_automatic",
        bindings::V4L2_CID_EXPOSURE_ABSOLUTE => "exposure_time_absolute",
        bindings::V4L2_CID_AUTO_EXPOSURE_BIAS => "exposure_compensation",
        bindings::V4L2_CID_ISO_SENSITIVITY => "iso_sensitivity",
        bindings::V4L2_CID_ISO_SENSITIVITY_AUTO => "iso_sensitivity_automatic",
        bindings::V4L2_CID_FOCUS_ABSOLUTE => "focus_absolute",
        bindings::V4L2_CID_FOCUS_AUTO => "focus_automatic_continuous",
        bindings::V4L2_CID_WIDE_DYNAMIC_RANGE => "wide_dynamic_range",
        bindings::V4L2_CID_HDR_SENSOR_MODE => "hdr_sensor_mode",
        bindings::V4L2_CID_ZOOM_ABSOLUTE => "zoom_absolute",
        bindings::V4L2_CID_PAN_ABSOLUTE => "pan_absolute",
        bindings::V4L2_CID_TILT_ABSOLUTE => "tilt_absolute",
        bindings::V4L2_CID_PAN_SPEED => "pan_speed",
        bindings::V4L2_CID_TILT_SPEED => "tilt_speed",
        _ => return None,
    })
}

fn dependency_names(id: u32) -> Vec<String> {
    manual_dependencies(id)
        .into_iter()
        .filter_map(|(parent, _)| known_name(parent).map(str::to_owned))
        .collect()
}

/// Convert a kernel control name or menu label to stable snake case.
#[must_use]
pub fn canonicalize(input: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !output.is_empty() {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    output
}

fn disambiguate_names(controls: &mut [ControlDescriptor]) {
    for index in 0..controls.len() {
        if controls
            .iter()
            .enumerate()
            .any(|(other, control)| other != index && control.name == controls[index].name)
        {
            controls[index].name = format!("{}_0x{:08x}", controls[index].name, controls[index].id);
        }
    }
}

fn enumerate_menu(
    file: &File,
    control: &v4l2_query_ext_ctrl,
    issue: &mut Option<ProbeIssue>,
) -> Vec<ControlMenuEntry> {
    if control.minimum < 0
        || control.maximum < control.minimum
        || control.maximum.saturating_sub(control.minimum) > 4096
    {
        *issue = Some(ProbeIssue::new(
            "v4l2",
            "invalid-menu-range",
            format!("control 0x{:08x} has an invalid menu range", control.id),
        ));
        return Vec::new();
    }
    (control.minimum..=control.maximum)
        .filter_map(|index| {
            let index_u32 = u32::try_from(index).ok()?;
            match abi::query_menu(file, control.id, index_u32, control.type_) {
                Ok(label) => Some(ControlMenuEntry { index, label }),
                Err(v4l2r::ioctl::QueryMenuError::InvalidIdOrIndex) => None,
                Err(error) => {
                    if issue.is_none() {
                        *issue = Some(ProbeIssue::new(
                            "v4l2",
                            "menu-read-failed",
                            format!("could not read menu item {index}: {error}"),
                        ));
                    }
                    None
                }
            }
        })
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

fn control_type_name(value: u32) -> &'static str {
    match value {
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER => "integer",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BOOLEAN => "boolean",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU => "menu",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BUTTON => "button",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64 => "integer64",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_CTRL_CLASS => "class",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_STRING => "string",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_BITMASK => "bitmask",
        bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU => "integer-menu",
        bindings::v4l2_ctrl_type_V4L2_CTRL_COMPOUND_TYPES => "compound",
        _ => "unknown",
    }
}

fn control_class_name(value: u32) -> &'static str {
    match value {
        bindings::V4L2_CTRL_CLASS_USER => "user",
        bindings::V4L2_CTRL_CLASS_CAMERA => "camera",
        bindings::V4L2_CTRL_CLASS_CODEC => "codec",
        bindings::V4L2_CTRL_CLASS_IMAGE_SOURCE => "image-source",
        bindings::V4L2_CTRL_CLASS_IMAGE_PROC => "image-processing",
        bindings::V4L2_CTRL_CLASS_COLORIMETRY => "colorimetry",
        _ => "unknown",
    }
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

fn open_error(path: &Path, error: &io::Error, writable: bool) -> LinkError {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => ErrorKind::DeviceNotFound,
        io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
        _ if error.raw_os_error() == Some(v4l2r::nix::libc::EBUSY) => ErrorKind::DeviceBusy,
        _ => ErrorKind::IoFailure,
    };
    LinkError::new(
        kind,
        if writable {
            "failed to open V4L2 node for control writes"
        } else {
            "failed to open V4L2 node for control reads"
        },
    )
    .with_detail("path", path.display().to_string())
    .with_detail("reason", error.to_string())
}

fn g_ctrl_error(error: GCtrlError) -> LinkError {
    match error {
        GCtrlError::Invalid => LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "driver rejected the V4L2 control or value",
        ),
        GCtrlError::IoctlError(errno) => errno_error(errno, "V4L2 control ioctl failed", ""),
    }
}

fn errno_error(errno: v4l2r::nix::errno::Errno, message: &'static str, path: &str) -> LinkError {
    let kind = match errno {
        v4l2r::nix::errno::Errno::ENOENT | v4l2r::nix::errno::Errno::ENODEV => {
            ErrorKind::DeviceNotFound
        }
        v4l2r::nix::errno::Errno::EBUSY => ErrorKind::DeviceBusy,
        v4l2r::nix::errno::Errno::EACCES | v4l2r::nix::errno::Errno::EPERM => {
            ErrorKind::PermissionDenied
        }
        v4l2r::nix::errno::Errno::ETIMEDOUT => ErrorKind::Timeout,
        _ => ErrorKind::IoFailure,
    };
    let error = LinkError::new(kind, message).with_detail("reason", errno.to_string());
    if path.is_empty() {
        error
    } else {
        error.with_detail("path", path.to_owned())
    }
}

fn ioctl_context(
    error: LinkError,
    message: &'static str,
    path: &str,
    control: &ControlDescriptor,
) -> LinkError {
    LinkError::new(error.kind(), message)
        .with_detail("path", path.to_owned())
        .with_detail("control", control.name.clone())
        .with_detail("control_id", u64::from(control.id))
        .with_detail("reason", error.message())
}

#[allow(unsafe_code)]
mod abi {
    use std::fs::File;

    use link_core::{ErrorKind, LinkError, control::ControlDescriptor};
    use v4l2r::{
        bindings::{
            self, v4l2_event, v4l2_ext_control, v4l2_ext_control__bindgen_ty_1, v4l2_querymenu,
        },
        ioctl::QueryMenuError,
    };

    pub(super) struct RawControlEvent {
        pub id: u32,
        pub value: i64,
    }

    impl TryFrom<v4l2_event> for RawControlEvent {
        type Error = ();

        fn try_from(event: v4l2_event) -> Result<Self, Self::Error> {
            if event.type_ != bindings::V4L2_EVENT_CTRL {
                return Err(());
            }
            // SAFETY: the event type selects the `ctrl` member of the V4L2 event union.
            let control = unsafe { event.u.ctrl };
            let value = if control.type_ == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64 {
                // SAFETY: INTEGER64 control events select the `value64` union member.
                unsafe { control.__bindgen_anon_1.value64 }
            } else {
                // SAFETY: scalar non-INTEGER64 control events select the `value` member.
                i64::from(unsafe { control.__bindgen_anon_1.value })
            };
            Ok(Self {
                id: event.id,
                value,
            })
        }
    }

    pub(super) fn scalar(
        control: &ControlDescriptor,
        value: i64,
    ) -> Result<v4l2_ext_control, LinkError> {
        if control.control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER64 {
            Ok(scalar64(control.id, value))
        } else {
            let value = i32::try_from(value).map_err(|_| {
                LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "control value does not fit i32",
                )
                .with_detail("control", control.name.clone())
            })?;
            Ok(v4l2_ext_control {
                id: control.id,
                __bindgen_anon_1: v4l2_ext_control__bindgen_ty_1 { value },
                ..Default::default()
            })
        }
    }

    pub(super) fn scalar64(id: u32, value64: i64) -> v4l2_ext_control {
        v4l2_ext_control {
            id,
            __bindgen_anon_1: v4l2_ext_control__bindgen_ty_1 { value64 },
            ..Default::default()
        }
    }

    pub(super) fn value64(control: &v4l2_ext_control) -> i64 {
        // SAFETY: callers construct this control with the `value64` union member and use it only
        // for an INTEGER64 ioctl.
        unsafe { control.__bindgen_anon_1.value64 }
    }

    pub(super) fn query_menu(
        file: &File,
        id: u32,
        index: u32,
        control_type: u32,
    ) -> Result<String, QueryMenuError> {
        let raw = v4l2r::ioctl::querymenu::<v4l2_querymenu>(file, id, index)?;
        if control_type == bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER_MENU {
            // SAFETY: V4L2 defines `value` for INTEGER_MENU entries.
            Ok(unsafe { raw.__bindgen_anon_1.value }.to_string())
        } else {
            // SAFETY: V4L2 defines `name` for MENU entries. Copying avoids an unaligned borrow.
            let name = unsafe { raw.__bindgen_anon_1.name };
            let end = name
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(name.len());
            Ok(String::from_utf8_lossy(&name[..end]).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use link_core::control::ControlDescriptor;
    use v4l2r::bindings;

    use super::{
        canonicalize, is_movement_control, manual_dependencies, normalized_to_raw, parse_value,
        validate_raw_value,
    };

    fn integer() -> ControlDescriptor {
        ControlDescriptor {
            id: bindings::V4L2_CID_BRIGHTNESS,
            id_hex: "0x00980900".into(),
            name: "brightness".into(),
            kernel_name: "Brightness".into(),
            control_type: bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_INTEGER,
            control_type_name: "integer".into(),
            control_class: bindings::V4L2_CTRL_CLASS_USER,
            control_class_name: "user".into(),
            flags: 0,
            flag_names: Vec::new(),
            minimum: 0,
            maximum: 100,
            step: 1,
            default: 50,
            current: Some(50),
            current_in_range: Some(true),
            default_is_valid: true,
            menu: Vec::new(),
            readable: true,
            writable: true,
            available: true,
            codec_supported: true,
            dependencies: Vec::new(),
            issue: None,
        }
    }

    #[test]
    fn canonical_names_match_v4l2_ctl_style() {
        assert_eq!(
            canonicalize("White Balance, Automatic"),
            "white_balance_automatic"
        );
    }

    #[test]
    fn normalized_values_round_trip_to_raw_steps() {
        let control = integer();
        assert_eq!(normalized_to_raw(&control, 0.52, false).unwrap(), 52);
        assert_eq!(parse_value(&control, "52%", false).unwrap().raw, 52);
        assert!(normalized_to_raw(&control, 1.1, false).is_err());
        assert_eq!(normalized_to_raw(&control, 1.1, true).unwrap(), 100);
    }

    #[test]
    fn movement_ids_are_always_denied_by_policy() {
        assert!(is_movement_control(bindings::V4L2_CID_PAN_ABSOLUTE));
        assert!(is_movement_control(bindings::V4L2_CID_TILT_SPEED));
        assert!(!is_movement_control(bindings::V4L2_CID_ZOOM_ABSOLUTE));
        let mut control = integer();
        control.id = bindings::V4L2_CID_PAN_ABSOLUTE;
        control.name = "pan_absolute".into();
        let error = validate_raw_value(&control, 0).unwrap_err();
        assert_eq!(error.kind(), link_core::ErrorKind::UnsafeOperationDenied);
    }

    #[test]
    fn manual_iso_disables_exposure_and_iso_automation() {
        assert_eq!(
            manual_dependencies(bindings::V4L2_CID_ISO_SENSITIVITY),
            [
                (
                    bindings::V4L2_CID_EXPOSURE_AUTO,
                    i64::from(bindings::v4l2_exposure_auto_type_V4L2_EXPOSURE_MANUAL),
                ),
                (
                    bindings::V4L2_CID_ISO_SENSITIVITY_AUTO,
                    i64::from(bindings::v4l2_iso_sensitivity_auto_type_V4L2_ISO_SENSITIVITY_MANUAL),
                ),
            ]
        );
    }

    #[test]
    fn manual_white_balance_disables_automatic_white_balance() {
        assert_eq!(
            manual_dependencies(bindings::V4L2_CID_WHITE_BALANCE_TEMPERATURE),
            [(bindings::V4L2_CID_AUTO_WHITE_BALANCE, 0)]
        );
    }

    #[test]
    fn manual_focus_disables_continuous_autofocus() {
        assert_eq!(
            manual_dependencies(bindings::V4L2_CID_FOCUS_ABSOLUTE),
            [(bindings::V4L2_CID_FOCUS_AUTO, 0)]
        );
    }

    #[test]
    fn invalid_driver_defaults_are_detectable_without_writing() {
        let mut control = integer();
        control.control_type = bindings::v4l2_ctrl_type_V4L2_CTRL_TYPE_MENU;
        control.control_type_name = "menu".into();
        control.minimum = 0;
        control.maximum = 2;
        control.default = 3;
        control.menu = vec![
            link_core::control::ControlMenuEntry {
                index: 0,
                label: "Disabled".into(),
            },
            link_core::control::ControlMenuEntry {
                index: 1,
                label: "50 Hz".into(),
            },
            link_core::control::ControlMenuEntry {
                index: 2,
                label: "60 Hz".into(),
            },
        ];
        assert!(validate_raw_value(&control, control.default).is_err());
        assert_eq!(parse_value(&control, "50 Hz", false).unwrap().raw, 1);
    }
}
