use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gstreamer as gst;
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
    gst::init().map_err(|error| {
        LinkError::new(
            ErrorKind::MediaPipelineFailure,
            "failed to initialize GStreamer",
        )
        .with_detail("reason", error.to_string())
    })?;
    for element in required_elements {
        if gst::ElementFactory::find(element).is_none() {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "required GStreamer element is unavailable",
            )
            .with_detail("element", *element));
        }
    }
    install_signal_handler()?;
    Ok(())
}

/// A minimal no-output camera stream held in PLAYING for a bounded XU operation.
pub struct ProbeStream {
    pipeline: gst::Pipeline,
}

impl ProbeStream {
    /// Open the camera and wait for a minimal source pipeline to reach PLAYING.
    pub fn open(node: &str, timeout: Duration) -> Result<Self, LinkError> {
        initialize(&["v4l2src", "fakesink"])?;
        let pipeline = gst::Pipeline::new();
        let source = gst::ElementFactory::make("v4l2src")
            .property("device", node)
            .build()
            .map_err(build_error)?;
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .map_err(build_error)?;
        pipeline_add(&pipeline, &[&source, &sink])?;
        source.link(&sink).map_err(link_error)?;
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
    let mut required = vec!["v4l2src", "capsfilter", parser_name, "splitmuxsink", muxer];
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
    pipeline_add(&pipeline, &[&source, &filter, &parser, &sink])?;
    gst::Element::link_many([&source, &filter, &parser, &sink]).map_err(link_error)?;
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
        pipeline.add(&encoder).map_err(pipeline_error)?;
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
            gst::Element::link_many([&audio_tail, &convert, &filter, &encoder])
                .map_err(link_error)?;
        } else {
            audio_tail.link(&encoder).map_err(link_error)?;
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
    video_clock_bias_ns: Option<i128>,
    audio_clock_bias_ns: Option<i128>,
    first_pair_time_ns: Option<u64>,
    measurement_first_time_ns: Option<u64>,
    last_pair_time_ns: Option<u64>,
    raw_initial_offset_ns: Option<i128>,
    raw_final_offset_ns: Option<i128>,
    initial_offset_sum_ns: i128,
    initial_offset_samples: u32,
    recent_offsets_ns: VecDeque<i128>,
    raw_max_abs_offset_ns: u128,
    max_abs_offset_ns: u128,
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
                state.video_clock_bias_ns = Some(clock_bias);
            } else {
                state.audio_clock_bias_ns = Some(clock_bias);
            }
            if let (Some(video_bias), Some(audio_bias)) =
                (state.video_clock_bias_ns, state.audio_clock_bias_ns)
            {
                let offset = audio_bias - video_bias;
                let first_pair = *state.first_pair_time_ns.get_or_insert(elapsed);
                state.raw_initial_offset_ns.get_or_insert(offset);
                state.raw_final_offset_ns = Some(offset);
                state.raw_max_abs_offset_ns =
                    state.raw_max_abs_offset_ns.max(offset.unsigned_abs());
                if elapsed.saturating_sub(first_pair) < AV_SYNC_WARMUP_NS {
                    return gst::PadProbeReturn::Ok;
                }
                state.measurement_first_time_ns.get_or_insert(elapsed);
                state.last_pair_time_ns = Some(elapsed);
                if state.initial_offset_samples < AV_SYNC_WINDOW_SAMPLES as u32 {
                    state.initial_offset_sum_ns += offset;
                    state.initial_offset_samples += 1;
                }
                state.recent_offsets_ns.push_back(offset);
                if state.recent_offsets_ns.len() > AV_SYNC_WINDOW_SAMPLES {
                    state.recent_offsets_ns.pop_front();
                }
                state.max_abs_offset_ns = state.max_abs_offset_ns.max(offset.unsigned_abs());
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
    let measured = state.initial_offset_samples > 0 && !state.recent_offsets_ns.is_empty();
    let initial = if measured {
        state.initial_offset_sum_ns / i128::from(state.initial_offset_samples)
    } else {
        state.raw_initial_offset_ns.unwrap_or_default()
    };
    let final_offset = if measured {
        state.recent_offsets_ns.iter().sum::<i128>() / state.recent_offsets_ns.len() as i128
    } else {
        state.raw_final_offset_ns.unwrap_or(initial)
    };
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
    use std::sync::{Arc, Mutex};

    use link_core::audio::AudioProcessing;

    use super::{AvSyncState, audio_processing_elements, finish_av_sync, parse_byte_size};

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
}
