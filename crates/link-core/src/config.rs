//! Strict, layered application configuration.

use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{ErrorKind, LinkError, SCHEMA_VERSION};

/// Whether the CLI may contact the daemon.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonMode {
    /// Prefer the daemon when it is available and useful.
    #[default]
    Auto,
    /// Require daemon operation.
    Always,
    /// Never contact the daemon.
    Never,
}

impl FromStr for DaemonMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            _ => Err(format!("expected auto, always, or never; got {value:?}")),
        }
    }
}

/// Command output representation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Concise terminal output.
    #[default]
    Human,
    /// One JSON object.
    Json,
    /// One JSON object per line.
    Jsonl,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            "jsonl" => Ok(Self::Jsonl),
            _ => Err(format!("expected human, json, or jsonl; got {value:?}")),
        }
    }
}

/// Application tracing verbosity.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Disable application logs.
    Off,
    /// Error events only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational events.
    #[default]
    Info,
    /// Debug events.
    Debug,
    /// All trace events.
    Trace,
}

impl LogLevel {
    /// Return the tracing filter directive.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl FromStr for LogLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(format!(
                "expected off, error, warn, info, debug, or trace; got {value:?}"
            )),
        }
    }
}

/// Human-readable duration used by configuration and CLI parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurationValue(Duration);

impl DurationValue {
    /// Construct from a standard duration.
    #[must_use]
    pub const fn new(value: Duration) -> Self {
        Self(value)
    }

    /// Return the standard duration.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl fmt::Display for DurationValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        humantime::format_duration(self.0).fmt(formatter)
    }
}

impl FromStr for DurationValue {
    type Err = humantime::DurationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        humantime::parse_duration(value).map(Self)
    }
}

impl Serialize for DurationValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DurationValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// Safety settings. They may narrow behavior but cannot enable missing backends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    /// User preference for raw XU operations. The current build still denies them.
    pub allow_raw_xu: bool,
    /// Minimum interval intended for future verified XU writes.
    pub minimum_xu_write_interval_ms: u64,
    /// User preference for profile-approved USB reset.
    pub allow_usb_reset: bool,
    /// User preference for driver detach. Normal operation still prohibits it.
    pub allow_driver_detach: bool,
    /// Redact serial numbers by default.
    pub redact_serials: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            allow_raw_xu: false,
            minimum_xu_write_interval_ms: 250,
            allow_usb_reset: false,
            allow_driver_detach: false,
            redact_serials: true,
        }
    }
}

/// Media preferences retained until media backends consume them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// Ordered transport preference.
    pub preferred_transport: Vec<String>,
    /// Default recording container.
    pub default_container: String,
    /// Required free-space threshold, kept in its human-readable form.
    pub disk_free_minimum: String,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            preferred_transport: vec!["H264".into(), "MJPG".into(), "YUYV".into()],
            default_container: "matroska".into(),
            disk_free_minimum: "5GiB".into(),
        }
    }
}

/// Virtual-camera installation preferences.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualCameraConfig {
    /// Whether documentation should recommend exclusive capabilities.
    pub exclusive_caps_recommended: bool,
}

impl Default for VirtualCameraConfig {
    fn default() -> Self {
        Self {
            exclusive_caps_recommended: true,
        }
    }
}

/// Effective application configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Config {
    /// Configuration schema version.
    pub schema_version: u32,
    /// Default device selector.
    pub default_device: Option<String>,
    /// Daemon selection policy.
    pub daemon: DaemonMode,
    /// Default output format.
    pub output: OutputFormat,
    /// Default operation timeout.
    pub timeout: DurationValue,
    /// Optional additional profile directory.
    pub profile_dir: Option<PathBuf>,
    /// Tracing verbosity.
    pub log_level: LogLevel,
    /// Whether terminal color is disabled.
    pub no_color: bool,
    /// Safety settings.
    pub safety: SafetyConfig,
    /// Media settings.
    pub media: MediaConfig,
    /// Virtual-camera settings.
    pub virtual_camera: VirtualCameraConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            default_device: None,
            daemon: DaemonMode::Auto,
            output: OutputFormat::Human,
            timeout: DurationValue::new(Duration::from_secs(3)),
            profile_dir: None,
            log_level: LogLevel::Info,
            no_color: false,
            safety: SafetyConfig::default(),
            media: MediaConfig::default(),
            virtual_camera: VirtualCameraConfig::default(),
        }
    }
}

