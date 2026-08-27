//! ALSA/PipeWire audio discovery and ordinary gain/mute controls.

#[cfg(feature = "pipewire")]
use std::collections::BTreeMap;

use alsa::{
    Direction, card,
    ctl::{Ctl, DeviceIter},
    mixer::{Mixer, Selem, SelemChannelId},
    pcm::{Access, Format, HwParams, PCM},
};
use link_core::{
    ErrorKind, LinkError,
    audio::{
        AudioBackendKind, AudioControlLayer, AudioControlState, AudioDirection, AudioEndpoint,
        AudioEndpointState, AudioInventory, AudioMixerControl, AudioSetReport, AudioStatus,
        AudioTransport,
    },
    probe::{AlsaPcmReport, AudioReport, ProbeIssue, UsbIdentity},
};
use sha2::{Digest, Sha256};

/// Camera identity fields needed to correlate audio endpoints without depending on `link-linux`.
#[derive(Clone, Debug)]
pub struct CameraAudioAssociation {
    pub stable_id: String,
    pub usb: UsbIdentity,
    pub card_indexes: Vec<i32>,
}

/// Enumerate all logical capture and playback endpoints and correlate camera microphones.
pub fn inventory(associations: &[CameraAudioAssociation]) -> Result<AudioInventory, LinkError> {
    let mut result = AudioInventory {
        pipewire_compiled: cfg!(feature = "pipewire"),
        ..AudioInventory::default()
    };
    let mut endpoints = Vec::new();
    for card in card::Iter::new() {
        let card = card.map_err(alsa_error("failed to enumerate ALSA cards"))?;
        let card_index = card.get_index();
        let card_name = card
            .get_name()
            .unwrap_or_else(|_| format!("ALSA card {card_index}"));
        let card_longname = card.get_longname().unwrap_or_else(|_| card_name.clone());
        let control_name = format!("hw:{card_index}");
        let control = match Ctl::new(&control_name, true) {
            Ok(control) => control,
            Err(error) => {
                result.issues.push(format!(
                    "could not open ALSA control {control_name}: {error}"
                ));
                continue;
            }
        };
        let association = associations
            .iter()
            .find(|association| association.card_indexes.contains(&card_index))
            .map(|association| association.stable_id.clone());
        for device in DeviceIter::new(&control) {
            for (alsa_direction, direction) in [
                (Direction::Capture, AudioDirection::Capture),
                (Direction::Playback, AudioDirection::Playback),
            ] {
                let Ok(device_u32) = u32::try_from(device) else {
                    continue;
                };
                let Ok(info) = control.pcm_info(device_u32, 0, alsa_direction) else {
                    continue;
                };
                let pcm_name = format!("hw:{card_index},{device}");
                let display_name = info
                    .get_name()
                    .map_or_else(|_| card_name.clone(), str::to_owned);
                let (ranges, formats, busy) = inspect_pcm(&pcm_name, alsa_direction);
                let mixer_controls = mixer_controls(card_index, direction).unwrap_or_default();
                endpoints.push(AudioEndpoint {
                    id: stable_endpoint_id(&card_longname, device, direction),
                    name: if display_name == "USB Audio" {
                        card_name.clone()
                    } else {
                        display_name
                    },
                    direction,
                    associated_camera: association.clone(),
                    channels_min: ranges.map(|value| value.0),
                    channels_max: ranges.map(|value| value.1),
                    rate_min: ranges.map(|value| value.2),
                    rate_max: ranges.map(|value| value.3),
                    formats,
                    transports: vec![AudioTransport {
                        backend: AudioBackendKind::Alsa,
                        selector: pcm_name,
                        numeric_id: None,
                    }],
                    mixer_controls,
                    default: false,
                    busy,
                });
            }
        }
    }

    #[cfg(feature = "pipewire")]
    match pipewire_system_inventory() {
        Ok(pipewire) => {
            result.pipewire_available = true;
            merge_pipewire(&mut endpoints, pipewire);
        }
        Err(error) => result
            .issues
            .push(format!("PipeWire registry is unavailable: {error}")),
    }

    endpoints.sort_by(|left, right| {
        left.direction
            .cmp(&right.direction)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    for endpoint in endpoints
        .iter()
        .filter(|endpoint| endpoint.direction == AudioDirection::Capture)
    {
        match status(endpoint) {
            Ok(status) => result.states.push(AudioEndpointState {
                endpoint_id: endpoint.id.clone(),
                hardware: status.hardware,
                host: status.host,
                effective_muted: status.effective_muted,
            }),
            Err(error) => result.issues.push(format!(
                "could not read gain/mute state for {}: {}",
                endpoint.id, error
            )),
        }
    }
    result.endpoints = endpoints;
    Ok(result)
}

/// Resolve a capture source by stable ID, `camera`, or an explicit backend selector.
pub fn resolve_capture_source(
    inventory: &AudioInventory,
    selector: Option<&str>,
    camera: Option<&str>,
) -> Result<AudioEndpoint, LinkError> {
    let selector = selector.unwrap_or("camera");
    let candidates = inventory
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.direction == AudioDirection::Capture)
        .filter(|endpoint| {
            if selector == "camera" {
                camera.is_some_and(|camera| endpoint.associated_camera.as_deref() == Some(camera))
            } else if endpoint.id == selector {
                true
            } else if let Some(value) = selector.strip_prefix("alsa:") {
                endpoint.transports.iter().any(|transport| {
                    transport.backend == AudioBackendKind::Alsa && transport.selector == value
                })
            } else if let Some(value) = selector.strip_prefix("pipewire:") {
                endpoint.transports.iter().any(|transport| {
                    transport.backend == AudioBackendKind::Pipewire && transport.selector == value
                })
            } else {
                false
            }
        })
        .cloned()
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [endpoint] => Ok(endpoint.clone()),
        [] if selector == "camera" && camera.is_none() => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "camera audio selection requires one selected camera",
        )),
        [] => Err(LinkError::new(
            ErrorKind::DeviceNotFound,
            "audio capture source was not found",
        )
        .with_detail("source", selector.to_owned())),
        _ => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio capture source selector is ambiguous",
        )
        .with_detail("source", selector.to_owned())
        .with_detail("matches", candidates.len() as u64)),
    }
}

