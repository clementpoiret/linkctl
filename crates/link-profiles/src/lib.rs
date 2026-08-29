//! Strict vendor-profile loading, matching, typed codecs, and trust classification.

use std::{collections::BTreeMap, fs, path::Path};

use link_core::{
    ErrorKind, LinkError,
    media::VideoTuple,
    probe::Rational,
    probe::{DeviceMode, ProfileReport, UsbIdentity},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub use link_core::safety::ProfileState;

const BUILTIN_LINK_2C_PRO: &str =
    include_str!("../../../profiles/read-only/insta360-link-2c-pro.toml");
const BUILTIN_LINK_2C_PRO_OTHER_PERSONALITIES: &str =
    include_str!("../../../profiles/read-only/insta360-link-2c-pro-other-personalities.toml");
const BUILTIN_LINK_2C_PRO_V0_2_9_8_BUILD3: &str =
    include_str!("../../../profiles/verified/insta360-link-2c-pro-v0.2.9.8_build3.toml");

/// Validation status declared by a profile document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileStatus {
    ReadOnly,
    Experimental,
    Verified,
}

/// Trust attached by the loader rather than accepted from profile input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileTrust {
    BuiltIn,
    External,
}

/// One descriptor-guarded USB personality.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileMatch {
    pub mode: DeviceMode,
    pub usb_vid: u16,
    pub usb_pid: u16,
    pub bcd_device_min: u16,
    pub bcd_device_max: u16,
    pub descriptor_sha256: String,
    #[serde(default)]
    pub firmware: Vec<String>,
}

/// Byte order used by integer fields.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ByteOrder {
    #[default]
    Little,
    Big,
}

/// Typed payload codec.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecKind {
    Raw,
    Utf8,
    Boolean,
    Unsigned,
    Signed,
    Enum,
    Bitmask,
    Rectangle,
    Structured,
}

/// One field in a structured payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodecField {
    pub name: String,
    pub offset: usize,
    pub width: u8,
    pub codec: CodecKind,
    #[serde(default)]
    pub byte_order: ByteOrder,
    #[serde(default)]
    pub values: BTreeMap<String, i64>,
}

/// Policy for bytes outside the typed portion of a payload.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TailPolicy {
    #[default]
    Preserve,
    Zero,
    Fixed {
        hex: String,
    },
    Computed {
        algorithm: ComputedTail,
    },
}

/// Deliberately small registry of deterministic tail algorithms.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComputedTail {
    Sum8,
    Xor8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamRequirement {
    Closed,
    Open,
    #[default]
    Either,
    Restart,
}

/// Control-transfer prelude required by a profiled vendor write.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WritePrelude {
    #[default]
    Capabilities,
    GetLengthTwice,
}

/// Exact media tuple required while issuing a vendor control transfer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StreamFormat {
    pub fourcc: String,
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
}