/// CLI values that override file and environment configuration.
#[derive(Clone, Debug, Default)]
pub struct ConfigOverrides {
    /// Device selector.
    pub default_device: Option<String>,
    /// Daemon policy.
    pub daemon: Option<DaemonMode>,
    /// Output format.
    pub output: Option<OutputFormat>,
    /// Timeout.
    pub timeout: Option<DurationValue>,
    /// Profile directory.
    pub profile_dir: Option<PathBuf>,
    /// Log level.
    pub log_level: Option<LogLevel>,
    /// Disable color when explicitly requested.
    pub no_color: Option<bool>,
}

/// Configuration file locations, injectable for tests.
#[derive(Clone, Debug, Default)]
pub struct ConfigPaths {
    /// Optional system configuration.
    pub system: Option<PathBuf>,
    /// Optional default user configuration.
    pub user: Option<PathBuf>,
    /// Explicit user configuration, which replaces `user` and is required.
    pub explicit: Option<PathBuf>,
    /// Optional per-device user configuration.
    pub device: Option<PathBuf>,
}

impl ConfigPaths {
    fn discover(explicit: Option<PathBuf>, environment: &BTreeMap<String, String>) -> Self {
        let user_base = environment
            .get("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                environment
                    .get("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|value| PathBuf::from(value).join(".config"))
            });

        Self {
            system: Some(PathBuf::from("/etc/linkctl/config.toml")),
            user: user_base.map(|base| base.join("linkctl/config.toml")),
            explicit,
            device: None,
        }
    }
}

/// Deterministic configuration loader.
#[derive(Clone, Debug)]
pub struct ConfigLoader {
    paths: ConfigPaths,
    environment: BTreeMap<String, String>,
    overrides: ConfigOverrides,
}

impl ConfigLoader {
    /// Construct a loader with explicit inputs.
    #[must_use]
    pub fn new(paths: ConfigPaths) -> Self {
        Self {
            paths,
            environment: BTreeMap::new(),
            overrides: ConfigOverrides::default(),
        }
    }

