//! Exact V4L2 format enumeration and negotiation.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use link_core::{
    ErrorKind, LinkError,
    media::{
        FormatSetReport, VideoFormatCapability, VideoFormatInventory, VideoStatus, VideoTuple,
        rational_cmp,
    },
    probe::{
        FrameInterval, FrameSize, NodeAssociation, Rational, VideoFormatReport, VideoNodeKind,
    },
};
use v4l2r::{
    Format, PixelFormat, QueueType,
    bindings::{self, v4l2_fract},
    ioctl::Capability,
};

/// Open capture-node format access.
pub struct VideoDevice {
    file: File,
    path: PathBuf,
    writable: bool,
    queue: QueueType,
}

impl VideoDevice {
    /// Open a capture node without permission to change its format.
    pub fn open_read(path: impl AsRef<Path>) -> Result<Self, LinkError> {
        Self::open(path.as_ref(), false)
    }

    /// Open a capture node for format negotiation and readback.
    pub fn open_write(path: impl AsRef<Path>) -> Result<Self, LinkError> {
        Self::open(path.as_ref(), true)
    }

    fn open(path: &Path, writable: bool) -> Result<Self, LinkError> {
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        let file = options
            .open(path)
            .map_err(|error| open_error(path, &error, writable))?;
        let capability: Capability = v4l2r::ioctl::querycap(&file).map_err(|error| {
            LinkError::new(ErrorKind::IoFailure, "VIDIOC_QUERYCAP failed")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
        let capabilities = capability.device_caps();
        let queue = if capabilities.contains(v4l2r::ioctl::Capabilities::VIDEO_CAPTURE) {
            QueueType::VideoCapture
        } else if capabilities.contains(v4l2r::ioctl::Capabilities::VIDEO_CAPTURE_MPLANE) {
            QueueType::VideoCaptureMplane
        } else {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "selected V4L2 node is not a video capture node",
            )
            .with_detail("path", path.display().to_string()));
        };
        Ok(Self {
            file,
            path: path.to_owned(),
            writable,
            queue,
        })
    }