impl StreamFormat {
    #[must_use]
    pub fn video_tuple(&self) -> VideoTuple {
        VideoTuple {
            fourcc: self.fourcc.clone(),
            width: self.width,
            height: self.height,
            fps: Rational {
                numerator: self.fps_numerator,
                denominator: self.fps_denominator,
            },
        }
        .normalized()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationMethod {
    #[default]
    Readback,
    Status,
    VideoObservation,
    AudioObservation,
    Reenumeration,
    Manual,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Persistence {
    #[default]
    Volatile,
    Stream,
    Reconnect,
    PowerCycle,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RollbackPolicy {
    #[default]
    Readback,
    None,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafetyClass {
    #[default]
    Normal,
    Firmware,
    Boot,
    Flash,
    Calibration,
    Motor,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotPolicy {
    #[default]
    Include,
    Volatile,
    Exclude,
}

/// One semantic control guarded by a profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileControl {
    pub name: String,
    pub entity_guid: String,
    pub selector: u8,
    pub length: u16,
    pub readable: bool,
    pub writable: bool,
    pub codec: CodecKind,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub width: u8,
    #[serde(default)]
    pub byte_order: ByteOrder,
    #[serde(default)]
    pub values: BTreeMap<String, i64>,
    #[serde(default)]
    pub fields: Vec<CodecField>,
    #[serde(default)]
    pub read_modify_write: bool,
    #[serde(default)]
    pub write_mask: Option<i64>,
    #[serde(default)]
    pub tail_policy: TailPolicy,
    #[serde(default)]
    pub verification: VerificationMethod,
    #[serde(default)]
    pub stream_requirement: StreamRequirement,
    #[serde(default)]
    pub stream_format: Option<StreamFormat>,
    #[serde(default)]
    pub stream_warmup_delay_ms: u64,
    #[serde(default)]
    pub write_prelude: WritePrelude,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub postconditions: Vec<String>,
    #[serde(default = "default_write_interval")]
    pub minimum_write_interval_ms: u64,
    #[serde(default)]
    pub verification_delay_ms: u64,
    #[serde(default)]
    pub readback_tolerance: u64,
    #[serde(default)]
    pub persistence: Persistence,
    #[serde(default)]
    pub rollback: RollbackPolicy,
    #[serde(default)]
    pub trace_ids: Vec<String>,
    #[serde(default)]
    pub safety: SafetyClass,
    #[serde(default)]
    pub snapshot: SnapshotPolicy,
}

const fn default_write_interval() -> u64 {
    250
}

/// Validated vendor profile document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VendorProfile {
    pub schema_version: u32,
    pub profile_id: String,
    pub model: String,
    pub status: ProfileStatus,
    #[serde(default)]
    pub provenance: Vec<String>,
    #[serde(rename = "match")]
    pub matches: Vec<ProfileMatch>,
    #[serde(default)]
    pub controls: Vec<ProfileControl>,
}

/// Compatibility name retained for users of the former read-only type.
pub type ReadOnlyProfile = VendorProfile;

impl VendorProfile {
    pub fn parse(source: &str, origin: &str) -> Result<Self, LinkError> {
        let profile: Self = toml::from_str(source).map_err(|error| {
            LinkError::new(ErrorKind::ProtocolProfileMismatch, "invalid vendor profile")
                .with_detail("origin", origin.to_owned())
                .with_detail("reason", error.to_string())
        })?;
        profile.validate(origin)?;
        Ok(profile)
    }

    fn validate(&self, origin: &str) -> Result<(), LinkError> {
        if self.schema_version != 1 {
            return Err(profile_error(origin, "unsupported profile schema"));
        }
        if empty_or_placeholder(&self.profile_id) || empty_or_placeholder(&self.model) {
            return Err(profile_error(origin, "profile identity fields are invalid"));
        }
        if self.matches.is_empty() {
            return Err(profile_error(
                origin,
                "profile must contain at least one match",
            ));
        }
        for guard in &self.matches {
            if guard.bcd_device_min > guard.bcd_device_max {
                return Err(profile_error(
                    origin,
                    "profile device revision range is inverted",
                ));
            }
            validate_hash(&guard.descriptor_sha256, origin)?;
            if guard
                .firmware
                .iter()
                .any(|value| empty_or_placeholder(value))
            {
                return Err(profile_error(origin, "profile firmware guard is invalid"));
            }
        }

        let mut names = BTreeMap::new();
        for control in &self.controls {
            if names.insert(control.name.as_str(), ()).is_some() {
                return Err(profile_error(
                    origin,
                    "profile control names must be unique",
                ));
            }
            validate_control(self, control, origin)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn matches(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
        firmware: Option<&str>,
    ) -> bool {
        self.match_specificity(identity, mode, firmware).is_some()
    }

    /// Return the strongest matching guard: firmware-specific profiles outrank generic identity
    /// profiles so a read-only bootstrap profile can coexist with verified firmware overlays.
    #[must_use]
    pub fn match_specificity(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
        firmware: Option<&str>,
    ) -> Option<u8> {
        self.matches
            .iter()
            .filter(|guard| {
                guard.mode == mode
                    && guard.usb_vid == identity.vendor_id
                    && guard.usb_pid == identity.product_id
                    && (guard.bcd_device_min..=guard.bcd_device_max)
                        .contains(&identity.device_revision)
                    && guard.descriptor_sha256 == identity.descriptor_sha256
                    && (guard.firmware.is_empty()
                        || firmware
                            .is_some_and(|value| guard.firmware.iter().any(|item| item == value)))
            })
            .map(|guard| u8::from(!guard.firmware.is_empty()))
            .max()
    }

    #[must_use]
    pub fn checksum(&self) -> String {
        let canonical = toml::to_string(self).expect("validated profiles serialize");
        lowercase_hash(canonical.as_bytes())
    }

    #[must_use]
    pub fn control(&self, name: &str) -> Option<&ProfileControl> {
        self.controls.iter().find(|control| control.name == name)
    }

    #[must_use]
    pub fn controls_for_selector(&self, guid: &str, selector: u8) -> Vec<&ProfileControl> {
        self.controls
            .iter()
            .filter(|control| {
                control.entity_guid.eq_ignore_ascii_case(guid) && control.selector == selector
            })
            .collect()
    }
}

fn validate_control(
    profile: &VendorProfile,
    control: &ProfileControl,
    origin: &str,
) -> Result<(), LinkError> {
    if empty_or_placeholder(&control.name)
        || !valid_guid(&control.entity_guid)
        || control.selector == 0
        || control.length == 0
    {
        return Err(profile_error(origin, "profile control identity is invalid"));
    }
    if control.writable {
        if profile.status == ProfileStatus::ReadOnly {
            return Err(profile_error(
                origin,
                "read-only profiles cannot define writable controls",
            ));
        }
        if profile
            .matches
            .iter()
            .any(|guard| guard.firmware.is_empty())
        {
            return Err(profile_error(
                origin,
                "writable controls require exact firmware guards",
            ));
        }
        if profile.provenance.is_empty() || control.trace_ids.is_empty() {
            return Err(profile_error(
                origin,
                "writable controls require provenance and trace identifiers",
            ));
        }
        if control.verification == VerificationMethod::Readback && !control.readable {
            return Err(profile_error(
                origin,
                "readback verification requires a readable control",
            ));
        }
        if control.verification_delay_ms > 60_000 {
            return Err(profile_error(
                origin,
                "profile verification delay exceeds the safety bound",
            ));
        }
    }
    if control.readback_tolerance != 0
        && (!control.readable
            || !control.writable
            || control.verification != VerificationMethod::Readback
            || !matches!(control.codec, CodecKind::Unsigned | CodecKind::Signed))
    {
        return Err(profile_error(
            origin,
            "readback tolerance requires a readable, writable numeric control",
        ));
    }
    if control.read_modify_write {
        if !control.readable {
            return Err(profile_error(
                origin,
                "read-modify-write controls must also be readable",
            ));
        }
        if !matches!(control.codec, CodecKind::Enum | CodecKind::Bitmask) {
            return Err(profile_error(
                origin,
                "masked read-modify-write requires an enum or bitmask codec",
            ));
        }
        let Some(mask) = control.write_mask else {
            return Err(profile_error(
                origin,
                "read-modify-write controls require a positive write mask",
            ));
        };
        let mask_fits_width = match control.width {
            1 => mask <= i64::from(u8::MAX),
            2 => mask <= i64::from(u16::MAX),
            4 => mask <= i64::from(u32::MAX),
            8 => true,
            _ => false,
        };
        if mask <= 0
            || !mask_fits_width
            || control
                .values
                .values()
                .any(|value| *value < 0 || *value & !mask != 0)
        {
            return Err(profile_error(
                origin,
                "read-modify-write values must fit within the positive write mask",
            ));
        }
        if !matches!(control.tail_policy, TailPolicy::Preserve) {
            return Err(profile_error(
                origin,
                "read-modify-write controls must preserve their baseline payload",
            ));
        }
    } else if control.write_mask.is_some() {
        return Err(profile_error(
            origin,
            "a write mask requires read-modify-write behavior",
        ));
    }
    if (control.stream_format.is_some() || control.stream_warmup_delay_ms != 0)
        && control.stream_requirement != StreamRequirement::Open
    {
        return Err(profile_error(
            origin,
            "stream format and warm-up settings require an open stream",
        ));
    }
    if let Some(format) = &control.stream_format
        && (format.fourcc.len() != 4
            || !format.fourcc.is_ascii()
            || format.width == 0
            || format.height == 0
            || format.fps_numerator == 0
            || format.fps_denominator == 0)
    {
        return Err(profile_error(origin, "profile stream format is invalid"));
    }
    if control.stream_warmup_delay_ms > 60_000 {
        return Err(profile_error(
            origin,
            "profile stream warm-up delay exceeds the safety bound",
        ));
    }
    if control.write_prelude != WritePrelude::Capabilities && !control.writable {
        return Err(profile_error(
            origin,
            "a custom write prelude requires a writable control",
        ));
    }
    let encoded = encoded_length(control)?;
    if encoded > usize::from(control.length) {
        return Err(profile_error(
            origin,
            "profile codec exceeds the payload length",
        ));
    }
    if matches!(control.codec, CodecKind::Enum | CodecKind::Bitmask) && control.values.is_empty() {
        return Err(profile_error(
            origin,
            "enum and bitmask codecs require values",
        ));
    }
    let mut field_names = BTreeMap::new();
    let mut field_ranges = Vec::new();
    for field in &control.fields {
        validate_field(field, control.length, origin)?;
        if field_names.insert(field.name.as_str(), ()).is_some() {
            return Err(profile_error(
                origin,
                "structured field names must be unique",
            ));
        }
        let range = field.offset..field.offset + usize::from(field.width);
        if field_ranges
            .iter()
            .any(|existing: &std::ops::Range<usize>| {
                range.start < existing.end && existing.start < range.end
            })
        {
            return Err(profile_error(origin, "structured fields cannot overlap"));
        }
        field_ranges.push(range);
    }
    if control.writable && matches!(control.tail_policy, TailPolicy::Preserve) && !control.readable
    {
        return Err(profile_error(
            origin,
            "preserved tail bytes require a readable baseline",
        ));
    }
    if let TailPolicy::Fixed { hex } = &control.tail_policy {
        let expected = usize::from(control.length).saturating_sub(encoded);
        if decode_hex(hex)?.len() != expected {
            return Err(profile_error(
                origin,
                "fixed tail length does not match the payload",
            ));
        }
    }
    if matches!(control.tail_policy, TailPolicy::Computed { .. })
        && usize::from(control.length) != encoded.saturating_add(1)
    {
        return Err(profile_error(
            origin,
            "computed tail requires one checksum byte",
        ));
    }
    Ok(())
}

fn validate_field(field: &CodecField, length: u16, origin: &str) -> Result<(), LinkError> {
    if field.name.trim().is_empty() || !valid_width(field.codec, field.width) {
        return Err(profile_error(origin, "structured codec field is invalid"));
    }
    let end = field.offset.saturating_add(usize::from(field.width));
    if end > usize::from(length) {
        return Err(profile_error(
            origin,
            "structured codec field exceeds the payload",
        ));
    }
    if matches!(field.codec, CodecKind::Enum | CodecKind::Bitmask) && field.values.is_empty() {
        return Err(profile_error(
            origin,
            "enum and bitmask fields require values",
        ));
    }
    Ok(())
}

fn valid_width(codec: CodecKind, width: u8) -> bool {
    match codec {
        CodecKind::Boolean => width == 1,
        CodecKind::Unsigned | CodecKind::Signed | CodecKind::Enum | CodecKind::Bitmask => {
            matches!(width, 1 | 2 | 4 | 8)
        }
        _ => false,
    }
}

fn encoded_length(control: &ProfileControl) -> Result<usize, LinkError> {
    match control.codec {
        CodecKind::Raw => Ok(usize::from(control.length)),
        CodecKind::Utf8 => {
            if control.width == 0 {
                return Err(LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "UTF-8 profile fields require a non-zero width",
                ));
            }
            Ok(control.offset.saturating_add(usize::from(control.width)))
        }
        CodecKind::Boolean
        | CodecKind::Unsigned
        | CodecKind::Signed
        | CodecKind::Enum
        | CodecKind::Bitmask => {
            if !valid_width(control.codec, control.width) {
                return Err(LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "scalar codec width is invalid",
                ));
            }
            Ok(control.offset.saturating_add(usize::from(control.width)))
        }
        CodecKind::Rectangle | CodecKind::Structured => control
            .fields
            .iter()
            .map(|field| field.offset.saturating_add(usize::from(field.width)))
            .max()
            .ok_or_else(|| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "structured codec requires fields",
                )
            }),
    }
}

/// Decode one exact payload according to a validated control.
pub fn decode_control(control: &ProfileControl, payload: &[u8]) -> Result<Value, LinkError> {
    ensure_payload(control, payload)?;
    match control.codec {
        CodecKind::Raw => Ok(Value::String(lowercase_hex(payload))),
        CodecKind::Utf8 => {
            let field = &payload[control.offset..control.offset + usize::from(control.width)];
            let end = field
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(field.len());
            let value = std::str::from_utf8(&field[..end]).map_err(|_| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "profile UTF-8 payload contains invalid text",
                )
            })?;
            Ok(Value::String(value.trim().to_owned()))
        }
        CodecKind::Boolean
        | CodecKind::Unsigned
        | CodecKind::Signed
        | CodecKind::Enum
        | CodecKind::Bitmask => decode_value(
            control.codec,
            &payload[control.offset..control.offset + usize::from(control.width)],
            control.byte_order,
            &control.values,
            control.write_mask.filter(|_| control.read_modify_write),
        ),
        CodecKind::Rectangle | CodecKind::Structured => {
            let mut result = Map::new();
            for field in &control.fields {
                let end = field.offset + usize::from(field.width);
                result.insert(
                    field.name.clone(),
                    decode_value(
                        field.codec,
                        &payload[field.offset..end],
                        field.byte_order,
                        &field.values,
                        None,
                    )?,
                );
            }
            Ok(Value::Object(result))
        }
    }
}