    /// Use an injected environment.
    #[must_use]
    pub fn with_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.environment = environment;
        self
    }

    /// Use CLI overrides.
    #[must_use]
    pub fn with_overrides(mut self, overrides: ConfigOverrides) -> Self {
        self.overrides = overrides;
        self
    }

    /// Add the default per-device layer for one resolved stable identifier.
    pub fn with_device(mut self, stable_id: &str) -> Result<Self, LinkError> {
        validate_stable_id(stable_id)?;
        let user_base = self
            .environment
            .get("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                self.environment
                    .get("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|value| PathBuf::from(value).join(".config"))
            })
            .ok_or_else(|| {
                LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "cannot resolve per-device configuration directory",
                )
            })?;
        self.paths.device = Some(
            user_base
                .join("linkctl/devices")
                .join(format!("{stable_id}.toml")),
        );
        Ok(self)
    }

    /// Build a loader from the current process environment.
    #[must_use]
    pub fn from_process(explicit: Option<PathBuf>, overrides: ConfigOverrides) -> Self {
        let environment: BTreeMap<String, String> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        let selected = explicit.or_else(|| environment.get("LINKCTL_CONFIG").map(PathBuf::from));
        let paths = ConfigPaths::discover(selected, &environment);

        Self::new(paths)
            .with_environment(environment)
            .with_overrides(overrides)
    }

    /// Load and merge every configured layer.
    pub fn load(self) -> Result<Config, LinkError> {
        let mut config = Config::default();

        if let Some(path) = &self.paths.system {
            apply_optional_file(&mut config, path)?;
        }
        if let Some(path) = &self.paths.explicit {
            apply_required_file(&mut config, path)?;
        } else if let Some(path) = &self.paths.user {
            apply_optional_file(&mut config, path)?;
        }
        if let Some(path) = &self.paths.device {
            apply_optional_device_file(&mut config, path)?;
        }

        apply_environment(&mut config, &self.environment)?;
        apply_overrides(&mut config, self.overrides);
        Ok(config)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigLayer {
    schema_version: u32,
    default_device: Option<String>,
    daemon: Option<DaemonMode>,
    output: Option<OutputFormat>,
    timeout: Option<DurationValue>,
    profile_dir: Option<PathBuf>,
    log_level: Option<LogLevel>,
    no_color: Option<bool>,
    safety: Option<SafetyLayer>,
    media: Option<MediaLayer>,
    virtual_camera: Option<VirtualCameraLayer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SafetyLayer {
    allow_raw_xu: Option<bool>,
    minimum_xu_write_interval_ms: Option<u64>,
    allow_usb_reset: Option<bool>,
    allow_driver_detach: Option<bool>,
    redact_serials: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaLayer {
    preferred_transport: Option<Vec<String>>,
    default_container: Option<String>,
    disk_free_minimum: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VirtualCameraLayer {
    exclusive_caps_recommended: Option<bool>,
}

fn apply_optional_file(config: &mut Config, path: &Path) -> Result<(), LinkError> {
    match fs::read_to_string(path) {
        Ok(contents) => apply_file_contents(config, path, &contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(file_error(path, &error)),
    }
}

fn apply_optional_device_file(config: &mut Config, path: &Path) -> Result<(), LinkError> {
    match fs::read_to_string(path) {
        Ok(contents) => apply_device_file_contents(config, path, &contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(file_error(path, &error)),
    }
}

fn apply_required_file(config: &mut Config, path: &Path) -> Result<(), LinkError> {
    let contents = fs::read_to_string(path).map_err(|error| file_error(path, &error))?;
    apply_file_contents(config, path, &contents)
}

fn apply_file_contents(config: &mut Config, path: &Path, contents: &str) -> Result<(), LinkError> {
    let layer: ConfigLayer = toml::from_str(contents).map_err(|error| {
        LinkError::new(ErrorKind::InvalidInvocation, "invalid configuration file")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if layer.schema_version != SCHEMA_VERSION {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "unsupported configuration schema",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("requested", u64::from(layer.schema_version))
        .with_detail("supported", u64::from(SCHEMA_VERSION)));
    }

    apply_layer(config, layer);
    Ok(())
}

fn apply_device_file_contents(
    config: &mut Config,
    path: &Path,
    contents: &str,
) -> Result<(), LinkError> {
    let layer: ConfigLayer = toml::from_str(contents).map_err(|error| {
        LinkError::new(
            ErrorKind::InvalidInvocation,
            "invalid per-device configuration file",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    if layer.schema_version != SCHEMA_VERSION {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "unsupported per-device configuration schema",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("requested", u64::from(layer.schema_version))
        .with_detail("supported", u64::from(SCHEMA_VERSION)));
    }
    if layer.default_device.is_some()
        || layer.output.is_some()
        || layer.profile_dir.is_some()
        || layer.log_level.is_some()
        || layer.no_color.is_some()
    {
        return Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "per-device configuration contains a process-wide setting",
        )
        .with_detail("path", path.display().to_string()));
    }
    apply_layer(config, layer);
    Ok(())
}

fn apply_layer(config: &mut Config, layer: ConfigLayer) {
    config.schema_version = layer.schema_version;
    if let Some(value) = layer.default_device {
        config.default_device = Some(value);
    }
    if let Some(value) = layer.daemon {
        config.daemon = value;
    }
    if let Some(value) = layer.output {
        config.output = value;
    }
    if let Some(value) = layer.timeout {
        config.timeout = value;
    }
    if let Some(value) = layer.profile_dir {
        config.profile_dir = Some(value);
    }
    if let Some(value) = layer.log_level {
        config.log_level = value;
    }
    if let Some(value) = layer.no_color {
        config.no_color = value;
    }
    if let Some(safety) = layer.safety {
        if let Some(value) = safety.allow_raw_xu {
            config.safety.allow_raw_xu = value;
        }
        if let Some(value) = safety.minimum_xu_write_interval_ms {
            config.safety.minimum_xu_write_interval_ms = value;
        }
        if let Some(value) = safety.allow_usb_reset {
            config.safety.allow_usb_reset = value;
        }
        if let Some(value) = safety.allow_driver_detach {
            config.safety.allow_driver_detach = value;
        }
        if let Some(value) = safety.redact_serials {
            config.safety.redact_serials = value;
        }
    }
    if let Some(media) = layer.media {
        if let Some(value) = media.preferred_transport {
            config.media.preferred_transport = value;
        }
        if let Some(value) = media.default_container {
            config.media.default_container = value;
        }
        if let Some(value) = media.disk_free_minimum {
            config.media.disk_free_minimum = value;
        }
    }
    if let Some(virtual_camera) = layer.virtual_camera
        && let Some(value) = virtual_camera.exclusive_caps_recommended
    {
        config.virtual_camera.exclusive_caps_recommended = value;
    }
}

fn apply_environment(
    config: &mut Config,
    environment: &BTreeMap<String, String>,
) -> Result<(), LinkError> {
    for (name, value) in environment
        .iter()
        .filter(|(name, _)| name.starts_with("LINKCTL_"))
    {
        match name.as_str() {
            "LINKCTL_CONFIG"
            | "LINKCTL_BACKEND"
            | "LINKCTL_DRY_RUN"
            | "LINKCTL_YES"
            | "LINKCTL_UNSAFE_XU"
            | "LINKCTL_SOURCE_REVISION"
            | "LINKCTL_SCHEMA_VERSION"
            | "LINKCTL_DAEMON_SOCKET"
            | "LINKCTL_DECODER"
            | "LINKCTL_DECODER_DEVICE" => {}
            "LINKCTL_DEFAULT_DEVICE" | "LINKCTL_DEVICE" => {
                config.default_device = Some(value.clone());
            }
            "LINKCTL_DAEMON" => config.daemon = parse_environment(name, value)?,
            "LINKCTL_OUTPUT" | "LINKCTL_FORMAT" => {
                config.output = parse_environment(name, value)?;
            }
            "LINKCTL_TIMEOUT" => config.timeout = parse_environment(name, value)?,
            "LINKCTL_PROFILE_DIR" => config.profile_dir = Some(PathBuf::from(value)),
            "LINKCTL_LOG_LEVEL" => config.log_level = parse_environment(name, value)?,
            "LINKCTL_NO_COLOR" => config.no_color = parse_environment(name, value)?,
            "LINKCTL_SAFETY__ALLOW_RAW_XU" => {
                config.safety.allow_raw_xu = parse_environment(name, value)?;
            }
            "LINKCTL_SAFETY__MINIMUM_XU_WRITE_INTERVAL_MS" => {
                config.safety.minimum_xu_write_interval_ms = parse_environment(name, value)?;
            }
            "LINKCTL_SAFETY__ALLOW_USB_RESET" => {
                config.safety.allow_usb_reset = parse_environment(name, value)?;
            }
            "LINKCTL_SAFETY__ALLOW_DRIVER_DETACH" => {
                config.safety.allow_driver_detach = parse_environment(name, value)?;
            }
            "LINKCTL_SAFETY__REDACT_SERIALS" => {
                config.safety.redact_serials = parse_environment(name, value)?;
            }
            "LINKCTL_MEDIA__PREFERRED_TRANSPORT" => {
                let transports: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
                    .collect();
                if transports.is_empty() {
                    return Err(environment_error(
                        name,
                        value,
                        "expected a comma-separated list",
                    ));
                }
                config.media.preferred_transport = transports;
            }
            "LINKCTL_MEDIA__DEFAULT_CONTAINER" => {
                config.media.default_container = value.clone();
            }
            "LINKCTL_MEDIA__DISK_FREE_MINIMUM" => {
                config.media.disk_free_minimum = value.clone();
            }
            "LINKCTL_VIRTUAL_CAMERA__EXCLUSIVE_CAPS_RECOMMENDED" => {
                config.virtual_camera.exclusive_caps_recommended = parse_environment(name, value)?;
            }
            _ => {
                return Err(LinkError::new(
                    ErrorKind::InvalidInvocation,
                    "unknown LINKCTL_ environment variable",
                )
                .with_detail("variable", name.clone()));
            }
        }
    }
    Ok(())
}

fn parse_environment<T>(name: &str, value: &str) -> Result<T, LinkError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|error| environment_error(name, value, &error.to_string()))
}

fn environment_error(name: &str, value: &str, reason: &str) -> LinkError {
    LinkError::new(
        ErrorKind::InvalidInvocation,
        "invalid LINKCTL_ environment variable",
    )
    .with_detail("variable", name.to_owned())
    .with_detail("value", value.to_owned())
    .with_detail("reason", reason.to_owned())
}

fn apply_overrides(config: &mut Config, overrides: ConfigOverrides) {
    if let Some(value) = overrides.default_device {
        config.default_device = Some(value);
    }
    if let Some(value) = overrides.daemon {
        config.daemon = value;
    }
    if let Some(value) = overrides.output {
        config.output = value;
    }
    if let Some(value) = overrides.timeout {
        config.timeout = value;
    }
    if let Some(value) = overrides.profile_dir {
        config.profile_dir = Some(value);
    }
    if let Some(value) = overrides.log_level {
        config.log_level = value;
    }
    if let Some(value) = overrides.no_color {
        config.no_color = value;
    }
}

fn file_error(path: &Path, error: &io::Error) -> LinkError {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, "failed to read configuration file")
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn validate_stable_id(stable_id: &str) -> Result<(), LinkError> {
    if !stable_id.is_empty()
        && stable_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && stable_id != "."
        && stable_id != ".."
    {
        Ok(())
    } else {
        Err(LinkError::new(
            ErrorKind::InvalidInvocation,
            "invalid stable device identifier for configuration lookup",
        )
        .with_detail("stable_id", stable_id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, time::Duration};

    use tempfile::tempdir;

    use super::{
        Config, ConfigLoader, ConfigOverrides, ConfigPaths, DaemonMode, DurationValue, LogLevel,
        OutputFormat,
    };
    use crate::ErrorKind;

    #[test]
    fn defaults_match_the_public_configuration_example() {
        let config = Config::default();

        assert_eq!(config.schema_version, 1);
        assert_eq!(config.daemon, DaemonMode::Auto);
        assert_eq!(config.output, OutputFormat::Human);
        assert_eq!(config.timeout.get(), Duration::from_secs(3));
        assert!(!config.safety.allow_raw_xu);
        assert_eq!(config.safety.minimum_xu_write_interval_ms, 250);
        assert!(config.safety.redact_serials);
    }

    #[test]
    fn layers_apply_in_documented_order() {
        let directory = tempdir().expect("temporary directory");
        let system = directory.path().join("system.toml");
        let user = directory.path().join("user.toml");
        fs::write(
            &system,
            "schema_version = 1\ntimeout = \"4s\"\noutput = \"human\"\n",
        )
        .expect("write system config");
        fs::write(
            &user,
            "schema_version = 1\ntimeout = \"5s\"\noutput = \"json\"\n",
        )
        .expect("write user config");

        let environment = BTreeMap::from([
            ("LINKCTL_TIMEOUT".into(), "6s".into()),
            ("LINKCTL_LOG_LEVEL".into(), "debug".into()),
        ]);
        let overrides = ConfigOverrides {
            timeout: Some(DurationValue::new(Duration::from_secs(7))),
            ..ConfigOverrides::default()
        };
        let config = ConfigLoader::new(ConfigPaths {
            system: Some(system),
            user: Some(user),
            explicit: None,
            device: None,
        })
        .with_environment(environment)
        .with_overrides(overrides)
        .load()
        .expect("configuration should load");

        assert_eq!(config.output, OutputFormat::Json);
        assert_eq!(config.timeout.get(), Duration::from_secs(7));
        assert_eq!(config.log_level, LogLevel::Debug);
    }

    #[test]
    fn explicit_config_replaces_the_default_user_file() {
        let directory = tempdir().expect("temporary directory");
        let user = directory.path().join("user.toml");
        let explicit = directory.path().join("explicit.toml");
        fs::write(&user, "schema_version = 1\noutput = \"json\"\n").expect("write user config");
        fs::write(&explicit, "schema_version = 1\noutput = \"jsonl\"\n")
            .expect("write explicit config");

        let config = ConfigLoader::new(ConfigPaths {
            system: None,
            user: Some(user),
            explicit: Some(explicit),
            device: None,
        })
        .load()
        .expect("configuration should load");

        assert_eq!(config.output, OutputFormat::Jsonl);
    }

    #[test]
    fn unknown_keys_and_schema_versions_are_rejected() {
        let directory = tempdir().expect("temporary directory");
        let unknown = directory.path().join("unknown.toml");
        let schema = directory.path().join("schema.toml");
        fs::write(&unknown, "schema_version = 1\ntypo = true\n").expect("write config");
        fs::write(&schema, "schema_version = 2\n").expect("write config");

        for path in [unknown, schema] {
            let error = ConfigLoader::new(ConfigPaths {
                system: None,
                user: None,
                explicit: Some(path),
                device: None,
            })
            .load()
            .expect_err("configuration must be rejected");
            assert_eq!(error.kind(), ErrorKind::InvalidInvocation);
        }
    }

    #[test]
    fn missing_explicit_file_is_an_io_failure() {
        let directory = tempdir().expect("temporary directory");
        let error = ConfigLoader::new(ConfigPaths {
            system: None,
            user: None,
            explicit: Some(directory.path().join("missing.toml")),
            device: None,
        })
        .load()
        .expect_err("missing explicit config must fail");

        assert_eq!(error.kind(), ErrorKind::IoFailure);
    }

    #[test]
    fn unknown_prefixed_environment_variables_are_rejected() {
        let error = ConfigLoader::new(ConfigPaths::default())
            .with_environment(BTreeMap::from([(
                "LINKCTL_SAFTEY__ALLOW_RAW_XU".into(),
                "true".into(),
            )]))
            .load()
            .expect_err("unknown variable must fail");

        assert_eq!(error.kind(), ErrorKind::InvalidInvocation);
    }

    #[test]
    fn compile_time_source_revision_is_not_runtime_configuration() {
        let config = ConfigLoader::new(ConfigPaths::default())
            .with_environment(BTreeMap::from([(
                "LINKCTL_SOURCE_REVISION".into(),
                "0123456789abcdef".into(),
            )]))
            .load()
            .unwrap();

        assert_eq!(config, Config::default());
    }

    #[test]
    fn daemon_runtime_environment_is_not_treated_as_configuration() {
        let config = ConfigLoader::new(ConfigPaths::default())
            .with_environment(BTreeMap::from([
                ("LINKCTL_DAEMON_SOCKET".into(), "/tmp/linkd.sock".into()),
                ("LINKCTL_DECODER".into(), "software".into()),
                (
                    "LINKCTL_DECODER_DEVICE".into(),
                    "/dev/dri/renderD128".into(),
                ),
            ]))
            .load()
            .unwrap();

        assert_eq!(config, Config::default());
    }

    #[test]
    fn per_device_layer_precedes_environment_and_rejects_process_settings() {
        let directory = tempdir().expect("temporary directory");
        let system = directory.path().join("system.toml");
        let user = directory.path().join("user.toml");
        let device = directory.path().join("device.toml");
        fs::write(&system, "schema_version = 1\ntimeout = \"4s\"\n").unwrap();
        fs::write(&user, "schema_version = 1\ntimeout = \"5s\"\n").unwrap();
        fs::write(&device, "schema_version = 1\ntimeout = \"6s\"\n").unwrap();
        let config = ConfigLoader::new(ConfigPaths {
            system: Some(system),
            user: Some(user),
            explicit: None,
            device: Some(device.clone()),
        })
        .with_environment(BTreeMap::from([("LINKCTL_TIMEOUT".into(), "7s".into())]))
        .load()
        .unwrap();
        assert_eq!(config.timeout.get(), Duration::from_secs(7));

        fs::write(&device, "schema_version = 1\noutput = \"json\"\n").unwrap();
        let error = ConfigLoader::new(ConfigPaths {
            system: None,
            user: None,
            explicit: None,
            device: Some(device),
        })
        .load()
        .expect_err("process-wide device setting must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInvocation);
    }
}
