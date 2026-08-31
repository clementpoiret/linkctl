use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use link_core::{
    ErrorKind, LinkError,
    audio::{
        AudioBackendKind, AudioLevelEvent, AudioProcessing, AudioRunReport, AudioStats,
        AudioStopReason, AvSyncStats,
    },
    media::{MediaRunReport, MediaStats, MediaStopReason, VideoTuple},
};
use serde::{Deserialize, Serialize};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_HANDLER: OnceLock<Result<(), String>> = OnceLock::new();

/// GStreamer elements required for temporary camera streams used by control reads.
pub const PROBE_STREAM_REQUIRED_ELEMENTS: &[&str] = &["v4l2src", "capsfilter", "fakesink"];

/// Read-only inspection of one GStreamer runtime and its required elements.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GStreamerRuntimeReport {
    pub version: String,
    pub required_elements: Vec<String>,
    pub missing_elements: Vec<String>,
}

/// Encoded snapshot output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotEncoding {
    Jpeg,
    Png,
    Raw,
}

/// Request for one or more direct snapshots.
#[derive(Clone, Debug)]
pub struct SnapshotRequest {
    pub node: PathBuf,
    pub tuple: VideoTuple,
    pub encoding: SnapshotEncoding,
    pub count: u32,
    pub interval: Duration,
    pub timeout: Duration,
}

/// One captured encoded frame.
#[derive(Clone, Debug)]
pub struct SnapshotFrame {
    pub bytes: Vec<u8>,
    pub captured_unix_ms: u128,
}

/// Common foreground stream settings.
#[derive(Clone, Debug)]
pub struct ForegroundRequest {
    pub node: PathBuf,
    pub tuple: VideoTuple,
    pub duration: Option<Duration>,
    pub shutdown_timeout: Duration,
}

/// One resolved ALSA or PipeWire capture source.
#[derive(Clone, Debug)]
pub struct AudioSourceRequest {
    pub id: String,
    pub backend: AudioBackendKind,
    pub selector: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration: Option<Duration>,
    pub shutdown_timeout: Duration,
    pub processing: AudioProcessing,
    pub delay_ns: i64,
}

/// Standalone audio output encoding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioEncoding {
    Wav,
    Flac,
    Raw,
}

/// Standalone audio capture settings.
#[derive(Clone, Debug)]
pub struct AudioCaptureRequest {
    pub source: AudioSourceRequest,
    pub output: Option<PathBuf>,
    pub encoding: AudioEncoding,
    pub overwrite: bool,
}

/// Audio metering settings.
#[derive(Clone, Debug)]
pub struct AudioMeterRequest {
    pub source: AudioSourceRequest,
    pub interval: Duration,
}

/// One resolved monitor sink. `None` selects the session default.
#[derive(Clone, Debug)]
pub struct AudioSinkRequest {
    pub backend: AudioBackendKind,
    pub selector: String,
}

/// Live microphone monitoring settings.
#[derive(Clone, Debug)]
pub struct AudioMonitorRequest {
    pub source: AudioSourceRequest,
    pub sink: Option<AudioSinkRequest>,
    pub latency: Duration,
}

/// Supported recording containers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordContainer {
    Matroska,
    Mp4,
}

/// Foreground recording settings.
#[derive(Clone, Debug)]
pub struct RecordRequest {
    pub source: ForegroundRequest,
    pub output: PathBuf,
    pub container: RecordContainer,
    pub require_video_copy: bool,
    pub max_total_bytes: Option<u64>,
    pub segment_duration: Option<Duration>,
    pub segment_bytes: Option<u64>,
    pub rolling_files: Option<u32>,
    pub disk_reserve_bytes: u64,
    pub overwrite: bool,
    pub audio: Option<AudioSourceRequest>,
}

/// Source selected for a long-lived daemon-owned pipeline.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharedSource {
    pub stable_id: String,
    pub node: PathBuf,
    pub tuple: VideoTuple,
}

/// One virtual-camera output and its bounded transform chain.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SharedOutput {
    pub name: String,
    pub device: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub format: String,
    pub rotation: SharedRotation,
    pub horizontal_flip: bool,
    pub vertical_flip: bool,
    pub crop: Option<SharedCrop>,
    pub fit: SharedFit,
    pub zoom: f64,
    pub frame_x: f64,
    pub frame_y: f64,
    pub text_overlay: Option<String>,
    pub image_overlay: Option<PathBuf>,
    pub privacy_frame: bool,
}

/// Aspect-ratio policy for a shared output.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SharedFit {
    #[default]
    Contain,
    Cover,
    Stretch,
}

/// Rotation for a shared output.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SharedRotation {
    #[default]
    None,
    Clockwise90,
    Rotate180,
    Counterclockwise90,
}

/// Normalized source crop for a shared output.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct SharedCrop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Optional recording branch in a shared pipeline.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharedRecording {
    pub output: PathBuf,
    pub container: RecordContainer,
    pub overwrite: bool,
}

/// Live counters for one daemon-owned source graph.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharedMetrics {
    pub frames: u64,
    pub bytes: u64,
    pub output_frames: BTreeMap<String, u64>,
    pub outputs: BTreeMap<String, SharedOutputMetrics>,
    pub started_unix_ms: u128,
    pub elapsed_ms: u64,
    pub average_bitrate_bps: u64,
    pub reconnects: u64,
    pub last_error: Option<String>,
}

/// Per-output delivery, queue-pressure, and recent processing-latency measurements.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharedOutputMetrics {
    pub frames: u64,
    pub dropped_buffers: u64,
    pub latency_samples: u64,
    pub latest_latency_us: u64,
    pub average_latency_us: u64,
    pub p95_latency_us: u64,
    pub max_latency_us: u64,
}

/// Runtime description of a daemon-owned graph.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SharedGraph {
    pub source: SharedSource,
    pub decode: String,
    pub outputs: Vec<SharedOutput>,
    pub recording: Option<SharedRecording>,
    pub snapshot_branches: Vec<String>,
    pub queue_max_buffers: u32,
    pub queue_policy: String,
    pub processing_backend: String,
    pub latency_window_samples: u32,
}

/// Raw stdout capture settings.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub source: ForegroundRequest,
}

/// RTP/UDP output settings.
#[cfg(feature = "network")]
#[derive(Clone, Debug)]
pub struct RtpRequest {
    pub source: ForegroundRequest,
    pub host: String,
    pub port: u16,
    pub payload_type: Option<u8>,
}

/// Cross-process direct-media ownership for one stable device.
pub struct MediaLease {
    _file: File,
    pub path: PathBuf,
}

impl MediaLease {
    /// Acquire a stale-safe advisory lock in a user-owned runtime directory.
    pub fn acquire(stable_id: &str, operation: &str) -> Result<Self, LinkError> {
        let directory = runtime_directory()?;
        let path = directory.join(format!("media-{stable_id}.lock"));
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| filesystem_error("failed to open media lock", &path, &error))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(LinkError::new(
                    ErrorKind::DeviceBusy,
                    "another direct media command owns the selected camera",
                )
                .with_detail("lock", path.display().to_string()));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(filesystem_error(
                    "failed to acquire media lock",
                    &path,
                    &error,
                ));
            }
        }
        file.set_len(0)
            .and_then(|()| writeln!(file, "pid={} operation={operation}", std::process::id()))
            .map_err(|error| filesystem_error("failed to record media owner", &path, &error))?;
        Ok(Self { _file: file, path })
    }
}

/// Ensure GStreamer and every element needed for the selected operation are available.
pub fn initialize(required_elements: &[&str]) -> Result<(), LinkError> {
    initialize_elements(required_elements)?;
    install_signal_handler()?;
    Ok(())
}

/// Inspect GStreamer without installing process signal handlers or starting a pipeline.
pub fn inspect_runtime(required_elements: &[&str]) -> Result<GStreamerRuntimeReport, LinkError> {
    gst::init().map_err(|error| {
        LinkError::new(
            ErrorKind::MediaPipelineFailure,
            "failed to initialize GStreamer",
        )
        .with_detail("reason", error.to_string())
    })?;
    Ok(GStreamerRuntimeReport {
        version: gst::version_string().to_string(),
        required_elements: required_elements
            .iter()
            .map(|element| (*element).to_owned())
            .collect(),
        missing_elements: missing_elements(required_elements, |element| {
            gst::ElementFactory::find(element).is_some()
        }),
    })
}

fn initialize_elements(required_elements: &[&str]) -> Result<(), LinkError> {
    let report = inspect_runtime(required_elements)?;
    if let Some(element) = report.missing_elements.first() {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "required GStreamer element is unavailable",
        )
        .with_detail("element", element.clone()));
    }
    Ok(())
}

fn missing_elements<F>(required_elements: &[&str], mut available: F) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    required_elements
        .iter()
        .filter(|element| !available(element))
        .map(|element| (*element).to_owned())
        .collect()
}

/// A minimal no-output camera stream held in PLAYING for a bounded XU operation.
pub struct ProbeStream {
    pipeline: gst::Pipeline,
}

impl ProbeStream {
    /// Open the camera and wait for a minimal source pipeline to reach PLAYING.
    pub fn open(node: &str, timeout: Duration) -> Result<Self, LinkError> {
        Self::open_with_format(node, timeout, None)
    }

    /// Open the camera at an exact media tuple and wait for PLAYING.
    pub fn open_with_format(
        node: &str,
        timeout: Duration,
        format: Option<&VideoTuple>,
    ) -> Result<Self, LinkError> {
        initialize(PROBE_STREAM_REQUIRED_ELEMENTS)?;
        let pipeline = gst::Pipeline::new();
        let source = gst::ElementFactory::make("v4l2src")
            .property("device", node)
            .build()
            .map_err(build_error)?;
        let filter = gst::ElementFactory::make("capsfilter")
            .build()
            .map_err(build_error)?;
        if let Some(format) = format {
            filter.set_property("caps", caps_for(format)?);
        }
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .map_err(build_error)?;
        pipeline_add(&pipeline, &[&source, &filter, &sink])?;
        gst::Element::link_many([&source, &filter, &sink]).map_err(link_error)?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(state_error)?;
        let (change, current, pending) =
            pipeline.state(gst::ClockTime::from_nseconds(duration_ns(timeout)));
        let result = change.map_err(state_error).and_then(|_| {
            if current == gst::State::Playing {
                Ok(())
            } else {
                Err(LinkError::new(
                    ErrorKind::Timeout,
                    "temporary camera pipeline did not reach Playing",
                )
                .with_detail("current", format!("{current:?}"))
                .with_detail("pending", format!("{pending:?}")))
            }
        });
        if let Err(error) = result {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(error);
        }
        Ok(Self { pipeline })
    }
}