/// Read hardware and host-session gain/mute state without conflating the layers.
pub fn status(endpoint: &AudioEndpoint) -> Result<AudioStatus, LinkError> {
    let hardware = hardware_state(endpoint)?;
    #[cfg(feature = "pipewire")]
    let host = pipewire_control_state(endpoint)?;
    #[cfg(not(feature = "pipewire"))]
    let host: Option<AudioControlState> = None;
    let effective_muted = hardware
        .as_ref()
        .and_then(|state| state.muted)
        .unwrap_or(false)
        || host.as_ref().and_then(|state| state.muted).unwrap_or(false);
    Ok(AudioStatus {
        source: endpoint.clone(),
        hardware,
        host,
        effective_muted,
    })
}

/// Set normalized capture gain through hardware or host policy with readback and rollback.
pub fn set_gain(
    endpoint: &AudioEndpoint,
    layer: AudioControlLayer,
    gain: f64,
    dry_run: bool,
) -> Result<AudioSetReport, LinkError> {
    if !(0.0..=1.0).contains(&gain) || !gain.is_finite() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "audio gain must be between 0% and 100%",
        ));
    }
    match layer {
        AudioControlLayer::Hardware => set_hardware(endpoint, Some(gain), None, dry_run),
        AudioControlLayer::Host => {
            #[cfg(feature = "pipewire")]
            {
                set_pipewire_control(endpoint, Some(gain), None, dry_run)
            }
            #[cfg(not(feature = "pipewire"))]
            {
                let _ = (endpoint, dry_run);
                Err(host_unavailable())
            }
        }
    }
}

/// Set capture mute through hardware or host policy with readback and rollback.
pub fn set_mute(
    endpoint: &AudioEndpoint,
    layer: AudioControlLayer,
    muted: bool,
    dry_run: bool,
) -> Result<AudioSetReport, LinkError> {
    match layer {
        AudioControlLayer::Hardware => set_hardware(endpoint, None, Some(muted), dry_run),
        AudioControlLayer::Host => {
            #[cfg(feature = "pipewire")]
            {
                set_pipewire_control(endpoint, None, Some(muted), dry_run)
            }
            #[cfg(not(feature = "pipewire"))]
            {
                let _ = (endpoint, muted, dry_run);
                Err(host_unavailable())
            }
        }
    }
}

/// Inventory the capture PCMs and PipeWire objects associated with one USB device.
#[must_use]
pub fn probe(card_indexes: &[i32], usb: &UsbIdentity) -> AudioReport {
    let mut report = AudioReport {
        pipewire_compiled: cfg!(feature = "pipewire"),
        ..AudioReport::default()
    };

    if card_indexes.is_empty() {
        report.issues.push(issue(
            "no-associated-alsa-card",
            "udev did not associate an ALSA card with this USB device",
        ));
    }
    for &card in card_indexes {
        enumerate_alsa_card(card, &mut report);
    }

    #[cfg(feature = "pipewire")]
    match pipewire_inventory(usb) {
        Ok(objects) => {
            report.pipewire_available = true;
            report.pipewire = objects;
        }
        Err(message) => report.issues.push(issue("pipewire-unavailable", message)),
    }

    #[cfg(not(feature = "pipewire"))]
    let _ = usb;

    report
}

fn enumerate_alsa_card(card: i32, report: &mut AudioReport) {
    let control_name = format!("hw:{card}");
    let control = match Ctl::new(&control_name, true) {
        Ok(control) => control,
        Err(error) => {
            report.issues.push(issue(
                "alsa-control-open-failed",
                format!("could not open ALSA control {control_name}: {error}"),
            ));
            return;
        }
    };

    for device in DeviceIter::new(&control) {
        let Ok(device_u32) = u32::try_from(device) else {
            continue;
        };
        let info = match control.pcm_info(device_u32, 0, Direction::Capture) {
            Ok(info) => info,
            Err(_) => continue,
        };
        let name = format!("hw:{card},{device}");
        let description = info
            .get_name()
            .unwrap_or("unknown ALSA capture PCM")
            .to_owned();
        let pcm = match PCM::new(&name, Direction::Capture, true) {
            Ok(pcm) => pcm,
            Err(error) => {
                report.issues.push(issue(
                    "alsa-pcm-open-failed",
                    format!(
                        "could not open camera capture PCM {name} for capability discovery: {error}"
                    ),
                ));
                continue;
            }
        };
        let params = match HwParams::any(&pcm) {
            Ok(params) => params,
            Err(error) => {
                report.issues.push(issue(
                    "alsa-hardware-parameters-failed",
                    format!("could not query hardware parameters for {name}: {error}"),
                ));
                continue;
            }
        };

        let Some((channels_min, channels_max, rate_min, rate_max)) =
            parameter_ranges(&params, &name, &mut report.issues)
        else {
            continue;
        };
        report.alsa.push(AlsaPcmReport {
            card,
            device,
            name,
            description,
            channels_min,
            channels_max,
            rate_min,
            rate_max,
            formats: supported_formats(&params),
            access_modes: supported_access_modes(&params),
        });
    }
}

fn parameter_ranges(
    params: &HwParams<'_>,
    name: &str,
    issues: &mut Vec<ProbeIssue>,
) -> Option<(u32, u32, u32, u32)> {
    match (
        params.get_channels_min(),
        params.get_channels_max(),
        params.get_rate_min(),
        params.get_rate_max(),
    ) {
        (Ok(channels_min), Ok(channels_max), Ok(rate_min), Ok(rate_max)) => {
            Some((channels_min, channels_max, rate_min, rate_max))
        }
        values => {
            issues.push(issue(
                "alsa-range-query-failed",
                format!("could not read all channel/rate ranges for {name}: {values:?}"),
            ));
            None
        }
    }
}

fn supported_formats(params: &HwParams<'_>) -> Vec<String> {
    [
        (Format::S8, "S8"),
        (Format::U8, "U8"),
        (Format::S16LE, "S16_LE"),
        (Format::S16BE, "S16_BE"),
        (Format::S243LE, "S24_3LE"),
        (Format::S243BE, "S24_3BE"),
        (Format::S24LE, "S24_LE"),
        (Format::S24BE, "S24_BE"),
        (Format::S32LE, "S32_LE"),
        (Format::S32BE, "S32_BE"),
        (Format::FloatLE, "FLOAT_LE"),
        (Format::FloatBE, "FLOAT_BE"),
    ]
    .into_iter()
    .filter(|(format, _)| params.test_format(*format).is_ok())
    .map(|(_, name)| name.to_owned())
    .collect()
}

