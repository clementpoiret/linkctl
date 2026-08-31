//! Strict semantic preset documents, immutable built-ins, and atomic local storage.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    ErrorKind, LinkError,
    audio::AudioControlLayer,
    media::VideoTuple,
    paths::{AppPaths, path_error},
};

pub const PRESET_SCHEMA_VERSION: u32 = 2;
const BUILTIN_DEFAULT: &str = include_str!("../../../presets/builtin/default.toml");

/// Preset state groups accepted by include/exclude selection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetCategory {
    Video,
    Camera,
    Image,
    Zoom,
    Controls,
    Audio,
    Gestures,
}

impl std::str::FromStr for PresetCategory {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "video" => Ok(Self::Video),
            "camera" => Ok(Self::Camera),
            "image" => Ok(Self::Image),
            "zoom" => Ok(Self::Zoom),
            "controls" => Ok(Self::Controls),
            "audio" => Ok(Self::Audio),
            "gestures" => Ok(Self::Gestures),
            _ => Err(format!(
                "expected video, camera, image, zoom, controls, audio, or gestures; got {value:?}"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetFallback {
    #[default]
    Fail,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRequirements {
    pub model: String,
    pub usb_vid: Option<u16>,
    pub usb_pid: Option<u16>,
    #[serde(default)]
    pub fallback: PresetFallback,
}

/// Preset-wide safety policy. Omitted state is always preserved.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetPolicy {
    #[serde(default)]
    pub allow_restart: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetVideo {
    pub fourcc: String,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

impl From<VideoTuple> for PresetVideo {
    fn from(value: VideoTuple) -> Self {
        Self {
            fourcc: value.fourcc,
            width: value.width,
            height: value.height,
            fps_num: value.fps.numerator,
            fps_den: value.fps.denominator,
        }
    }
}

impl From<&PresetVideo> for VideoTuple {
    fn from(value: &PresetVideo) -> Self {
        Self {
            fourcc: value.fourcc.clone(),
            width: value.width,
            height: value.height,
            fps: crate::probe::Rational {
                numerator: value.fps_num,
                denominator: value.fps_den,
            },
        }
        .normalized()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetCameraMode {
    Normal,
    AutoFraming,
    Whiteboard,
    Deskview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetFramingStyle {
    Head,
    HalfBody,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetCompatibility {
    Standard,
    LowResolution,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetCamera {
    pub mode: Option<PresetCameraMode>,
    pub framing_style: Option<PresetFramingStyle>,
    pub deskview_vertical_correction: Option<u8>,
    pub compatibility: Option<PresetCompatibility>,
    pub native_portrait: Option<bool>,
}

impl PresetCamera {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mode.is_none()
            && self.framing_style.is_none()
            && self.deskview_vertical_correction.is_none()
            && self.compatibility.is_none()
            && self.native_portrait.is_none()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetExposureMode {
    Auto,
    Manual,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetWhiteBalanceMode {
    Auto,
    Manual,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetFocusMode {
    Auto,
    Manual,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetAntiFlicker {
    Disabled,
    #[serde(rename = "50hz")]
    FiftyHz,
    #[serde(rename = "60hz")]
    SixtyHz,
}

/// Semantic image state. Every absent field is preserved.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetImage {
    pub hdr: Option<bool>,
    pub mirror: Option<bool>,
    pub flip: Option<bool>,
    pub exposure: Option<PresetExposureMode>,
    pub iso: Option<i64>,
    pub shutter: Option<String>,
    pub exposure_compensation_ev: Option<f64>,
    pub white_balance: Option<PresetWhiteBalanceMode>,
    pub white_balance_kelvin: Option<i64>,
    pub focus: Option<PresetFocusMode>,
    pub focus_position: Option<f64>,
    pub zoom: Option<f64>,
    pub anti_flicker: Option<PresetAntiFlicker>,
}

impl PresetImage {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hdr.is_none()
            && self.mirror.is_none()
            && self.flip.is_none()
            && self.exposure.is_none()
            && self.iso.is_none()
            && self.shutter.is_none()
            && self.exposure_compensation_ev.is_none()
            && self.white_balance.is_none()
            && self.white_balance_kelvin.is_none()
            && self.focus.is_none()
            && self.focus_position.is_none()
            && self.zoom.is_none()
            && self.anti_flicker.is_none()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetPickupMode {
    Standard,
    Wide,
    Focus,
    Original,
}

/// Camera pickup mode and one explicit gain/mute layer.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetAudio {
    pub pickup_mode: Option<PresetPickupMode>,
    pub source: Option<String>,
    pub layer: Option<AudioControlLayer>,
    pub gain_percent: Option<f64>,
    pub mute: Option<bool>,
}

impl PresetAudio {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pickup_mode.is_none() && self.gain_percent.is_none() && self.mute.is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetGestures {
    pub palm: Option<bool>,
    pub v_sign: Option<bool>,
    pub l_sign: Option<bool>,
}

impl PresetGestures {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.palm.is_none() && self.v_sign.is_none() && self.l_sign.is_none()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetControlMarker {
    Default,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PresetControlValue {
    Raw(i64),
    Marker(PresetControlMarker),
}

/// One semantic preset. Unspecified fields are preserved during application.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub schema_version: u32,
    pub name: String,
    pub description: Option<String>,
    pub requirements: PresetRequirements,
    #[serde(default)]
    pub policy: PresetPolicy,
    pub video: Option<PresetVideo>,
    pub camera: Option<PresetCamera>,
    pub image: Option<PresetImage>,
    #[serde(default)]
    pub standard_controls: BTreeMap<String, PresetControlValue>,
    pub audio: Option<PresetAudio>,
    pub gestures: Option<PresetGestures>,
}

impl Preset {
    pub fn parse(source: &str, origin: &str) -> Result<Self, LinkError> {
        #[derive(Deserialize)]
        struct VersionOnly {
            schema_version: u32,
        }
        let version: VersionOnly = toml::from_str(source).map_err(|error| {
            invalid_preset(origin, "invalid preset document", error.to_string())
        })?;
        if version.schema_version != PRESET_SCHEMA_VERSION {
            return Err(invalid_preset(
                origin,
                "unsupported preset schema",
                format!(
                    "requested {}, supported {}",
                    version.schema_version, PRESET_SCHEMA_VERSION
                ),
            ));
        }
        let preset: Self = toml::from_str(source).map_err(|error| {
            invalid_preset(origin, "invalid preset document", error.to_string())
        })?;
        preset.validate(origin)?;
        Ok(preset)
    }

    pub fn to_toml(&self) -> Result<String, LinkError> {
        self.validate("memory")?;
        toml::to_string_pretty(self).map_err(|error| {
            invalid_preset("memory", "failed to serialize preset", error.to_string())
        })
    }

    pub fn validate(&self, origin: &str) -> Result<(), LinkError> {
        if self.schema_version != PRESET_SCHEMA_VERSION {
            return Err(invalid_preset(
                origin,
                "unsupported preset schema",
                self.schema_version.to_string(),
            ));
        }
        validate_name(&self.name)?;
        if self.requirements.model.trim().is_empty() {
            return Err(invalid_preset(
                origin,
                "preset model requirement cannot be empty",
                "empty model",
            ));
        }
        if self.video.is_none()
            && self.camera.as_ref().is_none_or(PresetCamera::is_empty)
            && self.image.as_ref().is_none_or(PresetImage::is_empty)
            && self.standard_controls.is_empty()
            && self.audio.as_ref().is_none_or(PresetAudio::is_empty)
            && self.gestures.as_ref().is_none_or(PresetGestures::is_empty)
        {
            return Err(invalid_preset(
                origin,
                "preset contains no applicable state",
                "all state sections are absent or empty",
            ));
        }
        if let Some(video) = &self.video
            && (video.fourcc.len() != 4
                || !video.fourcc.is_ascii()
                || video.width == 0
                || video.height == 0
                || video.fps_num == 0
                || video.fps_den == 0)
        {
            return Err(invalid_preset(
                origin,
                "preset video tuple is invalid",
                "FourCC must be four ASCII characters and dimensions/frame rate must be positive",
            ));
        }
        if let Some(camera) = &self.camera {
            if camera.is_empty() {
                return Err(invalid_preset(
                    origin,
                    "preset camera section is empty",
                    "empty camera",
                ));
            }
            if camera.framing_style.is_some() && camera.mode != Some(PresetCameraMode::AutoFraming)
            {
                return Err(invalid_preset(
                    origin,
                    "framing style requires auto-framing camera mode",
                    "camera.mode must be auto-framing",
                ));
            }
            if let Some(value) = camera.deskview_vertical_correction
                && (camera.mode != Some(PresetCameraMode::Deskview) || !(10..=80).contains(&value))
            {
                return Err(invalid_preset(
                    origin,
                    "DeskView correction requires DeskView mode and a value from 10 through 80",
                    value.to_string(),
                ));
            }
            if camera.native_portrait == Some(true)
                && camera.compatibility == Some(PresetCompatibility::LowResolution)
            {
                return Err(invalid_preset(
                    origin,
                    "native portrait and low-resolution compatibility cannot be combined",
                    "unverified USB personality",
                ));
            }
        }
        if let Some(image) = &self.image {
            validate_image(image, origin)?;
        }
        for name in self.standard_controls.keys() {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(invalid_preset(
                    origin,
                    "preset control name is not canonical",
                    name.clone(),
                ));
            }
        }
        if let Some(audio) = &self.audio {
            validate_audio(audio, origin)?;
        }
        if self.gestures.as_ref().is_some_and(PresetGestures::is_empty) {
            return Err(invalid_preset(
                origin,
                "preset gestures section is empty",
                "empty gestures",
            ));
        }
        Ok(())
    }
}

fn validate_image(image: &PresetImage, origin: &str) -> Result<(), LinkError> {
    if image.is_empty() {
        return Err(invalid_preset(
            origin,
            "preset image section is empty",
            "empty image",
        ));
    }
    match image.exposure {
        Some(PresetExposureMode::Auto) if image.iso.is_some() || image.shutter.is_some() => {
            return Err(invalid_preset(
                origin,
                "automatic exposure cannot include manual values",
                "iso or shutter is present",
            ));
        }
        Some(PresetExposureMode::Manual) if image.iso.is_none() && image.shutter.is_none() => {
            return Err(invalid_preset(
                origin,
                "manual exposure requires ISO, shutter, or both",
                "manual exposure has no value",
            ));
        }
        None if image.iso.is_some() || image.shutter.is_some() => {
            return Err(invalid_preset(
                origin,
                "manual exposure values require an exposure mode",
                "image.exposure is absent",
            ));
        }
        _ => {}
    }
    if image
        .iso
        .is_some_and(|value| !(100..=3200).contains(&value))
    {
        return Err(invalid_preset(
            origin,
            "preset ISO is outside the verified range",
            "expected 100 through 3200",
        ));
    }
    if image
        .shutter
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(invalid_preset(
            origin,
            "preset shutter is empty",
            "empty shutter",
        ));
    }
    if let Some(ev) = image.exposure_compensation_ev {
        let stepped = (ev * 10.0).round();
        if !ev.is_finite() || !(-3.0..=3.0).contains(&ev) || (ev * 10.0 - stepped).abs() > 1e-9 {
            return Err(invalid_preset(
                origin,
                "preset exposure compensation must be -3.0 through +3.0 EV in 0.1 EV steps",
                ev.to_string(),
            ));
        }
    }
    match image.white_balance {
        Some(PresetWhiteBalanceMode::Auto) if image.white_balance_kelvin.is_some() => {
            return Err(invalid_preset(
                origin,
                "automatic white balance cannot include Kelvin",
                "white_balance_kelvin is present",
            ));
        }
        Some(PresetWhiteBalanceMode::Manual) if image.white_balance_kelvin.is_none() => {
            return Err(invalid_preset(
                origin,
                "manual white balance requires Kelvin",
                "white_balance_kelvin is absent",
            ));
        }
        None if image.white_balance_kelvin.is_some() => {
            return Err(invalid_preset(
                origin,
                "Kelvin requires a white-balance mode",
                "image.white_balance is absent",
            ));
        }
        _ => {}
    }
    if image
        .white_balance_kelvin
        .is_some_and(|value| !(2000..=10000).contains(&value))
    {
        return Err(invalid_preset(
            origin,
            "preset white balance is outside the product range",
            "expected 2000 through 10000 Kelvin",
        ));
    }
    match image.focus {
        Some(PresetFocusMode::Auto) if image.focus_position.is_some() => {
            return Err(invalid_preset(
                origin,
                "automatic focus cannot include a manual position",
                "focus_position is present",
            ));
        }
        Some(PresetFocusMode::Manual) if image.focus_position.is_none() => {
            return Err(invalid_preset(
                origin,
                "manual focus requires a position",
                "focus_position is absent",
            ));
        }
        None if image.focus_position.is_some() => {
            return Err(invalid_preset(
                origin,
                "manual focus position requires a focus mode",
                "image.focus is absent",
            ));
        }
        _ => {}
    }
    if image
        .focus_position
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(invalid_preset(
            origin,
            "preset focus position must be normalized from 0.0 through 1.0",
            "invalid focus_position",
        ));
    }
    if image
        .zoom
        .is_some_and(|value| !value.is_finite() || !(1.0..=4.0).contains(&value))
    {
        return Err(invalid_preset(
            origin,
            "preset zoom must be from 1.0x through 4.0x",
            "invalid zoom",
        ));
    }
    Ok(())
}

fn validate_audio(audio: &PresetAudio, origin: &str) -> Result<(), LinkError> {
    if audio.is_empty() {
        return Err(invalid_preset(
            origin,
            "preset audio section is empty",
            "empty audio",
        ));
    }
    let has_layer_state = audio.gain_percent.is_some() || audio.mute.is_some();
    if has_layer_state && (audio.source.is_none() || audio.layer.is_none()) {
        return Err(invalid_preset(
            origin,
            "preset gain/mute requires an explicit source and layer",
            "source or layer is absent",
        ));
    }
    if !has_layer_state && (audio.source.is_some() || audio.layer.is_some()) {
        return Err(invalid_preset(
            origin,
            "preset audio source/layer requires gain or mute state",
            "source or layer has no associated state",
        ));
    }
    if audio
        .source
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(invalid_preset(
            origin,
            "preset audio source cannot be empty",
            "empty source",
        ));
    }
    if audio
        .gain_percent
        .is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
    {
        return Err(invalid_preset(
            origin,
            "preset audio gain must be between 0 and 100 percent",
            "out-of-range gain",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetOrigin {
    Builtin,
    Local,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresetSummary {
    pub id: String,
    pub name: String,
    pub origin: PresetOrigin,
    pub description: Option<String>,
    pub model: String,
    pub has_video: bool,
    pub has_camera: bool,
    pub has_image: bool,
    pub standard_control_count: usize,
    pub has_audio: bool,
    pub has_gestures: bool,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ResolvedPreset {
    pub id: String,
    pub origin: PresetOrigin,
    pub preset: Preset,
}

#[derive(Clone, Debug)]
pub struct PresetStore {
    directory: PathBuf,
}

impl PresetStore {
    pub fn from_process() -> Result<Self, LinkError> {
        Ok(Self::new(AppPaths::from_process()?.config.join("presets")))
    }

    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn load(&self, name: &str) -> Result<Preset, LinkError> {
        reject_symlink(&self.directory)?;
        let path = self.path_for(name)?;
        reject_symlink(&path)?;
        let source = fs::read_to_string(&path).map_err(|error| {
            let message = if error.kind() == std::io::ErrorKind::NotFound {
                "preset was not found"
            } else {
                "failed to read preset"
            };
            path_error(message, &path, &error)
        })?;
        Preset::parse(&source, &path.display().to_string())
    }

    pub fn save(&self, preset: &Preset) -> Result<PresetSummary, LinkError> {
        preset.validate("memory")?;
        let path = self.path_for(&preset.name)?;
        write_atomic_new(&path, preset.to_toml()?.as_bytes(), true)?;
        Ok(summary(
            &preset.name,
            preset,
            PresetOrigin::Local,
            Some(path),
        ))
    }

    pub fn import(&self, source_path: &Path) -> Result<PresetSummary, LinkError> {
        reject_symlink(source_path)?;
        let source = fs::read_to_string(source_path)
            .map_err(|error| path_error("failed to read preset import", source_path, &error))?;
        let preset = Preset::parse(&source, &source_path.display().to_string())?;
        self.save(&preset)
    }

    pub fn export(&self, name: &str, destination: &Path) -> Result<PathBuf, LinkError> {
        let preset = self.load(name)?;
        write_atomic_new(destination, preset.to_toml()?.as_bytes(), false)?;
        Ok(destination.to_owned())
    }

    pub fn delete(&self, name: &str) -> Result<PathBuf, LinkError> {
        reject_symlink(&self.directory)?;
        let path = self.path_for(name)?;
        reject_symlink(&path)?;
        fs::remove_file(&path)
            .map_err(|error| path_error("failed to delete preset", &path, &error))?;
        fs::File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                path_error("failed to sync preset directory", &self.directory, &error)
            })?;
        Ok(path)
    }

    pub fn list(&self) -> Result<Vec<PresetSummary>, LinkError> {
        reject_symlink(&self.directory)?;
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(path_error(
                    "failed to list preset directory",
                    &self.directory,
                    &error,
                ));
            }
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "toml")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                reject_symlink(&path)?;
                let source = fs::read_to_string(&path)
                    .map_err(|error| path_error("failed to read preset", &path, &error))?;
                let preset = Preset::parse(&source, &path.display().to_string())?;
                Ok(summary(
                    &preset.name,
                    &preset,
                    PresetOrigin::Local,
                    Some(path),
                ))
            })
            .collect()
    }

    pub fn path_for(&self, name: &str) -> Result<PathBuf, LinkError> {
        validate_name(name)?;
        Ok(self.directory.join(format!("{name}.toml")))
    }
}

/// Immutable built-ins layered above the atomic local store.
#[derive(Clone, Debug)]
pub struct PresetCatalog {
    local: PresetStore,
    builtins: BTreeMap<String, Preset>,
}

impl PresetCatalog {
    pub fn from_process() -> Result<Self, LinkError> {
        Self::new(PresetStore::from_process()?)
    }

    pub fn new(local: PresetStore) -> Result<Self, LinkError> {
        let default = Preset::parse(BUILTIN_DEFAULT, "builtin:default")?;
        Ok(Self {
            local,
            builtins: BTreeMap::from([("default".into(), default)]),
        })
    }

    #[must_use]
    pub fn local(&self) -> &PresetStore {
        &self.local
    }

    pub fn load(&self, id: &str) -> Result<ResolvedPreset, LinkError> {
        if let Some(name) = id.strip_prefix("builtin:") {
            let preset = self.builtins.get(name).cloned().ok_or_else(|| {
                LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "built-in preset was not found",
                )
                .with_detail("id", id.to_owned())
            })?;
            return Ok(ResolvedPreset {
                id: id.to_owned(),
                origin: PresetOrigin::Builtin,
                preset,
            });
        }
        if id.contains(':') {
            return Err(
                LinkError::new(ErrorKind::InvalidInvocation, "unknown preset namespace")
                    .with_detail("id", id.to_owned()),
            );
        }
        Ok(ResolvedPreset {
            id: id.to_owned(),
            origin: PresetOrigin::Local,
            preset: self.local.load(id)?,
        })
    }

    pub fn list(&self) -> Result<Vec<PresetSummary>, LinkError> {
        let mut presets = self
            .builtins
            .iter()
            .map(|(name, preset)| {
                summary(
                    &format!("builtin:{name}"),
                    preset,
                    PresetOrigin::Builtin,
                    None,
                )
            })
            .collect::<Vec<_>>();
        presets.extend(self.local.list()?);
        presets.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(presets)
    }

    pub fn export(&self, id: &str, destination: &Path) -> Result<PathBuf, LinkError> {
        let resolved = self.load(id)?;
        write_atomic_new(destination, resolved.preset.to_toml()?.as_bytes(), false)?;
        Ok(destination.to_owned())
    }

    pub fn delete(&self, id: &str) -> Result<PathBuf, LinkError> {
        if id.starts_with("builtin:") {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "built-in presets are immutable",
            )
            .with_detail("id", id.to_owned()));
        }
        self.local.delete(id)
    }
}

fn validate_name(name: &str) -> Result<(), LinkError> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && name != "."
        && name != "..";
    if valid {
        Ok(())
    } else {
        Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "preset name must contain only ASCII letters, digits, dot, dash, or underscore",
        )
        .with_detail("name", name.to_owned()))
    }
}

fn summary(
    id: &str,
    preset: &Preset,
    origin: PresetOrigin,
    path: Option<PathBuf>,
) -> PresetSummary {
    PresetSummary {
        id: id.into(),
        name: preset.name.clone(),
        origin,
        description: preset.description.clone(),
        model: preset.requirements.model.clone(),
        has_video: preset.video.is_some(),
        has_camera: preset
            .camera
            .as_ref()
            .is_some_and(|value| !value.is_empty()),
        has_image: preset.image.as_ref().is_some_and(|value| !value.is_empty()),
        standard_control_count: preset.standard_controls.len(),
        has_audio: preset.audio.as_ref().is_some_and(|value| !value.is_empty()),
        has_gestures: preset
            .gestures
            .as_ref()
            .is_some_and(|value| !value.is_empty()),
        path,
    }
}

fn reject_symlink(path: &Path) -> Result<(), LinkError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "preset paths must not be symbolic links",
        )
        .with_detail("path", path.display().to_string())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(path_error("failed to inspect preset path", path, &error)),
    }
}

fn write_atomic_new(path: &Path, bytes: &[u8], private_parent: bool) -> Result<(), LinkError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if private_parent {
        AppPaths::ensure_private(parent)?;
    }
    reject_symlink(path)?;
    if path.exists() {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "preset destination already exists",
        )
        .with_detail("path", path.display().to_string()));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| path_error("failed to create temporary preset", parent, &error))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .and_then(|()| temporary.write_all(bytes))
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| path_error("failed to write preset", path, &error))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| path_error("failed to finalize preset", path, &error.error))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| path_error("failed to sync preset directory", parent, &error))?;
    Ok(())
}