    /// Enumerate raw ranges plus every discrete tuple and its derived annotations.
    pub fn formats(&self) -> Result<VideoFormatInventory, LinkError> {
        let report = crate::probe_node(NodeAssociation {
            path: self.path.display().to_string(),
            by_id: Vec::new(),
            by_path: Vec::new(),
        });
        if report.kind != VideoNodeKind::Capture {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "selected V4L2 node cannot capture video",
            ));
        }
        if report.formats.is_empty() && !report.issues.is_empty() {
            return Err(LinkError::new(
                ErrorKind::IoFailure,
                "V4L2 format enumeration returned no formats",
            )
            .with_detail(
                "issues",
                serde_json::to_value(report.issues).unwrap_or_default(),
            ));
        }
        let discrete = flatten_discrete(&report.formats);
        Ok(VideoFormatInventory {
            node: self.path.display().to_string(),
            formats: report.formats,
            discrete,
        })
    }

    /// Read the current format, frame rate, and colorimetry.
    pub fn status(&self) -> Result<VideoStatus, LinkError> {
        let raw = abi::g_format(&self.file, self.queue)
            .map_err(|error| ioctl_error(error, "VIDIOC_G_FMT failed", &self.path))?;
        let parameters = abi::get_parameters(&self.file, self.queue)
            .map_err(|error| ioctl_error(error, "VIDIOC_G_PARM failed", &self.path))?;
        let fps = fps_from_time_per_frame(parameters.timeperframe)?;
        Ok(VideoStatus {
            node: self.path.display().to_string(),
            tuple: VideoTuple {
                fourcc: String::from_utf8_lossy(&raw.pixelformat.to_le_bytes()).into_owned(),
                width: raw.width,
                height: raw.height,
                fps,
            }
            .normalized(),
            field: raw.field,
            colorspace: raw.colorspace,
            transfer_function: raw.transfer_function,
            ycbcr_encoding: raw.ycbcr_encoding,
            quantization: raw.quantization,
            bytes_per_line: raw.bytes_per_line,
            size_image: raw.size_image,
        })
    }

    /// Validate a tuple against live enumeration and `VIDIOC_TRY_FMT` without changing state.
    pub fn validate(&self, requested: &VideoTuple) -> Result<FormatSetReport, LinkError> {
        let requested = requested.clone().normalized();
        validate_tuple(&self.formats()?.formats, &requested)?;
        let previous = self.status()?.tuple;
        let tried = v4l2r::ioctl::try_fmt::<_, Format>(
            &self.file,
            (self.queue, &format_from_tuple(&requested)),
        )
        .map_err(|error| {
            LinkError::new(ErrorKind::IoFailure, "VIDIOC_TRY_FMT failed")
                .with_detail("path", self.path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
        let tried_tuple = VideoTuple {
            fourcc: fourcc(tried.pixelformat),
            width: tried.width,
            height: tried.height,
            fps: requested.fps,
        }
        .normalized();
        if !requested.equivalent(&tried_tuple) {
            return Err(format_mismatch(
                "driver adjusted the requested V4L2 format during dry-run",
                &requested,
                &tried_tuple,
                None,
            ));
        }
        Ok(FormatSetReport {
            requested,
            previous,
            applied: tried_tuple,
            verified: true,
            dry_run: true,
            rollback_succeeded: None,
        })
    }

    /// Apply a fully validated tuple and verify readback, restoring the previous tuple on mismatch.
    pub fn set_format(&mut self, requested: &VideoTuple) -> Result<FormatSetReport, LinkError> {
        if !self.writable {
            return Err(LinkError::new(
                ErrorKind::PermissionDenied,
                "V4L2 node was not opened for format changes",
            ));
        }
        let requested = requested.clone().normalized();
        validate_tuple(&self.formats()?.formats, &requested)?;
        let previous = self.status()?.tuple;
        if let Err(error) = self.apply(&requested) {
            let rollback_succeeded = self
                .apply(&previous)
                .and_then(|()| self.status())
                .is_ok_and(|status| status.tuple.equivalent(&previous));
            return Err(error
                .with_detail(
                    "requested",
                    serde_json::to_value(&requested).unwrap_or_default(),
                )
                .with_detail(
                    "previous",
                    serde_json::to_value(&previous).unwrap_or_default(),
                )
                .with_detail("rollback_succeeded", rollback_succeeded));
        }
        let applied = self.status()?.tuple;
        if !requested.equivalent(&applied) {
            let rollback_succeeded = self
                .apply(&previous)
                .and_then(|()| self.status())
                .is_ok_and(|status| status.tuple.equivalent(&previous));
            return Err(format_mismatch(
                "V4L2 format readback did not match the requested tuple",
                &requested,
                &applied,
                Some(rollback_succeeded),
            ));
        }
        Ok(FormatSetReport {
            requested,
            previous,
            applied,
            verified: true,
            dry_run: false,
            rollback_succeeded: None,
        })
    }

    fn apply(&mut self, tuple: &VideoTuple) -> Result<(), LinkError> {
        v4l2r::ioctl::s_fmt::<_, Format>(&mut self.file, (self.queue, &format_from_tuple(tuple)))
            .map_err(|error| {
            let kind = if matches!(error, v4l2r::ioctl::SFmtError::DeviceBusy) {
                ErrorKind::DeviceBusy
            } else {
                ErrorKind::IoFailure
            };
            LinkError::new(kind, "VIDIOC_S_FMT failed")
                .with_detail("path", self.path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
        let timeperframe = v4l2_fract {
            numerator: tuple.fps.denominator,
            denominator: tuple.fps.numerator,
        };
        abi::set_parameters(&self.file, self.queue, timeperframe)
            .map_err(|error| ioctl_error(error, "VIDIOC_S_PARM failed", &self.path))?;
        Ok(())
    }
}

/// Validate an exact tuple against discrete or stepwise enumeration.
pub fn validate_tuple(
    formats: &[VideoFormatReport],
    requested: &VideoTuple,
) -> Result<(), LinkError> {
    if requested.width == 0
        || requested.height == 0
        || requested.fps.numerator == 0
        || requested.fps.denominator == 0
        || requested.fourcc.len() != 4
    {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "video tuple contains an invalid FourCC, size, or frame rate",
        ));
    }
    let format = formats
        .iter()
        .find(|format| format.fourcc.eq_ignore_ascii_case(&requested.fourcc))
        .ok_or_else(|| unsupported_tuple(requested, "FourCC is not enumerated"))?;
    let time_per_frame = Rational {
        numerator: requested.fps.denominator,
        denominator: requested.fps.numerator,
    };
    let supported = format.sizes.iter().any(|size| match size {
        FrameSize::Discrete {
            width,
            height,
            intervals,
        } => {
            *width == requested.width
                && *height == requested.height
                && intervals
                    .iter()
                    .any(|interval| interval_supports(interval, time_per_frame))
        }
        FrameSize::Stepwise {
            min_width,
            max_width,
            step_width,
            min_height,
            max_height,
            step_height,
        } => {
            in_step_range(requested.width, *min_width, *max_width, *step_width)
                && in_step_range(requested.height, *min_height, *max_height, *step_height)
        }
    });
    if supported {
        Ok(())
    } else {
        Err(unsupported_tuple(
            requested,
            "exact size/frame-rate tuple is not enumerated",
        ))
    }
}

fn flatten_discrete(formats: &[VideoFormatReport]) -> Vec<VideoFormatCapability> {
    let mut output = Vec::new();
    for format in formats {
        let compressed = format.flags & bindings::V4L2_FMT_FLAG_COMPRESSED != 0
            || matches!(format.fourcc.as_str(), "H264" | "MJPG");
        for size in &format.sizes {
            let FrameSize::Discrete {
                width,
                height,
                intervals,
            } = size
            else {
                continue;
            };
            for interval in intervals {
                let FrameInterval::Discrete { value } = interval else {
                    continue;
                };
                if value.numerator == 0 {
                    continue;
                }
                let fps = Rational {
                    numerator: value.denominator,
                    denominator: value.numerator,
                };
                let tuple = VideoTuple {
                    fourcc: format.fourcc.clone(),
                    width: *width,
                    height: *height,
                    fps,
                }
                .normalized();
                output.push(VideoFormatCapability {
                    compressed,
                    portrait: height > width,
                    remuxable: matches!(format.fourcc.as_str(), "H264" | "MJPG"),
                    estimated_bandwidth_bps: (!compressed).then(|| raw_bandwidth(&tuple)),
                    product_envelope_hint: product_envelope_hint(&tuple),
                    tuple,
                });
            }
        }
    }
    output
}

fn interval_supports(interval: &FrameInterval, value: Rational) -> bool {
    match interval {
        FrameInterval::Discrete { value: advertised } => rational_cmp(*advertised, value).is_eq(),
        FrameInterval::Stepwise { min, max, step } => {
            rational_cmp(value, *min).is_ge()
                && rational_cmp(value, *max).is_le()
                && rational_on_step(value, *min, *step)
        }
    }
}

fn rational_on_step(value: Rational, min: Rational, step: Rational) -> bool {
    if step.numerator == 0
        || value.denominator == 0
        || min.denominator == 0
        || step.denominator == 0
    {
        return true;
    }
    let denominator = lcm(
        u128::from(value.denominator),
        lcm(u128::from(min.denominator), u128::from(step.denominator)),
    );
    let value = u128::from(value.numerator) * (denominator / u128::from(value.denominator));
    let min = u128::from(min.numerator) * (denominator / u128::from(min.denominator));
    let step = u128::from(step.numerator) * (denominator / u128::from(step.denominator));
    value >= min && step != 0 && (value - min).is_multiple_of(step)
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn lcm(left: u128, right: u128) -> u128 {
    left / gcd(left, right) * right
}

fn in_step_range(value: u32, min: u32, max: u32, step: u32) -> bool {
    value >= min && value <= max && (step == 0 || (value - min).is_multiple_of(step))
}

fn format_from_tuple(tuple: &VideoTuple) -> Format {
    let bytes: [u8; 4] = tuple
        .fourcc
        .as_bytes()
        .try_into()
        .expect("validated FourCC has four bytes");
    Format {
        width: tuple.width,
        height: tuple.height,
        pixelformat: PixelFormat::from_fourcc(&bytes),
        plane_fmt: Vec::new(),
    }
}

fn fps_from_time_per_frame(value: v4l2_fract) -> Result<Rational, LinkError> {
    if value.numerator == 0 || value.denominator == 0 {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "V4L2 capture node did not report a usable frame interval",
        ));
    }
    Ok(Rational {
        numerator: value.denominator,
        denominator: value.numerator,
    })
}

fn fourcc(format: PixelFormat) -> String {
    String::from_utf8_lossy(&format.to_fourcc()).into_owned()
}

fn raw_bandwidth(tuple: &VideoTuple) -> u64 {
    let bits_per_pixel = match tuple.fourcc.as_str() {
        "YUYV" | "UYVY" => 16_u64,
        "NV12" => 12,
        _ => 24,
    };
    u64::from(tuple.width)
        .saturating_mul(u64::from(tuple.height))
        .saturating_mul(bits_per_pixel)
        .saturating_mul(u64::from(tuple.fps.numerator))
        / u64::from(tuple.fps.denominator)
}

fn product_envelope_hint(tuple: &VideoTuple) -> bool {
    let fps = tuple.fps.numerator / tuple.fps.denominator;
    let rate_supported = matches!(fps, 24 | 25 | 30)
        || (matches!((tuple.width, tuple.height), (1920, 1080) | (1280, 720))
            && matches!(fps, 50 | 60));
    let size_supported = matches!(
        (tuple.width, tuple.height),
        (3840, 2160)
            | (1920, 1080)
            | (1280, 720)
            | (640, 360)
            | (2160, 3840)
            | (1080, 1920)
            | (720, 1280)
    );
    size_supported && rate_supported && matches!(tuple.fourcc.as_str(), "H264" | "MJPG")
}

fn unsupported_tuple(tuple: &VideoTuple, reason: &'static str) -> LinkError {
    LinkError::new(
        ErrorKind::CapabilityUnsupported,
        "requested video tuple is not advertised by the selected node",
    )
    .with_detail("requested", serde_json::to_value(tuple).unwrap_or_default())
    .with_detail("reason", reason)
}

fn format_mismatch(
    message: &'static str,
    requested: &VideoTuple,
    applied: &VideoTuple,
    rollback_succeeded: Option<bool>,
) -> LinkError {
    let mut error = LinkError::new(ErrorKind::MediaPipelineFailure, message)
        .with_detail(
            "requested",
            serde_json::to_value(requested).unwrap_or_default(),
        )
        .with_detail("applied", serde_json::to_value(applied).unwrap_or_default());
    if let Some(rollback_succeeded) = rollback_succeeded {
        error = error.with_detail("rollback_succeeded", rollback_succeeded);
    }
    error
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
            "failed to open V4L2 node for format changes"
        } else {
            "failed to open V4L2 node for format inspection"
        },
    )
    .with_detail("path", path.display().to_string())
    .with_detail("reason", error.to_string())
}