fn decode_value(
    codec: CodecKind,
    bytes: &[u8],
    order: ByteOrder,
    values: &BTreeMap<String, i64>,
    read_mask: Option<i64>,
) -> Result<Value, LinkError> {
    let raw = read_integer(bytes, order, codec == CodecKind::Signed)?;
    let raw = read_mask.map_or(raw, |mask| raw & mask);
    Ok(match codec {
        CodecKind::Boolean => Value::Bool(raw != 0),
        CodecKind::Enum => values
            .iter()
            .find_map(|(name, value)| (*value == raw).then(|| Value::String(name.clone())))
            .unwrap_or_else(|| json!(raw)),
        CodecKind::Bitmask => Value::Array(
            values
                .iter()
                .filter(|(_, mask)| raw & **mask == **mask)
                .map(|(name, _)| Value::String(name.clone()))
                .collect(),
        ),
        CodecKind::Unsigned | CodecKind::Signed => json!(raw),
        _ => {
            return Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "unsupported nested codec",
            ));
        }
    })
}

/// Build a complete payload from one semantic CLI value and an optional baseline.
pub fn encode_control(
    control: &ProfileControl,
    input: &str,
    current: Option<&[u8]>,
) -> Result<Vec<u8>, LinkError> {
    let length = usize::from(control.length);
    let encoded = encoded_length(control)?;
    let mut payload = match &control.tail_policy {
        TailPolicy::Preserve => {
            let current = current.ok_or_else(|| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "preserve tail policy requires a readable baseline",
                )
            })?;
            ensure_payload(control, current)?;
            current.to_vec()
        }
        TailPolicy::Zero | TailPolicy::Computed { .. } => vec![0; length],
        TailPolicy::Fixed { hex } => {
            let mut value = vec![0; length];
            value[encoded..].copy_from_slice(&decode_hex(hex)?);
            value
        }
    };

    match control.codec {
        CodecKind::Raw | CodecKind::Utf8 => {
            return Err(LinkError::new(
                ErrorKind::UnsafeOperationDenied,
                "raw and text codecs cannot be changed through semantic set",
            ));
        }
        CodecKind::Boolean
        | CodecKind::Unsigned
        | CodecKind::Signed
        | CodecKind::Enum
        | CodecKind::Bitmask => {
            let end = control.offset + usize::from(control.width);
            let mut raw = parse_value(control.codec, input, &control.values)?;
            if control.read_modify_write {
                let current = current.ok_or_else(|| {
                    LinkError::new(
                        ErrorKind::ProtocolProfileMismatch,
                        "read-modify-write requires a readable baseline",
                    )
                })?;
                ensure_payload(control, current)?;
                let mask = control.write_mask.ok_or_else(|| {
                    LinkError::new(
                        ErrorKind::ProtocolProfileMismatch,
                        "read-modify-write profile is missing its write mask",
                    )
                })?;
                let previous =
                    read_integer(&current[control.offset..end], control.byte_order, false)?;
                raw = (previous & !mask) | (raw & mask);
            }
            write_integer(
                &mut payload[control.offset..end],
                raw,
                control.byte_order,
                control.codec == CodecKind::Signed,
            )?;
        }
        CodecKind::Rectangle | CodecKind::Structured => {
            let assignments = parse_assignments(input)?;
            if assignments.len() != control.fields.len() {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "structured values must assign every field",
                ));
            }
            for field in &control.fields {
                let value = assignments.get(&field.name).ok_or_else(|| {
                    LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "structured value is missing a field",
                    )
                    .with_detail("field", field.name.clone())
                })?;
                let raw = parse_value(field.codec, value, &field.values)?;
                let end = field.offset + usize::from(field.width);
                write_integer(
                    &mut payload[field.offset..end],
                    raw,
                    field.byte_order,
                    field.codec == CodecKind::Signed,
                )?;
            }
        }
    }

    if let TailPolicy::Computed { algorithm } = control.tail_policy {
        let checksum = match algorithm {
            ComputedTail::Sum8 => payload[..length - 1]
                .iter()
                .fold(0_u8, |sum, value| sum.wrapping_add(*value)),
            ComputedTail::Xor8 => payload[..length - 1]
                .iter()
                .fold(0_u8, |value, byte| value ^ byte),
        };
        payload[length - 1] = checksum;
    }
    Ok(payload)
}