fn supported_access_modes(params: &HwParams<'_>) -> Vec<String> {
    [
        (Access::MMapInterleaved, "mmap-interleaved"),
        (Access::MMapNonInterleaved, "mmap-noninterleaved"),
        (Access::MMapComplex, "mmap-complex"),
        (Access::RWInterleaved, "rw-interleaved"),
        (Access::RWNonInterleaved, "rw-noninterleaved"),
    ]
    .into_iter()
    .filter(|(access, _)| params.test_access(*access).is_ok())
    .map(|(_, name)| name.to_owned())
    .collect()
}

fn issue(code: &str, message: impl Into<String>) -> ProbeIssue {
    ProbeIssue::new("audio", code, message)
}

#[cfg(feature = "pipewire")]
fn pipewire_inventory(
    usb: &UsbIdentity,
) -> Result<Vec<link_core::probe::PipeWireObjectReport>, String> {
    use std::{
        cell::{Cell, RefCell},
        collections::{BTreeMap, BTreeSet},
        rc::Rc,
    };

    use link_core::probe::PipeWireObjectReport;
    use pipewire as pw;

    #[derive(Clone)]
    struct Candidate {
        id: u32,
        object_type: String,
        properties: BTreeMap<String, String>,
    }

    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(|error| error.to_string())?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).map_err(|error| error.to_string())?;
    let core = context
        .connect_rc(None)
        .map_err(|error| error.to_string())?;
    let registry = core.get_registry_rc().map_err(|error| error.to_string())?;
    let candidates = Rc::new(RefCell::new(Vec::<Candidate>::new()));
    let candidates_for_callback = Rc::clone(&candidates);
    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let properties = global.props.map_or_else(BTreeMap::new, selected_properties);
            candidates_for_callback.borrow_mut().push(Candidate {
                id: global.id,
                object_type: global.type_.to_string(),
                properties,
            });
        })
        .register();

    let finished = Rc::new(Cell::new(false));
    let server_error = Rc::new(RefCell::new(None::<String>));
    let pending = core.sync(0).map_err(|error| error.to_string())?;
    let loop_for_done = main_loop.clone();
    let finished_for_done = Rc::clone(&finished);
    let loop_for_error = main_loop.clone();
    let error_for_callback = Rc::clone(&server_error);
    let core_listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            if id == pw::core::PW_ID_CORE && sequence == pending {
                finished_for_done.set(true);
                loop_for_done.quit();
            }
        })
        .error(move |id, sequence, result, message| {
            *error_for_callback.borrow_mut() = Some(format!(
                "PipeWire core error id={id} sequence={sequence:?} result={result}: {message}"
            ));
            loop_for_error.quit();
        })
        .register();

    while !finished.get() && server_error.borrow().is_none() {
        main_loop.run();
    }
    drop(core_listener);
    drop(registry_listener);
    if let Some(error) = server_error.take() {
        return Err(error);
    }

    let candidates = candidates.borrow();
    let component = format!("usb{:04x}:{:04x}", usb.vendor_id, usb.product_id);
    let associated_device_ids: BTreeSet<String> = candidates
        .iter()
        .filter(|candidate| directly_matches(&candidate.properties, usb, &component))
        .map(|candidate| candidate.id.to_string())
        .collect();

    let mut reports = candidates
        .iter()
        .filter(|candidate| {
            let media_class = candidate
                .properties
                .get("media.class")
                .map(String::as_str)
                .unwrap_or_default();
            let direct = directly_matches(&candidate.properties, usb, &component);
            let referenced = candidate
                .properties
                .get("device.id")
                .is_some_and(|id| associated_device_ids.contains(id));
            (direct || referenced) && media_class.starts_with("Audio/")
        })
        .map(|candidate| PipeWireObjectReport {
            id: candidate.id,
            object_type: candidate.object_type.clone(),
            properties: candidate.properties.clone(),
        })
        .collect::<Vec<_>>();
    reports.sort_by_key(|report| report.id);
    Ok(reports)
}