impl Drop for ProbeStream {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Open a camera briefly and confirm that a minimal source pipeline reaches PLAYING.
///
/// This is intentionally limited to rebuilding userspace media state. It does not
/// reset the USB device or mutate driver state.
pub fn probe_stream(node: &str, timeout: Duration) -> Result<(), LinkError> {
    drop(ProbeStream::open(node, timeout)?);
    Ok(())
}

/// Capture one or more encoded still images without retaining a running pipeline.
pub fn snapshot(request: &SnapshotRequest) -> Result<Vec<SnapshotFrame>, LinkError> {
    if request.count == 0 {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "snapshot count must be greater than zero",
        ));
    }
    initialize(&snapshot_elements(&request.tuple, request.encoding)?)?;
    INTERRUPTED.store(false, Ordering::SeqCst);
    let pipeline = gst::Pipeline::new();
    let (source, filter) = source_elements(&request.node, &request.tuple)?;
    pipeline_add(&pipeline, &[&source, &filter])?;
    source.link(&filter).map_err(link_error)?;

    let mut tail = filter;
    for element in snapshot_transform(&request.tuple, request.encoding)? {
        pipeline.add(&element).map_err(pipeline_error)?;
        tail.link(&element).map_err(link_error)?;
        tail = element;
    }
    let sink_element = gst::ElementFactory::make("appsink")
        .property("sync", false)
        .property("max-buffers", 1_u32)
        .property("drop", true)
        .build()
        .map_err(build_error)?;
    let sink = sink_element
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| media_error("GStreamer appsink had an unexpected type"))?;
    pipeline.add(&sink_element).map_err(pipeline_error)?;
    tail.link(&sink_element).map_err(link_error)?;

    pipeline
        .set_state(gst::State::Playing)
        .map_err(state_error)?;
    let result = (|| {
        let mut frames = Vec::with_capacity(request.count as usize);
        while frames.len() < request.count as usize {
            if INTERRUPTED.load(Ordering::SeqCst) {
                return Err(
                    LinkError::new(ErrorKind::PartialSuccess, "snapshot interrupted")
                        .with_detail("captured", frames.len() as u64),
                );
            }
            let sample = sink
                .try_pull_sample(gst::ClockTime::from_nseconds(duration_ns(request.timeout)))
                .ok_or_else(|| {
                    LinkError::new(ErrorKind::Timeout, "timed out waiting for a camera frame")
                })?;
            let buffer = sample
                .buffer()
                .ok_or_else(|| media_error("snapshot sample did not contain a buffer"))?;
            if request.encoding == SnapshotEncoding::Raw
                && request.tuple.fourcc.eq_ignore_ascii_case("H264")
                && buffer.flags().contains(gst::BufferFlags::DELTA_UNIT)
            {
                continue;
            }
            let map = buffer
                .map_readable()
                .map_err(|_| media_error("snapshot buffer could not be mapped"))?;
            frames.push(SnapshotFrame {
                bytes: map.as_slice().to_vec(),
                captured_unix_ms: unix_ms()?,
            });
            if frames.len() < request.count as usize && !request.interval.is_zero() {
                thread::sleep(request.interval);
            }
        }
        Ok(frames)
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Measure an exact direct stream without writing media.
pub fn stats(request: &ForegroundRequest) -> Result<MediaRunReport, LinkError> {
    initialize(&["v4l2src", "capsfilter", "fakesink"])?;
    let pipeline = gst::Pipeline::new();
    let (source, filter) = source_elements(&request.node, &request.tuple)?;
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .map_err(build_error)?;
    pipeline_add(&pipeline, &[&source, &filter, &sink])?;
    gst::Element::link_many([&source, &filter, &sink]).map_err(link_error)?;
    let stats = attach_stats(&filter)?;
    run_pipeline(
        pipeline,
        request,
        MediaTelemetry::video_only(stats),
        false,
        None,
        None,
    )
}

/// Copy a compressed camera stream to stdout.
pub fn capture_stdout(request: &CaptureRequest) -> Result<MediaRunReport, LinkError> {
    let parser_name = parser_name(&request.source.tuple)?;
    initialize(&["v4l2src", "capsfilter", parser_name, "fdsink"])?;
    let pipeline = gst::Pipeline::new();
    let (source, filter) = source_elements(&request.source.node, &request.source.tuple)?;
    let parser = gst::ElementFactory::make(parser_name)
        .build()
        .map_err(build_error)?;
    let sink = gst::ElementFactory::make("fdsink")
        .property("fd", 1_i32)
        .property("sync", false)
        .build()
        .map_err(build_error)?;
    pipeline_add(&pipeline, &[&source, &filter, &parser, &sink])?;
    gst::Element::link_many([&source, &filter, &parser, &sink]).map_err(link_error)?;
    let stats = attach_stats(&filter)?;
    run_pipeline(
        pipeline,
        &request.source,
        MediaTelemetry::video_only(stats),
        true,
        None,
        None,
    )
}

/// Capture one audio source to WAV, FLAC, raw PCM, or standard output.
pub fn audio_capture(request: &AudioCaptureRequest) -> Result<AudioRunReport, LinkError> {
    let mut required = audio_source_elements_required(&request.source);
    required.extend(["audioconvert", "audioresample", "audiorate", "capsfilter"]);
    match request.encoding {
        AudioEncoding::Wav => required.push("wavenc"),
        AudioEncoding::Flac => required.push("flacenc"),
        AudioEncoding::Raw => {}
    }
    required.push(if request.output.is_some() {
        "filesink"
    } else {
        "fdsink"
    });
    required.extend(audio_processing_elements(request.source.processing));
    initialize(&required)?;
    let (pipeline, tail, rate) = build_audio_front(&request.source)?;
    let telemetry = attach_audio_stats(
        &tail,
        request.source.sample_rate,
        request.source.channels,
        Duration::from_millis(100),
    )?;
    let mut last = tail;
    let encoder = match request.encoding {
        AudioEncoding::Wav => Some(
            gst::ElementFactory::make("wavenc")
                .build()
                .map_err(build_error)?,
        ),
        AudioEncoding::Flac => Some(
            gst::ElementFactory::make("flacenc")
                .build()
                .map_err(build_error)?,
        ),
        AudioEncoding::Raw => None,
    };
    if let Some(encoder) = &encoder {
        pipeline.add(encoder).map_err(pipeline_error)?;
        last.link(encoder).map_err(link_error)?;
        last = encoder.clone();
    }
    let output = request
        .output
        .as_ref()
        .map(|path| AudioOutputPlan::new(path, request.overwrite))
        .transpose()?;
    let sink = if let Some(output) = &output {
        gst::ElementFactory::make("filesink")
            .property("location", output.temporary.display().to_string())
            .property("sync", false)
            .build()
            .map_err(build_error)?
    } else {
        gst::ElementFactory::make("fdsink")
            .property("fd", 1_i32)
            .property("sync", false)
            .build()
            .map_err(build_error)?
    };
    pipeline.add(&sink).map_err(pipeline_error)?;
    last.link(&sink).map_err(link_error)?;
    let mut report = run_audio_pipeline(
        pipeline,
        &request.source,
        telemetry,
        &rate,
        Some(audio_encoding_name(request.encoding).into()),
        None,
    )?;
    if let Some(output) = output {
        report.outputs = output.finish(report.finalized)?;
    }
    Ok(report)
}

/// Emit periodic peak/RMS observations while measuring an audio source.
pub fn audio_meter<F>(request: &AudioMeterRequest, mut emit: F) -> Result<AudioRunReport, LinkError>
where
    F: FnMut(&AudioLevelEvent) -> Result<(), LinkError>,
{
    let mut required = audio_source_elements_required(&request.source);
    required.extend([
        "audioconvert",
        "audioresample",
        "audiorate",
        "capsfilter",
        "fakesink",
    ]);
    required.extend(audio_processing_elements(request.source.processing));
    initialize(&required)?;
    let (pipeline, tail, rate) = build_audio_front(&request.source)?;
    let telemetry = attach_audio_stats(
        &tail,
        request.source.sample_rate,
        request.source.channels,
        request.interval,
    )?;
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .map_err(build_error)?;
    pipeline.add(&sink).map_err(pipeline_error)?;
    tail.link(&sink).map_err(link_error)?;
    run_audio_pipeline(
        pipeline,
        &request.source,
        telemetry,
        &rate,
        None,
        Some(&mut emit),
    )
}

/// Monitor one source through a selected or session-default playback sink.
pub fn audio_monitor(request: &AudioMonitorRequest) -> Result<AudioRunReport, LinkError> {
    let mut required = audio_source_elements_required(&request.source);
    required.extend([
        "audioconvert",
        "audioresample",
        "audiorate",
        "capsfilter",
        "queue",
    ]);
    required.push(match request.sink.as_ref().map(|sink| sink.backend) {
        Some(AudioBackendKind::Alsa) => "alsasink",
        Some(AudioBackendKind::Pipewire) => "pipewiresink",
        None => "autoaudiosink",
    });
    required.extend(audio_processing_elements(request.source.processing));
    initialize(&required)?;
    let (pipeline, tail, rate) = build_audio_front(&request.source)?;
    let telemetry = attach_audio_stats(
        &tail,
        request.source.sample_rate,
        request.source.channels,
        Duration::from_millis(100),
    )?;
    let queue = gst::ElementFactory::make("queue")
        .property("min-threshold-time", duration_ns(request.latency))
        .property(
            "max-size-time",
            duration_ns(request.latency).saturating_mul(2),
        )
        .property("max-size-buffers", 0_u32)
        .property("max-size-bytes", 0_u32)
        .build()
        .map_err(build_error)?;
    let sink = match &request.sink {
        Some(sink) if sink.backend == AudioBackendKind::Alsa => {
            gst::ElementFactory::make("alsasink")
                .property("device", &sink.selector)
                .property("sync", true)
                .build()
                .map_err(build_error)?
        }
        Some(sink) => gst::ElementFactory::make("pipewiresink")
            .property("target-object", &sink.selector)
            .property("sync", true)
            .build()
            .map_err(build_error)?,
        None => gst::ElementFactory::make("autoaudiosink")
            .property("sync", true)
            .build()
            .map_err(build_error)?,
    };
    pipeline_add(&pipeline, &[&queue, &sink])?;
    gst::Element::link_many([&tail, &queue, &sink]).map_err(link_error)?;
    run_audio_pipeline(pipeline, &request.source, telemetry, &rate, None, None)
}

/// Record a compressed camera stream to Matroska or fragmented MP4.
pub fn record(request: &RecordRequest) -> Result<MediaRunReport, LinkError> {
    let parser_name = parser_name(&request.source.tuple)?;
    let muxer = match request.container {
        RecordContainer::Matroska => "matroskamux",
        RecordContainer::Mp4 => "mp4mux",
    };
    let mut required = vec![
        "v4l2src",
        "capsfilter",
        "queue",
        parser_name,
        "splitmuxsink",
        muxer,
    ];
    if let Some(audio) = &request.audio {
        required.extend(audio_source_elements_required(audio));
        required.extend(["audioconvert", "audioresample", "audiorate", "identity"]);
        required.push(match request.container {
            RecordContainer::Matroska => "flacenc",
            RecordContainer::Mp4 => "avenc_aac",
        });
        required.extend(audio_processing_elements(audio.processing));
    }
    initialize(&required)?;
    if request.require_video_copy && !is_pass_through(&request.source.tuple) {
        return Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "--video-copy requires H.264 or MJPEG input",
        ));
    }
    validate_output_parent(&request.output)?;
    let output = OutputPlan::new(request)?;
    ensure_disk_reserve(&request.output, request.disk_reserve_bytes)?;

    let pipeline = gst::Pipeline::new();
    let (source, filter) = source_elements(&request.source.node, &request.source.tuple)?;
    let video_queue = recording_queue()?;
    let parser = gst::ElementFactory::make(parser_name)
        .build()
        .map_err(build_error)?;
    let mut sink_builder = gst::ElementFactory::make("splitmuxsink")
        .property("location", output.gstreamer_location())
        .property("muxer-factory", muxer)
        .property("async-finalize", true)
        .property(
            "max-size-time",
            request.segment_duration.map_or(0, duration_ns),
        )
        .property("max-size-bytes", request.segment_bytes.unwrap_or(0))
        .property("max-files", request.rolling_files.unwrap_or(0));
    if request.container == RecordContainer::Mp4 {
        let properties = gst::Structure::builder("properties")
            .field("fragment-duration", 1_000_u32)
            .build();
        sink_builder = sink_builder
            .property("use-robust-muxing", true)
            .property("muxer-properties", properties);
    }
    let sink = sink_builder.build().map_err(build_error)?;
    pipeline_add(&pipeline, &[&source, &filter, &video_queue, &parser, &sink])?;
    gst::Element::link_many([&source, &filter, &video_queue, &parser, &sink])
        .map_err(link_error)?;
    let stats = attach_stats(&filter)?;
    let (audio_runtime, av_sync) = if let Some(audio) = &request.audio {
        let (audio_tail, audio_rate) = add_audio_front(&pipeline, audio)?;
        let audio_stats = attach_audio_stats(
            &audio_tail,
            audio.sample_rate,
            audio.channels,
            Duration::from_millis(100),
        )?;
        let encoder_name = match request.container {
            RecordContainer::Matroska => "flacenc",
            RecordContainer::Mp4 => "avenc_aac",
        };
        let encoder = gst::ElementFactory::make(encoder_name)
            .build()
            .map_err(build_error)?;
        let audio_queue = recording_queue()?;
        pipeline_add(&pipeline, &[&audio_queue, &encoder])?;
        if request.container == RecordContainer::Mp4 {
            let convert = gst::ElementFactory::make("audioconvert")
                .build()
                .map_err(build_error)?;
            let caps = gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("rate", i32::try_from(audio.sample_rate).unwrap_or(i32::MAX))
                .field(
                    "channels",
                    i32::try_from(audio.channels).unwrap_or(i32::MAX),
                )
                .field("layout", "interleaved")
                .build();
            let filter = gst::ElementFactory::make("capsfilter")
                .property("caps", caps)
                .build()
                .map_err(build_error)?;
            pipeline_add(&pipeline, &[&convert, &filter])?;
            gst::Element::link_many([&audio_tail, &convert, &filter, &audio_queue, &encoder])
                .map_err(link_error)?;
        } else {
            gst::Element::link_many([&audio_tail, &audio_queue, &encoder]).map_err(link_error)?;
        }
        let encoder_pad = encoder
            .static_pad("src")
            .ok_or_else(|| media_error("audio encoder has no source pad"))?;
        let sink_pad = sink
            .request_pad_simple("audio_%u")
            .ok_or_else(|| media_error("split muxer did not provide an audio pad"))?;
        encoder_pad.link(&sink_pad).map_err(|error| {
            media_error("failed to link audio into the split muxer")
                .with_detail("reason", error.to_string())
        })?;
        let sync = attach_av_sync(&filter, &audio_tail)?;
        (
            Some(AudioRuntime {
                telemetry: audio_stats,
                rate: audio_rate,
                sample_rate: audio.sample_rate,
                channels: audio.channels,
                codec: encoder_name.into(),
                processing: audio.processing,
            }),
            Some(sync),
        )
    } else {
        (None, None)
    };

    let mut report = run_pipeline(
        pipeline,
        &request.source,
        MediaTelemetry {
            video: stats,
            audio: audio_runtime,
            av_sync,
        },
        true,
        Some((&request.output, request.disk_reserve_bytes)),
        request.max_total_bytes.map(|limit| (&output, limit)),
    )?;
    report.outputs = output.finish(report.finalized, request.overwrite)?;
    if report.stop_reason == MediaStopReason::DiskReserve {
        return Err(LinkError::new(
            ErrorKind::MediaPipelineFailure,
            "recording stopped because free disk space crossed the configured reserve",
        )
        .with_detail("report", serde_json::to_value(&report).unwrap_or_default()));
    }
    Ok(report)
}