/// Re-encode a previously decoded semantic value using the control's write-tail policy.
pub fn encode_decoded_control(
    control: &ProfileControl,
    value: &Value,
    current: Option<&[u8]>,
) -> Result<Vec<u8>, LinkError> {
    let input = match control.codec {
        CodecKind::Boolean
        | CodecKind::Unsigned
        | CodecKind::Signed
        | CodecKind::Enum
        | CodecKind::Bitmask => decoded_value_input(control.codec, value)?,
        CodecKind::Rectangle | CodecKind::Structured => {
            let object = value.as_object().ok_or_else(decoded_value_error)?;
            control
                .fields
                .iter()
                .map(|field| {
                    object
                        .get(&field.name)
                        .ok_or_else(decoded_value_error)
                        .and_then(|value| decoded_value_input(field.codec, value))
                        .map(|value| format!("{}={value}", field.name))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        }
        CodecKind::Raw | CodecKind::Utf8 => return Err(decoded_value_error()),
    };
    encode_control(control, &input, current)
}

fn decoded_value_input(codec: CodecKind, value: &Value) -> Result<String, LinkError> {
    match codec {
        CodecKind::Boolean => value
            .as_bool()
            .map(|value| if value { "on" } else { "off" }.to_owned()),
        CodecKind::Unsigned | CodecKind::Signed => value.as_i64().map(|value| value.to_string()),
        CodecKind::Enum => value.as_str().map(ToOwned::to_owned),
        CodecKind::Bitmask => value.as_array().and_then(|values| {
            values
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
                .map(|values| values.join("|"))
        }),
        _ => None,
    }
    .ok_or_else(decoded_value_error)
}

fn decoded_value_error() -> LinkError {
    LinkError::new(
        ErrorKind::ProtocolProfileMismatch,
        "decoded profile value cannot be encoded for rollback",
    )
}

fn parse_assignments(input: &str) -> Result<BTreeMap<String, String>, LinkError> {
    let mut result = BTreeMap::new();
    for item in input.split(',') {
        let (name, value) = item.split_once('=').ok_or_else(|| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "structured values use field=value pairs",
            )
        })?;
        if name.trim().is_empty()
            || value.trim().is_empty()
            || result
                .insert(name.trim().to_owned(), value.trim().to_owned())
                .is_some()
        {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "structured value contains an invalid or duplicate field",
            ));
        }
    }
    Ok(result)
}

fn parse_value(
    codec: CodecKind,
    input: &str,
    values: &BTreeMap<String, i64>,
) -> Result<i64, LinkError> {
    match codec {
        CodecKind::Boolean => match input.to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => Ok(1),
            "off" | "false" | "0" => Ok(0),
            _ => Err(value_error(input)),
        },
        CodecKind::Enum => values.get(input).copied().ok_or_else(|| value_error(input)),
        CodecKind::Bitmask => {
            let mut result = 0_i64;
            for name in input.split('|') {
                result |= values
                    .get(name.trim())
                    .copied()
                    .ok_or_else(|| value_error(input))?;
            }
            Ok(result)
        }
        CodecKind::Unsigned | CodecKind::Signed => parse_integer(input),
        _ => Err(value_error(input)),
    }
}

fn parse_integer(input: &str) -> Result<i64, LinkError> {
    let parsed = if let Some(hex) = input.strip_prefix("0x") {
        i64::from_str_radix(hex, 16)
    } else {
        input.parse()
    };
    parsed.map_err(|_| value_error(input))
}

fn value_error(input: &str) -> LinkError {
    LinkError::new(
        ErrorKind::InvalidInvocation,
        "value is invalid for the profile codec",
    )
    .with_detail("value", input.to_owned())
}