#[cfg(feature = "pipewire")]
fn selected_properties(
    properties: &pipewire::spa::utils::dict::DictRef,
) -> std::collections::BTreeMap<String, String> {
    const KEYS: &[&str] = &[
        "alsa.components",
        "alsa.card",
        "alsa.device",
        "alsa.card_name",
        "alsa.long_card_name",
        "api.alsa.card",
        "api.alsa.card.longname",
        "api.alsa.card.name",
        "api.alsa.pcm.card",
        "api.alsa.pcm.device",
        "api.alsa.pcm.stream",
        "audio.channels",
        "audio.format",
        "audio.position",
        "audio.rate",
        "device.bus-id",
        "device.description",
        "device.id",
        "device.name",
        "device.nick",
        "device.product.id",
        "device.product.name",
        "device.vendor.id",
        "device.vendor.name",
        "media.class",
        "media.name",
        "metadata.name",
        "node.description",
        "node.name",
        "node.nick",
    ];
    properties
        .iter()
        .filter(|(key, _)| KEYS.contains(key))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

type PcmRanges = (u32, u32, u32, u32);
type PcmInspection = (Option<PcmRanges>, Vec<String>, bool);

fn inspect_pcm(name: &str, direction: Direction) -> PcmInspection {
    let pcm = match PCM::new(name, direction, true) {
        Ok(pcm) => pcm,
        Err(error) => return (None, Vec::new(), error.errno() == linux_errno_ebusy()),
    };
    let Ok(params) = HwParams::any(&pcm) else {
        return (None, Vec::new(), false);
    };
    let ranges = match (
        params.get_channels_min(),
        params.get_channels_max(),
        params.get_rate_min(),
        params.get_rate_max(),
    ) {
        (Ok(channels_min), Ok(channels_max), Ok(rate_min), Ok(rate_max)) => {
            Some((channels_min, channels_max, rate_min, rate_max))
        }
        _ => None,
    };
    (ranges, supported_formats(&params), false)
}

const fn linux_errno_ebusy() -> i32 {
    16
}

fn stable_endpoint_id(card: &str, device: i32, direction: AudioDirection) -> String {
    let mut digest = Sha256::new();
    digest.update(card.as_bytes());
    digest.update(device.to_le_bytes());
    digest.update(match direction {
        AudioDirection::Capture => b"capture".as_slice(),
        AudioDirection::Playback => b"playback".as_slice(),
    });
    let digest = digest.finalize();
    let prefix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("audio-{prefix}")
}

fn mixer_controls(
    card: i32,
    direction: AudioDirection,
) -> Result<Vec<AudioMixerControl>, LinkError> {
    let mixer =
        Mixer::new(&format!("hw:{card}"), true).map_err(alsa_error("failed to open ALSA mixer"))?;
    let mut controls = mixer
        .iter()
        .filter_map(Selem::new)
        .filter_map(|element| {
            let id = element.get_id();
            let name = id.get_name().ok()?.to_owned();
            let (has_gain, has_mute, range) = match direction {
                AudioDirection::Capture => (
                    element.has_capture_volume(),
                    element.has_capture_switch(),
                    element
                        .has_capture_volume()
                        .then(|| element.get_capture_volume_range()),
                ),
                AudioDirection::Playback => (
                    element.has_playback_volume(),
                    element.has_playback_switch(),
                    element
                        .has_playback_volume()
                        .then(|| element.get_playback_volume_range()),
                ),
            };
            (has_gain || has_mute).then_some(AudioMixerControl {
                name,
                has_gain,
                has_mute,
                gain_min_raw: range.map(|value| value.0),
                gain_max_raw: range.map(|value| value.1),
            })
        })
        .collect::<Vec<_>>();
    controls.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(controls)
}

fn hardware_state(endpoint: &AudioEndpoint) -> Result<Option<AudioControlState>, LinkError> {
    let Some(card) = alsa_card(endpoint) else {
        return Ok(None);
    };
    let mixer =
        Mixer::new(&format!("hw:{card}"), true).map_err(alsa_error("failed to open ALSA mixer"))?;
    let element = select_capture_element(&mixer, false, false)?;
    read_capture_state(&element).map(Some)
}

fn set_hardware(
    endpoint: &AudioEndpoint,
    gain: Option<f64>,
    muted: Option<bool>,
    dry_run: bool,
) -> Result<AudioSetReport, LinkError> {
    let card = alsa_card(endpoint).ok_or_else(|| {
        LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "selected source has no ALSA hardware mixer",
        )
    })?;
    let mixer =
        Mixer::new(&format!("hw:{card}"), true).map_err(alsa_error("failed to open ALSA mixer"))?;
    let element = select_capture_element(&mixer, gain.is_some(), muted.is_some())?;
    let previous = read_capture_state(&element)?;
    let mut requested = previous.clone();
    let field = if let Some(gain) = gain {
        let (min, max) = element.get_capture_volume_range();
        let raw = normalized_to_raw(gain, min, max);
        requested.gain = Some(gain);
        requested.gain_raw = Some(raw);
        "gain"
    } else if let Some(muted) = muted {
        requested.muted = Some(muted);
        "mute"
    } else {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "no audio hardware change was requested",
        ));
    };
    if dry_run {
        return Ok(AudioSetReport {
            field: field.into(),
            layer: AudioControlLayer::Hardware,
            previous: previous.clone(),
            requested,
            observed: previous,
            dry_run: true,
            verified: false,
            rollback_succeeded: None,
        });
    }
    if let Some(raw) = requested.gain_raw.filter(|_| gain.is_some()) {
        element
            .set_capture_volume_all(raw)
            .map_err(alsa_error("failed to set ALSA capture gain"))?;
    }
    if let Some(muted) = muted {
        element
            .set_capture_switch_all(i32::from(!muted))
            .map_err(alsa_error("failed to set ALSA capture mute"))?;
    }
    let observed = read_capture_state(&element)?;
    let verified = gain.is_none_or(|_| observed.gain_raw == requested.gain_raw)
        && muted.is_none_or(|value| observed.muted == Some(value));
    if !verified {
        let rollback_succeeded = restore_capture_state(&element, &previous).is_ok()
            && read_capture_state(&element).is_ok_and(|state| {
                state.gain_raw == previous.gain_raw && state.muted == previous.muted
            });
        return Err(LinkError::new(
            ErrorKind::PartialSuccess,
            "audio hardware readback did not match the requested value",
        )
        .with_detail("rollback_succeeded", rollback_succeeded));
    }
    Ok(AudioSetReport {
        field: field.into(),
        layer: AudioControlLayer::Hardware,
        previous,
        requested,
        observed,
        dry_run: false,
        verified,
        rollback_succeeded: None,
    })
}

fn alsa_card(endpoint: &AudioEndpoint) -> Option<i32> {
    endpoint
        .transports
        .iter()
        .find(|transport| transport.backend == AudioBackendKind::Alsa)
        .and_then(|transport| transport.selector.strip_prefix("hw:"))
        .and_then(|selector| {
            selector
                .split_once(',')
                .map_or(Some(selector), |value| Some(value.0))
        })
        .and_then(|card| card.parse().ok())
}

fn select_capture_element<'a>(
    mixer: &'a Mixer,
    need_gain: bool,
    need_mute: bool,
) -> Result<Selem<'a>, LinkError> {
    let mut candidates = mixer
        .iter()
        .filter_map(Selem::new)
        .filter(|element| {
            element.can_capture()
                && (!need_gain || element.has_capture_volume())
                && (!need_mute || element.has_capture_switch())
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|element| {
        let name = element
            .get_id()
            .get_name()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match name.as_str() {
            "mic" => 0,
            "capture" => 1,
            _ => 2,
        }
    });
    if candidates.is_empty() {
        Err(LinkError::new(
            ErrorKind::CapabilityUnsupported,
            "selected source has no matching ALSA capture mixer control",
        ))
    } else {
        Ok(candidates.remove(0))
    }
}

fn read_capture_state(element: &Selem<'_>) -> Result<AudioControlState, LinkError> {
    let channel = SelemChannelId::mono();
    let (gain, gain_raw, min, max) = if element.has_capture_volume() {
        let raw = element
            .get_capture_volume(channel)
            .map_err(alsa_error("failed to read ALSA capture gain"))?;
        let (min, max) = element.get_capture_volume_range();
        (
            Some(raw_to_normalized(raw, min, max)),
            Some(raw),
            Some(min),
            Some(max),
        )
    } else {
        (None, None, None, None)
    };
    let muted = element
        .has_capture_switch()
        .then(|| {
            element
                .get_capture_switch(channel)
                .map(|value| value == 0)
                .map_err(alsa_error("failed to read ALSA capture mute"))
        })
        .transpose()?;
    Ok(AudioControlState {
        layer: AudioControlLayer::Hardware,
        backend: AudioBackendKind::Alsa,
        gain,
        gain_raw,
        gain_min_raw: min,
        gain_max_raw: max,
        muted,
    })
}

