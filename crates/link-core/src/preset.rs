//! Strict, versioned local preset documents and storage.

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

pub const PRESET_SCHEMA_VERSION: u32 = 1;

/// Preset state groups accepted by include/exclude selection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetCategory {
    Video,
    Image,
    Zoom,
    Controls,
    Audio,
}

impl std::str::FromStr for PresetCategory {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "video" => Ok(Self::Video),
            "image" => Ok(Self::Image),
            "zoom" => Ok(Self::Zoom),
            "controls" => Ok(Self::Controls),
            "audio" => Ok(Self::Audio),
            _ => Err(format!(
                "expected video, image, zoom, controls, or audio; got {value:?}"
            )),
        }
    }
}

/// Current fallback behavior. Alternate backends are not available yet.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetFallback {
    #[default]
    Fail,
}

/// Exact target-device requirements checked before any write.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRequirements {
    pub model: String,
    pub usb_vid: Option<u16>,
    pub usb_pid: Option<u16>,
    #[serde(default)]
    pub fallback: PresetFallback,
}

/// Exact video tuple represented with TOML-friendly frame-rate fields.
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

/// Reproducible gain/mute state for one explicit audio control layer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetAudio {
    pub source: String,
    pub layer: AudioControlLayer,
    pub gain_percent: Option<f64>,
    pub mute: Option<bool>,
}

/// One local preset. Standard controls retain exact raw values under canonical names.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub schema_version: u32,
    pub name: String,
    pub description: Option<String>,
    pub requirements: PresetRequirements,
    pub video: Option<PresetVideo>,
    #[serde(default)]
    pub standard_controls: BTreeMap<String, i64>,
    pub audio: Option<PresetAudio>,
}

impl Preset {
    /// Parse the current schema through the version-dispatch boundary.
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

    /// Return canonical TOML suitable for storage and export.
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
        if self.video.is_none() && self.standard_controls.is_empty() && self.audio.is_none() {
            return Err(invalid_preset(
                origin,
                "preset contains no applicable state",
                "video, standard_controls, and audio are all absent",
            ));
        }
        if let Some(video) = &self.video
            && (video.fourcc.len() != 4
                || video.width == 0
                || video.height == 0
                || video.fps_num == 0
                || video.fps_den == 0)
        {
            return Err(invalid_preset(
                origin,
                "preset video tuple is invalid",
                "FourCC must be four characters and dimensions/frame rate must be positive",
            ));
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
            if audio.source.trim().is_empty() {
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
            if audio.gain_percent.is_none() && audio.mute.is_none() {
                return Err(invalid_preset(
                    origin,
                    "preset audio section contains no state",
                    "gain_percent and mute are absent",
                ));
            }
        }
        Ok(())
    }
}

/// Compact metadata returned by list/save/import operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresetSummary {
    pub name: String,
    pub description: Option<String>,
    pub model: String,
    pub has_video: bool,
    pub standard_control_count: usize,
    pub has_audio: bool,
    pub path: PathBuf,
}

/// Atomic local preset repository.
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
        Ok(summary(preset, path))
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
                Ok(summary(&preset, path))
            })
            .collect()
    }

    /// Return the safe store path for a validated preset name.
    pub fn path_for(&self, name: &str) -> Result<PathBuf, LinkError> {
        validate_name(name)?;
        Ok(self.directory.join(format!("{name}.toml")))
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

fn summary(preset: &Preset, path: PathBuf) -> PresetSummary {
    PresetSummary {
        name: preset.name.clone(),
        description: preset.description.clone(),
        model: preset.requirements.model.clone(),
        has_video: preset.video.is_some(),
        standard_control_count: preset.standard_controls.len(),
        has_audio: preset.audio.is_some(),
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
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use tempfile::tempdir;

    use super::{PRESET_SCHEMA_VERSION, Preset, PresetRequirements, PresetStore, PresetVideo};

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
            video: Some(PresetVideo {
                fourcc: "MJPG".into(),
                width: 1920,
                height: 1080,
                fps_num: 30,
                fps_den: 1,
            }),
            standard_controls: BTreeMap::from([("brightness".into(), 50)]),
            audio: None,
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
    fn names_cannot_escape_the_store() {
        let directory = tempdir().unwrap();
        let store = PresetStore::new(directory.path().join("presets"));
        let mut preset = example("bad");
        for name in ["../bad", "a/b", "", ".."] {
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
