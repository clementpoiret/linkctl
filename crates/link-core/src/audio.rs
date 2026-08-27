//! Stable audio discovery, control, processing, and statistics contracts.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Direction of one logical audio endpoint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioDirection {
    Capture,
    Playback,
}

/// Linux backend that exposes an audio transport or control.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioBackendKind {
    Alsa,
    Pipewire,
}

/// One backend-specific route to a logical endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioTransport {
    pub backend: AudioBackendKind,
    pub selector: String,
    pub numeric_id: Option<u32>,
}

/// A mixer capability discovered through ALSA/UAC.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioMixerControl {
    pub name: String,
    pub has_gain: bool,
    pub has_mute: bool,
    pub gain_min_raw: Option<i64>,
    pub gain_max_raw: Option<i64>,
}

/// One logical capture or playback endpoint, possibly exposed through several backends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioEndpoint {
    pub id: String,
    pub name: String,
    pub direction: AudioDirection,
    pub associated_camera: Option<String>,
    pub channels_min: Option<u32>,
    pub channels_max: Option<u32>,
    pub rate_min: Option<u32>,
    pub rate_max: Option<u32>,
    pub formats: Vec<String>,
    pub transports: Vec<AudioTransport>,
    pub mixer_controls: Vec<AudioMixerControl>,
    pub default: bool,
    pub busy: bool,
}

/// Complete system audio inventory.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct AudioInventory {
    pub endpoints: Vec<AudioEndpoint>,
    pub states: Vec<AudioEndpointState>,
    pub pipewire_compiled: bool,
    pub pipewire_available: bool,
    pub issues: Vec<String>,
}

/// Current control state associated with an endpoint in a device inventory.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioEndpointState {
    pub endpoint_id: String,
    pub hardware: Option<AudioControlState>,
    pub host: Option<AudioControlState>,
    pub effective_muted: bool,
}

/// Whether a gain or mute value is implemented by hardware or host session policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioControlLayer {
    Hardware,
    Host,
}

/// Gain and mute state for one control layer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioControlState {
    pub layer: AudioControlLayer,
    pub backend: AudioBackendKind,
    pub gain: Option<f64>,
    pub gain_raw: Option<i64>,
    pub gain_min_raw: Option<i64>,
    pub gain_max_raw: Option<i64>,
    pub muted: Option<bool>,
}

/// Selected source plus independently reported hardware and host state.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioStatus {
    pub source: AudioEndpoint,
    pub hardware: Option<AudioControlState>,
    pub host: Option<AudioControlState>,
    pub effective_muted: bool,
}

/// Transaction report for gain and mute changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioSetReport {
    pub field: String,
    pub layer: AudioControlLayer,
    pub previous: AudioControlState,
    pub requested: AudioControlState,
    pub observed: AudioControlState,
    pub dry_run: bool,
    pub verified: bool,
    pub rollback_succeeded: Option<bool>,
}

/// Optional host processing applied to an audio stream.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AudioProcessing {
    pub gate: bool,
    pub compressor: bool,
    pub limiter: bool,
}

/// One streaming peak/RMS observation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioLevelEvent {
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    pub clipped: bool,
    pub discontinuities: u64,
}

/// Counters and levels collected from an audio branch.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioStats {
    pub buffers: u64,
    pub bytes: u64,
    pub clipping_events: u64,
    pub timestamp_discontinuities: u64,
    pub dropped_samples: u64,
    pub added_samples: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    pub codec: Option<String>,
    pub processing: AudioProcessing,
}

/// Why a foreground audio operation stopped normally.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioStopReason {
    Completed,
    Interrupted,
    BrokenPipe,
}

/// Final report for standalone capture, metering, or monitoring.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AudioRunReport {
    pub source_id: String,
    pub stats: AudioStats,
    pub stop_reason: AudioStopReason,
    pub outputs: Vec<PathBuf>,
    pub finalized: bool,
}

/// Timestamp relationship observed between an audio branch and its video branch.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AvSyncStats {
    pub initial_offset_ms: f64,
    pub final_offset_ms: f64,
    pub max_abs_offset_ms: f64,
    pub drift_ms: f64,
    pub drift_ppm: f64,
    pub corrected: bool,
}