/// One continuously running source with snapshot, recording, and virtual-camera branches.
pub struct SharedPipeline {
    _lease: MediaLease,
    pipeline: gst::Pipeline,
    graph: SharedGraph,
    jpeg_sink: gst_app::AppSink,
    jpeg_valve: gst::Element,
    png_sink: gst_app::AppSink,
    png_valve: gst::Element,
    source_frames: Arc<AtomicU64>,
    source_bytes: Arc<AtomicU64>,
    output_metrics: BTreeMap<String, SharedOutputTelemetry>,
    started: Instant,
    started_unix_ms: u128,
}

const SHARED_QUEUE_MAX_BUFFERS: u32 = 2;
const SHARED_LATENCY_WINDOW_SAMPLES: usize = 2048;

#[derive(Clone, Default)]
struct SharedOutputTelemetry {
    frames: Arc<AtomicU64>,
    dropped_buffers: Arc<AtomicU64>,
    latency_ns: Arc<Mutex<VecDeque<u64>>>,
}

impl SharedPipeline {
    /// Build and start one physical source with every requested consumer branch.
    pub fn start(
        source: SharedSource,
        outputs: Vec<SharedOutput>,
        recording: Option<SharedRecording>,
        timeout: Duration,
    ) -> Result<Self, LinkError> {
        validate_shared_contracts(&outputs, recording.as_ref())?;
        let lease = MediaLease::acquire(&source.stable_id, "linkd")?;
        let mut required = vec![
            "v4l2src",
            "capsfilter",
            "tee",
            "queue",
            "videoconvert",
            "valve",
            "jpegenc",
            "pngenc",
            "appsink",
        ];
        match source.tuple.fourcc.to_ascii_uppercase().as_str() {
            "MJPG" => required.extend(["jpegparse", "jpegdec"]),
            "H264" => required.extend(["h264parse", "avdec_h264"]),
            _ => {}
        }
        if !outputs.is_empty() {
            required.extend([
                "videoscale",
                "videorate",
                "videoflip",
                "videocrop",
                "v4l2sink",
            ]);
        }
        if outputs.iter().any(|output| output.text_overlay.is_some()) {
            required.push("textoverlay");
        }
        if outputs.iter().any(|output| output.image_overlay.is_some()) {
            required.push("gdkpixbufoverlay");
        }
        if outputs.iter().any(|output| output.privacy_frame) {
            required.push("videobalance");
        }
        if let Some(recording) = &recording {
            required.extend([parser_name(&source.tuple)?, "filesink"]);
            required.push(match recording.container {
                RecordContainer::Matroska => "matroskamux",
                RecordContainer::Mp4 => "mp4mux",
            });
        }
        required.sort_unstable();
        required.dedup();
        initialize_elements(&required)?;

        let pipeline = gst::Pipeline::new();
        let (source_element, source_filter) = source_elements(&source.node, &source.tuple)?;
        let encoded_tee = make("tee")?;
        pipeline_add(&pipeline, &[&source_element, &source_filter, &encoded_tee])?;
        gst::Element::link_many([&source_element, &source_filter, &encoded_tee])
            .map_err(link_error)?;

        let source_frames = Arc::new(AtomicU64::new(0));
        let source_bytes = Arc::new(AtomicU64::new(0));
        attach_atomic_stats(&source_filter, &source_frames, &source_bytes)?;

        let raw_queue = queue_element()?;
        let mut raw_front = vec![raw_queue.clone()];
        match source.tuple.fourcc.to_ascii_uppercase().as_str() {
            "MJPG" => raw_front.extend([make("jpegparse")?, make("jpegdec")?]),
            "H264" => raw_front.extend([make("h264parse")?, make("avdec_h264")?]),
            _ => {}
        }
        raw_front.push(make("videoconvert")?);
        let raw_tee = make("tee")?;
        raw_front.push(raw_tee.clone());
        add_and_link_branch(&pipeline, &encoded_tee, &raw_front)?;

        let (jpeg_sink, jpeg_valve, jpeg_elements) = snapshot_branch("jpegenc")?;
        add_and_link_branch(&pipeline, &raw_tee, &jpeg_elements)?;
        let (png_sink, png_valve, png_elements) = snapshot_branch("pngenc")?;
        add_and_link_branch(&pipeline, &raw_tee, &png_elements)?;

        let mut output_metrics = BTreeMap::new();
        for output in &outputs {
            let telemetry = SharedOutputTelemetry::default();
            let branch = output_branch(output, &source.tuple, &telemetry)?;
            add_and_link_branch(&pipeline, &raw_tee, &branch)?;
            output_metrics.insert(output.name.clone(), telemetry);
        }
        if let Some(recording) = &recording {
            let branch = recording_branch(recording, &source.tuple)?;
            add_and_link_branch(&pipeline, &encoded_tee, &branch)?;
        }

        let bus = pipeline
            .bus()
            .ok_or_else(|| media_error("shared GStreamer pipeline has no bus"))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(state_error)?;
        let (change, current, pending) =
            pipeline.state(gst::ClockTime::from_nseconds(duration_ns(timeout)));
        if let Err(error) = change.map_err(state_error).and_then(|_| {
            if current == gst::State::Playing {
                Ok(())
            } else {
                Err(LinkError::new(
                    ErrorKind::Timeout,
                    "shared camera pipeline did not reach Playing",
                )
                .with_detail("current", format!("{current:?}"))
                .with_detail("pending", format!("{pending:?}")))
            }
        }) {
            let detail = pipeline_error_detail(&bus, Duration::from_millis(200));
            let _ = pipeline.set_state(gst::State::Null);
            return Err(match detail {
                Some(detail) => error.with_detail("reason", detail),
                None => error,
            });
        }
        let started_unix_ms = unix_ms()?;
        Ok(Self {
            _lease: lease,
            pipeline,
            graph: SharedGraph {
                source,
                decode: "decode-to-raw".into(),
                outputs,
                recording,
                snapshot_branches: vec!["jpeg".into(), "png".into()],
                queue_max_buffers: SHARED_QUEUE_MAX_BUFFERS,
                queue_policy: "leaky-downstream".into(),
                processing_backend: "gstreamer-cpu".into(),
                latency_window_samples: SHARED_LATENCY_WINDOW_SAMPLES as u32,
            },
            jpeg_sink,
            jpeg_valve,
            png_sink,
            png_valve,
            source_frames,
            source_bytes,
            output_metrics,
            started: Instant::now(),
            started_unix_ms,
        })
    }

    #[must_use]
    pub fn graph(&self) -> SharedGraph {
        self.graph.clone()
    }

    #[must_use]
    pub fn metrics(&self, reconnects: u64, last_error: Option<String>) -> SharedMetrics {
        let elapsed = self.started.elapsed();
        let bytes = self.source_bytes.load(Ordering::Relaxed);
        let average_bitrate_bps = if elapsed.is_zero() {
            0
        } else {
            u64::try_from(
                u128::from(bytes)
                    .saturating_mul(8)
                    .saturating_mul(1_000_000_000)
                    / elapsed.as_nanos(),
            )
            .unwrap_or(u64::MAX)
        };
        SharedMetrics {
            frames: self.source_frames.load(Ordering::Relaxed),
            bytes,
            output_frames: self
                .output_metrics
                .iter()
                .map(|(name, telemetry)| (name.clone(), telemetry.frames.load(Ordering::Relaxed)))
                .collect(),
            outputs: self
                .output_metrics
                .iter()
                .map(|(name, telemetry)| (name.clone(), shared_output_metrics(telemetry)))
                .collect(),
            started_unix_ms: self.started_unix_ms,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            average_bitrate_bps,
            reconnects,
            last_error,
        }
    }

    /// Pull a still frame from the running decoded stream without reopening the source.
    pub fn snapshot(
        &self,
        encoding: SnapshotEncoding,
        timeout: Duration,
    ) -> Result<SnapshotFrame, LinkError> {
        let (sink, valve) = match encoding {
            SnapshotEncoding::Jpeg => (&self.jpeg_sink, &self.jpeg_valve),
            SnapshotEncoding::Png => (&self.png_sink, &self.png_valve),
            SnapshotEncoding::Raw => {
                return Err(LinkError::new(
                    ErrorKind::CapabilityUnsupported,
                    "shared raw snapshots are unavailable after decode",
                ));
            }
        };
        valve.set_property("drop", false);
        let result = (|| {
            let sample = sink
                .try_pull_sample(gst::ClockTime::from_nseconds(duration_ns(timeout)))
                .ok_or_else(|| {
                    LinkError::new(ErrorKind::Timeout, "timed out waiting for a shared frame")
                })?;
            let buffer = sample
                .buffer()
                .ok_or_else(|| media_error("shared snapshot sample did not contain a buffer"))?;
            let map = buffer
                .map_readable()
                .map_err(|_| media_error("shared snapshot buffer could not be mapped"))?;
            Ok(SnapshotFrame {
                bytes: map.as_slice().to_vec(),
                captured_unix_ms: unix_ms()?,
            })
        })();
        valve.set_property("drop", true);
        result
    }

    /// Return an asynchronous pipeline failure, if one is pending.
    pub fn poll_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        while let Some(message) = bus.timed_pop(gst::ClockTime::ZERO) {
            if let gst::MessageView::Error(error) = message.view() {
                return Some(format!(
                    "{} ({})",
                    error.error(),
                    error.debug().unwrap_or_default()
                ));
            }
        }
        None
    }

    /// Gracefully finalize sinks before releasing the physical source.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.pipeline.send_event(gst::event::Eos::new());
        let finalized = self.pipeline.bus().is_some_and(|bus| {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
                    match message.view() {
                        gst::MessageView::Eos(..) => return true,
                        gst::MessageView::Error(..) => return false,
                        _ => {}
                    }
                }
            }
            false
        });
        let _ = self.pipeline.set_state(gst::State::Null);
        finalized
    }
}

fn pipeline_error_detail(bus: &gst::Bus, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(10)) else {
            continue;
        };
        if let gst::MessageView::Error(error) = message.view() {
            return Some(format!(
                "{} ({})",
                error.error(),
                error.debug().unwrap_or_default()
            ));
        }
    }
    None
}