fn restore_capture_state(
    element: &Selem<'_>,
    previous: &AudioControlState,
) -> Result<(), LinkError> {
    if let Some(raw) = previous.gain_raw {
        element
            .set_capture_volume_all(raw)
            .map_err(alsa_error("failed to restore ALSA capture gain"))?;
    }
    if let Some(muted) = previous.muted {
        element
            .set_capture_switch_all(i32::from(!muted))
            .map_err(alsa_error("failed to restore ALSA capture mute"))?;
    }
    Ok(())
}

fn raw_to_normalized(raw: i64, min: i64, max: i64) -> f64 {
    if max == min {
        return 0.0;
    }
    ((raw - min) as f64 / (max - min) as f64).clamp(0.0, 1.0)
}

fn normalized_to_raw(value: f64, min: i64, max: i64) -> i64 {
    min + ((max - min) as f64 * value).round() as i64
}

fn alsa_error(message: &'static str) -> impl Fn(alsa::Error) -> LinkError {
    move |error| {
        let kind = match error.errno() {
            13 | 1 => ErrorKind::PermissionDenied,
            16 => ErrorKind::DeviceBusy,
            2 | 19 => ErrorKind::DeviceNotFound,
            _ => ErrorKind::IoFailure,
        };
        LinkError::new(kind, message).with_detail("reason", error.to_string())
    }
}

fn host_unavailable() -> LinkError {
    LinkError::new(
        ErrorKind::CapabilityUnsupported,
        "PipeWire host gain/mute is unavailable for the selected source",
    )
}

#[cfg(feature = "pipewire")]
#[derive(Clone)]
struct PipeWireEndpoint {
    id: u32,
    properties: BTreeMap<String, String>,
    default: bool,
}

#[cfg(feature = "pipewire")]
fn pipewire_system_inventory() -> Result<Vec<PipeWireEndpoint>, String> {
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeSet,
        rc::Rc,
    };

    use pipewire as pw;

    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(|error| error.to_string())?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).map_err(|error| error.to_string())?;
    let core = context
        .connect_rc(None)
        .map_err(|error| error.to_string())?;
    let registry = core.get_registry_rc().map_err(|error| error.to_string())?;
    let endpoints = Rc::new(RefCell::new(Vec::<PipeWireEndpoint>::new()));
    let defaults = Rc::new(RefCell::new(BTreeSet::<String>::new()));
    let metadata_holders = Rc::new(RefCell::new(Vec::<pw::metadata::Metadata>::new()));
    let metadata_listener_holders =
        Rc::new(RefCell::new(Vec::<pw::metadata::MetadataListener>::new()));
    let pending = Rc::new(Cell::new(core.sync(0).map_err(|error| error.to_string())?));
    let endpoints_for_callback = Rc::clone(&endpoints);
    let defaults_for_callback = Rc::clone(&defaults);
    let metadata_for_callback = Rc::clone(&metadata_holders);
    let metadata_listeners_for_callback = Rc::clone(&metadata_listener_holders);
    let pending_for_callback = Rc::clone(&pending);
    let core_for_callback = core.clone();
    let registry_weak = registry.downgrade();
    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let properties = global.props.map_or_else(BTreeMap::new, selected_properties);
            let media_class = properties.get("media.class").map(String::as_str);
            if matches!(media_class, Some("Audio/Source" | "Audio/Sink")) {
                endpoints_for_callback.borrow_mut().push(PipeWireEndpoint {
                    id: global.id,
                    properties,
                    default: false,
                });
            } else if global.type_ == pw::types::ObjectType::Metadata
                && global
                    .props
                    .as_ref()
                    .and_then(|props| props.get("metadata.name"))
                    == Some("default")
                && let Some(registry) = registry_weak.upgrade()
                && let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(global)
            {
                let defaults_for_property = Rc::clone(&defaults_for_callback);
                let listener = metadata
                    .add_listener_local()
                    .property(move |_subject, key, _type, value| {
                        if matches!(key, Some("default.audio.source" | "default.audio.sink"))
                            && let Some(value) = value
                            && let Ok(value) = serde_json::from_str::<serde_json::Value>(value)
                            && let Some(name) = value.get("name").and_then(|value| value.as_str())
                        {
                            defaults_for_property.borrow_mut().insert(name.to_owned());
                        }
                        0
                    })
                    .register();
                metadata_for_callback.borrow_mut().push(metadata);
                metadata_listeners_for_callback.borrow_mut().push(listener);
                if let Ok(sequence) = core_for_callback.sync(0) {
                    pending_for_callback.set(sequence);
                }
            }
        })
        .register();

    let finished = Rc::new(Cell::new(false));
    let server_error = Rc::new(RefCell::new(None::<String>));
    let pending_for_done = Rc::clone(&pending);
    let loop_for_done = main_loop.clone();
    let finished_for_done = Rc::clone(&finished);
    let loop_for_error = main_loop.clone();
    let error_for_callback = Rc::clone(&server_error);
    let core_listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            if id == pw::core::PW_ID_CORE && sequence == pending_for_done.get() {
                finished_for_done.set(true);
                loop_for_done.quit();
            }
        })
        .error(move |id, sequence, result, message| {
            *error_for_callback.borrow_mut() = Some(format!(
                "PipeWire core error id={id} sequence={sequence:?} result={result}: {message}"
            ));
            loop_for_error.quit();
        })
        .register();
    while !finished.get() && server_error.borrow().is_none() {
        main_loop.run();
    }
    drop(core_listener);
    drop(registry_listener);
    if let Some(error) = server_error.take() {
        return Err(error);
    }
    let defaults = defaults.borrow();
    let mut discovered = endpoints.borrow().clone();
    for endpoint in &mut discovered {
        endpoint.default = endpoint
            .properties
            .get("node.name")
            .is_some_and(|name| defaults.contains(name));
    }
    Ok(discovered)
}

