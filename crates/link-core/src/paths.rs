//! User-owned configuration, state, and runtime paths.

use std::{collections::BTreeMap, env, fs, os::unix::fs::PermissionsExt, path::PathBuf};

use crate::{ErrorKind, LinkError};

/// Resolved application directories, injectable for tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
}

impl AppPaths {
    /// Resolve XDG paths from the current process environment.
    pub fn from_process() -> Result<Self, LinkError> {
        let environment = env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        Self::from_environment(&environment)
    }

    /// Resolve XDG paths from an explicit environment.
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Result<Self, LinkError> {
        let home = environment.get("HOME").filter(|value| !value.is_empty());
        let config = xdg_or_home(environment, "XDG_CONFIG_HOME", home, ".config")?.join("linkctl");
        let state =
            xdg_or_home(environment, "XDG_STATE_HOME", home, ".local/state")?.join("linkctl");
        let runtime = environment
            .get("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(|value| PathBuf::from(value).join("linkctl"))
            .unwrap_or_else(|| {
                env::temp_dir().join(format!("linkctl-{}", rustix::process::getuid().as_raw()))
            });
        Ok(Self {
            config,
            state,
            runtime,
        })
    }

    /// Create a private application directory and enforce owner-only access.
    pub fn ensure_private(directory: &std::path::Path) -> Result<(), LinkError> {
        fs::create_dir_all(directory).map_err(|error| {
            path_error("failed to create application directory", directory, &error)
        })?;
        let metadata = fs::symlink_metadata(directory).map_err(|error| {
            path_error("failed to inspect application directory", directory, &error)
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "application directory must be a real directory",
            )
            .with_detail("path", directory.display().to_string()));
        }
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            path_error("failed to secure application directory", directory, &error)
        })
    }
}

fn xdg_or_home(
    environment: &BTreeMap<String, String>,
    variable: &str,
    home: Option<&String>,
    suffix: &str,
) -> Result<PathBuf, LinkError> {
    environment
        .get(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|value| PathBuf::from(value).join(suffix)))
        .ok_or_else(|| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "cannot resolve user application directories",
            )
            .with_detail("missing", format!("{variable} or HOME"))
        })
}

pub(crate) fn path_error(
    message: &'static str,
    path: &std::path::Path,
    error: &std::io::Error,
) -> LinkError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
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
    use std::{collections::BTreeMap, path::PathBuf};

    use super::AppPaths;

    #[test]
    fn xdg_paths_override_home_defaults() {
        let environment = BTreeMap::from([
            ("HOME".into(), "/home/test".into()),
            ("XDG_CONFIG_HOME".into(), "/config".into()),
            ("XDG_STATE_HOME".into(), "/state".into()),
            ("XDG_RUNTIME_DIR".into(), "/runtime".into()),
        ]);
        let paths = AppPaths::from_environment(&environment).unwrap();
        assert_eq!(paths.config, PathBuf::from("/config/linkctl"));
        assert_eq!(paths.state, PathBuf::from("/state/linkctl"));
        assert_eq!(paths.runtime, PathBuf::from("/runtime/linkctl"));
    }
}