fn invalid_preset(
    origin: &str,
    message: impl Into<String>,
    reason: impl Into<String>,
) -> LinkError {
    LinkError::new(ErrorKind::InvalidInvocation, message)
        .with_detail("origin", origin.to_owned())
        .with_detail("reason", reason.into())
}

#[cfg(test)]
mod tests {
    use super::{
        PRESET_SCHEMA_VERSION, Preset, PresetCamera, PresetCameraMode, PresetCatalog,
        PresetCompatibility, PresetControlMarker, PresetControlValue, PresetExposureMode,
        PresetFocusMode, PresetImage, PresetOrigin, PresetPickupMode, PresetPolicy,
        PresetRequirements, PresetStore, PresetVideo, PresetWhiteBalanceMode,
    };
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    use tempfile::tempdir;

    fn example(name: &str) -> Preset {
        Preset {
            schema_version: PRESET_SCHEMA_VERSION,
            name: name.into(),
            description: Some("test".into()),
            requirements: PresetRequirements {
                model: "Insta360 Link 2C Pro".into(),
                usb_vid: Some(0x2e1a),
                usb_pid: Some(0x4c05),
                fallback: Default::default(),
            },
            policy: PresetPolicy::default(),
            video: Some(PresetVideo {
                fourcc: "MJPG".into(),
                width: 1920,
                height: 1080,
                fps_num: 30,
                fps_den: 1,
            }),
            camera: None,
            image: None,
            standard_controls: BTreeMap::from([
                ("brightness".into(), PresetControlValue::Raw(50)),
                (
                    "contrast".into(),
                    PresetControlValue::Marker(PresetControlMarker::Default),
                ),
            ]),
            audio: None,
            gestures: None,
        }
    }

