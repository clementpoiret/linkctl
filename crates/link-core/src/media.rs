//! Stable video negotiation and media-operation contracts.

use std::{cmp::Ordering, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    audio::{AudioStats, AvSyncStats},
    control::ControlValue,
    probe::{Rational, VideoFormatReport},
};

/// An exact V4L2 capture tuple.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoTuple {
    /// Canonical uppercase FourCC.
    pub fourcc: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frames per second.
    pub fps: Rational,
}

impl VideoTuple {
    /// Return a tuple with its rational frame rate reduced.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.fourcc.make_ascii_uppercase();
        self.fps = normalize_rational(self.fps);
        self
    }

    /// Compare tuples while treating equivalent rational frame rates as equal.
    #[must_use]
    pub fn equivalent(&self, other: &Self) -> bool {
        self.fourcc.eq_ignore_ascii_case(&other.fourcc)
            && self.width == other.width
            && self.height == other.height
            && rational_cmp(self.fps, other.fps) == Ordering::Equal
    }
}

/// A selectable video tuple with derived transport information.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoFormatCapability {
    /// Exact selectable tuple.
    pub tuple: VideoTuple,
    /// Whether the format is compressed on the USB transport.
    pub compressed: bool,
    /// Whether width is less than height.
    pub portrait: bool,
    /// Whether the encoded stream can be remuxed without decoding.
    pub remuxable: bool,
    /// Estimated raw USB payload rate. Compressed formats report `null`.
    pub estimated_bandwidth_bps: Option<u64>,
    /// Whether product documentation advertises the tuple. Runtime enumeration remains authoritative.
    pub product_envelope_hint: bool,
}

/// Complete advertised format inventory for one capture node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoFormatInventory {
    /// Capture-node path.
    pub node: String,
    /// Kernel format/size/interval enumeration, including stepwise ranges.
    pub formats: Vec<VideoFormatReport>,
    /// Flattened discrete tuples with derived annotations.
    pub discrete: Vec<VideoFormatCapability>,
}

/// Current V4L2 format and stream parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoStatus {
    /// Capture-node path.
    pub node: String,
    /// Applied tuple.
    pub tuple: VideoTuple,
    /// Kernel field value.
    pub field: u32,
    /// Kernel colorspace value.
    pub colorspace: u32,
    /// Kernel transfer-function value.
    pub transfer_function: u32,
    /// Kernel YCbCr/HSV encoding value.
    pub ycbcr_encoding: u32,
    /// Kernel quantization value.
    pub quantization: u32,
    /// Bytes per line where meaningful.
    pub bytes_per_line: u32,
    /// Maximum image buffer size.
    pub size_image: u32,
}

/// Result of validating or applying a format change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FormatSetReport {
    /// Requested tuple.
    pub requested: VideoTuple,
    /// Tuple observed before the operation.
    pub previous: VideoTuple,
    /// Tuple returned by the driver through TRY_FMT or final readback.
    pub applied: VideoTuple,
    /// Whether readback matched the request exactly.
    pub verified: bool,
    /// Whether no write was performed.
    pub dry_run: bool,
    /// Whether a failed/mismatched write restored the previous tuple.
    pub rollback_succeeded: Option<bool>,
}

/// Aggregated counters collected without decoding the stream.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MediaStats {
    /// Buffers observed at the source.
    pub frames: u64,
    /// Encoded or raw payload bytes observed.
    pub bytes: u64,
    /// Missing V4L2 sequence numbers when offsets are available.
    pub sequence_drops: u64,
    /// Drops reported through GStreamer QoS messages.
    pub qos_drops: u64,
    /// Buffers with missing or regressing timestamps.
    pub timestamp_discontinuities: u64,
    /// Elapsed monotonic time in milliseconds.
    pub elapsed_ms: u64,
    /// Average payload bitrate.
    pub average_bitrate_bps: u64,
}

/// Why a foreground media operation stopped.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaStopReason {
    /// The requested frame count or duration completed.
    Completed,
    /// The user requested graceful shutdown.
    Interrupted,
    /// The output size limit was reached.
    SizeLimit,
    /// Free disk space crossed the configured reserve.
    DiskReserve,
    /// The downstream pipe closed.
    BrokenPipe,
}

/// Final report for capture, recording, statistics, or restreaming.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MediaRunReport {
    /// Applied source tuple.
    pub tuple: VideoTuple,
    /// Operation counters.
    pub stats: MediaStats,
    /// Normal stop reason.
    pub stop_reason: MediaStopReason,
    /// Output files, if any.
    pub outputs: Vec<PathBuf>,
    /// Whether encoded data passed through without decoding.
    pub pass_through: bool,
    /// Whether the container received EOS and was finalized.
    pub finalized: bool,
    /// Audio-branch statistics when audio was part of the operation.
    pub audio: Option<AudioStats>,
    /// Audio/video timestamp relationship for muxed operations.
    pub av_sync: Option<AvSyncStats>,
}

/// One control value captured in snapshot metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SnapshotControl {
    /// Canonical control name.
    pub name: String,
    /// Current semantic/raw value.
    pub value: ControlValue,
}

/// Metadata written beside a decoded or raw snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SnapshotMetadata {
    /// Metadata schema version.
    pub schema_version: u32,
    /// Capture time as Unix milliseconds.
    pub captured_unix_ms: u128,
    /// Redacted stable device identifier.
    pub stable_id: String,
    /// Human-readable model.
    pub model: String,
    /// Applied capture tuple.
    pub tuple: VideoTuple,
    /// Output encoding or raw FourCC.
    pub encoding: String,
    /// Matched profile identifier, if any.
    pub profile_id: Option<String>,
    /// Readable standard controls captured before streaming.
    pub controls: Vec<SnapshotControl>,
}

/// Reduce a rational while preserving zero denominators for validation to reject.
#[must_use]
pub const fn normalize_rational(value: Rational) -> Rational {
    if value.numerator == 0 || value.denominator == 0 {
        return value;
    }
    let divisor = gcd(value.numerator, value.denominator);
    Rational {
        numerator: value.numerator / divisor,
        denominator: value.denominator / divisor,
    }
}

/// Compare positive rational values without floating-point rounding.
#[must_use]
pub fn rational_cmp(left: Rational, right: Rational) -> Ordering {
    (u128::from(left.numerator) * u128::from(right.denominator))
        .cmp(&(u128::from(right.numerator) * u128::from(left.denominator)))
}

const fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::{VideoTuple, normalize_rational};
    use crate::probe::Rational;

    #[test]
    fn equivalent_frame_rates_compare_exactly() {
        let left = VideoTuple {
            fourcc: "h264".into(),
            width: 1920,
            height: 1080,
            fps: Rational {
                numerator: 60,
                denominator: 2,
            },
        };
        let right = VideoTuple {
            fourcc: "H264".into(),
            fps: Rational {
                numerator: 30,
                denominator: 1,
            },
            ..left.clone()
        };
        assert!(left.equivalent(&right));
        assert_eq!(normalize_rational(left.fps), right.fps);
    }
}