fn read_integer(bytes: &[u8], order: ByteOrder, signed: bool) -> Result<i64, LinkError> {
    let mut raw = [0_u8; 8];
    match order {
        ByteOrder::Little => raw[..bytes.len()].copy_from_slice(bytes),
        ByteOrder::Big => raw[8 - bytes.len()..].copy_from_slice(bytes),
    }
    let unsigned = match order {
        ByteOrder::Little => u64::from_le_bytes(raw),
        ByteOrder::Big => u64::from_be_bytes(raw),
    };
    let sign_byte = match order {
        ByteOrder::Little => bytes.last(),
        ByteOrder::Big => bytes.first(),
    };
    if signed && bytes.len() < 8 && sign_byte.is_some_and(|value| value & 0x80 != 0) {
        let bits = bytes.len() * 8;
        Ok((unsigned | (!0_u64 << bits)) as i64)
    } else {
        i64::try_from(unsigned).map_err(|_| {
            LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "unsigned profile value exceeds the supported JSON integer range",
            )
        })
    }
}

fn write_integer(
    bytes: &mut [u8],
    value: i64,
    order: ByteOrder,
    signed: bool,
) -> Result<(), LinkError> {
    let bits = bytes.len() * 8;
    let valid = if signed {
        let min = -(1_i128 << (bits - 1));
        let max = (1_i128 << (bits - 1)) - 1;
        (min..=max).contains(&i128::from(value))
    } else {
        value >= 0 && u128::try_from(value).is_ok_and(|value| value < (1_u128 << bits))
    };
    if !valid {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "profile value does not fit the encoded field",
        ));
    }
    let encoded = match order {
        ByteOrder::Little => value.to_le_bytes(),
        ByteOrder::Big => value.to_be_bytes(),
    };
    match order {
        ByteOrder::Little => bytes.copy_from_slice(&encoded[..bytes.len()]),
        ByteOrder::Big => bytes.copy_from_slice(&encoded[8 - bytes.len()..]),
    }
    Ok(())
}

fn ensure_payload(control: &ProfileControl, payload: &[u8]) -> Result<(), LinkError> {
    if payload.len() == usize::from(control.length) {
        Ok(())
    } else {
        Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "profile payload length does not match GET_LEN",
        )
        .with_detail("expected", u64::from(control.length))
        .with_detail("observed", payload.len() as u64))
    }
}

/// One loaded profile and loader-assigned trust classification.
#[derive(Clone, Debug)]
pub struct CatalogProfile {
    pub profile: VendorProfile,
    pub trust: ProfileTrust,
    pub checksum: String,
}

/// A semantic control capability minted only by a trusted built-in verified profile.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizedControl<'a> {
    control: &'a ProfileControl,
}

impl<'a> AuthorizedControl<'a> {
    #[must_use]
    pub const fn control(self) -> &'a ProfileControl {
        self.control
    }
}

impl CatalogProfile {
    /// Built-in read-only profiles can establish readable semantics without authorizing writes.
    #[must_use]
    pub fn semantic_read_verified(&self) -> bool {
        self.trust == ProfileTrust::BuiltIn
            && matches!(
                self.profile.status,
                ProfileStatus::ReadOnly | ProfileStatus::Verified
            )
    }

    #[must_use]
    pub fn semantic_write_authorized(&self) -> bool {
        self.trust == ProfileTrust::BuiltIn && self.profile.status == ProfileStatus::Verified
    }

    /// Resolve a writable semantic control without exposing an authorization constructor.
    #[must_use]
    pub fn authorized_control(&self, name: &str) -> Option<AuthorizedControl<'_>> {
        if !self.semantic_write_authorized() {
            return None;
        }
        self.profile
            .control(name)
            .filter(|control| control.writable)
            .map(|control| AuthorizedControl { control })
    }

    #[must_use]
    pub fn research_write_authorized(&self) -> bool {
        matches!(
            self.profile.status,
            ProfileStatus::Experimental | ProfileStatus::Verified
        )
    }

    #[must_use]
    pub fn state(&self) -> ProfileState {
        match self.profile.status {
            ProfileStatus::ReadOnly => ProfileState::ReadOnly,
            ProfileStatus::Experimental => ProfileState::Experimental,
            ProfileStatus::Verified if self.trust == ProfileTrust::BuiltIn => {
                ProfileState::Verified
            }
            ProfileStatus::Verified => ProfileState::Untrusted,
        }
    }
}

/// Built-in and explicitly supplied vendor profiles.
#[derive(Clone, Debug)]
pub struct ProfileCatalog {
    profiles: Vec<CatalogProfile>,
}

impl ProfileCatalog {
    pub fn load(additional_directory: Option<&Path>) -> Result<Self, LinkError> {
        let mut profiles = Vec::new();
        for (source, origin) in [
            (BUILTIN_LINK_2C_PRO, "builtin:insta360-link-2c-pro"),
            (
                BUILTIN_LINK_2C_PRO_OTHER_PERSONALITIES,
                "builtin:insta360-link-2c-pro-other-personalities",
            ),
            (
                BUILTIN_LINK_2C_PRO_V0_2_9_8_BUILD3,
                "builtin:insta360-link-2c-pro-v0.2.9.8_build3",
            ),
        ] {
            let built_in = VendorProfile::parse(source, origin)?;
            profiles.push(CatalogProfile {
                checksum: built_in.checksum(),
                profile: built_in,
                trust: ProfileTrust::BuiltIn,
            });
        }
        if let Some(directory) = additional_directory {
            let mut paths = fs::read_dir(directory)
                .map_err(|error| profile_io_error(directory, &error))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| profile_io_error(directory, &error))?
                .into_iter()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|value| value == "toml"))
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let metadata =
                    fs::symlink_metadata(&path).map_err(|error| profile_io_error(&path, &error))?;
                if !metadata.file_type().is_file() {
                    return Err(profile_error(
                        &path.display().to_string(),
                        "profile input must be a regular file and cannot be a symlink",
                    ));
                }
                let source =
                    fs::read_to_string(&path).map_err(|error| profile_io_error(&path, &error))?;
                let profile = VendorProfile::parse(&source, &path.display().to_string())?;
                profiles.push(CatalogProfile {
                    checksum: profile.checksum(),
                    profile,
                    trust: ProfileTrust::External,
                });
            }
        }
        Ok(Self { profiles })
    }

    pub fn matching_profile(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
    ) -> Result<Option<&VendorProfile>, LinkError> {
        Ok(self
            .matching(identity, mode, None)?
            .map(|entry| &entry.profile))
    }

    pub fn matching(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
        firmware: Option<&str>,
    ) -> Result<Option<&CatalogProfile>, LinkError> {
        let matches = self
            .profiles
            .iter()
            .filter_map(|entry| {
                entry
                    .profile
                    .match_specificity(identity, mode, firmware)
                    .map(|specificity| (entry, specificity))
            })
            .collect::<Vec<_>>();
        let strongest = matches.iter().map(|(_, specificity)| *specificity).max();
        let matches = matches
            .into_iter()
            .filter(|(_, specificity)| Some(*specificity) == strongest)
            .map(|(entry, _)| entry)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Ok(None),
            [profile] => Ok(Some(*profile)),
            _ => Err(LinkError::new(
                ErrorKind::ProtocolProfileMismatch,
                "multiple vendor profiles match the same device",
            )
            .with_detail("matches", matches.len() as u64)),
        }
    }

    pub fn report(
        &self,
        identity: &UsbIdentity,
        mode: DeviceMode,
    ) -> Result<ProfileReport, LinkError> {
        if let Some(entry) = self.matching(identity, mode, None)? {
            return Ok(ProfileReport {
                profile_id: Some(entry.profile.profile_id.clone()),
                reasons: vec!["exact USB revision and descriptor fingerprint match".into()],
                writable: entry.semantic_write_authorized(),
            });
        }
        let related = self.profiles.iter().any(|entry| {
            entry.profile.matches.iter().any(|guard| {
                guard.usb_vid == identity.vendor_id && guard.usb_pid == identity.product_id
            })
        });
        Ok(ProfileReport {
            profile_id: None,
            reasons: vec![if related {
                "USB model recognized, but revision, mode, descriptor, or firmware guard mismatched"
                    .into()
            } else {
                "no vendor profile recognizes this USB model".into()
            }],
            writable: false,
        })
    }

    #[must_use]
    pub fn profiles(&self) -> &[CatalogProfile] {
        &self.profiles
    }
}