    #[test]
    fn current_schema_round_trips_and_unknown_fields_fail() {
        let preset = example("interview");
        assert_eq!(
            Preset::parse(&preset.to_toml().unwrap(), "test").unwrap(),
            preset
        );
        let source = preset.to_toml().unwrap() + "\nfuture_effect = true\n";
        assert!(Preset::parse(&source, "test").is_err());
        assert!(
            Preset::parse(
                &source.replace("schema_version = 2", "schema_version = 1"),
                "test"
            )
            .is_err()
        );
    }

    #[test]
    fn semantic_dependencies_are_strict() {
        let mut preset = example("strict");
        preset.video = None;
        preset.standard_controls.clear();
        preset.camera = Some(PresetCamera {
            framing_style: Some(super::PresetFramingStyle::Head),
            ..PresetCamera::default()
        });
        assert!(preset.validate("test").is_err());
        preset.camera = Some(PresetCamera {
            mode: Some(PresetCameraMode::AutoFraming),
            framing_style: Some(super::PresetFramingStyle::Head),
            ..PresetCamera::default()
        });
        preset.image = Some(PresetImage {
            exposure: Some(PresetExposureMode::Auto),
            iso: Some(400),
            ..PresetImage::default()
        });
        assert!(preset.validate("test").is_err());
    }