impl Drop for SharedPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn validate_shared_contracts(
    outputs: &[SharedOutput],
    recording: Option<&SharedRecording>,
) -> Result<(), LinkError> {
    let mut names = std::collections::BTreeSet::new();
    let mut devices = std::collections::BTreeSet::new();
    for output in outputs {
        if output.name.is_empty()
            || !output.name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "virtual-camera name must contain only letters, digits, '-' or '_'",
            ));
        }
        if !names.insert(&output.name) {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "virtual-camera name is already active",
            )
            .with_detail("name", output.name.clone()));
        }
        if !devices.insert(&output.device) {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "virtual-camera output device is already active",
            )
            .with_detail("device", output.device.display().to_string()));
        }
        if output.width == 0
            || output.height == 0
            || output.fps_numerator == 0
            || output.fps_denominator == 0
            || output.zoom < 1.0
            || !(0.0..=1.0).contains(&output.frame_x)
            || !(0.0..=1.0).contains(&output.frame_y)
        {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "virtual-camera output contract or framing is invalid",
            )
            .with_detail("name", output.name.clone()));
        }
        if let Some(crop) = output.crop
            && (crop.width <= 0.0
                || crop.height <= 0.0
                || crop.x < 0.0
                || crop.y < 0.0
                || crop.x + crop.width > 1.0
                || crop.y + crop.height > 1.0)
        {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "normalized virtual-camera crop is outside the source frame",
            )
            .with_detail("name", output.name.clone()));
        }
        if let Some(image) = &output.image_overlay
            && !image.is_file()
        {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "virtual-camera overlay image does not exist",
            )
            .with_detail("path", image.display().to_string()));
        }
    }
    if let Some(recording) = recording {
        validate_output_parent(&recording.output)?;
        if let Ok(metadata) = fs::symlink_metadata(&recording.output) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "recording output must be a regular file and never a symbolic link",
                )
                .with_detail("path", recording.output.display().to_string()));
            }
            if !recording.overwrite {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "recording output already exists; use --overwrite to replace it",
                )
                .with_detail("path", recording.output.display().to_string()));
            }
        }
    }
    Ok(())
}

fn make(name: &'static str) -> Result<gst::Element, LinkError> {
    gst::ElementFactory::make(name).build().map_err(build_error)
}

fn recording_queue() -> Result<gst::Element, LinkError> {
    gst::ElementFactory::make("queue")
        .property("max-size-buffers", 0_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 2_000_000_000_u64)
        .build()
        .map_err(build_error)
}

fn queue_element() -> Result<gst::Element, LinkError> {
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", SHARED_QUEUE_MAX_BUFFERS)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 0_u64)
        .build()
        .map_err(build_error)?;
    set_enum_property(&queue, "leaky", "downstream")?;
    Ok(queue)
}

fn monitored_queue(dropped_buffers: &Arc<AtomicU64>) -> Result<gst::Element, LinkError> {
    let queue = queue_element()?;
    let dropped_buffers = Arc::clone(dropped_buffers);
    queue.connect_closure(
        "overrun",
        false,
        glib::closure!(move |_queue: gst::Element| {
            dropped_buffers.fetch_add(1, Ordering::Relaxed);
        }),
    );
    Ok(queue)
}

fn add_and_link_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    elements: &[gst::Element],
) -> Result<(), LinkError> {
    pipeline.add_many(elements.iter()).map_err(pipeline_error)?;
    let first = elements
        .first()
        .ok_or_else(|| media_error("shared pipeline branch was empty"))?;
    tee.link(first).map_err(link_error)?;
    gst::Element::link_many(elements).map_err(link_error)
}

fn snapshot_branch(
    encoder_name: &'static str,
) -> Result<(gst_app::AppSink, gst::Element, Vec<gst::Element>), LinkError> {
    let queue = queue_element()?;
    let valve = gst::ElementFactory::make("valve")
        .property("drop", true)
        .build()
        .map_err(build_error)?;
    let convert = make("videoconvert")?;
    let encoder = make(encoder_name)?;
    let sink_element = gst::ElementFactory::make("appsink")
        .property("sync", false)
        .property("async", false)
        .property("max-buffers", 1_u32)
        .property("drop", true)
        .build()
        .map_err(build_error)?;
    let sink = sink_element
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| media_error("shared appsink had an unexpected type"))?;
    Ok((
        sink,
        valve.clone(),
        vec![queue, valve, convert, encoder, sink_element],
    ))
}

fn output_branch(
    output: &SharedOutput,
    source: &VideoTuple,
    telemetry: &SharedOutputTelemetry,
) -> Result<Vec<gst::Element>, LinkError> {
    let mut branch = vec![
        monitored_queue(&telemetry.dropped_buffers)?,
        make("videoconvert")?,
    ];
    let (left, right, top, bottom) = crop_pixels(output, source);
    if left > 0 || right > 0 || top > 0 || bottom > 0 {
        branch.push(
            gst::ElementFactory::make("videocrop")
                .property("left", left)
                .property("right", right)
                .property("top", top)
                .property("bottom", bottom)
                .build()
                .map_err(build_error)?,
        );
    }
    for method in flip_methods(output) {
        let flip = make("videoflip")?;
        set_enum_property(&flip, "method", method)?;
        branch.push(flip);
    }
    if output.privacy_frame {
        branch.push(
            gst::ElementFactory::make("videobalance")
                .property("contrast", 0.0_f64)
                .property("brightness", -1.0_f64)
                .build()
                .map_err(build_error)?,
        );
    }
    let scale = gst::ElementFactory::make("videoscale")
        .property("add-borders", output.fit == SharedFit::Contain)
        .build()
        .map_err(build_error)?;
    branch.extend([scale, make("videorate")?]);
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", output.format.as_str())
        .field("width", i32::try_from(output.width).unwrap_or(i32::MAX))
        .field("height", i32::try_from(output.height).unwrap_or(i32::MAX))
        .field(
            "framerate",
            gst::Fraction::new(
                i32::try_from(output.fps_numerator).unwrap_or(i32::MAX),
                i32::try_from(output.fps_denominator).unwrap_or(i32::MAX),
            ),
        )
        .build();
    branch.push(
        gst::ElementFactory::make("capsfilter")
            .property("caps", caps)
            .build()
            .map_err(build_error)?,
    );
    if let Some(text) = &output.text_overlay {
        let overlay = gst::ElementFactory::make("textoverlay")
            .property("text", text)
            .build()
            .map_err(build_error)?;
        set_enum_property(&overlay, "valignment", "bottom")?;
        branch.push(overlay);
    }
    if let Some(image) = &output.image_overlay {
        branch.push(
            gst::ElementFactory::make("gdkpixbufoverlay")
                .property("location", image.display().to_string())
                .build()
                .map_err(build_error)?,
        );
    }
    let sink = gst::ElementFactory::make("v4l2sink")
        .property("device", output.device.display().to_string())
        .property("sync", false)
        .build()
        .map_err(build_error)?;
    set_enum_property(&sink, "io-mode", "mmap")?;
    attach_output_telemetry(&sink, telemetry)?;
    branch.push(sink);
    Ok(branch)
}

fn recording_branch(
    recording: &SharedRecording,
    source: &VideoTuple,
) -> Result<Vec<gst::Element>, LinkError> {
    let muxer = match recording.container {
        RecordContainer::Matroska => "matroskamux",
        RecordContainer::Mp4 => "mp4mux",
    };
    Ok(vec![
        queue_element()?,
        make(parser_name(source)?)?,
        make(muxer)?,
        gst::ElementFactory::make("filesink")
            .property("location", recording.output.display().to_string())
            .property("sync", false)
            .build()
            .map_err(build_error)?,
    ])
}