fn ioctl_error(error: v4l2r::nix::errno::Errno, message: &'static str, path: &Path) -> LinkError {
    let kind = match error {
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
    LinkError::new(kind, message)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

#[allow(unsafe_code)]
mod abi {
    use std::{fs::File, os::fd::AsRawFd};

    use v4l2r::{
        QueueType,
        bindings::{v4l2_format, v4l2_pix_format, v4l2_streamparm},
        nix::{errno::Errno, ioctl_readwrite},
    };

    use super::{CaptureParameters, RawFormat};

    ioctl_readwrite!(vidioc_g_fmt, b'V', 4, v4l2_format);
    ioctl_readwrite!(vidioc_g_parm, b'V', 21, v4l2_streamparm);
    ioctl_readwrite!(vidioc_s_parm, b'V', 22, v4l2_streamparm);

    pub(super) fn g_format(file: &File, queue: QueueType) -> Result<RawFormat, Errno> {
        let mut raw = v4l2_format {
            type_: queue as u32,
            ..Default::default()
        };
        // SAFETY: `raw` is initialized for the requested queue and the kernel writes the matching
        // union member on successful VIDIOC_G_FMT.
        unsafe { vidioc_g_fmt(file.as_raw_fd(), &mut raw) }?;
        if queue != QueueType::VideoCapture {
            return Err(Errno::EOPNOTSUPP);
        }
        // SAFETY: the queue type selects the single-planar `pix` union member.
        let pix: v4l2_pix_format = unsafe { raw.fmt.pix };
        // SAFETY: bindgen represents the two integer fields as unions; the active members are
        // selected by the kernel pixel-format contract.
        let ycbcr_encoding = unsafe { pix.__bindgen_anon_1.ycbcr_enc };
        let quantization = pix.quantization;
        Ok(RawFormat {
            width: pix.width,
            height: pix.height,
            pixelformat: pix.pixelformat,
            field: pix.field,
            bytes_per_line: pix.bytesperline,
            size_image: pix.sizeimage,
            colorspace: pix.colorspace,
            transfer_function: pix.xfer_func,
            ycbcr_encoding,
            quantization,
        })
    }

    pub(super) fn get_parameters(
        file: &File,
        queue: QueueType,
    ) -> Result<CaptureParameters, Errno> {
        let mut raw = v4l2_streamparm {
            type_: queue as u32,
            ..Default::default()
        };
        // SAFETY: the initialized structure is valid for VIDIOC_G_PARM.
        unsafe { vidioc_g_parm(file.as_raw_fd(), &mut raw) }?;
        // SAFETY: capture queue types select the `capture` member.
        let capture = unsafe { raw.parm.capture };
        Ok(CaptureParameters {
            timeperframe: capture.timeperframe,
        })
    }

    pub(super) fn set_parameters(
        file: &File,
        queue: QueueType,
        timeperframe: v4l2r::bindings::v4l2_fract,
    ) -> Result<(), Errno> {
        let capture = v4l2r::bindings::v4l2_captureparm {
            timeperframe,
            ..Default::default()
        };
        let mut raw = v4l2_streamparm {
            type_: queue as u32,
            parm: v4l2r::bindings::v4l2_streamparm__bindgen_ty_1 { capture },
        };
        // SAFETY: capture queue types select the initialized `capture` member.
        unsafe { vidioc_s_parm(file.as_raw_fd(), &mut raw) }?;
        Ok(())
    }
}

struct CaptureParameters {
    timeperframe: v4l2_fract,
}

struct RawFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytes_per_line: u32,
    size_image: u32,
    colorspace: u32,
    transfer_function: u32,
    ycbcr_encoding: u32,
    quantization: u32,
}

#[cfg(test)]
mod tests {
    use link_core::{
        ErrorKind,
        media::VideoTuple,
        probe::{FrameInterval, FrameSize, Rational, VideoFormatReport},
    };

    use super::validate_tuple;

    fn formats() -> Vec<VideoFormatReport> {
        vec![VideoFormatReport {
            fourcc: "MJPG".into(),
            description: "Motion-JPEG".into(),
            flags: 1,
            sizes: vec![FrameSize::Discrete {
                width: 3840,
                height: 2160,
                intervals: vec![FrameInterval::Discrete {
                    value: Rational {
                        numerator: 1,
                        denominator: 30,
                    },
                }],
            }],
        }]
    }

    #[test]
    fn exact_tuple_validation_inverts_frame_intervals() {
        let tuple = VideoTuple {
            fourcc: "mjpg".into(),
            width: 3840,
            height: 2160,
            fps: Rational {
                numerator: 30,
                denominator: 1,
            },
        };
        assert!(validate_tuple(&formats(), &tuple).is_ok());
        let mut invalid = tuple;
        invalid.fps.numerator = 60;
        assert_eq!(
            validate_tuple(&formats(), &invalid).unwrap_err().kind(),
            ErrorKind::CapabilityUnsupported
        );
    }
}