#[cfg(feature = "pipewire")]
fn merge_pipewire(endpoints: &mut Vec<AudioEndpoint>, pipewire: Vec<PipeWireEndpoint>) {
    for candidate in pipewire {
        let direction = match candidate.properties.get("media.class").map(String::as_str) {
            Some("Audio/Source") => AudioDirection::Capture,
            Some("Audio/Sink") => AudioDirection::Playback,
            _ => continue,
        };
        let card = candidate
            .properties
            .get("api.alsa.pcm.card")
            .or_else(|| candidate.properties.get("alsa.card"))
            .and_then(|value| value.parse::<i32>().ok());
        let device = candidate
            .properties
            .get("api.alsa.pcm.device")
            .or_else(|| candidate.properties.get("alsa.device"))
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        let matching_alsa = card.map(|card| format!("hw:{card},{device}"));
        let node_name = candidate
            .properties
            .get("node.name")
            .cloned()
            .unwrap_or_else(|| candidate.id.to_string());
        let transport = AudioTransport {
            backend: AudioBackendKind::Pipewire,
            selector: node_name.clone(),
            numeric_id: Some(candidate.id),
        };
        let card_name = candidate
            .properties
            .get("api.alsa.card.name")
            .or_else(|| candidate.properties.get("alsa.card_name"))
            .or_else(|| candidate.properties.get("node.nick"))
            .map(|name| normalized_name(name));
        if let Some(endpoint) = endpoints.iter_mut().find(|endpoint| {
            if endpoint.direction != direction
                || !endpoint
                    .transports
                    .iter()
                    .any(|transport| transport.backend == AudioBackendKind::Alsa)
            {
                return false;
            }
            matching_alsa.as_ref().is_some_and(|alsa| {
                endpoint
                    .transports
                    .iter()
                    .any(|transport| &transport.selector == alsa)
            }) || card_name
                .as_ref()
                .is_some_and(|card_name| normalized_name(&endpoint.name) == *card_name)
        }) {
            endpoint.transports.push(transport);
            endpoint.default |= candidate.default;
            if let Some(channels) = property_u32(&candidate.properties, "audio.channels") {
                endpoint.channels_min.get_or_insert(channels);
                endpoint.channels_max.get_or_insert(channels);
            }
            if let Some(rate) = property_u32(&candidate.properties, "audio.rate") {
                endpoint.rate_min.get_or_insert(rate);
                endpoint.rate_max.get_or_insert(rate);
            }
            if let Some(format) = candidate.properties.get("audio.format")
                && !endpoint.formats.contains(format)
            {
                endpoint.formats.push(format.clone());
            }
            continue;
        }
        let display_name = candidate
            .properties
            .get("node.description")
            .or_else(|| candidate.properties.get("node.nick"))
            .cloned()
            .unwrap_or_else(|| node_name.clone());
        let channels = property_u32(&candidate.properties, "audio.channels");
        let rate = property_u32(&candidate.properties, "audio.rate");
        endpoints.push(AudioEndpoint {
            id: stable_endpoint_id(&node_name, 0, direction),
            name: display_name,
            direction,
            associated_camera: None,
            channels_min: channels,
            channels_max: channels,
            rate_min: rate,
            rate_max: rate,
            formats: candidate
                .properties
                .get("audio.format")
                .cloned()
                .into_iter()
                .collect(),
            transports: vec![transport],
            mixer_controls: Vec::new(),
            default: candidate.default,
            busy: false,
        });
    }
}

#[cfg(feature = "pipewire")]
fn property_u32(properties: &BTreeMap<String, String>, key: &str) -> Option<u32> {
    properties.get(key).and_then(|value| value.parse().ok())
}

#[cfg(feature = "pipewire")]
fn pipewire_control_state(
    endpoint: &AudioEndpoint,
) -> Result<Option<AudioControlState>, LinkError> {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use pipewire as pw;
    use pw::spa::{param::ParamType, pod::Value, pod::deserialize::PodDeserializer};

    let Some(selector) = endpoint
        .transports
        .iter()
        .find(|transport| transport.backend == AudioBackendKind::Pipewire)
        .map(|transport| transport.selector.clone())
    else {
        return Ok(None);
    };
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(pipewire_error)?;
    let context = pw::context::ContextRc::new(&main_loop, None).map_err(pipewire_error)?;
    let core = context.connect_rc(None).map_err(pipewire_error)?;
    let registry = core.get_registry_rc().map_err(pipewire_error)?;
    let pending = Rc::new(Cell::new(core.sync(0).map_err(pipewire_error)?));
    let found = Rc::new(Cell::new(false));
    let values = Rc::new(RefCell::new((None::<f64>, None::<bool>)));
    let failure = Rc::new(RefCell::new(None::<String>));
    let node_holder = Rc::new(RefCell::new(None::<pw::node::Node>));
    let node_listener_holder = Rc::new(RefCell::new(None::<pw::node::NodeListener>));
    let registry_weak = registry.downgrade();
    let selector_for_callback = selector.clone();
    let found_for_callback = Rc::clone(&found);
    let values_for_callback = Rc::clone(&values);
    let failure_for_callback = Rc::clone(&failure);
    let node_for_callback = Rc::clone(&node_holder);
    let listener_for_callback = Rc::clone(&node_listener_holder);
    let pending_for_callback = Rc::clone(&pending);
    let core_for_callback = core.clone();
    let loop_for_failure = main_loop.clone();
    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if found_for_callback.get()
                || global.type_ != pw::types::ObjectType::Node
                || global
                    .props
                    .as_ref()
                    .and_then(|props| props.get("node.name"))
                    != Some(selector_for_callback.as_str())
            {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            let node = match registry.bind::<pw::node::Node, _>(global) {
                Ok(node) => node,
                Err(error) => {
                    *failure_for_callback.borrow_mut() = Some(error.to_string());
                    loop_for_failure.quit();
                    return;
                }
            };
            let values_for_param = Rc::clone(&values_for_callback);
            let listener = node
                .add_listener_local()
                .param(move |_sequence, id, _index, _next, param| {
                    if id != ParamType::Props {
                        return;
                    }
                    let Some(param) = param else {
                        return;
                    };
                    let Ok((_, Value::Object(object))) =
                        PodDeserializer::deserialize_any_from(param.as_bytes())
                    else {
                        return;
                    };
                    let mut values = values_for_param.borrow_mut();
                    for property in object.properties {
                        match (property.key, property.value) {
                            (pw::spa::sys::SPA_PROP_mute, Value::Bool(muted)) => {
                                values.1 = Some(muted);
                            }
                            (
                                pw::spa::sys::SPA_PROP_channelVolumes,
                                Value::ValueArray(pw::spa::pod::ValueArray::Float(volumes)),
                            ) if !volumes.is_empty() => {
                                values.0 = Some(
                                    volumes.iter().copied().map(f64::from).sum::<f64>()
                                        / volumes.len() as f64,
                                );
                            }
                            _ => {}
                        }
                    }
                })
                .register();
            node.enum_params(0, Some(ParamType::Props), 0, u32::MAX);
            found_for_callback.set(true);
            *node_for_callback.borrow_mut() = Some(node);
            *listener_for_callback.borrow_mut() = Some(listener);
            match core_for_callback.sync(0) {
                Ok(sequence) => pending_for_callback.set(sequence),
                Err(error) => {
                    *failure_for_callback.borrow_mut() = Some(error.to_string());
                    loop_for_failure.quit();
                }
            }
        })
        .register();
    let pending_for_done = Rc::clone(&pending);
    let found_for_done = Rc::clone(&found);
    let loop_for_done = main_loop.clone();
    let failure_for_error = Rc::clone(&failure);
    let loop_for_error = main_loop.clone();
    let core_listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            if id == pw::core::PW_ID_CORE && sequence == pending_for_done.get() {
                let _ = found_for_done.get();
                loop_for_done.quit();
            }
        })
        .error(move |id, sequence, result, message| {
            *failure_for_error.borrow_mut() = Some(format!(
                "PipeWire core error id={id} sequence={sequence:?} result={result}: {message}"
            ));
            loop_for_error.quit();
        })
        .register();
    main_loop.run();
    drop(core_listener);
    drop(registry_listener);
    drop(node_listener_holder);
    drop(node_holder);
    if let Some(error) = failure.take() {
        return Err(pipewire_link_error(error));
    }
    if !found.get() {
        return Ok(None);
    }
    let (gain, muted) = *values.borrow();
    if gain.is_none() && muted.is_none() {
        return Ok(None);
    }
    Ok(Some(AudioControlState {
        layer: AudioControlLayer::Host,
        backend: AudioBackendKind::Pipewire,
        gain,
        gain_raw: None,
        gain_min_raw: None,
        gain_max_raw: None,
        muted,
    }))
}

