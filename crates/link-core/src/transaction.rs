//! Serializable preset transaction plans, reports, leases, and recovery journals.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ErrorKind, LinkError,
    paths::{AppPaths, path_error},
};

pub const TRANSACTION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransactionStepKind {
    VideoFormat,
    ControlPrerequisite,
    StandardControls,
    AudioGain,
    AudioMute,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionStepPlan {
    pub sequence: u32,
    pub kind: TransactionStepKind,
    pub backend: String,
    pub previous: Value,
    pub requested: Value,
    pub reversible: bool,
    pub no_op: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionPlan {
    pub schema_version: u32,
    pub transaction_id: String,
    pub preset: String,
    pub stable_id: String,
    pub dry_run: bool,
    pub restart_required: bool,
    pub stream_restart_required: bool,
    pub rollback_feasible: bool,
    pub steps: Vec<TransactionStepPlan>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransactionStepStatus {
    Pending,
    Skipped,
    Applied,
    Verified,
    Failed,
    RolledBack,
    RollbackFailed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionStepReport {
    pub sequence: u32,
    pub status: TransactionStepStatus,
    pub observed: Option<Value>,
    pub error: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransactionOutcome {
    Planned,
    InProgress,
    Completed,
    RolledBack,
    Partial,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionReport {
    pub schema_version: u32,
    pub plan: TransactionPlan,
    pub outcome: TransactionOutcome,
    pub steps: Vec<TransactionStepReport>,
    pub rollback_attempted: bool,
    pub rollback_failures: Vec<String>,
    pub journal: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecoveryJournal {
    pub schema_version: u32,
    pub updated_unix_ms: u128,
    pub report: TransactionReport,
}

impl RecoveryJournal {
    #[must_use]
    pub fn new(report: TransactionReport) -> Self {
        Self {
            schema_version: TRANSACTION_SCHEMA_VERSION,
            updated_unix_ms: now_unix_ms(),
            report,
        }
    }
}

/// Cross-process ownership for one device or audio endpoint.
pub struct DeviceLease {
    _file: File,
    pub path: PathBuf,
}

impl DeviceLease {
    pub fn acquire(
        paths: &AppPaths,
        key: &str,
        operation: &str,
        timeout: Duration,
    ) -> Result<Self, LinkError> {
        AppPaths::ensure_private(&paths.runtime)?;
        let digest = Sha256::digest(key.as_bytes());
        let suffix = digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = paths.runtime.join(format!("device-{suffix}.lock"));
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .truncate(false)
            .open(&path)
            .map_err(|error| path_error("failed to open device lease", &path, &error))?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(LinkError::new(
                        ErrorKind::DeviceBusy,
                        "another linkctl operation owns the selected device",
                    )
                    .with_detail("lock", path.display().to_string())
                    .with_detail("timeout_ms", timeout.as_millis() as u64));
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(path_error("failed to acquire device lease", &path, &error));
                }
            }
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| file.set_len(0))
            .and_then(|()| {
                writeln!(
                    file,
                    "pid={} operation={operation} key={key}",
                    std::process::id()
                )
            })
            .and_then(|()| file.sync_data())
            .map_err(|error| path_error("failed to record device lease owner", &path, &error))?;
        Ok(Self { _file: file, path })
    }
}

/// Crash-visible transaction journal store.
#[derive(Clone, Debug)]
pub struct JournalStore {
    directory: PathBuf,
}

impl JournalStore {
    pub fn from_process() -> Result<Self, LinkError> {
        Ok(Self::new(
            AppPaths::from_process()?.state.join("transactions"),
        ))
    }

    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    #[must_use]
    pub fn path(&self, stable_id: &str) -> PathBuf {
        let digest = Sha256::digest(stable_id.as_bytes());
        let suffix = digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.directory.join(format!("{suffix}.json"))
    }

    pub fn existing(&self, stable_id: &str) -> Result<Option<RecoveryJournal>, LinkError> {
        let path = self.path(stable_id);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                LinkError::new(ErrorKind::PartialSuccess, "recovery journal is invalid")
                    .with_detail("path", path.display().to_string())
                    .with_detail("reason", error.to_string())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(path_error("failed to read recovery journal", &path, &error)),
        }
    }

    pub fn write(&self, journal: &RecoveryJournal) -> Result<PathBuf, LinkError> {
        AppPaths::ensure_private(&self.directory)?;
        let path = self.path(&journal.report.plan.stable_id);
        let bytes = serde_json::to_vec_pretty(journal).map_err(|error| {
            LinkError::new(ErrorKind::IoFailure, "failed to serialize recovery journal")
                .with_detail("reason", error.to_string())
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory).map_err(|error| {
            path_error(
                "failed to create temporary recovery journal",
                &self.directory,
                &error,
            )
        })?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| temporary.write_all(&bytes))
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| path_error("failed to write recovery journal", &path, &error))?;
        temporary.persist(&path).map_err(|error| {
            path_error("failed to finalize recovery journal", &path, &error.error)
        })?;
        sync_directory(&self.directory)?;
        Ok(path)
    }

    pub fn remove(&self, stable_id: &str) -> Result<(), LinkError> {
        let path = self.path(stable_id);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(path_error(
                "failed to remove recovery journal",
                &path,
                &error,
            )),
        }
    }

    pub fn paths(&self) -> Result<Vec<PathBuf>, LinkError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(path_error(
                    "failed to list recovery journals",
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
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }
}

fn sync_directory(directory: &std::path::Path) -> Result<(), LinkError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| path_error("failed to sync transaction directory", directory, &error))
}