    #[test]
    fn catalog_exposes_one_immutable_builtin() {
        let directory = tempdir().unwrap();
        let catalog =
            PresetCatalog::new(PresetStore::new(directory.path().join("presets"))).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "builtin:default");
        assert_eq!(listed[0].origin, PresetOrigin::Builtin);
        let preset = catalog.load("builtin:default").unwrap().preset;
        let camera = preset.camera.unwrap();
        assert_eq!(camera.mode, Some(PresetCameraMode::Normal));
        assert_eq!(camera.compatibility, Some(PresetCompatibility::Standard));
        assert_eq!(camera.native_portrait, Some(false));
        let image = preset.image.unwrap();
        assert_eq!(image.hdr, Some(true));
        assert_eq!(image.mirror, Some(false));
        assert_eq!(image.flip, Some(false));
        assert_eq!(image.exposure, Some(PresetExposureMode::Auto));
        assert_eq!(image.white_balance, Some(PresetWhiteBalanceMode::Auto));
        assert_eq!(image.focus, Some(PresetFocusMode::Auto));
        assert_eq!(image.anti_flicker, None);
        assert_eq!(
            preset.audio.unwrap().pickup_mode,
            Some(PresetPickupMode::Standard)
        );
        assert!(
            preset
                .standard_controls
                .values()
                .all(|value| *value == PresetControlValue::Marker(PresetControlMarker::Default))
        );
        let gestures = preset.gestures.unwrap();
        assert_eq!(
            (gestures.palm, gestures.v_sign, gestures.l_sign),
            (Some(true), Some(true), Some(true))
        );
        assert!(catalog.delete("builtin:default").is_err());
    }

    #[test]
    fn store_is_atomic_sorted_and_no_clobber() {
        let directory = tempdir().unwrap();
        let store = PresetStore::new(directory.path().join("presets"));
        store.save(&example("z-last")).unwrap();
        store.save(&example("a-first")).unwrap();
        assert!(store.save(&example("a-first")).is_err());
        assert_eq!(
            store
                .list()
                .unwrap()
                .into_iter()
                .map(|item| item.name)
                .collect::<Vec<_>>(),
            ["a-first", "z-last"]
        );
        store.delete("a-first").unwrap();
        assert!(!directory.path().join("presets/a-first.toml").exists());
        let export_directory = directory.path().join("exports");
        fs::create_dir(&export_directory).unwrap();
        fs::set_permissions(&export_directory, fs::Permissions::from_mode(0o750)).unwrap();
        store
            .export("z-last", &export_directory.join("z-last.toml"))
            .unwrap();
        assert_eq!(
            fs::metadata(export_directory).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn names_cannot_escape_the_store_or_use_namespaces() {
        let directory = tempdir().unwrap();
        let store = PresetStore::new(directory.path().join("presets"));
        let mut preset = example("bad");
        for name in ["../bad", "a/b", "builtin:bad", "", ".."] {
            preset.name = name.into();
            assert!(store.save(&preset).is_err());
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_store_cannot_redirect_deletion() {
        let directory = tempdir().unwrap();
        let outside = directory.path().join("outside");
        let outside_store = PresetStore::new(outside.clone());
        outside_store.save(&example("protected")).unwrap();
        let redirected = directory.path().join("redirected");
        symlink(&outside, &redirected).unwrap();
        let redirected_store = PresetStore::new(redirected);
        assert!(redirected_store.delete("protected").is_err());
        assert!(outside.join("protected.toml").exists());
    }
}