fn validate_hash(hash: &str, origin: &str) -> Result<(), LinkError> {
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && hash.bytes().any(|byte| byte != b'0')
        && hash.bytes().any(|byte| byte != hash.as_bytes()[0])
        && !empty_or_placeholder(hash)
    {
        Ok(())
    } else {
        Err(profile_error(
            origin,
            "profile descriptor fingerprint must be a real lowercase SHA-256",
        ))
    }
}

fn valid_guid(guid: &str) -> bool {
    guid.len() == 36
        && guid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        && guid
            .bytes()
            .any(|byte| byte.is_ascii_hexdigit() && byte != b'0')
        && !empty_or_placeholder(guid)
}

fn empty_or_placeholder(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.is_empty() || lower.contains("replace") || lower.contains("example")
}

fn profile_error(origin: &str, reason: &'static str) -> LinkError {
    LinkError::new(ErrorKind::ProtocolProfileMismatch, "invalid vendor profile")
        .with_detail("origin", origin.to_owned())
        .with_detail("reason", reason)
}

fn profile_io_error(path: &Path, error: &std::io::Error) -> LinkError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, "failed to read profile input")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn decode_hex(value: &str) -> Result<Vec<u8>, LinkError> {
    let normalized = value.trim().replace([' ', ':', '-'], "");
    if !normalized.len().is_multiple_of(2)
        || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(LinkError::new(
            ErrorKind::ProtocolProfileMismatch,
            "profile contains invalid hexadecimal bytes",
        ));
    }
    normalized
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16).map_err(|_| {
                LinkError::new(
                    ErrorKind::ProtocolProfileMismatch,
                    "profile contains invalid hexadecimal bytes",
                )
            })
        })
        .collect()
}

fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn lowercase_hash(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use link_core::probe::{DeviceMode, UsbIdentity};
    use serde_json::json;

    use super::{
        ProfileCatalog, VendorProfile, decode_control, encode_control, encode_decoded_control,
    };

    fn identity(hash: &str) -> UsbIdentity {
        UsbIdentity {
            vendor_id: 0x2e1a,
            product_id: 0x4c05,
            device_revision: 0x0200,
            manufacturer: Some("Insta360".into()),
            product: Some("Insta360 Link 2C Pro".into()),
            serial: None,
            topology: "1-2.1".into(),
            descriptor_sha256: hash.into(),
        }
    }

    #[test]
    fn builtin_profile_is_read_only_and_exact() {
        let catalog = ProfileCatalog::load(None).expect("built-in profile");
        let observed = identity("1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c");
        let matched = catalog
            .matching(&observed, DeviceMode::Camera, None)
            .unwrap()
            .unwrap();
        assert_eq!(matched.profile.profile_id, "insta360-link-2c-pro");
        assert!(matched.semantic_read_verified());
        assert!(!matched.semantic_write_authorized());

        let portrait = identity("7a60c8dd0f5e3d83e6c1c1fb245d96e02cc4ea6fdea8c10cc5a2e3b1094a2cc8");
        let portrait = catalog
            .matching(&portrait, DeviceMode::Camera, None)
            .unwrap()
            .unwrap();
        assert_eq!(portrait.profile.profile_id, "insta360-link-2c-pro");
        assert!(portrait.profile.controls.is_empty());

        let mut u_disk =
            identity("8c9226df8b126f700d738b42f38c0163549a37a19753832527ce27742d3d7f2e");
        u_disk.vendor_id = 0x070a;
        u_disk.product_id = 0x4026;
        u_disk.device_revision = 0x0001;
        let u_disk = catalog
            .matching(&u_disk, DeviceMode::UDisk, None)
            .unwrap()
            .unwrap();
        assert_eq!(u_disk.profile.profile_id, "insta360-link-2c-pro");
        assert!(u_disk.profile.controls.is_empty());

        let mismatched =
            identity("2d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c");
        assert!(
            catalog
                .matching(&mismatched, DeviceMode::Camera, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn placeholders_and_unproven_writes_are_rejected() {
        let source = r#"
schema_version = 1
profile_id = "bad"
model = "bad"
status = "verified"

[[match]]
mode = "camera"
usb_vid = 1
usb_pid = 2
bcd_device_min = 3
bcd_device_max = 3
descriptor_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#;
        assert!(VendorProfile::parse(source, "test").is_err());
    }

    #[test]
    fn payload_lengths_and_tail_policies_are_profile_specific() {
        let legacy_52 = VendorProfile::parse(
            include_str!("../../../fixtures/xu-profiles/legacy-52.toml"),
            "legacy-52 fixture",
        )
        .unwrap();
        let control_52 = &legacy_52.controls[0];
        let mut current = vec![0xaa; 52];
        current[48..].fill(0);
        let encoded_52 = encode_control(control_52, "value=287454020", Some(&current)).unwrap();
        assert_eq!(encoded_52.len(), 52);
        assert_eq!(&encoded_52[..48], &current[..48]);
        assert_eq!(&encoded_52[48..], &[0x44, 0x33, 0x22, 0x11]);
        assert_eq!(
            decode_control(control_52, &encoded_52).unwrap(),
            json!({"value": 287454020})
        );

        let legacy_61 = VendorProfile::parse(
            include_str!("../../../fixtures/xu-profiles/legacy-61.toml"),
            "legacy-61 fixture",
        )
        .unwrap();
        let control_61 = &legacy_61.controls[0];
        let encoded_61 = encode_control(control_61, "value=1", None).unwrap();
        assert_eq!(encoded_61.len(), 61);
        assert_eq!(
            &encoded_61[52..],
            &[0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9]
        );
        let decoded_61 = decode_control(control_61, &encoded_61).unwrap();
        assert_eq!(
            encode_decoded_control(control_61, &decoded_61, None).unwrap(),
            encoded_61
        );
    }

    #[test]
    fn firmware_specific_guards_outrank_identity_bootstrap_guards() {
        let observed = identity("1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c");
        let generic = ProfileCatalog::load(None)
            .unwrap()
            .matching(&observed, DeviceMode::Camera, None)
            .unwrap()
            .unwrap()
            .profile
            .clone();
        assert_eq!(
            generic.match_specificity(&observed, DeviceMode::Camera, Some("v1.2.3")),
            Some(0)
        );

        let source = r#"
schema_version = 1
profile_id = "firmware-specific"
model = "Insta360 Link 2C Pro"
status = "read-only"

[[match]]
mode = "camera"
usb_vid = 0x2e1a
usb_pid = 0x4c05
bcd_device_min = 0x0200
bcd_device_max = 0x0200
descriptor_sha256 = "1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c"
firmware = ["v1.2.3"]
"#;
        let specific = VendorProfile::parse(source, "specific fixture").unwrap();
        assert_eq!(
            specific.match_specificity(&observed, DeviceMode::Camera, Some("v1.2.3")),
            Some(1)
        );
        assert_eq!(
            specific.match_specificity(&observed, DeviceMode::Camera, Some("v9")),
            None
        );
    }

    #[test]
    fn exact_firmware_profile_authorizes_trace_matched_camera_controls() {
        let observed = identity("1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c");
        let catalog = ProfileCatalog::load(None).unwrap();
        let matched = catalog
            .matching(&observed, DeviceMode::Camera, Some("v0.2.9.8_build3"))
            .unwrap()
            .unwrap();
        assert_eq!(
            matched.profile.profile_id,
            "insta360-link-2c-pro-v0.2.9.8-build3"
        );
        assert!(matched.semantic_read_verified());
        assert!(matched.semantic_write_authorized());

        let control = matched.profile.control("auto-framing.enabled").unwrap();
        assert!(control.writable);
        assert_eq!(control.stream_requirement, super::StreamRequirement::Open);
        assert_eq!(control.stream_warmup_delay_ms, 1_000);
        assert_eq!(control.verification_delay_ms, 500);
        assert_eq!(control.write_prelude, super::WritePrelude::GetLengthTwice);
        assert_eq!(
            control.stream_format.as_ref().unwrap().video_tuple(),
            link_core::media::VideoTuple {
                fourcc: "MJPG".into(),
                width: 1920,
                height: 1080,
                fps: link_core::probe::Rational {
                    numerator: 30,
                    denominator: 1,
                },
            }
        );

        let mut on_readback = vec![0; 61];
        on_readback[0] = 0x07;
        on_readback[38..42].fill(0xff);
        assert_eq!(decode_control(control, &on_readback).unwrap(), json!("on"));
        on_readback[0] = 0x00;
        assert_eq!(decode_control(control, &on_readback).unwrap(), json!("off"));

        let smart_composition = matched
            .profile
            .control("auto-framing.smart-composition")
            .unwrap();
        assert!(smart_composition.writable);
        assert_eq!(smart_composition.stream_warmup_delay_ms, 1_000);
        assert_eq!(smart_composition.verification_delay_ms, 500);
        assert!(smart_composition.read_modify_write);
        assert_eq!(smart_composition.write_mask, Some(1));
        assert_eq!(
            decode_control(smart_composition, &[0xd4, 0x01]).unwrap(),
            json!("off")
        );
        assert_eq!(
            decode_control(smart_composition, &[0xd5, 0x01]).unwrap(),
            json!("on")
        );
        assert_eq!(
            encode_control(smart_composition, "off", Some(&[0xd5, 0x01])).unwrap(),
            [0xd4, 0x01]
        );
        assert_eq!(
            encode_control(smart_composition, "on", Some(&[0xd1, 0x01])).unwrap(),
            [0xd1, 0x01]
        );

        let hdr = matched.profile.control("image.hdr").unwrap();
        assert!(hdr.writable);
        assert!(hdr.read_modify_write);
        assert_eq!(hdr.write_mask, Some(4));
        assert_eq!(hdr.stream_requirement, super::StreamRequirement::Open);
        assert_eq!(hdr.stream_warmup_delay_ms, 1_000);
        assert_eq!(hdr.verification_delay_ms, 500);
        assert_eq!(hdr.write_prelude, super::WritePrelude::GetLengthTwice);
        assert_eq!(decode_control(hdr, &[0xd1, 0x01]).unwrap(), json!("off"));
        assert_eq!(decode_control(hdr, &[0xd5, 0x01]).unwrap(), json!("on"));
        assert_eq!(
            encode_control(hdr, "off", Some(&[0xd5, 0x01])).unwrap(),
            [0xd1, 0x01]
        );
        assert_eq!(
            encode_control(hdr, "on", Some(&[0xd0, 0x01])).unwrap(),
            [0xd4, 0x01]
        );

        let mirror = matched.profile.control("image.mirror").unwrap();
        assert!(mirror.writable);
        assert!(mirror.read_modify_write);
        assert_eq!(mirror.write_mask, Some(8));
        assert_eq!(mirror.stream_requirement, super::StreamRequirement::Open);
        assert_eq!(mirror.stream_warmup_delay_ms, 1_000);
        assert_eq!(mirror.verification_delay_ms, 500);
        assert_eq!(mirror.write_prelude, super::WritePrelude::GetLengthTwice);
        assert_eq!(decode_control(mirror, &[0xd5, 0x01]).unwrap(), json!("off"));
        assert_eq!(decode_control(mirror, &[0xdd, 0x01]).unwrap(), json!("on"));
        assert_eq!(
            encode_control(mirror, "on", Some(&[0xd5, 0x11])).unwrap(),
            [0xdd, 0x11]
        );
        assert_eq!(
            encode_control(mirror, "off", Some(&[0xdd, 0x11])).unwrap(),
            [0xd5, 0x11]
        );

        let flip = matched.profile.control("image.flip").unwrap();
        assert!(flip.writable);
        assert!(flip.read_modify_write);
        assert_eq!(flip.write_mask, Some(4096));
        assert_eq!(flip.stream_requirement, super::StreamRequirement::Open);
        assert_eq!(flip.stream_warmup_delay_ms, 1_000);
        assert_eq!(flip.verification_delay_ms, 500);
        assert_eq!(flip.write_prelude, super::WritePrelude::GetLengthTwice);
        assert_eq!(decode_control(flip, &[0xd5, 0x01]).unwrap(), json!("off"));
        assert_eq!(decode_control(flip, &[0xd5, 0x11]).unwrap(), json!("on"));
        assert_eq!(
            encode_control(flip, "on", Some(&[0xdd, 0x01])).unwrap(),
            [0xdd, 0x11]
        );
        assert_eq!(
            encode_control(flip, "off", Some(&[0xdd, 0x11])).unwrap(),
            [0xdd, 0x01]
        );

        let pickup_mode = matched.profile.control("audio.pickup-mode").unwrap();
        assert!(pickup_mode.writable);
        assert_eq!(pickup_mode.selector, 31);
        assert_eq!(pickup_mode.length, 1);
        assert_eq!(
            pickup_mode.stream_requirement,
            super::StreamRequirement::Either
        );
        assert_eq!(pickup_mode.verification_delay_ms, 250);
        assert_eq!(
            pickup_mode.write_prelude,
            super::WritePrelude::GetLengthTwice
        );
        assert_eq!(
            decode_control(pickup_mode, &[0]).unwrap(),
            json!("standard")
        );
        assert_eq!(decode_control(pickup_mode, &[1]).unwrap(), json!("wide"));
        assert_eq!(decode_control(pickup_mode, &[2]).unwrap(), json!("focus"));
        assert_eq!(
            decode_control(pickup_mode, &[3]).unwrap(),
            json!("original")
        );
        assert_eq!(
            encode_control(pickup_mode, "focus", Some(&[3])).unwrap(),
            [2]
        );

        let exposure = matched.profile.control("image.exposure").unwrap();
        assert_eq!(exposure.selector, 30);
        assert_eq!(exposure.length, 1);
        assert_eq!(exposure.stream_requirement, super::StreamRequirement::Open);
        assert_eq!(exposure.stream_warmup_delay_ms, 1_000);
        assert_eq!(exposure.verification_delay_ms, 250);
        assert_eq!(exposure.write_prelude, super::WritePrelude::GetLengthTwice);
        assert_eq!(decode_control(exposure, &[1]).unwrap(), json!("manual"));
        assert_eq!(decode_control(exposure, &[2]).unwrap(), json!("auto"));
        assert_eq!(encode_control(exposure, "manual", Some(&[2])).unwrap(), [1]);

        let iso = matched.profile.control("image.exposure.iso").unwrap();
        assert_eq!(iso.selector, 25);
        assert_eq!(iso.length, 2);
        assert_eq!(iso.verification_delay_ms, 250);
        assert_eq!(decode_control(iso, &[0x40, 0x01]).unwrap(), json!(320));
        assert_eq!(
            encode_control(iso, "3200", Some(&[0x40, 0x01])).unwrap(),
            [0x80, 0x0c]
        );

        let shutter = matched
            .profile
            .control("image.exposure.shutter-denominator")
            .unwrap();
        assert_eq!(shutter.selector, 29);
        assert_eq!(shutter.length, 2);
        assert_eq!(shutter.verification_delay_ms, 250);
        assert_eq!(shutter.readback_tolerance, 1);
        assert_eq!(decode_control(shutter, &[0x64, 0]).unwrap(), json!(100));
        assert_eq!(
            encode_control(shutter, "8000", Some(&[0x64, 0])).unwrap(),
            [0x40, 0x1f]
        );
        assert!(matched.profile.control("image.exposure.curve").is_none());

        let exposure_compensation = matched
            .profile
            .control("image.exposure_compensation")
            .unwrap();
        assert_eq!(exposure_compensation.selector, 9);
        assert_eq!(exposure_compensation.length, 2);
        assert_eq!(
            exposure_compensation.stream_requirement,
            super::StreamRequirement::Open
        );
        assert_eq!(exposure_compensation.stream_warmup_delay_ms, 1_000);
        assert_eq!(exposure_compensation.verification_delay_ms, 250);
        assert_eq!(
            exposure_compensation.write_prelude,
            super::WritePrelude::GetLengthTwice
        );
        assert_eq!(
            decode_control(exposure_compensation, &[0xd4, 0xfe]).unwrap(),
            json!(-300)
        );
        assert_eq!(
            encode_control(exposure_compensation, "300", Some(&[0, 0])).unwrap(),
            [0x2c, 0x01]
        );

        let mut invalid_tolerance = matched.profile.clone();
        invalid_tolerance
            .controls
            .iter_mut()
            .find(|control| control.name == "image.exposure")
            .unwrap()
            .readback_tolerance = 1;
        assert!(invalid_tolerance.validate("invalid tolerance").is_err());

        let style = matched.profile.control("auto-framing.style").unwrap();
        assert!(style.writable);
        assert_eq!(style.stream_warmup_delay_ms, 1_000);
        assert_eq!(style.verification_delay_ms, 500);
        assert_eq!(decode_control(style, &[0x01]).unwrap(), json!("head"));
        assert_eq!(decode_control(style, &[0x02]).unwrap(), json!("half-body"));

        let bootstrap = catalog
            .matching(&observed, DeviceMode::Camera, Some("unknown"))
            .unwrap()
            .unwrap();
        assert_eq!(bootstrap.profile.profile_id, "insta360-link-2c-pro");
        assert!(!bootstrap.semantic_write_authorized());
    }

    #[test]
    fn utf8_firmware_values_trim_nul_padding() {
        let source = r#"
schema_version = 1
profile_id = "firmware-text"
model = "Insta360 Link 2C Pro"
status = "read-only"

[[match]]
mode = "camera"
usb_vid = 0x2e1a
usb_pid = 0x4c05
bcd_device_min = 0x0200
bcd_device_max = 0x0200
descriptor_sha256 = "1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c"

[[controls]]
name = "firmware.version"
entity_guid = "faf1672d-b71b-4793-8c91-7b1c9b7f95f8"
selector = 1
length = 16
readable = true
writable = false
codec = "utf8"
offset = 2
width = 8
"#;
        let profile = VendorProfile::parse(source, "firmware fixture").unwrap();
        let mut payload = vec![0; 16];
        payload[2..9].copy_from_slice(b"v1.2.3\0");
        assert_eq!(
            decode_control(&profile.controls[0], &payload).unwrap(),
            json!("v1.2.3")
        );
    }
}