#[cfg(feature = "pipewire")]
fn set_pipewire_control(
    endpoint: &AudioEndpoint,
    gain: Option<f64>,
    muted: Option<bool>,
    dry_run: bool,
) -> Result<AudioSetReport, LinkError> {
    let previous = pipewire_control_state(endpoint)?.ok_or_else(host_unavailable)?;
    let mut requested = previous.clone();
    let field = if let Some(gain) = gain {
        requested.gain = Some(gain);
        "gain"
    } else if let Some(muted) = muted {
        requested.muted = Some(muted);
        "mute"
    } else {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "no PipeWire audio change was requested",
        ));
    };
    if dry_run {
        return Ok(AudioSetReport {
            field: field.into(),
            layer: AudioControlLayer::Host,
            previous: previous.clone(),
            requested,
            observed: previous,
            dry_run: true,
            verified: false,
            rollback_succeeded: None,
        });
    }
    write_pipewire_control(endpoint, gain, muted)?;
    let observed = pipewire_control_state(endpoint)?.ok_or_else(host_unavailable)?;
    let verified = gain.is_none_or(|value| {
        observed
            .gain
            .is_some_and(|observed| (observed - value).abs() <= 0.000_1)
    }) && muted.is_none_or(|value| observed.muted == Some(value));
    if !verified {
        let rollback_succeeded =
            write_pipewire_control(endpoint, gain.and(previous.gain), muted.and(previous.muted))
                .is_ok()
                && pipewire_control_state(endpoint).is_ok_and(|state| {
                    state.is_some_and(|state| {
                        state.gain == previous.gain && state.muted == previous.muted
                    })
                });
        return Err(LinkError::new(
            ErrorKind::PartialSuccess,
            "PipeWire host control readback did not match the requested value",
        )
        .with_detail("rollback_succeeded", rollback_succeeded));
    }
    Ok(AudioSetReport {
        field: field.into(),
        layer: AudioControlLayer::Host,
        previous,
        requested,
        observed,
        dry_run: false,
        verified,
        rollback_succeeded: None,
    })
}

#[cfg(feature = "pipewire")]
fn write_pipewire_control(
    endpoint: &AudioEndpoint,
    gain: Option<f64>,
    muted: Option<bool>,
) -> Result<(), LinkError> {
    use std::{
        cell::{Cell, RefCell},
        io::Cursor,
        rc::Rc,
    };

    use pipewire as pw;
    use pw::spa::{
        param::ParamType,
        pod::{Object, Pod, Property, Value, ValueArray, serialize::PodSerializer},
        utils::SpaTypes,
    };

    let selector = endpoint
        .transports
        .iter()
        .find(|transport| transport.backend == AudioBackendKind::Pipewire)
        .map(|transport| transport.selector.clone())
        .ok_or_else(host_unavailable)?;
    let channels = endpoint.channels_max.unwrap_or(1).max(1) as usize;
    let mut properties = Vec::new();
    if let Some(gain) = gain {
        properties.push(Property::new(
            pw::spa::sys::SPA_PROP_channelVolumes,
            Value::ValueArray(ValueArray::Float(vec![gain as f32; channels])),
        ));
    }
    if let Some(muted) = muted {
        properties.push(Property::new(
            pw::spa::sys::SPA_PROP_mute,
            Value::Bool(muted),
        ));
    }
    let value = Value::Object(Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties,
    });
    let bytes = PodSerializer::serialize(Cursor::new(Vec::new()), &value)
        .map_err(|error| pipewire_link_error(format!("could not encode properties: {error:?}")))?
        .0
        .into_inner();
    let bytes = Rc::new(bytes);
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(pipewire_error)?;
    let context = pw::context::ContextRc::new(&main_loop, None).map_err(pipewire_error)?;
    let core = context.connect_rc(None).map_err(pipewire_error)?;
    let registry = core.get_registry_rc().map_err(pipewire_error)?;
    let pending = Rc::new(Cell::new(core.sync(0).map_err(pipewire_error)?));
    let found = Rc::new(Cell::new(false));
    let failure = Rc::new(RefCell::new(None::<String>));
    let node_holder = Rc::new(RefCell::new(None::<pw::node::Node>));
    let registry_weak = registry.downgrade();
    let selector_for_callback = selector.clone();
    let found_for_callback = Rc::clone(&found);
    let failure_for_callback = Rc::clone(&failure);
    let node_for_callback = Rc::clone(&node_holder);
    let pending_for_callback = Rc::clone(&pending);
    let core_for_callback = core.clone();
    let bytes_for_callback = Rc::clone(&bytes);
    let loop_for_failure = main_loop.clone();
    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if found_for_callback.get()
                || global.type_ != pw::types::ObjectType::Node
                || global
                    .props
                    .as_ref()
                    .and_then(|props| props.get("node.name"))
                    != Some(selector_for_callback.as_str())
            {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            let node = match registry.bind::<pw::node::Node, _>(global) {
                Ok(node) => node,
                Err(error) => {
                    *failure_for_callback.borrow_mut() = Some(error.to_string());
                    loop_for_failure.quit();
                    return;
                }
            };
            let Some(pod) = Pod::from_bytes(&bytes_for_callback) else {
                *failure_for_callback.borrow_mut() =
                    Some("could not decode encoded PipeWire properties".into());
                loop_for_failure.quit();
                return;
            };
            node.set_param(ParamType::Props, 0, pod);
            found_for_callback.set(true);
            *node_for_callback.borrow_mut() = Some(node);
            match core_for_callback.sync(0) {
                Ok(sequence) => pending_for_callback.set(sequence),
                Err(error) => {
                    *failure_for_callback.borrow_mut() = Some(error.to_string());
                    loop_for_failure.quit();
                }
            }
        })
        .register();
    let pending_for_done = Rc::clone(&pending);
    let loop_for_done = main_loop.clone();
    let failure_for_error = Rc::clone(&failure);
    let loop_for_error = main_loop.clone();
    let core_listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            if id == pw::core::PW_ID_CORE && sequence == pending_for_done.get() {
                loop_for_done.quit();
            }
        })
        .error(move |id, sequence, result, message| {
            *failure_for_error.borrow_mut() = Some(format!(
                "PipeWire core error id={id} sequence={sequence:?} result={result}: {message}"
            ));
            loop_for_error.quit();
        })
        .register();
    main_loop.run();
    drop(core_listener);
    drop(registry_listener);
    drop(node_holder);
    if let Some(error) = failure.take() {
        return Err(pipewire_link_error(error));
    }
    if !found.get() {
        return Err(host_unavailable());
    }
    Ok(())
}