#[must_use]
pub fn new_transaction_id(stable_id: &str, preset: &str) -> String {
    let now = now_unix_ms();
    let digest = Sha256::digest(format!("{stable_id}:{preset}:{now}:{}", std::process::id()));
    format!(
        "tx-{now}-{}",
        digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink, path::PathBuf, time::Duration};

    use tempfile::tempdir;

    use super::{
        DeviceLease, JournalStore, RecoveryJournal, TRANSACTION_SCHEMA_VERSION, TransactionOutcome,
        TransactionPlan, TransactionReport,
    };
    use crate::paths::AppPaths;

    fn report(stable_id: &str) -> TransactionReport {
        TransactionReport {
            schema_version: TRANSACTION_SCHEMA_VERSION,
            plan: TransactionPlan {
                schema_version: TRANSACTION_SCHEMA_VERSION,
                transaction_id: "tx-test".into(),
                preset: "test".into(),
                stable_id: stable_id.into(),
                dry_run: false,
                restart_required: false,
                stream_restart_required: false,
                rollback_feasible: true,
                steps: Vec::new(),
            },
            outcome: TransactionOutcome::Completed,
            steps: Vec::new(),
            rollback_attempted: false,
            rollback_failures: Vec::new(),
            journal: None,
        }
    }

    #[test]
    fn journals_round_trip_and_remove() {
        let directory = tempdir().unwrap();
        let store = JournalStore::new(directory.path().join("transactions"));
        let journal = RecoveryJournal::new(report("camera"));
        store.write(&journal).unwrap();
        assert_eq!(store.existing("camera").unwrap(), Some(journal));
        store.remove("camera").unwrap();
        assert!(store.existing("camera").unwrap().is_none());
    }

    #[test]
    fn device_leases_reject_concurrent_owners() {
        let directory = tempdir().unwrap();
        let paths = AppPaths {
            config: PathBuf::new(),
            state: PathBuf::new(),
            runtime: directory.path().join("runtime"),
        };
        let _first = DeviceLease::acquire(&paths, "camera", "first", Duration::ZERO).unwrap();
        let second = DeviceLease::acquire(&paths, "camera", "second", Duration::ZERO);
        assert!(second.is_err());
    }

    #[test]
    fn device_leases_do_not_follow_lock_file_symlinks() {
        let directory = tempdir().unwrap();
        let paths = AppPaths {
            config: PathBuf::new(),
            state: PathBuf::new(),
            runtime: directory.path().join("runtime"),
        };
        let lease = DeviceLease::acquire(&paths, "camera", "first", Duration::ZERO).unwrap();
        let lock_path = lease.path.clone();
        drop(lease);
        fs::remove_file(&lock_path).unwrap();
        let protected = directory.path().join("protected");
        fs::write(&protected, "unchanged").unwrap();
        symlink(&protected, &lock_path).unwrap();

        assert!(DeviceLease::acquire(&paths, "camera", "second", Duration::ZERO).is_err());
        assert_eq!(fs::read_to_string(protected).unwrap(), "unchanged");
    }
}
