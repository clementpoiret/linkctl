//! Read-only ALSA and optional PipeWire audio discovery.

use alsa::{
    Direction,
    ctl::{Ctl, DeviceIter},
    pcm::{Access, Format, HwParams, PCM},
};
use link_core::probe::{AlsaPcmReport, AudioReport, ProbeIssue, UsbIdentity};

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
        "api.alsa.card",
        "api.alsa.card.longname",
        "api.alsa.card.name",
        "api.alsa.pcm.card",
        "api.alsa.pcm.device",
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
}