fn crop_pixels(output: &SharedOutput, source: &VideoTuple) -> (i32, i32, i32, i32) {
    let crop = output.crop.unwrap_or(SharedCrop {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    let mut zoom_width = crop.width / output.zoom;
    let mut zoom_height = crop.height / output.zoom;
    if output.fit == SharedFit::Cover {
        let source_aspect =
            zoom_width * f64::from(source.width) / (zoom_height * f64::from(source.height));
        let output_aspect = f64::from(output.width) / f64::from(output.height);
        if source_aspect > output_aspect {
            zoom_width *= output_aspect / source_aspect;
        } else {
            zoom_height *= source_aspect / output_aspect;
        }
    }
    let available_x = (crop.width - zoom_width).max(0.0);
    let available_y = (crop.height - zoom_height).max(0.0);
    let x = crop.x + available_x * output.frame_x;
    let y = crop.y + available_y * output.frame_y;
    let left = (x * f64::from(source.width)).round() as i32;
    let top = (y * f64::from(source.height)).round() as i32;
    let right = ((1.0 - x - zoom_width).max(0.0) * f64::from(source.width)).round() as i32;
    let bottom = ((1.0 - y - zoom_height).max(0.0) * f64::from(source.height)).round() as i32;
    (left, right, top, bottom)
}

fn flip_methods(output: &SharedOutput) -> Vec<&'static str> {
    let mut methods = Vec::new();
    methods.push(match output.rotation {
        SharedRotation::None => "none",
        SharedRotation::Clockwise90 => "clockwise",
        SharedRotation::Rotate180 => "rotate-180",
        SharedRotation::Counterclockwise90 => "counterclockwise",
    });
    if output.horizontal_flip {
        methods.push("horizontal-flip");
    }
    if output.vertical_flip {
        methods.push("vertical-flip");
    }
    methods
}

fn set_enum_property(
    element: &gst::Element,
    property: &'static str,
    nick: &'static str,
) -> Result<(), LinkError> {
    let specification = element.find_property(property).ok_or_else(|| {
        media_error("required GStreamer enum property is unavailable")
            .with_detail("element", element.type_().name())
            .with_detail("property", property)
    })?;
    let enumeration = glib::EnumClass::with_type(specification.value_type()).ok_or_else(|| {
        media_error("required GStreamer property is not an enum")
            .with_detail("element", element.type_().name())
            .with_detail("property", property)
    })?;
    let value = enumeration.value_by_nick(nick).ok_or_else(|| {
        media_error("GStreamer enum value is unavailable")
            .with_detail("element", element.type_().name())
            .with_detail("property", property)
            .with_detail("value", nick)
    })?;
    element.set_property_from_value(property, &value.to_value(&enumeration));
    Ok(())
}

fn attach_atomic_stats(
    element: &gst::Element,
    frames: &Arc<AtomicU64>,
    bytes: &Arc<AtomicU64>,
) -> Result<(), LinkError> {
    let frames = Arc::clone(frames);
    let bytes = Arc::clone(bytes);
    let pad = element
        .static_pad("src")
        .ok_or_else(|| media_error("shared source filter has no source pad"))?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref() {
            frames.fetch_add(1, Ordering::Relaxed);
            bytes.fetch_add(buffer.size() as u64, Ordering::Relaxed);
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

fn attach_output_telemetry(
    element: &gst::Element,
    telemetry: &SharedOutputTelemetry,
) -> Result<(), LinkError> {
    let frames = Arc::clone(&telemetry.frames);
    let latency_ns = Arc::clone(&telemetry.latency_ns);
    let element = element.downgrade();
    let pad = element
        .upgrade()
        .and_then(|element| element.static_pad("sink"))
        .ok_or_else(|| media_error("shared output sink has no sink pad"))?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref() {
            frames.fetch_add(1, Ordering::Relaxed);
            if let (Some(pts), Some(element)) = (buffer.pts(), element.upgrade())
                && let Some(running_time) = element.current_running_time()
                && let Some(latency) = running_time.checked_sub(pts)
            {
                let mut samples = latency_ns
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if samples.len() == SHARED_LATENCY_WINDOW_SAMPLES {
                    samples.pop_front();
                }
                samples.push_back(latency.nseconds());
            }
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

fn shared_output_metrics(telemetry: &SharedOutputTelemetry) -> SharedOutputMetrics {
    let samples = telemetry
        .latency_ns
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut sorted = samples.iter().copied().collect::<Vec<_>>();
    sorted.sort_unstable();
    let sample_count = sorted.len();
    let latest = samples.back().copied().unwrap_or_default();
    let average = if sample_count == 0 {
        0
    } else {
        u64::try_from(
            sorted.iter().map(|value| u128::from(*value)).sum::<u128>() / sample_count as u128,
        )
        .unwrap_or(u64::MAX)
    };
    let p95 = if sample_count == 0 {
        0
    } else {
        sorted[(sample_count * 95).div_ceil(100).saturating_sub(1)]
    };
    SharedOutputMetrics {
        frames: telemetry.frames.load(Ordering::Relaxed),
        dropped_buffers: telemetry.dropped_buffers.load(Ordering::Relaxed),
        latency_samples: sample_count as u64,
        latest_latency_us: latest / 1_000,
        average_latency_us: average / 1_000,
        p95_latency_us: p95 / 1_000,
        max_latency_us: sorted.last().copied().unwrap_or_default() / 1_000,
    }
}

/// Send a compressed stream as RTP over UDP.
#[cfg(feature = "network")]
pub fn rtp(request: &RtpRequest) -> Result<MediaRunReport, LinkError> {
    let parser_name = parser_name(&request.source.tuple)?;
    let payloader_name = if request.source.tuple.fourcc.eq_ignore_ascii_case("H264") {
        "rtph264pay"
    } else {
        "rtpjpegpay"
    };
    initialize(&[
        "v4l2src",
        "capsfilter",
        parser_name,
        payloader_name,
        "udpsink",
    ])?;
    let pipeline = gst::Pipeline::new();
    let (source, filter) = source_elements(&request.source.node, &request.source.tuple)?;
    let parser = gst::ElementFactory::make(parser_name)
        .build()
        .map_err(build_error)?;
    let mut payloader_builder = gst::ElementFactory::make(payloader_name);
    if let Some(payload_type) = request.payload_type {
        payloader_builder = payloader_builder.property("pt", u32::from(payload_type));
    }
    if payloader_name == "rtph264pay" {
        payloader_builder = payloader_builder.property("config-interval", -1_i32);
    }
    let payloader = payloader_builder.build().map_err(build_error)?;
    let sink = gst::ElementFactory::make("udpsink")
        .property("host", &request.host)
        .property("port", i32::from(request.port))
        .property("sync", false)
        .property("async", false)
        .build()
        .map_err(build_error)?;
    pipeline_add(&pipeline, &[&source, &filter, &parser, &payloader, &sink])?;
    gst::Element::link_many([&source, &filter, &parser, &payloader, &sink]).map_err(link_error)?;
    let stats = attach_stats(&filter)?;
    run_pipeline(
        pipeline,
        &request.source,
        MediaTelemetry::video_only(stats),
        true,
        None,
        None,
    )
}

struct AudioRuntime {
    telemetry: Arc<Mutex<AudioStatsState>>,
    rate: gst::Element,
    sample_rate: u32,
    channels: u32,
    codec: String,
    processing: AudioProcessing,
}

struct MediaTelemetry {
    video: Arc<Mutex<StatsState>>,
    audio: Option<AudioRuntime>,
    av_sync: Option<Arc<Mutex<AvSyncState>>>,
}

impl MediaTelemetry {
    fn video_only(video: Arc<Mutex<StatsState>>) -> Self {
        Self {
            video,
            audio: None,
            av_sync: None,
        }
    }
}

type AudioEventSink<'a> = Option<&'a mut dyn FnMut(&AudioLevelEvent) -> Result<(), LinkError>>;

#[derive(Default)]
struct AudioStatsState {
    buffers: u64,
    bytes: u64,
    clipping_events: u64,
    timestamp_discontinuities: u64,
    total_samples: u64,
    total_sum_squares: f64,
    peak: f64,
    last_pts: Option<gst::ClockTime>,
    event_samples: u64,
    event_sum_squares: f64,
    event_peak: f64,
    sequence: u64,
    events: VecDeque<AudioLevelEvent>,
}

#[derive(Default)]
struct AvSyncState {
    video: AvSyncSeries,
    audio: AvSyncSeries,
    first_pair_time_ns: Option<u64>,
    measurement_first_time_ns: Option<u64>,
    last_pair_time_ns: Option<u64>,
    raw_initial_offset_ns: Option<i128>,
    raw_final_offset_ns: Option<i128>,
    raw_max_abs_offset_ns: u128,
    max_abs_offset_ns: u128,
}

#[derive(Default)]
struct AvSyncSeries {
    raw_first_bias_ns: Option<i128>,
    raw_latest_bias_ns: Option<i128>,
    initial_bias_sum_ns: i128,
    initial_bias_samples: u32,
    recent_biases_ns: VecDeque<i128>,
}

impl AvSyncSeries {
    fn observe_raw(&mut self, bias: i128) {
        self.raw_first_bias_ns.get_or_insert(bias);
        self.raw_latest_bias_ns = Some(bias);
    }

    fn observe_measured(&mut self, bias: i128) {
        if self.initial_bias_samples < AV_SYNC_WINDOW_SAMPLES as u32 {
            self.initial_bias_sum_ns += bias;
            self.initial_bias_samples += 1;
        }
        self.recent_biases_ns.push_back(bias);
        if self.recent_biases_ns.len() > AV_SYNC_WINDOW_SAMPLES {
            self.recent_biases_ns.pop_front();
        }
    }

    fn initial_bias(&self) -> Option<i128> {
        (self.initial_bias_samples > 0)
            .then(|| self.initial_bias_sum_ns / i128::from(self.initial_bias_samples))
    }

    fn recent_bias(&self) -> Option<i128> {
        (!self.recent_biases_ns.is_empty()).then(|| {
            self.recent_biases_ns.iter().sum::<i128>()
                / i128::try_from(self.recent_biases_ns.len()).unwrap_or(1)
        })
    }
}

const AV_SYNC_WARMUP_NS: u64 = 500_000_000;
const AV_SYNC_WINDOW_SAMPLES: usize = 32;

fn audio_source_elements_required(request: &AudioSourceRequest) -> Vec<&'static str> {
    vec![match request.backend {
        AudioBackendKind::Alsa => "alsasrc",
        AudioBackendKind::Pipewire => "pipewiresrc",
    }]
}

fn audio_processing_elements(processing: AudioProcessing) -> Vec<&'static str> {
    let mut elements = Vec::new();
    if processing.gate || processing.compressor || processing.limiter {
        elements.push("audiodynamic");
    }
    if processing.limiter {
        elements.push("audioamplify");
    }
    elements
}

fn build_audio_front(
    request: &AudioSourceRequest,
) -> Result<(gst::Pipeline, gst::Element, gst::Element), LinkError> {
    let pipeline = gst::Pipeline::new();
    let (tail, rate) = add_audio_front(&pipeline, request)?;
    Ok((pipeline, tail, rate))
}

fn add_audio_front(
    pipeline: &gst::Pipeline,
    request: &AudioSourceRequest,
) -> Result<(gst::Element, gst::Element), LinkError> {
    if request.sample_rate == 0 || request.channels == 0 {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio sample rate and channel count must be greater than zero",
        ));
    }
    let source = match request.backend {
        AudioBackendKind::Alsa => gst::ElementFactory::make("alsasrc")
            .property("device", &request.selector)
            .property("do-timestamp", true)
            .build()
            .map_err(build_error)?,
        AudioBackendKind::Pipewire => gst::ElementFactory::make("pipewiresrc")
            .property("target-object", &request.selector)
            .property("provide-clock", false)
            .property("do-timestamp", true)
            .build()
            .map_err(build_error)?,
    };
    let convert = gst::ElementFactory::make("audioconvert")
        .build()
        .map_err(build_error)?;
    let resample = gst::ElementFactory::make("audioresample")
        .build()
        .map_err(build_error)?;
    let rate = gst::ElementFactory::make("audiorate")
        .property("skip-to-first", true)
        .property("tolerance", 10_000_000_u64)
        .build()
        .map_err(build_error)?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field(
            "rate",
            i32::try_from(request.sample_rate).unwrap_or(i32::MAX),
        )
        .field(
            "channels",
            i32::try_from(request.channels).unwrap_or(i32::MAX),
        )
        .field("layout", "interleaved")
        .build();
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", caps)
        .build()
        .map_err(build_error)?;
    let delay = gst::ElementFactory::make("identity")
        .property("ts-offset", request.delay_ns)
        .build()
        .map_err(build_error)?;
    pipeline_add(
        pipeline,
        &[&source, &convert, &resample, &rate, &filter, &delay],
    )?;
    gst::Element::link_many([&source, &convert, &resample, &rate, &filter, &delay])
        .map_err(link_error)?;
    let mut tail = delay;
    for element in build_audio_processing(request.processing)? {
        pipeline.add(&element).map_err(pipeline_error)?;
        tail.link(&element).map_err(link_error)?;
        tail = element;
    }
    Ok((tail, rate))
}

fn build_audio_processing(processing: AudioProcessing) -> Result<Vec<gst::Element>, LinkError> {
    let mut elements = Vec::new();
    if processing.gate {
        elements.push(
            gst::ElementFactory::make("audiodynamic")
                .property_from_str("mode", "expander")
                .property_from_str("characteristics", "hard-knee")
                .property("threshold", db_to_linear(-50.0) as f32)
                .property("ratio", 10.0_f32)
                .build()
                .map_err(build_error)?,
        );
    }
    if processing.compressor {
        elements.push(
            gst::ElementFactory::make("audiodynamic")
                .property_from_str("mode", "compressor")
                .property_from_str("characteristics", "soft-knee")
                .property("threshold", db_to_linear(-12.0) as f32)
                .property("ratio", 4.0_f32)
                .build()
                .map_err(build_error)?,
        );
    }
    if processing.limiter {
        elements.push(
            gst::ElementFactory::make("audiodynamic")
                .property_from_str("mode", "compressor")
                .property_from_str("characteristics", "hard-knee")
                .property("threshold", db_to_linear(-1.0) as f32)
                .property("ratio", 20.0_f32)
                .build()
                .map_err(build_error)?,
        );
        elements.push(
            gst::ElementFactory::make("audioamplify")
                .property("amplification", 1.0_f32)
                .property_from_str("clipping-method", "clip")
                .build()
                .map_err(build_error)?,
        );
    }
    Ok(elements)
}

fn attach_audio_stats(
    element: &gst::Element,
    sample_rate: u32,
    channels: u32,
    interval: Duration,
) -> Result<Arc<Mutex<AudioStatsState>>, LinkError> {
    let state = Arc::new(Mutex::new(AudioStatsState::default()));
    let state_for_probe = Arc::clone(&state);
    let interval_samples = ((interval.as_secs_f64() * f64::from(sample_rate) * f64::from(channels))
        .round() as u64)
        .max(1);
    let pad = element
        .static_pad("src")
        .ok_or_else(|| media_error("audio telemetry element has no source pad"))?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref()
            && let Ok(mut stats) = state_for_probe.lock()
        {
            stats.buffers = stats.buffers.saturating_add(1);
            stats.bytes = stats.bytes.saturating_add(buffer.size() as u64);
            if stats.buffers > 1 && buffer.flags().contains(gst::BufferFlags::DISCONT) {
                stats.timestamp_discontinuities = stats.timestamp_discontinuities.saturating_add(1);
            }
            match buffer.pts() {
                Some(pts) if stats.last_pts.is_some_and(|last| pts < last) => {
                    stats.timestamp_discontinuities =
                        stats.timestamp_discontinuities.saturating_add(1);
                    stats.last_pts = Some(pts);
                }
                Some(pts) => stats.last_pts = Some(pts),
                None => {
                    stats.timestamp_discontinuities =
                        stats.timestamp_discontinuities.saturating_add(1);
                }
            }
            if let Ok(map) = buffer.map_readable() {
                let mut local_peak = 0.0_f64;
                let mut local_sum_squares = 0.0_f64;
                let mut local_samples = 0_u64;
                for bytes in map.as_slice().chunks_exact(2) {
                    let sample = f64::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0;
                    let absolute = sample.abs();
                    local_peak = local_peak.max(absolute);
                    local_sum_squares += sample * sample;
                    local_samples = local_samples.saturating_add(1);
                }
                if local_peak >= db_to_linear(-0.1) {
                    stats.clipping_events = stats.clipping_events.saturating_add(1);
                }
                stats.peak = stats.peak.max(local_peak);
                stats.total_samples = stats.total_samples.saturating_add(local_samples);
                stats.total_sum_squares += local_sum_squares;
                stats.event_samples = stats.event_samples.saturating_add(local_samples);
                stats.event_sum_squares += local_sum_squares;
                stats.event_peak = stats.event_peak.max(local_peak);
                if stats.event_samples >= interval_samples {
                    let rms = (stats.event_sum_squares / stats.event_samples as f64).sqrt();
                    stats.sequence = stats.sequence.saturating_add(1);
                    let event = AudioLevelEvent {
                        sequence: stats.sequence,
                        elapsed_ms: stats.total_samples.saturating_mul(1_000)
                            / u64::from(sample_rate)
                                .saturating_mul(u64::from(channels))
                                .max(1),
                        peak_dbfs: linear_to_db(stats.event_peak),
                        rms_dbfs: linear_to_db(rms),
                        clipped: stats.event_peak >= db_to_linear(-0.1),
                        discontinuities: stats.timestamp_discontinuities,
                    };
                    stats.events.push_back(event);
                    stats.event_samples = 0;
                    stats.event_sum_squares = 0.0;
                    stats.event_peak = 0.0;
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
    Ok(state)
}

fn finish_audio_stats(
    state: &Arc<Mutex<AudioStatsState>>,
    rate: &gst::Element,
    sample_rate: u32,
    channels: u32,
    codec: Option<String>,
    processing: AudioProcessing,
) -> AudioStats {
    let state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let rms = if state.total_samples == 0 {
        0.0
    } else {
        (state.total_sum_squares / state.total_samples as f64).sqrt()
    };
    AudioStats {
        buffers: state.buffers,
        bytes: state.bytes,
        clipping_events: state.clipping_events,
        timestamp_discontinuities: state.timestamp_discontinuities,
        dropped_samples: rate.property::<u64>("drop"),
        added_samples: rate.property::<u64>("add"),
        sample_rate,
        channels,
        peak_dbfs: linear_to_db(state.peak),
        rms_dbfs: linear_to_db(rms),
        codec,
        processing,
    }
}

fn drain_audio_events<F>(
    state: &Arc<Mutex<AudioStatsState>>,
    emit: &mut Option<&mut F>,
) -> Result<(), LinkError>
where
    F: FnMut(&AudioLevelEvent) -> Result<(), LinkError> + ?Sized,
{
    let events = {
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.events.drain(..).collect::<Vec<_>>()
    };
    if let Some(emit) = emit.as_deref_mut() {
        for event in &events {
            emit(event)?;
        }
    }
    Ok(())
}

fn run_audio_pipeline(
    pipeline: gst::Pipeline,
    request: &AudioSourceRequest,
    telemetry: Arc<Mutex<AudioStatsState>>,
    rate: &gst::Element,
    codec: Option<String>,
    mut emit: AudioEventSink<'_>,
) -> Result<AudioRunReport, LinkError> {
    INTERRUPTED.store(false, Ordering::SeqCst);
    let bus = pipeline
        .bus()
        .ok_or_else(|| media_error("GStreamer audio pipeline has no bus"))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(state_error)?;
    let started = Instant::now();
    let mut requested_stop = None;
    let mut stop_requested_at = None;
    let mut finalized = false;
    let mut failure = None;
    loop {
        drain_audio_events(&telemetry, &mut emit)?;
        if requested_stop.is_none() {
            let reason = if INTERRUPTED.load(Ordering::SeqCst) {
                Some(AudioStopReason::Interrupted)
            } else if request
                .duration
                .is_some_and(|limit| started.elapsed() >= limit)
            {
                Some(AudioStopReason::Completed)
            } else {
                None
            };
            if let Some(reason) = reason {
                requested_stop = Some(reason);
                stop_requested_at = Some(Instant::now());
                pipeline.send_event(gst::event::Eos::new());
            }
        } else if stop_requested_at.is_some_and(|at| at.elapsed() >= request.shutdown_timeout) {
            failure = Some(LinkError::new(
                ErrorKind::Timeout,
                "audio pipeline did not finalize before the shutdown timeout",
            ));
            break;
        }
        let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Eos(..) => {
                finalized = true;
                break;
            }
            gst::MessageView::Error(error) => {
                let reason = error.error().to_string();
                let debug = error
                    .debug()
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                if reason.to_ascii_lowercase().contains("broken pipe")
                    || debug.to_ascii_lowercase().contains("broken pipe")
                {
                    requested_stop = Some(AudioStopReason::BrokenPipe);
                    break;
                }
                failure = Some(
                    LinkError::new(
                        ErrorKind::MediaPipelineFailure,
                        "GStreamer audio pipeline failed",
                    )
                    .with_detail("reason", reason)
                    .with_detail("debug", debug),
                );
                break;
            }
            _ => {}
        }
    }
    drain_audio_events(&telemetry, &mut emit)?;
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(AudioRunReport {
        source_id: request.id.clone(),
        stats: finish_audio_stats(
            &telemetry,
            rate,
            request.sample_rate,
            request.channels,
            codec,
            request.processing,
        ),
        stop_reason: requested_stop.unwrap_or(AudioStopReason::Completed),
        outputs: Vec::new(),
        finalized,
    })
}

fn attach_av_sync(
    video: &gst::Element,
    audio: &gst::Element,
) -> Result<Arc<Mutex<AvSyncState>>, LinkError> {
    let state = Arc::new(Mutex::new(AvSyncState::default()));
    let epoch = Instant::now();
    attach_sync_pad(video, Arc::clone(&state), epoch, true)?;
    attach_sync_pad(audio, Arc::clone(&state), epoch, false)?;
    Ok(state)
}

fn attach_sync_pad(
    element: &gst::Element,
    state: Arc<Mutex<AvSyncState>>,
    epoch: Instant,
    video: bool,
) -> Result<(), LinkError> {
    let pad = element
        .static_pad("src")
        .ok_or_else(|| media_error("sync telemetry element has no source pad"))?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref()
            && let Some(pts) = buffer.pts()
            && let Ok(mut state) = state.lock()
        {
            let elapsed = u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let clock_bias = i128::from(pts.nseconds()) - i128::from(elapsed);
            if video {
                state.video.observe_raw(clock_bias);
            } else {
                state.audio.observe_raw(clock_bias);
            }
            if let (Some(video_bias), Some(audio_bias)) = (
                state.video.raw_latest_bias_ns,
                state.audio.raw_latest_bias_ns,
            ) {
                let offset = audio_bias - video_bias;
                let first_pair = *state.first_pair_time_ns.get_or_insert(elapsed);
                state.raw_initial_offset_ns.get_or_insert(offset);
                state.raw_final_offset_ns = Some(offset);
                state.raw_max_abs_offset_ns =
                    state.raw_max_abs_offset_ns.max(offset.unsigned_abs());
                if elapsed.saturating_sub(first_pair) < AV_SYNC_WARMUP_NS {
                    return gst::PadProbeReturn::Ok;
                }
                if video {
                    state.video.observe_measured(clock_bias);
                } else {
                    state.audio.observe_measured(clock_bias);
                }
                if let (Some(video_bias), Some(audio_bias)) =
                    (state.video.recent_bias(), state.audio.recent_bias())
                {
                    let averaged_offset = audio_bias - video_bias;
                    state.measurement_first_time_ns.get_or_insert(elapsed);
                    state.last_pair_time_ns = Some(elapsed);
                    state.max_abs_offset_ns =
                        state.max_abs_offset_ns.max(averaged_offset.unsigned_abs());
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

fn finish_av_sync(state: &Arc<Mutex<AvSyncState>>) -> AvSyncStats {
    let state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let measured_initial = state
        .audio
        .initial_bias()
        .zip(state.video.initial_bias())
        .map(|(audio, video)| audio - video);
    let measured_final = state
        .audio
        .recent_bias()
        .zip(state.video.recent_bias())
        .map(|(audio, video)| audio - video);
    let measured = measured_initial.is_some() && measured_final.is_some();
    let initial = measured_initial
        .or(state.raw_initial_offset_ns)
        .unwrap_or_default();
    let final_offset = measured_final
        .or(state.raw_final_offset_ns)
        .unwrap_or(initial);
    let drift = final_offset - initial;
    let elapsed = state
        .last_pair_time_ns
        .zip(state.measurement_first_time_ns.or(state.first_pair_time_ns))
        .map_or(0, |(last, first)| last.saturating_sub(first));
    AvSyncStats {
        initial_offset_ms: initial as f64 / 1_000_000.0,
        final_offset_ms: final_offset as f64 / 1_000_000.0,
        max_abs_offset_ms: if measured {
            state.max_abs_offset_ns
        } else {
            state.raw_max_abs_offset_ns
        } as f64
            / 1_000_000.0,
        drift_ms: drift as f64 / 1_000_000.0,
        drift_ppm: if elapsed == 0 {
            0.0
        } else {
            drift as f64 / elapsed as f64 * 1_000_000.0
        },
        corrected: true,
    }
}

struct AudioOutputPlan {
    requested: PathBuf,
    temporary: PathBuf,
    overwrite: bool,
}

impl AudioOutputPlan {
    fn new(requested: &Path, overwrite: bool) -> Result<Self, LinkError> {
        validate_output_parent(requested)?;
        if requested.exists() && !overwrite {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "audio output already exists; use --overwrite to replace it",
            )
            .with_detail("path", requested.display().to_string()));
        }
        let parent = requested.parent().unwrap_or_else(|| Path::new("."));
        let stem = requested
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| media_error("audio output has no valid file stem"))?;
        let extension = requested
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("audio");
        let mut counter = 0_u32;
        let temporary = loop {
            let candidate = parent.join(format!(
                ".{stem}.linkctl-audio-part-{}-{counter}.{extension}",
                std::process::id()
            ));
            if !candidate.exists() {
                break candidate;
            }
            counter = counter.saturating_add(1);
        };
        Ok(Self {
            requested: requested.to_owned(),
            temporary,
            overwrite,
        })
    }

    fn finish(self, finalized: bool) -> Result<Vec<PathBuf>, LinkError> {
        if !finalized {
            return Ok(vec![self.temporary]);
        }
        if self.requested.exists() && !self.overwrite {
            return Err(LinkError::new(
                ErrorKind::IoFailure,
                "audio destination appeared before finalization",
            )
            .with_detail("path", self.requested.display().to_string())
            .with_detail("recoverable", self.temporary.display().to_string()));
        }
        fs::rename(&self.temporary, &self.requested).map_err(|error| {
            filesystem_error("failed to finalize audio output", &self.requested, &error)
                .with_detail("recoverable", self.temporary.display().to_string())
        })?;
        Ok(vec![self.requested])
    }
}

const fn audio_encoding_name(encoding: AudioEncoding) -> &'static str {
    match encoding {
        AudioEncoding::Wav => "pcm-s16le",
        AudioEncoding::Flac => "flac",
        AudioEncoding::Raw => "pcm-s16le",
    }
}

fn db_to_linear(db: f64) -> f64 {
    10_f64.powf(db / 20.0)
}

fn linear_to_db(value: f64) -> f64 {
    if value <= 0.0 {
        -120.0
    } else {
        20.0 * value.log10()
    }
}

/// Parse a binary size such as `5GiB`, `100MB`, or a raw byte count.
pub fn parse_byte_size(input: &str) -> Result<u64, LinkError> {
    let trimmed = input.trim();
    let split = trimmed
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split);
    let number = f64::from_str(number).map_err(|_| invalid_size(input))?;
    if !number.is_finite() || number < 0.0 {
        return Err(invalid_size(input));
    }
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1_f64,
        "kb" => 1_000_f64,
        "mb" => 1_000_000_f64,
        "gb" => 1_000_000_000_f64,
        "kib" => 1_024_f64,
        "mib" => 1_048_576_f64,
        "gib" => 1_073_741_824_f64,
        _ => return Err(invalid_size(input)),
    };
    let bytes = number * multiplier;
    if bytes > u64::MAX as f64 {
        return Err(invalid_size(input));
    }
    Ok(bytes.round() as u64)
}

fn run_pipeline(
    pipeline: gst::Pipeline,
    request: &ForegroundRequest,
    telemetry: MediaTelemetry,
    pass_through: bool,
    disk_guard: Option<(&Path, u64)>,
    size_guard: Option<(&OutputPlan, u64)>,
) -> Result<MediaRunReport, LinkError> {
    INTERRUPTED.store(false, Ordering::SeqCst);
    let bus = pipeline
        .bus()
        .ok_or_else(|| media_error("GStreamer pipeline has no bus"))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(state_error)?;
    let started = Instant::now();
    let mut requested_stop = None;
    let mut stop_requested_at = None;
    let mut finalized = false;
    let mut failure = None;
    loop {
        if requested_stop.is_none() {
            let reason = if INTERRUPTED.load(Ordering::SeqCst) {
                Some(MediaStopReason::Interrupted)
            } else if request
                .duration
                .is_some_and(|limit| started.elapsed() >= limit)
            {
                Some(MediaStopReason::Completed)
            } else if let Some((path, reserve)) = disk_guard
                && available_bytes(path)? < reserve
            {
                Some(MediaStopReason::DiskReserve)
            } else if let Some((output, limit)) = size_guard
                && output_size(output)? >= limit
            {
                Some(MediaStopReason::SizeLimit)
            } else {
                None
            };
            if let Some(reason) = reason {
                requested_stop = Some(reason);
                stop_requested_at = Some(Instant::now());
                pipeline.send_event(gst::event::Eos::new());
            }
        } else if stop_requested_at.is_some_and(|at| at.elapsed() >= request.shutdown_timeout) {
            failure = Some(LinkError::new(
                ErrorKind::Timeout,
                "media pipeline did not finalize before the shutdown timeout",
            ));
            break;
        }

        let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Eos(..) => {
                finalized = true;
                break;
            }
            gst::MessageView::Error(error) => {
                let reason = error.error().to_string();
                let debug = error
                    .debug()
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                if reason.to_ascii_lowercase().contains("broken pipe")
                    || debug.to_ascii_lowercase().contains("broken pipe")
                {
                    requested_stop = Some(MediaStopReason::BrokenPipe);
                    break;
                }
                failure = Some(
                    LinkError::new(ErrorKind::MediaPipelineFailure, "GStreamer pipeline failed")
                        .with_detail("reason", reason)
                        .with_detail("debug", debug),
                );
                break;
            }
            _ => {}
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(failure) = failure {
        return Err(failure);
    }
    let audio = telemetry.audio.map(|runtime| {
        finish_audio_stats(
            &runtime.telemetry,
            &runtime.rate,
            runtime.sample_rate,
            runtime.channels,
            Some(runtime.codec),
            runtime.processing,
        )
    });
    let av_sync = telemetry.av_sync.as_ref().map(finish_av_sync);
    let report = MediaRunReport {
        tuple: request.tuple.clone().normalized(),
        stats: finish_stats(&telemetry.video, started.elapsed()),
        stop_reason: requested_stop.unwrap_or(MediaStopReason::Completed),
        outputs: Vec::new(),
        pass_through,
        finalized,
        audio,
        av_sync,
    };
    Ok(report)
}

#[derive(Default)]
struct StatsState {
    frames: u64,
    bytes: u64,
    sequence_drops: u64,
    timestamp_discontinuities: u64,
    last_offset: Option<u64>,
    last_pts: Option<gst::ClockTime>,
}

fn attach_stats(element: &gst::Element) -> Result<Arc<Mutex<StatsState>>, LinkError> {
    let state = Arc::new(Mutex::new(StatsState::default()));
    let state_for_probe = Arc::clone(&state);
    let pad = element
        .static_pad("src")
        .ok_or_else(|| media_error("caps filter has no source pad"))?;
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref()
            && let Ok(mut stats) = state_for_probe.lock()
        {
            stats.frames = stats.frames.saturating_add(1);
            stats.bytes = stats.bytes.saturating_add(buffer.size() as u64);
            let offset = buffer.offset();
            if offset != gst::format::Buffers::OFFSET_NONE {
                if let Some(previous) = stats.last_offset
                    && offset > previous.saturating_add(1)
                {
                    stats.sequence_drops =
                        stats.sequence_drops.saturating_add(offset - previous - 1);
                }
                stats.last_offset = Some(offset);
            }
            match buffer.pts() {
                Some(pts) if stats.last_pts.is_some_and(|last| pts < last) => {
                    stats.timestamp_discontinuities =
                        stats.timestamp_discontinuities.saturating_add(1);
                    stats.last_pts = Some(pts);
                }
                Some(pts) => stats.last_pts = Some(pts),
                None => {
                    stats.timestamp_discontinuities =
                        stats.timestamp_discontinuities.saturating_add(1);
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
    Ok(state)
}

fn finish_stats(state: &Arc<Mutex<StatsState>>, elapsed: Duration) -> MediaStats {
    let state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let average_bitrate_bps = if elapsed.as_nanos() == 0 {
        0
    } else {
        u64::try_from(
            u128::from(state.bytes)
                .saturating_mul(8)
                .saturating_mul(1_000_000_000)
                / elapsed.as_nanos(),
        )
        .unwrap_or(u64::MAX)
    };
    MediaStats {
        frames: state.frames,
        bytes: state.bytes,
        sequence_drops: state.sequence_drops,
        qos_drops: 0,
        timestamp_discontinuities: state.timestamp_discontinuities,
        elapsed_ms,
        average_bitrate_bps,
    }
}

fn source_elements(
    node: &Path,
    tuple: &VideoTuple,
) -> Result<(gst::Element, gst::Element), LinkError> {
    let source = gst::ElementFactory::make("v4l2src")
        .property("device", node.display().to_string())
        .property("do-timestamp", true)
        .build()
        .map_err(build_error)?;
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", caps_for(tuple)?)
        .build()
        .map_err(build_error)?;
    Ok((source, filter))
}

fn caps_for(tuple: &VideoTuple) -> Result<gst::Caps, LinkError> {
    let fps_numerator = i32::try_from(tuple.fps.numerator)
        .map_err(|_| media_error("frame-rate numerator exceeds GStreamer limits"))?;
    let fps_denominator = i32::try_from(tuple.fps.denominator)
        .map_err(|_| media_error("frame-rate denominator exceeds GStreamer limits"))?;
    let mut builder = if tuple.fourcc.eq_ignore_ascii_case("MJPG") {
        gst::Caps::builder("image/jpeg")
    } else if tuple.fourcc.eq_ignore_ascii_case("H264") {
        gst::Caps::builder("video/x-h264")
    } else {
        let format = match tuple.fourcc.to_ascii_uppercase().as_str() {
            "YUYV" => "YUY2",
            "UYVY" => "UYVY",
            value => {
                return Err(LinkError::new(
                    ErrorKind::CapabilityUnsupported,
                    "GStreamer mapping is unavailable for the selected raw FourCC",
                )
                .with_detail("fourcc", value));
            }
        };
        gst::Caps::builder("video/x-raw").field("format", format)
    };
    builder = builder
        .field("width", i32::try_from(tuple.width).unwrap_or(i32::MAX))
        .field("height", i32::try_from(tuple.height).unwrap_or(i32::MAX))
        .field(
            "framerate",
            gst::Fraction::new(fps_numerator, fps_denominator),
        );
    Ok(builder.build())
}

fn snapshot_elements(
    tuple: &VideoTuple,
    encoding: SnapshotEncoding,
) -> Result<Vec<&'static str>, LinkError> {
    let mut elements = vec!["v4l2src", "capsfilter", "appsink"];
    match (tuple.fourcc.to_ascii_uppercase().as_str(), encoding) {
        ("MJPG", SnapshotEncoding::Raw | SnapshotEncoding::Jpeg) => elements.push("jpegparse"),
        ("MJPG", SnapshotEncoding::Png) => {
            elements.extend(["jpegparse", "jpegdec", "videoconvert", "pngenc"]);
        }
        ("H264", SnapshotEncoding::Raw) => elements.push("h264parse"),
        ("H264", SnapshotEncoding::Jpeg) => {
            elements.extend(["h264parse", "avdec_h264", "videoconvert", "jpegenc"]);
        }
        ("H264", SnapshotEncoding::Png) => {
            elements.extend(["h264parse", "avdec_h264", "videoconvert", "pngenc"]);
        }
        (_, SnapshotEncoding::Raw) => {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "raw snapshots require MJPEG or H.264 input",
            ));
        }
        (_, SnapshotEncoding::Jpeg) => elements.extend(["videoconvert", "jpegenc"]),
        (_, SnapshotEncoding::Png) => elements.extend(["videoconvert", "pngenc"]),
    }
    Ok(elements)
}

fn snapshot_transform(
    tuple: &VideoTuple,
    encoding: SnapshotEncoding,
) -> Result<Vec<gst::Element>, LinkError> {
    let names: &[&str] = match (tuple.fourcc.to_ascii_uppercase().as_str(), encoding) {
        ("MJPG", SnapshotEncoding::Raw | SnapshotEncoding::Jpeg) => &["jpegparse"],
        ("MJPG", SnapshotEncoding::Png) => &["jpegparse", "jpegdec", "videoconvert", "pngenc"],
        ("H264", SnapshotEncoding::Raw) => &["h264parse"],
        ("H264", SnapshotEncoding::Jpeg) => &["h264parse", "avdec_h264", "videoconvert", "jpegenc"],
        ("H264", SnapshotEncoding::Png) => &["h264parse", "avdec_h264", "videoconvert", "pngenc"],
        (_, SnapshotEncoding::Raw) => return Err(media_error("raw snapshot mapping unavailable")),
        (_, SnapshotEncoding::Jpeg) => &["videoconvert", "jpegenc"],
        (_, SnapshotEncoding::Png) => &["videoconvert", "pngenc"],
    };
    names
        .iter()
        .map(|name| gst::ElementFactory::make(name).build().map_err(build_error))
        .collect()
}

fn parser_name(tuple: &VideoTuple) -> Result<&'static str, LinkError> {
    if tuple.fourcc.eq_ignore_ascii_case("H264") {
        Ok("h264parse")
    } else if tuple.fourcc.eq_ignore_ascii_case("MJPG") {
        Ok("jpegparse")
    } else {
        Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "encoded pass-through requires H.264 or MJPEG input",
        )
        .with_detail("fourcc", tuple.fourcc.clone()))
    }
}