#[cfg(feature = "pipewire")]
fn pipewire_error(error: pipewire::Error) -> LinkError {
    pipewire_link_error(error.to_string())
}

#[cfg(feature = "pipewire")]
fn pipewire_link_error(error: String) -> LinkError {
    LinkError::new(ErrorKind::IoFailure, "PipeWire audio control failed")
        .with_detail("reason", error)
}

#[cfg(feature = "pipewire")]
fn directly_matches(
    properties: &std::collections::BTreeMap<String, String>,
    usb: &UsbIdentity,
    component: &str,
) -> bool {
    let vendor = properties
        .get("device.vendor.id")
        .and_then(|value| parse_usb_id(value));
    let product = properties
        .get("device.product.id")
        .and_then(|value| parse_usb_id(value));
    (vendor == Some(usb.vendor_id) && product == Some(usb.product_id))
        || properties
            .get("alsa.components")
            .is_some_and(|value| value.to_ascii_lowercase().contains(component))
        || usb.product.as_deref().is_some_and(|product| {
            let product = normalized_name(product);
            !product.is_empty()
                && ["device.description", "device.name", "device.nick"]
                    .into_iter()
                    .filter_map(|key| properties.get(key))
                    .any(|value| normalized_name(value).contains(&product))
        })
}

#[cfg(feature = "pipewire")]
fn normalized_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(feature = "pipewire")]
fn parse_usb_id(value: &str) -> Option<u16> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use link_core::audio::{
        AudioBackendKind, AudioDirection, AudioEndpoint, AudioInventory, AudioTransport,
    };

    fn capture_endpoint(id: &str, name: &str, selector: &str) -> AudioEndpoint {
        AudioEndpoint {
            id: id.into(),
            name: name.into(),
            direction: AudioDirection::Capture,
            associated_camera: None,
            channels_min: Some(1),
            channels_max: Some(1),
            rate_min: Some(48_000),
            rate_max: Some(48_000),
            formats: vec!["S16_LE".into()],
            transports: vec![AudioTransport {
                backend: AudioBackendKind::Alsa,
                selector: selector.into(),
                numeric_id: None,
            }],
            mixer_controls: Vec::new(),
            default: false,
            busy: false,
        }
    }

    #[test]
    fn normalized_gain_round_trips_hardware_ranges() {
        for value in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let raw = super::normalized_to_raw(value, 0, 32_767);
            assert!((super::raw_to_normalized(raw, 0, 32_767) - value).abs() < 0.000_1);
        }
    }

    #[test]
    fn explicit_third_party_source_selectors_do_not_require_a_camera() {
        let endpoint = capture_endpoint("audio-external", "External microphone", "hw:8,0");
        let inventory = AudioInventory {
            endpoints: vec![endpoint.clone()],
            ..AudioInventory::default()
        };
        assert_eq!(
            super::resolve_capture_source(&inventory, Some("audio-external"), None).unwrap(),
            endpoint
        );
        assert_eq!(
            super::resolve_capture_source(&inventory, Some("alsa:hw:8,0"), None).unwrap(),
            endpoint
        );
    }

    #[cfg(feature = "pipewire")]
    #[test]
    fn usb_ids_accept_pipewire_hex_notation() {
        assert_eq!(super::parse_usb_id("0x2e1a"), Some(0x2e1a));
        assert_eq!(super::parse_usb_id("4c05"), Some(0x4c05));
        assert_eq!(super::parse_usb_id("not-an-id"), None);
    }

    #[cfg(feature = "pipewire")]
    #[test]
    fn product_names_match_pipewire_safe_names() {
        assert_eq!(
            super::normalized_name("alsa_card.usb-Insta360_Insta360_Link_2C_Pro-02"),
            "alsacardusbinsta360insta360link2cpro02"
        );
    }

    #[cfg(feature = "pipewire")]
    #[test]
    fn pipewire_and_alsa_routes_merge_into_one_logical_endpoint() {
        use std::collections::BTreeMap;

        let mut endpoints = vec![capture_endpoint(
            "audio-camera",
            "Insta360 Link 2C Pro",
            "hw:4,0",
        )];
        super::merge_pipewire(
            &mut endpoints,
            vec![super::PipeWireEndpoint {
                id: 94,
                properties: BTreeMap::from([
                    ("media.class".into(), "Audio/Source".into()),
                    ("node.nick".into(), "Insta360 Link 2C Pro".into()),
                    ("node.name".into(), "alsa_input.camera".into()),
                    ("api.alsa.pcm.card".into(), "4".into()),
                ]),
                default: true,
            }],
        );
        assert_eq!(endpoints.len(), 1);
        assert!(endpoints[0].default);
        assert!(endpoints[0].transports.iter().any(|transport| {
            transport.backend == AudioBackendKind::Pipewire
                && transport.selector == "alsa_input.camera"
        }));
    }
}