fn is_pass_through(tuple: &VideoTuple) -> bool {
    tuple.fourcc.eq_ignore_ascii_case("H264") || tuple.fourcc.eq_ignore_ascii_case("MJPG")
}

struct OutputPlan {
    requested: PathBuf,
    location: PathBuf,
    segmented: bool,
    prefix: String,
    extension: String,
}

impl OutputPlan {
    fn new(request: &RecordRequest) -> Result<Self, LinkError> {
        let segmented = request.segment_duration.is_some()
            || request.segment_bytes.is_some()
            || request.rolling_files.is_some();
        let parent = request.output.parent().unwrap_or_else(|| Path::new("."));
        let stem = request
            .output
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| media_error("recording output has no valid file stem"))?;
        let extension = request
            .output
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or(match request.container {
                RecordContainer::Matroska => "mkv",
                RecordContainer::Mp4 => "mp4",
            })
            .to_owned();
        if !segmented && request.output.exists() && !request.overwrite {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "recording output already exists; use --overwrite to replace it",
            )
            .with_detail("path", request.output.display().to_string()));
        }
        let prefix = if segmented {
            format!("{stem}-")
        } else {
            format!(".{stem}.linkctl-part-{}-", std::process::id())
        };
        if segmented {
            let existing = segment_outputs(parent, &prefix, &extension)?;
            if !existing.is_empty() && !request.overwrite {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "recording segment outputs already exist; use --overwrite to replace them",
                )
                .with_detail("first_path", existing[0].display().to_string()));
            }
        }
        let location = if segmented {
            parent.join(format!("{prefix}%05d.{extension}"))
        } else {
            let mut counter = 0_u32;
            loop {
                let candidate = parent.join(format!("{prefix}{counter}.{extension}"));
                if !candidate.exists() {
                    break candidate;
                }
                counter = counter.saturating_add(1);
            }
        };
        Ok(Self {
            requested: request.output.clone(),
            location,
            segmented,
            prefix,
            extension,
        })
    }

    fn gstreamer_location(&self) -> String {
        self.location.display().to_string()
    }

    fn parent(&self) -> &Path {
        self.requested.parent().unwrap_or_else(|| Path::new("."))
    }

    fn finish(self, finalized: bool, overwrite: bool) -> Result<Vec<PathBuf>, LinkError> {
        if self.segmented {
            return segment_outputs(self.parent(), &self.prefix, &self.extension);
        }
        if !finalized {
            return Ok(vec![self.location]);
        }
        if self.requested.exists() && !overwrite {
            return Err(LinkError::new(
                ErrorKind::IoFailure,
                "recording destination appeared before finalization",
            )
            .with_detail("path", self.requested.display().to_string())
            .with_detail("recoverable", self.location.display().to_string()));
        }
        fs::rename(&self.location, &self.requested).map_err(|error| {
            filesystem_error(
                "failed to finalize recording output",
                &self.requested,
                &error,
            )
            .with_detail("recoverable", self.location.display().to_string())
        })?;
        Ok(vec![self.requested])
    }
}

fn segment_outputs(
    parent: &Path,
    prefix: &str,
    extension: &str,
) -> Result<Vec<PathBuf>, LinkError> {
    let mut outputs = fs::read_dir(parent)
        .map_err(|error| filesystem_error("failed to inspect output directory", parent, &error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(prefix) && name.ends_with(&format!(".{extension}"))
                })
        })
        .collect::<Vec<_>>();
    outputs.sort();
    Ok(outputs)
}

fn output_size(output: &OutputPlan) -> Result<u64, LinkError> {
    let parent = output.parent();
    let requested_name = output
        .requested
        .file_name()
        .and_then(|value| value.to_str());
    let extension_suffix = format!(".{}", output.extension);
    let mut total = 0_u64;
    for entry in fs::read_dir(parent)
        .map_err(|error| filesystem_error("failed to inspect recording size", parent, &error))?
    {
        let entry = entry.map_err(|error| {
            filesystem_error("failed to inspect recording size", parent, &error)
        })?;
        if entry.file_name().to_str().is_some_and(|name| {
            let generated = name.starts_with(&output.prefix) && name.ends_with(&extension_suffix);
            Some(name) == requested_name || generated
        }) {
            total = total.saturating_add(entry.metadata().map(|value| value.len()).unwrap_or(0));
        }
    }
    Ok(total)
}

fn validate_output_parent(path: &Path) -> Result<(), LinkError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(LinkError::new(
            ErrorKind::IoFailure,
            "recording output directory does not exist",
        )
        .with_detail("path", parent.display().to_string()));
    }
    Ok(())
}

fn ensure_disk_reserve(path: &Path, reserve: u64) -> Result<(), LinkError> {
    let available = available_bytes(path)?;
    if available < reserve {
        return Err(LinkError::new(
            ErrorKind::MediaPipelineFailure,
            "insufficient free disk space for recording",
        )
        .with_detail("available_bytes", available)
        .with_detail("required_reserve_bytes", reserve));
    }
    Ok(())
}

fn available_bytes(path: &Path) -> Result<u64, LinkError> {
    let parent = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or_else(|| Path::new("."))
    };
    let status = rustix::fs::statvfs(parent).map_err(|error| {
        LinkError::new(ErrorKind::IoFailure, "failed to inspect free disk space")
            .with_detail("path", parent.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    Ok(status.f_bavail.saturating_mul(status.f_frsize))
}

fn runtime_directory() -> Result<PathBuf, LinkError> {
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("linkctl-{}", rustix::process::getuid().as_raw()))
        })
        .join("linkctl");
    fs::create_dir_all(&directory).map_err(|error| {
        filesystem_error(
            "failed to create media runtime directory",
            &directory,
            &error,
        )
    })?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
        filesystem_error(
            "failed to secure media runtime directory",
            &directory,
            &error,
        )
    })?;
    Ok(directory)
}

fn install_signal_handler() -> Result<(), LinkError> {
    let result = SIGNAL_HANDLER.get_or_init(|| {
        ctrlc::set_handler(|| {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
        })
        .map_err(|error| error.to_string())
    });
    result.clone().map_err(|reason| {
        LinkError::new(
            ErrorKind::MediaPipelineFailure,
            "failed to install media signal handler",
        )
        .with_detail("reason", reason)
    })
}

fn pipeline_add(pipeline: &gst::Pipeline, elements: &[&gst::Element]) -> Result<(), LinkError> {
    pipeline.add_many(elements).map_err(pipeline_error)
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn unix_ms() -> Result<u128, LinkError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            LinkError::new(
                ErrorKind::IoFailure,
                "system clock is before the Unix epoch",
            )
            .with_detail("reason", error.to_string())
        })
}

fn invalid_size(input: &str) -> LinkError {
    LinkError::new(ErrorKind::InvalidInvocation, "invalid byte-size value")
        .with_detail("value", input.to_owned())
        .with_detail("expected", "bytes, KB/MB/GB, or KiB/MiB/GiB")
}

fn media_error(message: &'static str) -> LinkError {
    LinkError::new(ErrorKind::MediaPipelineFailure, message)
}

fn build_error(error: gst::glib::BoolError) -> LinkError {
    media_error("failed to construct GStreamer element").with_detail("reason", error.to_string())
}

fn pipeline_error(error: gst::glib::BoolError) -> LinkError {
    media_error("failed to add element to GStreamer pipeline")
        .with_detail("reason", error.to_string())
}

fn link_error(error: gst::glib::BoolError) -> LinkError {
    media_error("failed to link GStreamer elements").with_detail("reason", error.to_string())
}

fn state_error(error: gst::StateChangeError) -> LinkError {
    media_error("failed to start GStreamer pipeline").with_detail("reason", error.to_string())
}

fn filesystem_error(message: &'static str, path: &Path, error: &io::Error) -> LinkError {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, message)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex, atomic::Ordering},
    };

    use super::gst;
    use link_core::audio::AudioProcessing;

    use super::{
        AvSyncSeries, AvSyncState, SharedCrop, SharedFit, SharedOutput, SharedOutputTelemetry,
        SharedRotation, audio_processing_elements, crop_pixels, finish_av_sync, make,
        missing_elements, parse_byte_size, set_enum_property, shared_output_metrics,
        validate_shared_contracts,
    };
    use link_core::{media::VideoTuple, probe::Rational};

    #[test]
    fn binary_and_decimal_byte_sizes_are_explicit() {
        assert_eq!(parse_byte_size("5GiB").unwrap(), 5 * 1_073_741_824);
        assert_eq!(parse_byte_size("1.5MB").unwrap(), 1_500_000);
        assert!(parse_byte_size("lots").is_err());
    }

    #[test]
    fn requested_processing_presets_map_to_the_needed_plugins() {
        assert!(audio_processing_elements(AudioProcessing::default()).is_empty());
        assert_eq!(
            audio_processing_elements(AudioProcessing {
                gate: true,
                compressor: true,
                limiter: true,
            }),
            vec!["audiodynamic", "audioamplify"]
        );
    }

    #[test]
    fn runtime_inspection_reports_each_missing_required_element() {
        let missing = missing_elements(&["v4l2src", "capsfilter", "fakesink"], |element| {
            element == "v4l2src"
        });
        assert_eq!(missing, ["capsfilter", "fakesink"]);
    }

    #[test]
    fn sync_report_exposes_offset_and_drift_in_stable_units() {
        let state = Arc::new(Mutex::new(AvSyncState {
            first_pair_time_ns: Some(0),
            last_pair_time_ns: Some(1_000_000_000),
            raw_initial_offset_ns: Some(2_000_000),
            raw_final_offset_ns: Some(3_000_000),
            raw_max_abs_offset_ns: 4_000_000,
            ..AvSyncState::default()
        }));
        let report = finish_av_sync(&state);
        assert!((report.initial_offset_ms - 2.0).abs() < f64::EPSILON);
        assert!((report.final_offset_ms - 3.0).abs() < f64::EPSILON);
        assert!((report.drift_ms - 1.0).abs() < f64::EPSILON);
        assert!((report.drift_ppm - 1_000.0).abs() < f64::EPSILON);
        assert!(report.corrected);
    }

    #[test]
    fn sync_report_averages_each_stream_clock_independently() {
        let mut video = AvSyncSeries::default();
        let mut audio = AvSyncSeries::default();
        for _ in 0..64 {
            video.observe_measured(1_000_000);
        }
        for _ in 0..48 {
            audio.observe_measured(6_000_000);
        }
        let state = Arc::new(Mutex::new(AvSyncState {
            video,
            audio,
            measurement_first_time_ns: Some(0),
            last_pair_time_ns: Some(1_000_000_000),
            max_abs_offset_ns: 5_000_000,
            ..AvSyncState::default()
        }));
        let report = finish_av_sync(&state);
        assert!((report.initial_offset_ms - 5.0).abs() < f64::EPSILON);
        assert!((report.final_offset_ms - 5.0).abs() < f64::EPSILON);
        assert!(report.drift_ms.abs() < f64::EPSILON);
        assert!(report.drift_ppm.abs() < f64::EPSILON);
    }

    fn shared_output(name: &str) -> SharedOutput {
        SharedOutput {
            name: name.into(),
            device: PathBuf::from(format!("/dev/{name}")),
            width: 1280,
            height: 720,
            fps_numerator: 30,
            fps_denominator: 1,
            format: "YUY2".into(),
            rotation: SharedRotation::None,
            horizontal_flip: false,
            vertical_flip: false,
            crop: None,
            fit: SharedFit::Contain,
            zoom: 1.0,
            frame_x: 0.5,
            frame_y: 0.5,
            text_overlay: None,
            image_overlay: None,
            privacy_frame: false,
        }
    }

    #[test]
    fn shared_output_contract_rejects_duplicate_names_and_devices() {
        let first = shared_output("clean");
        let mut duplicate_name = shared_output("clean");
        duplicate_name.device = PathBuf::from("/dev/other");
        assert!(validate_shared_contracts(&[first.clone(), duplicate_name], None).is_err());

        let mut duplicate_device = shared_output("effects");
        duplicate_device.device = first.device.clone();
        assert!(validate_shared_contracts(&[first, duplicate_device], None).is_err());
    }

    #[test]
    fn cover_fit_crops_source_to_output_aspect() {
        let mut output = shared_output("portrait");
        output.width = 1080;
        output.height = 1920;
        output.fit = SharedFit::Cover;
        output.crop = Some(SharedCrop {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        let source = VideoTuple {
            fourcc: "MJPG".into(),
            width: 1920,
            height: 1080,
            fps: Rational {
                numerator: 30,
                denominator: 1,
            },
        };
        let (left, right, top, bottom) = crop_pixels(&output, &source);
        assert!(left > 0 && right > 0);
        assert_eq!(top, 0);
        assert_eq!(bottom, 0);
    }

    #[test]
    fn shared_metrics_report_recent_p95_latency_and_drops() {
        let telemetry = SharedOutputTelemetry::default();
        telemetry.frames.store(20, Ordering::Relaxed);
        telemetry.dropped_buffers.store(2, Ordering::Relaxed);
        telemetry
            .latency_ns
            .lock()
            .unwrap()
            .extend((1_u64..=20).map(|milliseconds| milliseconds * 1_000_000));

        let metrics = shared_output_metrics(&telemetry);
        assert_eq!(metrics.frames, 20);
        assert_eq!(metrics.dropped_buffers, 2);
        assert_eq!(metrics.latest_latency_us, 20_000);
        assert_eq!(metrics.average_latency_us, 10_500);
        assert_eq!(metrics.p95_latency_us, 19_000);
        assert_eq!(metrics.max_latency_us, 20_000);
    }

    #[test]
    fn shared_transform_enum_values_are_validated_without_panicking() {
        gst::init().unwrap();
        let flip = make("videoflip").unwrap();
        for method in [
            "none",
            "clockwise",
            "rotate-180",
            "counterclockwise",
            "horizontal-flip",
            "vertical-flip",
        ] {
            set_enum_property(&flip, "method", method).unwrap();
        }
        assert!(set_enum_property(&flip, "method", "invalid").is_err());
        let sink = make("v4l2sink").unwrap();
        set_enum_property(&sink, "io-mode", "mmap").unwrap();
    }
}
